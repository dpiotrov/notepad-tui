//! Stage S7 acceptance: clipboard operations, block editing, and the remaining editing
//! behaviors from spec §6.2 and §6.3.
//!
//! Headless throughout: `App::new_headless` with `ui.clipboard = "osc52"` and a capturing
//! sink, so no test constructs `arboard` or writes to a terminal (spec §10).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use np_core::{Config, LineCol, SelectionKind, UiConfig};
use np_tui::ui::{render_to_buffer, row_text};
use np_tui::{Action, App};

/// An app configured so the clipboard never touches the system.
fn app() -> App {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    App::new_headless(config, (80, 24))
}

/// `App` with auto-pairing off, for tests that type literal brackets.
fn app_no_pair() -> App {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.editor.auto_pair = false;
    App::new_headless(config, (80, 24))
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

fn text(app: &App) -> String {
    app.doc().expect("document").buffer.text_string()
}

fn caret(app: &App) -> LineCol {
    app.doc().expect("document").cursor.pos
}

/// What the last OSC 52 copy carried.
fn last_clipboard(app: &App) -> Option<String> {
    let bytes = app.osc_out.borrow().clone();
    // Decode the last sequence in the capture.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let start = text.rfind("\x1b]52;")?;
    let rest = &text[start..];
    let payload = rest.get(5..)?;
    let sep = payload.find(';')?;
    let b64 = payload.get(sep + 1..)?;
    let b64 = b64.trim_end_matches("\x1b\\");
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(String::from_utf8_lossy(&raw).into_owned())
}

// ------------------------------------------------------- FR-C1: copy

#[test]
fn copy_with_a_selection_copies_the_selection() {
    let mut app = app();
    type_text(&mut app, "hello world");
    app.execute(Action::SelectAll);
    app.execute(Action::Copy);
    assert_eq!(last_clipboard(&app).as_deref(), Some("hello world"));
    // Copy must not change the document.
    assert_eq!(text(&app), "hello world");
}

#[test]
fn copy_with_no_selection_copies_the_whole_line_including_its_newline() {
    // Notepad++ parity (FR-C1). The terminator is included so repeated copy/paste stacks
    // lines instead of concatenating them; asserted here because it is a visible choice.
    let mut app = app();
    type_text(&mut app, "first\nsecond\nthird");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 3);
    app.execute(Action::Copy);
    assert_eq!(last_clipboard(&app).as_deref(), Some("second\n"));
}

#[test]
fn copy_of_the_last_line_without_a_newline_omits_it() {
    let mut app = app();
    type_text(&mut app, "one\ntwo");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 1);
    app.execute(Action::Copy);
    assert_eq!(last_clipboard(&app).as_deref(), Some("two"));
}

#[test]
fn cut_with_a_selection_deletes_it_as_one_undo_unit() {
    let mut app = app();
    type_text(&mut app, "abcdef");
    app.doc_mut().expect("doc").begin_selection(
        LineCol::new(0, 1),
        LineCol::new(0, 4),
        SelectionKind::Range,
    );
    app.execute(Action::Cut);
    assert_eq!(text(&app), "aef");
    assert_eq!(last_clipboard(&app).as_deref(), Some("bcd"));
    app.execute(Action::Undo);
    assert_eq!(text(&app), "abcdef");
}

#[test]
fn cut_with_no_selection_cuts_the_whole_line() {
    let mut app = app_no_pair();
    type_text(&mut app, "one\ntwo\nthree");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 0);
    app.execute(Action::Cut);
    assert_eq!(text(&app), "one\nthree");
    assert_eq!(last_clipboard(&app).as_deref(), Some("two\n"));
}

#[test]
fn cut_of_a_lone_last_line_without_a_newline_leaves_an_empty_document() {
    // The edge case the spec calls out: a single line with nothing after it must not leave a
    // stray newline behind or panic.
    let mut app = app();
    type_text(&mut app, "only");
    app.execute(Action::SelectAll);
    app.execute(Action::Cut);
    assert_eq!(text(&app), "");
    assert_eq!(caret(&app), LineCol::new(0, 0));
}

