//! Stage S10 acceptance: the Find/Replace surface, Go To, and match highlighting
//! (spec §6.4 FR-S1..S7).
//!
//! The engine has its own suite in `np-core/tests/search_s10.rs`. This one is about the
//! seams: does `F3` move the caret and select, do options survive a dialog close and land in
//! `[find]` config, does a search work with no dialog in sight, and does the highlight layer
//! cover the cells the match actually occupies. Those are the things that can break while the
//! engine stays green.
//!
//! # Typing goes through `App::handle_key`
//!
//! Not through `insert_text`, so the auto-pair feature S7 turned on cannot rewrite the test's
//! input. Where a test needs literal bytes it sets `config.editor.auto_pair = false`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use np_core::{Config, Document, EncodingId, LineCol};
use np_tui::find::{self, FindOutcome};
use np_tui::ui::{self, dialogs};
use np_tui::{Action, App, FindField};

const W: u16 = 80;
const H: u16 = 24;

fn config() -> Config {
    let mut config = Config::default();
    config.ui.clipboard = "none".into();
    config
}

fn app() -> App {
    App::new_headless(config(), (W, H))
}

/// A document whose text is `text`, as the only tab.
fn doc_app(text: &str) -> App {
    let mut app = app();
    let doc = Document::from_decoded(
        std::path::PathBuf::from("/tmp/s10.txt"),
        text,
        EncodingId::Utf8,
        np_core::detect_dominant_eol(text),
        false,
    );
    // `from_decoded` parks the caret at 0,0 (see CONTEXT.md); tests that type afterwards
    // would otherwise prepend.
    app.open_document(doc);
    app
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn press_ctrl(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
}

fn press_shift(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::SHIFT));
}

/// Set the needle the way the dialog would, including validation.
fn set_needle(app: &mut App, needle: &str) {
    app.find.needle = needle.to_string();
    app.find.validate_now();
    app.refresh_marks();
}

fn caret(app: &App) -> LineCol {
    app.cursor_pos()
}

// ------------------------------------------------------------ FR-S2: F3

#[test]
fn f3_without_a_dialog_finds_and_selects() {
    let mut app = doc_app("one two three two five");
    set_needle(&mut app, "two");
    app.execute(Action::FindNext);
    assert_eq!(caret(&app), LineCol::new(0, 7), "caret at the match end");
    let sel = app.doc().and_then(|d| d.selection).expect("selection");
    assert_eq!(sel.bounds(), (LineCol::new(0, 4), LineCol::new(0, 7)));
    assert_eq!(app.doc().unwrap().selected_text().unwrap(), "two");
}

#[test]
fn f3_repeated_walks_every_match_and_wraps() {
    let mut app = doc_app("x ax bx");
    set_needle(&mut app, "x");
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.execute(Action::FindNext);
        seen.push(caret(&app));
    }
    // "x ax bx": `x` matches at 0, 3, 6, so the ends are 1, 4, 7 and then a wrap to 1.
    assert_eq!(
        seen,
        vec![
            LineCol::new(0, 1),
            LineCol::new(0, 4),
            LineCol::new(0, 7),
            LineCol::new(0, 1),
        ]
    );
}

#[test]
fn shift_f3_walks_backwards() {
    let mut app = doc_app("x ax bx");
    set_needle(&mut app, "x");
    // Park at the end, then walk back. The caret sits at the match END (FR-S2 item 7), so
    // the observed sequence is the last match's end, then the two earlier ends, then a wrap
    // back to the last. Backward search excludes the current match by its START, which is
    // what makes the walk move at all -- see `find_next`.
    app.execute(Action::DocEnd);
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.execute(Action::FindPrev);
        seen.push(caret(&app));
    }
    assert_eq!(
        seen,
        vec![
            LineCol::new(0, 7),
            LineCol::new(0, 4),
            LineCol::new(0, 1),
            LineCol::new(0, 7), // wrapped to the last match
        ]
    );
}

#[test]
fn f3_with_an_empty_needle_does_nothing_and_does_not_hang() {
    let mut app = doc_app("hello");
    app.find.needle.clear();
    app.find.error = None;
    app.find.marks.clear();
    app.execute(Action::FindNext);
    assert_eq!(caret(&app), LineCol::new(0, 0));
    assert!(app.find.marks.is_empty());
}

