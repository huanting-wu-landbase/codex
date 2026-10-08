//! Render-only suffixes with generation-scoped asynchronous requests and replies.

use std::cell::Cell;
use std::sync::LazyLock;

use pulldown_cmark::Event;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use regex::Regex;

use super::*;
use crate::bottom_pane::textarea::TextAreaState;
use crate::width::display_width;

pub(super) struct WordPrediction {
    draft: String,
    suffix: String,
    pub(super) rendered: Cell<bool>,
}

static WORD: LazyLock<Regex> = LazyLock::new(|| match Regex::new(r"^[\p{L}'][\p{L}\p{M}']*$") {
    Ok(regex) => regex,
    Err(error) => panic!("invalid word prediction regex: {error}"),
});

#[derive(Clone, Copy, Debug)]
pub(crate) struct PredictionTicket {
    pub(crate) composer: uuid::Uuid,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PredictionRequest {
    pub(crate) ticket: PredictionTicket,
    pub(crate) before: String,
    pub(crate) prefix: String,
}

impl ChatComposer {
    pub(super) fn invalidate_word_prediction_request(&self) {
        self.prediction_generation
            .set(self.prediction_generation.get().wrapping_add(1));
        self.prediction_requested.borrow_mut().take();
        self.prediction_pending.set(false);
        #[cfg(unix)]
        if let Some(runtime) = &self.prediction_runtime {
            runtime.request(None);
        }
    }

    pub(crate) fn take_word_prediction_request(&self) -> Option<PredictionRequest> {
        if self.prediction_dismissed.get() || !self.word_prediction_allowed() {
            return None;
        }
        let generation = self.prediction_generation.get();
        let draft = self.draft.textarea.text();
        if self
            .prediction_requested
            .borrow()
            .as_ref()
            .is_some_and(|(g, text)| *g == generation && text == draft)
        {
            return None;
        }
        let prefix = draft.rsplit(char::is_whitespace).next()?;
        let before = &draft[..draft.len() - prefix.len()];
        self.prediction_requested
            .replace(Some((generation, draft.to_owned())));
        self.prediction_pending.set(true);
        Some(PredictionRequest {
            ticket: PredictionTicket {
                composer: self.prediction_id,
                generation,
            },
            before: before.to_owned(),
            prefix: prefix.to_owned(),
        })
    }

    pub(crate) fn apply_word_prediction(
        &mut self,
        ticket: PredictionTicket,
        suffix: Option<String>,
    ) {
        if ticket.composer != self.prediction_id
            || ticket.generation != self.prediction_generation.get()
            || self.prediction_dismissed.get()
            || !self.word_prediction_allowed()
        {
            return;
        }
        let current =
            self.prediction_requested
                .borrow()
                .as_ref()
                .is_some_and(|(generation, draft)| {
                    *generation == ticket.generation && draft == self.draft.textarea.text()
                });
        if !current {
            return;
        }
        self.prediction_pending.set(false);
        let draft = self.draft.textarea.text().to_owned();
        self.word_prediction = None;
        if let Some(suffix) = suffix {
            self.set_word_prediction(&draft, &suffix);
        }
    }

    pub(crate) fn has_word_prediction(&self) -> bool {
        !self.popup_active() && (self.word_prediction.is_some() || self.prediction_pending.get())
    }

    pub(crate) fn dismiss_word_prediction(&mut self) -> bool {
        if !self.has_word_prediction() {
            return false;
        }
        self.invalidate_word_prediction_request();
        self.prediction_dismissed.set(true);
        self.word_prediction = None;
        true
    }

    pub(super) fn schedule_word_prediction(&self, masked: bool) {
        #[cfg(unix)]
        if self.prediction_runtime.is_none() && self.prediction_requested.borrow().is_none() {
            return;
        }
        #[cfg(not(unix))]
        if self.prediction_requested.borrow().is_none() {
            return;
        }
        if masked || !self.word_prediction_allowed() {
            if self.prediction_requested.borrow().is_some() {
                self.invalidate_word_prediction_request();
            }
            return;
        }
        #[cfg(unix)]
        if let Some(runtime) = &self.prediction_runtime
            && let Some(request) = self.take_word_prediction_request()
        {
            runtime.request(Some(request));
        }
    }

    #[cfg(unix)]
    pub(crate) fn enable_word_prediction(&mut self, home: std::path::PathBuf) {
        self.prediction_runtime =
            crate::word_prediction::Runtime::from_env(home, self.app_event_tx.clone());
    }

