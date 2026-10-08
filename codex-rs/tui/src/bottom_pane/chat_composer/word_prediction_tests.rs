use super::tests::new_test_composer;
use super::*;

fn composer_with_prediction() -> ChatComposer {
    let (mut composer, _rx) = new_test_composer();
    composer.set_disable_paste_burst(true);
    let config: codex_config::types::TuiKeymap = toml::from_str(
        "[composer]\nqueue = 'ctrl-q'\n[editor]\ninsert_newline = ['ctrl-j', 'ctrl-m', 'enter', 'shift-enter', 'alt-enter']",
    ).unwrap();
    let keymap = RuntimeKeymap::from_config(&config).unwrap();
    composer.set_keymap_bindings(&keymap);
    composer.set_text_content("please ref".into(), Vec::new(), Vec::new());
    composer
        .draft
        .textarea
        .set_cursor(composer.current_text().len());
    composer.set_word_prediction("please ref", "actor");
    composer
}

#[test]
fn word_prediction_vim_acceptance_can_be_undone() {
    let mut composer = composer_with_prediction();
    composer.set_vim_enabled(true);
    composer.handle_key_event(KeyCode::Char('A').into());
    composer.set_word_prediction("please ref", "actor");
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please refactor ");
    composer.handle_key_event(KeyCode::Esc.into());
    composer.handle_key_event(KeyCode::Char('u').into());
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_vim_normal_and_replace_hide_suffix() {
    for replace in [false, true] {
        let mut composer = composer_with_prediction();
        composer.set_vim_enabled(true);
        if replace {
            composer.handle_key_event(KeyCode::Char('R').into());
        }
        composer
            .draft
            .textarea
            .set_cursor(composer.current_text().len());
        composer.set_word_prediction("please ref", "actor");
        let buffer = paint(&composer, 80);
        let text = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(!text.contains("refactor"));
    }
}

#[test]
fn word_prediction_slash_menu_keeps_tab_priority() {
    let mut composer = composer_with_prediction();
    composer.set_text_content("/mo".into(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor(3);
    composer.sync_popups();
    composer.set_word_prediction("/mo", "del");
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "/model ");
}

#[test]
fn word_prediction_queue_remap_preserves_ctrl_j_newline() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    assert_eq!(composer.current_text(), "please ref\n");
}

fn paint(composer: &ChatComposer, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 8);
    let mut buffer = Buffer::empty(area);
    composer.render(area, &mut buffer);
    buffer
}

#[test]
fn word_prediction_tab_accepts_visible_suffix_with_space() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    let (result, _) = composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(result, InputResult::None);
    assert_eq!(composer.current_text(), "please refactor ");
}

#[test]
fn word_prediction_enter_never_submits_ghost_text() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    let (result, _) = composer.handle_key_event(KeyCode::Enter.into());
    assert!(matches!(result, InputResult::Submitted { text, .. } if text == "please ref"));
}

#[test]
fn word_prediction_remapped_queue_never_submits_ghost_text() {
    let mut composer = composer_with_prediction();
    composer.set_task_running(true);
    paint(&composer, 80);
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert!(matches!(result, InputResult::Queued { text, .. } if text == "please ref"));
}

#[test]
fn word_prediction_is_render_only_and_dim() {
    let composer = composer_with_prediction();
    let buffer = paint(&composer, 80);
    let row = (0..buffer.area.height)
        .find(|&y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains("please refactor")
        })
        .expect("ghost word should be visible");
    let line = (0..buffer.area.width)
        .map(|x| buffer[(x, row)].symbol())
        .collect::<String>();
    let start = line.find("actor").unwrap() as u16;
    assert!(buffer[(start, row)].modifier.contains(Modifier::DIM));
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_hidden_suffix_cannot_be_accepted() {
    for width in [0, 4, 15] {
        let mut composer = composer_with_prediction();
        paint(&composer, 80);
        paint(&composer, width);
        composer.handle_key_event(KeyCode::Tab.into());
        assert_eq!(composer.current_text(), "please ref", "width {width}");
    }
}

#[test]
fn word_prediction_active_paste_burst_cannot_accept() {
    let mut composer = composer_with_prediction();
    composer.set_disable_paste_burst(false);
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Char('x').into());
    composer.handle_key_event(KeyCode::Char('y').into());
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert!(!composer.current_text().contains("refactor"));
}