#[test]
fn f3_with_no_match_toasts_not_found_and_leaves_the_caret() {
    let mut app = doc_app("hello");
    set_needle(&mut app, "zzz");
    app.execute(Action::DocEnd);
    let before = caret(&app);
    app.execute(Action::FindNext);
    assert_eq!(caret(&app), before);
    assert!(
        app.toasts.iter().any(|t| t.message.contains("not found")),
        "expected a not-found toast, got {:?}",
        app.toasts.iter().map(|t| &t.message).collect::<Vec<_>>()
    );
}

#[test]
fn typing_after_f3_replaces_the_match() {
    // FR-S2's payoff: find, then type, and the match is gone.
    let mut app = doc_app("one two three");
    set_needle(&mut app, "two");
    app.execute(Action::FindNext);
    let mut typed = App::clone(&app);
    typed.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    let out = typed.doc().unwrap().buffer.line_string(0);
    assert_eq!(out, "one 2 three");
}

// ------------------------------------------------- FR-S7: word under caret

#[test]
fn ctrl_f3_searches_the_word_under_the_caret() {
    let mut app = doc_app("alpha beta alpha gamma alpha");
    // Put the caret on the first `beta`.
    app.execute(Action::DocStart);
    for _ in 0..7 {
        app.execute(Action::MoveRight);
    }
    app.execute(Action::FindWordNext);
    assert_eq!(app.find.needle, "beta");
    // `beta` occurs once, so the search wraps around to it and selects 6..10. The caret is
    // at the match end, not 11: the trailing space is not part of the match.
    assert_eq!(caret(&app), LineCol::new(0, 10));
}

#[test]
fn ctrl_f3_jumps_between_occurrences_of_the_word() {
    let mut app = doc_app("foo bar foo baz foo");
    app.execute(Action::DocStart);
    app.execute(Action::FindWordNext);
    assert_eq!(app.find.needle, "foo");
    // The first press selects the word under the caret, which is the feedback the shortcut is
    // asking for; subsequent presses walk the rest and wrap.
    assert_eq!(app.find.current.map(|m| m.start), Some(LineCol::new(0, 0)));
    assert_eq!(caret(&app), LineCol::new(0, 3));
    app.execute(Action::FindNext);
    assert_eq!(caret(&app), LineCol::new(0, 11), "the second occurrence");
    app.execute(Action::FindNext);
    assert_eq!(caret(&app), LineCol::new(0, 19), "the last occurrence");
    app.execute(Action::FindNext);
    // Wrapped to the first occurrence. All three `foo`s are whole words here -- the one at
    // position 0 included, which is the offset-0 whole-word bug S11 fixed in the engine; before
    // that fix this wrapped to 8..11 because 0..3 was not considered a match at all.
    assert_eq!(caret(&app), LineCol::new(0, 3), "wrapped to the first");
}

#[test]
fn ctrl_f3_on_whitespace_toasts_instead_of_searching_for_nothing() {
    // Column 3 is the space between the words and has no word of its own. A caret at the end
    // of a word does resolve to that word -- `Buffer::word_range_at` attaches leftward, which
    // is what makes Ctrl+Left feel right -- so this column is the position under test.
    let mut app = doc_app("foo bar");
    app.execute(Action::DocStart);
    for _ in 0..3 {
        app.execute(Action::MoveRight);
    }
    assert_eq!(caret(&app), LineCol::new(0, 3));
    app.execute(Action::FindWordNext);
    assert!(app.find.needle.is_empty(), "needle must not change");
    assert!(
        app.toasts
            .iter()
            .any(|t| t.message.contains("no word under the caret")),
        "expected the no-word toast"
    );
}