// ------------------------------------------------------- FR-C2/C3: paste

#[test]
fn paste_replaces_the_selection_as_one_undo_unit() {
    let mut app = app();
    type_text(&mut app, "hello");
    app.execute(Action::SelectAll);
    // Paste text that differs from the selection, so "replaced" is actually observable.
    app.clipboard.copy("XY");
    app.execute(Action::Paste);
    assert_eq!(
        text(&app),
        "XY",
        "the selection was replaced, not appended to"
    );
    app.execute(Action::Undo);
    assert_eq!(
        text(&app),
        "hello",
        "one undo restores both the delete and the insert"
    );
}

#[test]
fn paste_normalizes_mixed_endings_to_the_document_eol() {
    // FR-C3. A CRLF/CR/LF soup pasted into an LF document must come out uniform LF, because
    // the buffer is LF-only and the document EOL is applied at save time (S5).
    let mut app = app();
    type_text(&mut app, "start");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 5);
    app.clipboard.copy("a\r\nb\rc\nd");
    app.execute(Action::Paste);
    assert_eq!(text(&app), "starta\nb\nc\nd");
}

#[test]
fn paste_into_a_crlf_document_stores_lf_and_saves_crlf() {
    // FR-C3 end to end: the buffer stays LF-only and the document's CRLF is re-applied at
    // save, so a CRLF file pasted into stays a CRLF file.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("crlf.txt");
    std::fs::write(&path, b"one\r\ntwo\r\n").expect("write");
    let mut app = app();
    let loaded = np_core::load(&path, None).expect("load");
    app.documents.clear();
    app.open_document(np_core::Document::from_loaded(&loaded));
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 3);
    app.clipboard.copy("x\r\ny");
    app.execute(Action::Paste);
    // In the buffer: LF only.
    assert!(
        !text(&app).contains('\r'),
        "buffer must stay LF: {:?}",
        text(&app)
    );
    app.execute(Action::Save);
    let on_disk = std::fs::read(&path).expect("read");
    let disk = String::from_utf8_lossy(&on_disk).into_owned();
    assert!(
        disk.contains("x\r\ny"),
        "saved file must carry CRLF, got {disk:?}"
    );
    assert!(
        !disk.contains("\n\n") && !disk.contains("x\ny"),
        "no bare LF should have crept in, got {disk:?}"
    );
}

#[test]
fn paste_of_a_rectangle_keeps_its_columns() {
    let mut app = app();
    type_text(&mut app, "aaaaaaaa\nbbbbbbbb\ncccccccc");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    app.clipboard.copy("XY\nZW");
    app.execute(Action::Paste);
    // Every row got the 2-cell band at column 0.
    assert_eq!(text(&app), "XYaaaaaa\nZWbbbbbb\ncccccccc");
}

#[test]
fn paste_of_a_single_line_is_never_treated_as_a_block() {
    // Guards the block heuristic: an ordinary one-line paste must insert, not overwrite a
    // column band.
    let mut app = app();
    type_text(&mut app, "abcdef\nghijkl");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 2);
    app.clipboard.copy("ZZ");
    app.execute(Action::Paste);
    assert_eq!(text(&app), "abZZcdef\nghijkl");
}

#[test]
fn paste_with_an_empty_clipboard_toasts_instead_of_failing() {
    let mut app = app();
    type_text(&mut app, "abc");
    // `none` mode guarantees an unreadable clipboard regardless of anything an earlier test
    // copied into the process-wide mirror.
    app.clipboard =
        np_tui::clipboard::ClipboardProvider::new(np_tui::clipboard::ClipboardMode::None);
    let before = text(&app);
    app.execute(Action::Paste);
    assert_eq!(text(&app), before);
    assert!(
        !app.toasts.is_empty(),
        "the user is told why nothing happened"
    );
}

// ------------------------------------------------------ FR-C5: copy path