#[test]
fn word_prediction_disabled_input_cannot_accept() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.set_input_enabled(false, None);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_stale_result_does_not_replace_new_draft() {
    let mut composer = composer_with_prediction();
    composer.set_text_content("please review".into(), Vec::new(), Vec::new());
    composer
        .draft
        .textarea
        .set_cursor(composer.current_text().len());
    composer.set_word_prediction("please ref", "actor");
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please review");
}

#[test]
fn word_prediction_typing_through_preserves_remaining_suffix() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Char('a').into());
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please refactor ");
}

#[test]
fn word_prediction_escape_dismisses_without_changing_draft() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Esc.into());
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_masked_input_cannot_accept() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    let area = Rect::new(0, 0, 80, 8);
    composer.render_with_mask(area, &mut Buffer::empty(area), Some('*'));
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_code_paths_and_commands_are_suppressed() {
    for draft in [
        "/ref",
        "!ref",
        "open src/ref",
        "fix foo_ref",
        "```rust\nref",
        "`ref",
        "<tag>ref",
    ] {
        let mut composer = composer_with_prediction();
        composer.set_text_content(draft.into(), Vec::new(), Vec::new());
        composer
            .draft
            .textarea
            .set_cursor(composer.current_text().len());
        composer.set_word_prediction(draft, "actor");
        let buffer = paint(&composer, 80);
        let text = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(!text.contains("refactor"), "draft {draft}");
    }
}

#[test]
fn word_prediction_unicode_right_accepts_without_space() {
    let mut composer = composer_with_prediction();
    composer.set_text_content("explain ré".into(), Vec::new(), Vec::new());
    composer
        .draft
        .textarea
        .set_cursor(composer.current_text().len());
    composer.set_word_prediction("explain ré", "sumé");
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Right.into());
    assert_eq!(composer.current_text(), "explain résumé");
}

#[test]
fn word_prediction_paste_and_cursor_moves_invalidate() {
    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.handle_paste("a".into());
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please refa");

    let mut composer = composer_with_prediction();
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Left.into());
    composer.handle_key_event(KeyCode::Right.into());
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please ref");
}

#[test]
fn word_prediction_async_reply_is_rejected_after_escape_and_draft_reuse() {
    let mut composer = composer_with_prediction();
    let request = composer.take_word_prediction_request().unwrap();
    composer.handle_key_event(KeyCode::Esc.into());
    composer.apply_word_prediction(request.ticket, Some("actor".into()));
    assert!(composer.word_prediction.is_none());
    assert!(composer.take_word_prediction_request().is_none());
    composer.set_text_content("please ref".into(), Vec::new(), Vec::new());
    composer.apply_word_prediction(request.ticket, Some("actor".into()));
    assert!(composer.word_prediction.is_none());
}

#[test]
fn word_prediction_async_reply_is_scoped_to_composer_and_latest_draft() {
    let mut composer = composer_with_prediction();
    let request = composer.take_word_prediction_request().unwrap();
    assert_eq!(request.before, "please ");
    assert_eq!(request.prefix, "ref");
    let mut other = composer_with_prediction();
    other.word_prediction = None;
    other.apply_word_prediction(request.ticket, Some("actor".into()));
    assert!(other.word_prediction.is_none());
    composer.apply_word_prediction(request.ticket, Some("actor".into()));
    paint(&composer, 80);
    composer.handle_key_event(KeyCode::Tab.into());
    assert_eq!(composer.current_text(), "please refactor ");
}

#[test]
fn word_prediction_masking_invalidates_pending_reply() {
    let mut composer = composer_with_prediction();
    composer.word_prediction = None;
    let request = composer.take_word_prediction_request().unwrap();
    let area = Rect::new(0, 0, 80, 10);
    let mut buffer = Buffer::empty(area);
    composer.render_with_mask(area, &mut buffer, Some('*'));
    composer.apply_word_prediction(request.ticket, Some("actor".into()));
    assert!(composer.word_prediction.is_none());
}

#[test]
fn word_prediction_queue_remap_preserves_shift_enter_and_kitty_alias() {
    for key in [
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
    ] {
        let mut composer = composer_with_prediction();
        paint(&composer, 80);
        let (result, _) = composer.handle_key_event(key);
        assert_eq!(result, InputResult::None);
        assert_eq!(composer.current_text(), "please ref\n");
    }
}