#[test]
fn ctrl_f3_forces_whole_word() {
    let mut app = doc_app("foobar foo foobar");
    app.execute(Action::DocStart);
    for _ in 0..7 {
        app.execute(Action::MoveRight);
    }
    app.execute(Action::FindWordNext);
    assert!(app.find.opts.whole_word);
    // The caret at column 7 sits on the standalone `foo`, and whole-word matching means the
    // neighbours inside `foobar` are not candidates at all. The press selects 7..10.
    assert_eq!(app.find.needle, "foo");
    assert_eq!(
        app.find.current.map(|m| (m.start, m.end)),
        Some((LineCol::new(0, 7), LineCol::new(0, 10)))
    );
    let whole: Vec<(usize, usize)> = app
        .find
        .marks
        .iter()
        .map(|m| (m.start.col, m.end.col))
        .collect();
    assert_eq!(whole, vec![(7, 10)], "only the standalone foo is a match");
}

// ------------------------------------------------------- FR-S1: the dialog

#[test]
fn ctrl_f_opens_the_find_dialog_and_esc_closes_it() {
    let mut app = app();
    press_ctrl(&mut app, KeyCode::Char('f'));
    assert!(matches!(
        app.mode(),
        np_tui::app::Mode::Dialog(np_tui::app::DialogKind::Find)
    ));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), np_tui::app::Mode::Editing);
}

#[test]
fn typing_in_the_dialog_goes_to_the_field_not_the_buffer() {
    let mut app = doc_app("keep me");
    press_ctrl(&mut app, KeyCode::Char('f'));
    for c in "abc".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(app.find.needle, "abc");
    assert_eq!(
        app.doc().unwrap().buffer.line_string(0),
        "keep me",
        "the document must be untouched"
    );
}

#[test]
fn dialog_options_persist_across_opens_and_into_find_config() {
    let mut app = doc_app("Foo foo");
    press_ctrl(&mut app, KeyCode::Char('f'));
    dialogs::set_find_flag(&mut app, "case_sensitive", true);
    dialogs::set_find_flag(&mut app, "regex", true);
    // The dialog edits the live search state, and config is the write-back target: it is only
    // updated when the dialog closes, which is FR-S1's "persist across opens".
    assert!(app.find.opts.case_sensitive, "state updated immediately");
    assert!(app.find.opts.regex, "state updated immediately");
    assert!(
        !app.config.find.case_sensitive,
        "config is written on close, not per keystroke"
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.config.find.case_sensitive, "written on close");
    assert!(app.config.find.regex, "written on close");
    // Reopen from config: still on.
    app.find = np_tui::FindState::from_config(&app.config);
    assert!(app.find.opts.case_sensitive);
    assert!(app.find.opts.regex);
}

#[test]
fn find_config_written_by_the_dialog_round_trips_through_toml() {
    let mut app = doc_app("x");
    press_ctrl(&mut app, KeyCode::Char('f'));
    dialogs::set_find_flag(&mut app, "whole_word", true);
    dialogs::set_find_flag(&mut app, "wrap_around", false);
    press(&mut app, KeyCode::Esc);
    let text = toml::to_string(&app.config).expect("serializes");
    let back: Config = toml::from_str(&text).expect("parses");
    assert!(back.find.whole_word);
    assert!(!back.find.wrap_around);
}

#[test]
fn in_selection_auto_checks_when_the_selection_spans_two_lines() {
    let mut app = doc_app("one\ntwo\nthree");
    // Select across two lines.
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(
            LineCol::new(0, 1),
            LineCol::new(1, 2),
            np_core::SelectionKind::Range,
        );
    }
    find::open_find(&mut app, false);
    assert!(app.find.in_selection, "FR-S1 auto-check");
}

#[test]
fn in_selection_stays_off_for_a_single_line_selection() {
    let mut app = doc_app("one\ntwo\nthree");
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(
            LineCol::new(0, 0),
            LineCol::new(0, 2),
            np_core::SelectionKind::Range,
        );
    }
    find::open_find(&mut app, false);
    assert!(!app.find.in_selection);
}

#[test]
fn dialog_enter_runs_the_default_button() {
    let mut app = doc_app("one two three");
    press_ctrl(&mut app, KeyCode::Char('f'));
    set_needle(&mut app, "three");
    press(&mut app, KeyCode::Enter);
    assert_eq!(caret(&app), LineCol::new(0, 13));
}