    #[cfg(unix)]
    pub(crate) fn observe_word_prediction(&self, id: String, text: String) {
        if let Some(runtime) = &self.prediction_runtime {
            runtime.observe(id, text);
        }
    }

    /// Receive a suffix for exactly this draft; stale asynchronous results are ignored.
    pub(crate) fn set_word_prediction(&mut self, draft: &str, suffix: &str) {
        if self.draft.textarea.text() != draft {
            return;
        }
        self.word_prediction = None;
        if !WORD.is_match(suffix) || !self.word_prediction_allowed() {
            return;
        }
        self.word_prediction = Some(WordPrediction {
            draft: draft.to_owned(),
            suffix: suffix.to_owned(),
            rendered: Cell::new(false),
        });
    }

    fn word_prediction_allowed(&self) -> bool {
        let textarea = &self.draft.textarea;
        let draft = textarea.text();
        if !self.has_focus
            || !self.draft.input_enabled
            || self.blocks_direct_input
            || !self.config.popups_enabled
            || self.popup_active()
            || self.draft.is_bash_mode
            || self.is_in_paste_burst()
            || textarea.mouse_selection_range().is_some()
            || (textarea.is_vim_enabled() && !textarea.uses_vim_insert_cursor())
            || textarea.cursor() != draft.len()
            || draft.is_empty()
            || draft.len() > 20_000
            || draft.starts_with(['/', '!'])
        {
            return false;
        }
        let word = draft.rsplit(char::is_whitespace).next().unwrap_or_default();
        if !WORD.is_match(word)
            || word
                .chars()
                .zip(word.chars().skip(1))
                .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
        {
            return false;
        }
        let mut code_block = false;
        for (event, range) in Parser::new(draft).into_offset_iter() {
            match event {
                Event::Start(Tag::CodeBlock(_)) => code_block = true,
                Event::End(TagEnd::CodeBlock) => code_block = false,
                Event::Text(_) if !code_block && range.end == draft.len() => return true,
                _ => {}
            }
        }
        false
    }

    pub(super) fn render_word_prediction(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: TextAreaState,
    ) {
        let Some(prediction) = &self.word_prediction else {
            return;
        };
        if prediction.draft != self.draft.textarea.text() || !self.word_prediction_allowed() {
            return;
        }
        let Some((x, y)) = self.draft.textarea.cursor_pos_with_state(area, state) else {
            return;
        };
        let remaining = area.right().saturating_sub(x);
        let width = display_width(&prediction.suffix);
        if width == 0 || width > usize::from(remaining) {
            return;
        }
        buf.set_stringn(
            x,
            y,
            &prediction.suffix,
            usize::from(remaining),
            Style::default().add_modifier(Modifier::DIM),
        );
        prediction.rendered.set(true);
    }

    pub(super) fn handle_word_prediction_key(
        &mut self,
        key: KeyEvent,
    ) -> Option<(InputResult, bool)> {
        if !key.modifiers.is_empty() || !matches!(key.code, KeyCode::Tab | KeyCode::Right) {
            return None;
        }
        let prediction = self.word_prediction.as_ref()?;
        if !prediction.rendered.get()
            || prediction.draft != self.draft.textarea.text()
            || !self.word_prediction_allowed()
        {
            return None;
        }
        let mut suffix = self.word_prediction.take()?.suffix;
        if key.code == KeyCode::Tab {
            suffix.push(' ');
        }
        let started_edit = self.begin_direct_vim_edit();
        self.draft.textarea.insert_str(&suffix);
        if started_edit {
            self.finish_vim_edit();
        }
        Some((InputResult::None, true))
    }

    pub(super) fn update_word_prediction_after_key(&mut self, key: KeyEvent) {
        let Some(mut prediction) = self.word_prediction.take() else {
            return;
        };
        if !matches!(key.code, KeyCode::Char(_))
            || !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
            || !self.word_prediction_allowed()
        {
            return;
        }
        let text = self.draft.textarea.text();
        let completed = format!("{}{}", prediction.draft, prediction.suffix);
        if text.len() > prediction.draft.len()
            && text.len() < completed.len()
            && completed.starts_with(text)
        {
            prediction.suffix = completed[text.len()..].to_owned();
            prediction.draft = text.to_owned();
            prediction.rendered.set(false);
            self.word_prediction = Some(prediction);
        }
    }
}
