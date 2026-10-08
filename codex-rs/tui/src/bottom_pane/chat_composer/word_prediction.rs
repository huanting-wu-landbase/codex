//! Render-only word suffixes. The prediction worker will supply results through the setter.

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

static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}'][\p{L}\p{M}']*$").expect("valid word regex"));

impl ChatComposer {
    /// Receive a suffix for exactly this draft; stale asynchronous results are ignored.
    /// No prediction source is enabled by this UI-only integration.
    #[allow(dead_code)] // Entry point for the upcoming prediction worker.
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