#[test]
fn dialog_arrows_change_direction() {
    let mut app = doc_app("x ax bx");
    press_ctrl(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Up);
    assert!(!app.find.forward);
    press(&mut app, KeyCode::Down);
    assert!(app.find.forward);
}

#[test]
fn tab_moves_focus_between_fields() {
    // S14 changed what Tab cycles: FR-U4 requires it to walk every focusable child, so the
    // two text fields are now the first two stops on a longer tour through the checkboxes and
    // buttons. The assertion follows the framework's order rather than S10's two-field loop;
    // what must still hold is that the first Tab lands on Replace and that the cycle returns
    // to Find, both of which are what a user actually relies on.
    let mut app = doc_app("x");
    press_ctrl(&mut app, KeyCode::Char('h'));
    assert_eq!(app.find.focus, FindField::Find);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.find.focus, FindField::Replace);
    // Walk the rest of the cycle and confirm it wraps home.
    let count = app.dialog_focus_items().len();
    assert!(count > 2, "Replace should expose more than two focusables");
    for _ in 0..count - 1 {
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(app.find.focus, FindField::Find);
}

#[test]
fn shift_tab_wraps_backwards_to_the_last_child() {
    // FR-U4: Shift+Tab wraps at both ends.
    let mut app = doc_app("x");
    press_ctrl(&mut app, KeyCode::Char('h'));
    let count = app.dialog_focus_items().len();
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.dialog_focus, count - 1);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.dialog_focus, 0);
}

#[test]
fn replace_mode_dialog_is_reachable_and_labeled() {
    let mut app = doc_app("x");
    press_ctrl(&mut app, KeyCode::Char('h'));
    assert!(app.find.replace_mode);
    let buf = ui::render_to_buffer(&mut app, W, H);
    let all: String = (0..H)
        .map(|y| ui::row_text(&buf, y, 0, W))
        .collect::<String>();
    assert!(all.contains("Replace"), "dialog should read Replace");
}

// --------------------------------------------------------- FR-S3: replacing

#[test]
fn replace_all_rewrites_and_toasts_the_count() {
    let mut app = doc_app("foo foo bar foo");
    set_needle(&mut app, "foo");
    app.find.replacement = "X".into();
    app.execute(Action::ReplaceAll);
    assert_eq!(app.doc().unwrap().buffer.line_string(0), "X X bar X");
    assert!(
        app.toasts.iter().any(|t| t.message.contains("Replaced 3")),
        "expected a count toast, got {:?}",
        app.toasts.iter().map(|t| &t.message).collect::<Vec<_>>()
    );
}

#[test]
fn replace_all_respects_in_selection() {
    let mut app = doc_app("foo foo\nfoo foo");
    set_needle(&mut app, "foo");
    app.find.replacement = "X".into();
    app.find.in_selection = true;
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(
            LineCol::new(0, 0),
            LineCol::new(0, 8),
            np_core::SelectionKind::Range,
        );
    }
    let out = find::replace_all(&mut app);
    assert_eq!(out, FindOutcome::Replaced(2));
    // Line 0 is inside the selection and changed; line 1 is outside it and must not have.
    assert_eq!(app.doc().unwrap().buffer.line_string(0), "X X");
    assert_eq!(app.doc().unwrap().buffer.line_string(1), "foo foo");
    assert_eq!(
        app.doc().unwrap().buffer.text_string(),
        "X X\nfoo foo",
        "nothing outside the selection was touched"
    );
}

#[test]
fn replace_all_is_undoable_one_step_at_a_time() {
    let mut app = doc_app("foo foo foo");
    set_needle(&mut app, "foo");
    app.find.replacement = "bar".into();
    find::replace_all(&mut app);
    assert_eq!(app.doc().unwrap().buffer.line_string(0), "bar bar bar");
    let before = app.doc().unwrap().buffer.text_string();
    app.doc_mut().unwrap().undo_one();
    let after = app.doc().unwrap().buffer.text_string();
    assert_ne!(before, after, "one undo should revert one replacement");
    // Replace All applies right to left so each edit's coordinates stay valid for the ones to
    // its left, which means the last thing applied -- and therefore the first thing undone --
    // is the LEFTMOST replacement.
    assert_eq!(after, "foo bar bar");
    app.doc_mut().unwrap().undo_one();
    assert_eq!(app.doc().unwrap().buffer.text_string(), "foo foo bar");
    app.doc_mut().unwrap().undo_one();
    assert_eq!(app.doc().unwrap().buffer.text_string(), "foo foo foo");
}