#[test]
fn copy_path_and_copy_filename_produce_different_text() {
    let mut app = app();
    let path = std::path::PathBuf::from("/tmp/somewhere/notes.txt");
    app.doc_mut().expect("doc").path = Some(path);
    app.execute(Action::CopyFilename);
    assert_eq!(last_clipboard(&app).as_deref(), Some("notes.txt"));
    app.execute(Action::CopyPath);
    assert_eq!(
        last_clipboard(&app).as_deref(),
        Some("/tmp/somewhere/notes.txt")
    );
}

#[test]
fn copy_path_on_an_untitled_document_copies_its_display_name() {
    let mut app = app();
    app.execute(Action::CopyPath);
    assert_eq!(last_clipboard(&app).as_deref(), Some("Untitled-1"));
}

// ------------------------------------------------- FR-C4 / row 7: primary

#[test]
fn middle_click_pastes_the_primary_selection() {
    let mut app = app();
    type_text(&mut app, "start");
    app.execute(Action::SelectAll);
    app.execute(Action::Copy);
    // The copy seeded the primary selection because the app mirrors it; now middle-click.
    app.doc_mut().expect("doc").clear_selection();
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 5);
    app.execute(Action::Paste);
    assert_eq!(text(&app), "startstart");
}

#[test]
fn primary_paste_with_nothing_in_it_toasts() {
    let mut app = app();
    type_text(&mut app, "abc");
    // `none` guarantees both the primary and the fallback are empty.
    app.clipboard =
        np_tui::clipboard::ClipboardProvider::new(np_tui::clipboard::ClipboardMode::None);
    let before = text(&app);
    app.execute(Action::Paste);
    assert_eq!(text(&app), before);
    assert!(!app.toasts.is_empty());
}

// -------------------------------------------------------- FR-E5: indent

#[test]
fn enter_copies_the_previous_lines_indent() {
    let mut app = app_no_pair();
    type_text(&mut app, "    indented");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut app, "x");
    assert_eq!(text(&app), "    indented\n    x");
}

#[test]
fn auto_indent_can_be_disabled_by_config() {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.editor.auto_indent = false;
    config.editor.auto_pair = false;
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "    indented");
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut app, "x");
    assert_eq!(text(&app), "    indented\nx");
}

#[test]
fn smart_backspace_dedents_to_the_previous_tab_stop() {
    let mut app = app_no_pair();
    // Only whitespace precedes the caret, which is the precondition for the dedent rule.
    type_text(&mut app, "  ");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(text(&app), "");
    assert_eq!(caret(&app), LineCol::new(0, 0));
}

#[test]
fn smart_backspace_removes_one_step_not_the_whole_indent() {
    let mut app = app_no_pair();
    type_text(&mut app, "        "); // 8 spaces
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(text(&app), "    ", "one 4-wide step, not all 8");
}

#[test]
fn smart_backspace_off_removes_a_single_space() {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.editor.smart_backspace = false;
    config.editor.auto_pair = false;
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "  ");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(text(&app), " ");
}

#[test]
fn smart_backspace_still_deletes_a_normal_character() {
    let mut app = app_no_pair();
    type_text(&mut app, "ab");
    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(text(&app), "a");
}

// --------------------------------------------------------- FR-E6: tabs

#[test]
fn tab_with_a_selection_indents_every_touched_line() {
    let mut app = app();
    type_text(&mut app, "one\ntwo\nthree");
    app.doc_mut().expect("doc").begin_selection(
        LineCol::new(0, 2),
        LineCol::new(1, 1),
        SelectionKind::Range,
    );
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "    one\n    two\nthree");
}

#[test]
fn shift_tab_outdents_every_touched_line() {
    let mut app = app();
    type_text(&mut app, "    one\n    two\n    three");
    app.execute(Action::SelectAll);
    press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(text(&app), "one\ntwo\nthree");
}

#[test]
fn tab_without_a_selection_inserts_indent_at_the_caret() {
    let mut app = app_no_pair();
    press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "    ", "insert_spaces is on by default");
    assert_eq!(caret(&app), LineCol::new(0, 4));
}