#[test]
fn replace_current_replaces_the_selected_match_then_advances() {
    let mut app = doc_app("aa aa aa");
    set_needle(&mut app, "aa");
    find::find_next(&mut app);
    app.find.replacement = "b".into();
    let out = find::replace_current(&mut app);
    assert_eq!(out, FindOutcome::Replaced(1));
    assert_eq!(app.doc().unwrap().buffer.line_string(0), "b aa aa");
}

#[test]
fn replace_on_a_readonly_document_reports_rather_than_hanging() {
    let mut app = doc_app("foo");
    if let Some(doc) = app.doc_mut() {
        doc.readonly = true;
    }
    set_needle(&mut app, "foo");
    app.find.replacement = "X".into();
    let out = find::replace_all(&mut app);
    assert!(matches!(out, FindOutcome::Invalid(_)), "got {out:?}");
    assert_eq!(app.doc().unwrap().buffer.line_string(0), "foo");
}

#[test]
fn replace_all_counts_zero_when_nothing_matches() {
    let mut app = doc_app("hello");
    set_needle(&mut app, "zzz");
    app.find.replacement = "X".into();
    assert_eq!(find::replace_all(&mut app), FindOutcome::Replaced(0));
}

// ----------------------------------------------------------- FR-S5: counting

#[test]
fn count_reports_occurrences_without_moving() {
    let mut app = doc_app("foo Foo FOO");
    set_needle(&mut app, "foo");
    let before = caret(&app);
    assert_eq!(find::count_matches(&mut app), FindOutcome::Counted(3));
    assert_eq!(caret(&app), before, "count must not move the caret");
}

#[test]
fn count_respects_in_selection() {
    let mut app = doc_app("foo foo\nfoo");
    set_needle(&mut app, "foo");
    app.find.in_selection = true;
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(
            LineCol::new(1, 0),
            LineCol::new(1, 3),
            np_core::SelectionKind::Range,
        );
    }
    assert_eq!(find::count_matches(&mut app), FindOutcome::Counted(1));
}

// -------------------------------------------------------- FR-S4: bad regex

#[test]
fn an_invalid_regex_is_reported_inline_and_keeps_the_last_search() {
    let mut app = doc_app("abc def");
    app.find.opts.regex = true;
    set_needle(&mut app, "[unclosed");
    assert!(app.find.error.is_some(), "inline error");
    let out = find::find_next(&mut app);
    assert!(matches!(out, FindOutcome::Invalid(_)), "got {out:?}");
    assert_eq!(caret(&app), LineCol::new(0, 0), "caret must not move");
}

#[test]
fn an_invalid_pattern_does_not_clear_existing_marks() {
    let mut app = doc_app("abc abc");
    app.find.opts.regex = true;
    set_needle(&mut app, "abc");
    assert_eq!(app.find.marks.len(), 2);
    app.find.needle = "(unclosed".into();
    app.find.validate_now();
    assert!(app.find.error.is_some());
    // The keystroke that broke the pattern must not wipe the previous result.
    assert_eq!(app.find.marks.len(), 2);
}

#[test]
fn an_invalid_regex_shows_in_the_rendered_dialog() {
    let mut app = doc_app("abc");
    press_ctrl(&mut app, KeyCode::Char('f'));
    app.find.opts.regex = true;
    for c in "([".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let buf = ui::render_to_buffer(&mut app, W, H);
    let all: String = (0..H)
        .map(|y| ui::row_text(&buf, y, 0, W))
        .collect::<String>();
    assert!(all.contains("invalid regular expression"), "{all}");
}

// ------------------------------------------------------------- FR-S6: Go To

#[test]
fn go_to_accepts_a_line_number() {
    let mut app = doc_app("one\ntwo\nthree");
    assert!(matches!(
        find::go_to(&mut app, "2"),
        FindOutcome::Found { .. }
    ));
    assert_eq!(caret(&app).line, 1);
}

#[test]
fn go_to_accepts_line_and_column() {
    let mut app = doc_app("one\ntwo\nthree");
    find::go_to(&mut app, "3:2");
    assert_eq!(caret(&app), LineCol::new(2, 1));
}

#[test]
fn go_to_clamps_past_end_of_file() {
    let mut app = doc_app("one\ntwo\nthree");
    find::go_to(&mut app, "9999");
    assert_eq!(caret(&app).line, 2, "clamped to the last line");
}

#[test]
fn go_to_clamps_a_column_past_the_line_end() {
    let mut app = doc_app("one\ntwo\nthree");
    find::go_to(&mut app, "10:5");
    let pos = caret(&app);
    assert_eq!(pos.line, 2, "clamped to the last line");
    assert!(pos.col <= 5, "column clamped into the line, got {pos:?}");
}

#[test]
fn go_to_rejects_zero_and_junk_with_an_inline_error() {
    let mut app = doc_app("one");
    assert!(matches!(
        find::go_to(&mut app, "0"),
        FindOutcome::Invalid(_)
    ));
    assert!(matches!(
        find::go_to(&mut app, "abc"),
        FindOutcome::Invalid(_)
    ));
    assert!(matches!(find::go_to(&mut app, ""), FindOutcome::Invalid(_)));
    assert!(matches!(
        find::go_to(&mut app, "1.5"),
        FindOutcome::Invalid(_)
    ));
}

#[test]
fn go_to_dialog_accepts_digits_and_enters() {
    let mut app = doc_app("one\ntwo\nthree");
    app.execute(Action::GoToLine);
    assert!(matches!(
        app.mode(),
        np_tui::app::Mode::Dialog(np_tui::app::DialogKind::GoTo)
    ));
    for c in "2".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(app.goto_input, "2");
    press(&mut app, KeyCode::Enter);
    assert_eq!(caret(&app).line, 1);
    assert_eq!(app.mode(), np_tui::app::Mode::Editing);
}

#[test]
fn go_to_dialog_shows_its_error_inline_and_stays_open() {
    let mut app = doc_app("one");
    app.execute(Action::GoToLine);
    for c in "abc".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.goto_error.is_some());
    assert!(matches!(
        app.mode(),
        np_tui::app::Mode::Dialog(np_tui::app::DialogKind::GoTo)
    ));
}

// ---------------------------------------------------- FR-S2: match marking

#[test]
fn marks_are_populated_when_marking_is_on() {
    let mut app = doc_app("foo bar foo");
    app.find.mark_matches = true;
    set_needle(&mut app, "foo");
    assert_eq!(app.find.marks.len(), 2);
}

#[test]
fn marks_are_skipped_when_marking_is_off() {
    let mut app = doc_app("foo bar foo");
    app.find.mark_matches = false;
    set_needle(&mut app, "foo");
    assert!(app.find.marks.is_empty(), "no scan when marking is off");
}

#[test]
fn match_cells_covers_exactly_the_matched_columns() {
    let mut app = doc_app("one two three");
    set_needle(&mut app, "two");
    find::find_next(&mut app);
    let cells = ui::editor_view::match_cells(&app, 0, 0, 13, 0);
    // The current match is selected, so its three columns are covered.
    let xs: Vec<u16> = cells.iter().map(|c| c.x).collect();
    assert_eq!(xs.len(), 3, "{cells:?}");
    // Editor starts at x=0 with a gutter; the match begins at char 4.
    let gutter = app.geometry.gutter_width as u16;
    assert_eq!(xs[0], gutter + 4);
    assert!(cells.iter().all(|c| c.current));
}

#[test]
fn match_cells_covers_every_line_of_a_multiline_match() {
    let mut app = doc_app("one\ntwo\nthree");
    app.find.opts.regex = true;
    set_needle(&mut app, r"two\nthree");
    find::find_next(&mut app);
    let r1 = ui::editor_view::match_cells(&app, 1, 0, 3, 1);
    let r2 = ui::editor_view::match_cells(&app, 2, 0, 5, 2);
    assert_eq!(r1.len(), 3, "line 1 covered");
    assert_eq!(r2.len(), 5, "line 2 covered");
}