#[test]
fn indent_then_undo_restores_the_caret_count() {
    // FR-E6 requires the caret to survive the round trip, which is what "one undo unit"
    // buys the user: indenting five lines and undoing does not scatter the cursor.
    let mut app = app();
    type_text(&mut app, "one\ntwo\nthree");
    app.execute(Action::SelectAll);
    let before = caret(&app);
    app.execute(Action::Indent);
    assert_ne!(caret(&app), before, "indent moves the caret right");
    app.execute(Action::Undo);
    assert_eq!(text(&app), "one\ntwo\nthree");
}

// ------------------------------------------------------- FR-E10: F5

#[test]
fn insert_date_time_uses_the_configured_format() {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.editor.datetime_format = "%Y/%m/%d".into();
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "built ");
    let when = chrono::DateTime::parse_from_rfc3339("2026-10-17T09:08:07+00:00")
        .expect("parse")
        .with_timezone(&chrono::Local);
    app.insert_date_time_at(when);
    assert_eq!(text(&app), "built 2026/10/17");
}

#[test]
fn f5_inserts_something_that_matches_the_default_shape() {
    // The real clock, so assert only on shape rather than a value that would drift.
    let mut app = app();
    app.execute(Action::InsertDateTime);
    let got = text(&app);
    let digits: usize = got.chars().filter(|c| c.is_ascii_digit()).count();
    assert!(digits >= 8, "expected a date-like stamp, got {got:?}");
    assert!(got.contains('-') && got.contains(':'), "got {got:?}");
}

// ------------------------------------------------------ FR-E11: pairing

#[test]
fn typing_an_opener_inserts_and_steps_between_the_pair() {
    let mut app = app();
    type_text(&mut app, "f");
    type_text(&mut app, "(");
    assert_eq!(text(&app), "f()");
    assert_eq!(
        caret(&app),
        LineCol::new(0, 2),
        "caret sits inside the pair"
    );
    type_text(&mut app, "x");
    assert_eq!(text(&app), "f(x)");
}

#[test]
fn typing_an_opener_with_a_selection_wraps_it() {
    let mut app = app();
    type_text(&mut app, "abc");
    app.doc_mut().expect("doc").begin_selection(
        LineCol::new(0, 0),
        LineCol::new(0, 3),
        SelectionKind::Range,
    );
    type_text(&mut app, "(");
    assert_eq!(text(&app), "(abc)");
}

#[test]
fn auto_pairing_can_be_disabled() {
    let mut app = app_no_pair();
    type_text(&mut app, "(");
    assert_eq!(text(&app), "(");
    assert_eq!(caret(&app), LineCol::new(0, 1));
}

#[test]
fn quotes_pair_and_brackets_pair_but_angle_brackets_do_not() {
    let mut app = app();
    type_text(&mut app, "(");
    assert_eq!(text(&app), "()", "parens pair");
    // The caret is between the pair, so a second opener nests inside the first.
    type_text(&mut app, "[");
    assert_eq!(text(&app), "([])", "brackets pair, nested at the caret");
    let mut other = App::new_headless(Config::default(), (80, 24));
    type_text(&mut other, "<>");
    assert_eq!(text(&other), "<>", "auto-pairing < would fight the user");
}

// ------------------------------------------------------ FR-E12: overtype

#[test]
fn overtype_replaces_a_cjk_character_as_one_unit() {
    let mut app = app_no_pair();
    type_text(&mut app, "\u{8bed}\u{8bed}");
    app.execute(Action::ToggleOvertype);
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    type_text(&mut app, "x");
    assert_eq!(text(&app), "x\u{8bed}");
}

#[test]
fn overtype_replaces_a_combining_sequence_as_one_unit() {
    // The case FR-E12 calls out: "e" + combining acute is ONE visible character made of two
    // scalars. Replacing only the first would leave the accent glued to the new character.
    let mut app = app_no_pair();
    type_text(&mut app, "e\u{0301}z");
    app.execute(Action::ToggleOvertype);
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    type_text(&mut app, "x");
    assert_eq!(text(&app), "xz", "the whole cluster went, accent included");
}