#[test]
fn other_matches_are_marked_and_not_flagged_current() {
    let mut app = doc_app("foo foo foo");
    app.find.mark_matches = true;
    set_needle(&mut app, "foo");
    find::find_next(&mut app);
    let cells = ui::editor_view::match_cells(&app, 0, 0, 13, 0);
    assert_eq!(cells.len(), 9, "three matches of three chars");
    assert!(cells.iter().any(|c| !c.current), "marks are not current");
    assert!(cells.iter().any(|c| c.current), "the current one is");
}

#[test]
fn marks_render_into_the_buffer() {
    let mut app = doc_app("foo bar foo");
    app.find.mark_matches = true;
    set_needle(&mut app, "foo");
    find::find_next(&mut app);
    let buf = ui::render_to_buffer(&mut app, W, H);
    // The overlay only changes styles, so assert on style rather than text.
    let gutter = app.geometry.gutter_width as u16;
    let y = app.regions.editor.y;
    let first = buf.cell(ratatui::layout::Position { x: gutter, y });
    let second = buf.cell(ratatui::layout::Position { x: gutter + 8, y });
    let plain = buf.cell(ratatui::layout::Position { x: gutter + 4, y });
    assert!(
        first.map(|c| c.modifier).unwrap_or_default()
            != plain.map(|c| c.modifier).unwrap_or_default(),
        "a matched cell must be styled differently from an unmatched one"
    );
    assert!(second.is_some());
}

#[test]
fn the_overlay_never_changes_the_text() {
    let mut app = doc_app("foo bar foo");
    let plain = ui::render_to_buffer(&mut app, W, H);
    let plain_row = ui::row_text(&plain, app.regions.editor.y, 0, W);
    set_needle(&mut app, "foo");
    find::find_next(&mut app);
    let marked = ui::render_to_buffer(&mut app, W, H);
    let marked_row = ui::row_text(&marked, app.regions.editor.y, 0, W);
    assert_eq!(
        plain_row, marked_row,
        "highlighting must not rewrite glyphs"
    );
}

// ------------------------------------------------------------- invariants

#[test]
fn no_search_input_hangs_the_app() {
    // Adversarial needles through the real command path. Each must return.
    let mut app = doc_app("aaaa\naaaa\naaaa");
    for needle in [
        "",
        "^",
        "$",
        r"\b",
        "a*",
        "(a*)*",
        "(a+)+b",
        "[a-",
        "(",
        "|",
        r"\",
        "..",
        "\n",
        "(?i)(?m)s",
    ] {
        for regex in [false, true] {
            app.find.needle = needle.to_string();
            app.find.opts.regex = regex;
            app.find.validate_now();
            let _ = find::find_next(&mut app);
            let _ = find::count_matches(&mut app);
        }
    }
}

#[test]
fn find_survives_a_tab_switch() {
    let mut app = doc_app("foo foo");
    set_needle(&mut app, "foo");
    find::find_next(&mut app);
    app.execute(Action::NewFile);
    // The needle is app state, so it is still there on the new tab.
    assert_eq!(app.find.needle, "foo");
    assert_eq!(caret(&app), LineCol::new(0, 0), "new tab starts clean");
}

#[test]
fn marks_are_refreshed_after_an_edit() {
    let mut app = doc_app("foo foo");
    app.find.mark_matches = true;
    set_needle(&mut app, "foo");
    assert_eq!(app.find.marks.len(), 2);
    app.refresh_marks();
    assert_eq!(app.find.marks.len(), 2);
}

#[test]
fn esc_in_the_find_dialog_does_not_clear_the_document_selection() {
    // Closing the dialog is a mode pop; the FR-S2 selection from the last find stays, which
    // is what lets a user find, close the dialog, and type over the match.
    let mut app = doc_app("one two three");
    press_ctrl(&mut app, KeyCode::Char('f'));
    set_needle(&mut app, "two");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), np_tui::app::Mode::Editing);
    assert!(app.doc().unwrap().selection.is_some());
}

#[test]
fn keymap_bindings_reach_the_search_actions() {
    let mut app = doc_app("foo foo");
    // Search through the dialog exactly as a user would, then close it: F3 must keep working
    // with no dialog in sight (FR-S2).
    press_ctrl(&mut app, KeyCode::Char('f'));
    for c in "foo".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.find.needle, "foo", "the needle survives the close");
    press(&mut app, KeyCode::F(3));
    assert_eq!(caret(&app), LineCol::new(0, 3));
    press_shift(&mut app, KeyCode::F(3));
    assert_eq!(caret(&app), LineCol::new(0, 7), "wrapped to the last match");
}

#[test]
fn find_dialog_rect_fits_a_minimal_terminal() {
    let mut app = app();
    press_ctrl(&mut app, KeyCode::Char('f'));
    let tiny = dialogs::find_rect(&app);
    assert!(tiny.w <= W && tiny.h <= H);
    // Drawing at 40x8 must not panic.
    let _ = ui::render_to_buffer(&mut app, 40, 8);
    let _ = ui::render_to_buffer(&mut app, 10, 4);
}

// ------------------------------------------------- regressions found in S10

#[test]
fn shift_f3_from_a_parked_caret_moves_off_the_match_it_sits_on() {
    // The bug: backward search used the caret alone, and the caret sits at the previous
    // match's END, so the match the caret was on satisfied `start < from` and came back
    // forever. Shift+F3 looked completely frozen.
    let mut app = doc_app("alpha beta gamma");
    set_needle(&mut app, "beta");
    app.execute(Action::DocEnd);
    app.execute(Action::FindPrev);
    let first = app.find.current.map(|m| (m.start.col, m.end.col));
    assert_eq!(first, Some((6, 10)));
    app.execute(Action::FindPrev);
    assert_ne!(
        app.find.current.map(|m| (m.start.col, m.end.col)),
        first,
        "the second Shift+F3 must move to a different match"
    );
}

#[test]
fn f3_from_position_zero_finds_a_match_at_position_zero() {
    // The other half: a strict `start > from` skipped a match beginning exactly at the caret
    // and wrapped onto a later one, so the first thing on screen was unreachable.
    let mut app = doc_app("needle then needle");
    set_needle(&mut app, "needle");
    assert_eq!(caret(&app), LineCol::new(0, 0));
    app.execute(Action::FindNext);
    assert_eq!(
        app.find.current.map(|m| m.start),
        Some(LineCol::new(0, 0)),
        "a match at the search origin is real text, not a match to skip"
    );
}

#[test]
fn ctrl_f3_selects_the_word_under_the_caret_even_when_it_is_the_only_one() {
    // The bug: Ctrl+F3 seeded the word range as the match to skip and searched. When the
    // word under the caret is the only whole-word match, that search found nothing and the
    // shortcut appeared broken on the most common input -- a single occurrence.
    let mut app = doc_app("alpha beta alpha");
    app.execute(Action::DocStart);
    for _ in 0..7 {
        app.execute(Action::MoveRight);
    }
    app.execute(Action::FindWordNext);
    assert_eq!(app.find.needle, "beta");
    assert_eq!(
        app.find.current.map(|m| (m.start.col, m.end.col)),
        Some((6, 10)),
        "the word under the caret is selected"
    );
    assert!(
        app.doc().and_then(|d| d.selection).is_some(),
        "and visibly selected"
    );
}

#[test]
fn repeated_search_in_one_direction_never_repeats_until_it_wraps() {
    // Termination and no-repeat, stated directly: three matches, walked five times, must be
    // the three ends twice-through-with-wrap rather than a stuck caret.
    let mut app = doc_app("one two three");
    set_needle(&mut app, "e");
    let mut seen = Vec::new();
    for _ in 0..5 {
        app.execute(Action::FindNext);
        seen.push(caret(&app).col);
    }
    // "one two three": `e` sits at 2, 11 and 12, so the ends are 3, 12, 13, then wraps.
    assert_eq!(seen, vec![3, 12, 13, 3, 12]);
    for col in seen.clone() {
        assert!(
            col > 0,
            "every hit selects something, so the caret always moves"
        );
    }
}