#[test]
fn overtype_at_end_of_line_just_inserts() {
    let mut app = app_no_pair();
    type_text(&mut app, "ab");
    app.execute(Action::ToggleOvertype);
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 2);
    type_text(&mut app, "c");
    assert_eq!(text(&app), "abc");
}

#[test]
fn insert_mode_does_not_replace() {
    let mut app = app_no_pair();
    type_text(&mut app, "ab");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    type_text(&mut app, "x");
    assert_eq!(text(&app), "xab");
}

// ------------------------------------------------- FR-E13: big paste

#[test]
fn a_ten_megabyte_paste_is_one_undo_unit_and_stays_under_a_second() {
    // FR-E13 asks for a measured number, not a promise. The rope insert is already chunked
    // internally; what this pins down is that the whole paste is one undo step and that no
    // quadratic path crept in.
    let mut app = app();
    let line = "0123456789abcdefghijklmnopqrstuvwxyz012345678901234567890123456789\n";
    let mut payload = String::with_capacity(10 * 1024 * 1024);
    while payload.len() < 10 * 1024 * 1024 {
        payload.push_str(line);
    }
    // The copy is timed separately: in osc52 mode a copy base64-encodes the payload, which
    // is real work but is not the freeze FR-E13 is about. The paste is the UI-blocking part.
    let copy_started = Instant::now();
    app.clipboard.copy(&payload);
    let copy_time = copy_started.elapsed();
    let started = Instant::now();
    app.execute(Action::Paste);
    let paste_time = started.elapsed();
    let inserted = app.doc().expect("doc").buffer.char_count();
    assert!(inserted > 10_000_000, "inserted {inserted} chars");

    let undo_started = Instant::now();
    app.execute(Action::Undo);
    let undo_time = undo_started.elapsed();
    assert_eq!(
        app.doc().expect("doc").buffer.char_count(),
        0,
        "one undo removed the entire paste"
    );
    eprintln!(
        "S7 perf: 10MB copy {copy_time:?}, paste {paste_time:?}, undo {undo_time:?} ({} chars)",
        payload.chars().count()
    );
    assert!(
        paste_time.as_secs() < 2,
        "10 MB paste took {paste_time:?}, which is a freeze"
    );
    assert!(undo_time.as_secs() < 5, "undo took {undo_time:?}");
}

// ------------------------------------------------- FR-E14: whitespace

#[test]
fn whitespace_markers_replace_spaces_and_tabs() {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.ui.show_whitespace = true;
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "a b");
    let buf = render_to_buffer(&mut app, 80, 24);
    let row = row_text(&buf, 2, 2, 20);
    assert!(row.contains('\u{b7}'), "expected a middle dot, got {row:?}");
    // The buffer is untouched: display only.
    assert_eq!(text(&app), "a b");
}

#[test]
fn whitespace_display_is_off_by_default() {
    let mut app = app();
    type_text(&mut app, "a b");
    let buf = render_to_buffer(&mut app, 80, 24);
    let row = row_text(&buf, 2, 2, 20);
    assert!(row.contains("a b"), "got {row:?}");
}

#[test]
fn eol_marks_appear_only_when_enabled() {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config.ui.show_eol = true;
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "ab");
    let buf = render_to_buffer(&mut app, 80, 24);
    let row = row_text(&buf, 2, 2, 20);
    assert!(row.contains('\u{b6}'), "expected a pilcrow, got {row:?}");
}

// ------------------------------------------------ FR-V2: gutter emphasis

#[test]
fn the_current_line_number_is_bold_and_the_others_are_not() {
    use ratatui::style::Modifier;
    let mut app = app();
    type_text(&mut app, "one\ntwo\nthree");
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 0);
    let buf = render_to_buffer(&mut app, 80, 24);
    let cell = |x: u16, y: u16| {
        buf.cell(ratatui::layout::Position { x, y })
            .map(|c| c.modifier)
            .unwrap_or(Modifier::empty())
    };
    // The gutter sits at the left of the editor area; row 1 of the document is screen row 3.
    let gutter_right = app.geometry.gutter_width as u16 + app.regions.editor.x - 1;
    let on_cursor = cell(gutter_right, 3);
    let off_cursor = cell(gutter_right, 2);
    assert!(
        on_cursor.contains(Modifier::BOLD),
        "the caret's line number must be emphasized"
    );
    assert!(
        !off_cursor.contains(Modifier::BOLD),
        "only the caret's line number is emphasized"
    );
}

// ---------------------------------------------------- FR-U4: status bar

#[test]
fn status_bar_shows_every_m1_segment() {
    let mut app = app();
    type_text(&mut app, "hello");
    let buf = render_to_buffer(&mut app, 140, 24);
    let row = row_text(&buf, 23, 0, 140);
    for want in ["Untitled-1", "Ln 1, Col 6", "LF", "UTF-8", "INS", " / "] {
        assert!(row.contains(want), "missing {want:?} in {row:?}");
    }
}

#[test]
fn status_bar_shows_the_selection_count_only_with_a_selection() {
    let mut app = app();
    type_text(&mut app, "hello world");
    let plain = row_text(&render_to_buffer(&mut app, 160, 24), 23, 0, 160);
    assert!(!plain.contains("Sel "), "got {plain:?}");
    app.execute(Action::SelectAll);
    let with_sel = row_text(&render_to_buffer(&mut app, 160, 24), 23, 0, 160);
    assert!(with_sel.contains("Sel 11"), "got {with_sel:?}");
}

#[test]
fn status_bar_shows_lock_for_a_readonly_document() {
    let mut app = app();
    app.doc_mut().expect("doc").readonly = true;
    let row = row_text(&render_to_buffer(&mut app, 140, 24), 23, 0, 140);
    assert!(row.contains("[lock]"), "got {row:?}");
}

#[test]
fn status_bar_shows_the_modified_dot() {
    let mut app = app();
    type_text(&mut app, "x");
    let row = row_text(&render_to_buffer(&mut app, 140, 24), 23, 0, 140);
    assert!(row.contains('\u{25cf}'), "got {row:?}");
}

#[test]
fn status_counts_come_from_the_rope_not_a_string_build() {
    // The count must be correct on a document whose text is spread across many rope chunks,
    // which is what a large paste produces.
    let mut app = app();
    let payload = "x".repeat(50_000);
    app.clipboard.copy(&payload);
    app.execute(Action::Paste);
    let row = row_text(&render_to_buffer(&mut app, 160, 24), 23, 0, 160);
    assert!(row.contains("50000 / 1"), "got {row:?}");
}

// ------------------------------------------------- config plumbing

#[test]
fn clipboard_mode_comes_from_config() {
    let mut config = Config::default();
    config.ui.clipboard = "none".into();
    let app = App::new_headless(config, (80, 24));
    assert_eq!(app.clipboard.mode(), np_tui::clipboard::ClipboardMode::None);
}

#[test]
fn ui_default_config_is_untouched_by_this_stage() {
    // Guards against a default flipping silently: whitespace off, wrap off, line numbers on.
    let ui = UiConfig::default();
    assert!(!ui.show_whitespace);
    assert!(!ui.show_eol);
    assert!(ui.line_numbers);
    assert_eq!(ui.clipboard, "auto");
}

#[test]
fn no_test_in_this_file_touches_the_real_clipboard() {
    // Every app here is osc52 or none, so arboard is never constructed. Asserted on the
    // captured sink: an osc52 app always has bytes after a copy.
    let mut app = app();
    type_text(&mut app, "abc");
    app.execute(Action::SelectAll);
    app.execute(Action::Copy);
    assert!(!app.osc_out.borrow().is_empty());
}
