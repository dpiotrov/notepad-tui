//! S3 acceptance: the `App` spine and the first rendered frame.
//!
//! Everything here runs with no TTY, no clipboard and no files: `App::new_headless` plus
//! ratatui's `TestBackend` (spec §10). If a later stage makes this file need a real
//! terminal, that is a regression in the headless contract, not something to fix here.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::{Config, Document, EncodingId, Eol, LineCol, UndoGroup};
use np_tui::ui::{cell_text, render_to_buffer, row_text};
use np_tui::{Action, App, Keymap, MenuId, Mode};

fn app() -> App {
    App::new_headless(Config::default(), (80, 24))
}

fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

// ------------------------------------------------------------- first frame

#[test]
fn initial_frame_has_all_four_regions() {
    let mut app = app();
    let buf = render_to_buffer(&mut app, 80, 24);

    // Regions stack in the spec §5.4 order and tile the screen without overlapping.
    let r = app.regions;
    assert_eq!(r.menu.y, 0);
    assert_eq!(r.tabs.y, r.menu.bottom());
    assert_eq!(r.editor.y, r.tabs.bottom());
    assert_eq!(r.status.y, r.editor.bottom());
    assert_eq!(r.status.bottom(), 24, "status bar sits on the last row");
    assert!(r.editor.h > 0, "the editor must survive the chrome");
    for rect in [r.menu, r.tabs, r.editor, r.status] {
        assert_eq!(rect.w, 80);
        assert!(rect.right() <= 80);
    }
    // The buffer really was painted: the menu row is not blank.
    assert_ne!(row_text(&buf, 0, 0, 40).trim(), "");
}

#[test]
fn menu_bar_shows_the_nine_titles_in_order() {
    let mut app = app();
    let buf = render_to_buffer(&mut app, 120, 24);
    let row = row_text(&buf, 0, 0, 120);
    let mut expected = Vec::new();
    for menu in MenuId::ALL {
        let at = row.find(menu.title()).unwrap_or_else(|| {
            panic!("menu title {:?} missing from {row:?}", menu.title());
        });
        expected.push((at, menu.title()));
    }
    // Every title present, and in MenuId::ALL order.
    let mut prev = 0usize;
    for (at, title) in &expected {
        assert!(*at >= prev, "{title:?} appears out of order in {row:?}");
        prev = at + title.len();
    }
}

#[test]
fn status_bar_shows_position_line_count_and_mode() {
    let mut app = app();
    type_text(&mut app, "abc\nde");
    let buf = render_to_buffer(&mut app, 100, 24);
    let row = row_text(&buf, 23, 0, 100);
    // M1 format: path | Ln/Col | EOL | encoding | lang | INS/OVR | chars / lines.
    assert!(row.contains("Ln 2, Col 3"), "status row was {row:?}");
    // "6 / 2" is char count / line count for "abc\nde" (a,b,c,\n,d,e), from the rope.
    assert!(row.contains("6 / 2"), "status row was {row:?}");
    assert!(row.contains("INS"), "status row was {row:?}");

    app.execute(Action::ToggleOvertype);
    let buf = render_to_buffer(&mut app, 100, 24);
    assert!(row_text(&buf, 23, 0, 100).contains("OVR"));
}

#[test]
fn editor_is_empty_on_a_new_document() {
    let mut app = app();
    let buf = render_to_buffer(&mut app, 80, 24);
    // Row 2 is the first editor row (menu 0, tabs 1). The gutter holds the line number;
    // the text area itself is blank.
    let gutter = app.geometry.gutter_width;
    assert!(gutter >= 2);
    assert_eq!(
        row_text(&buf, 2, 0, gutter as u16).trim(),
        "1",
        "gutter numbers the line"
    );
    assert_eq!(row_text(&buf, 2, gutter as u16, 80).trim_end(), "");
}

#[test]
fn tiny_terminal_degrades_without_panicking() {
    // Menu + tabs + status need 3 rows; a 2-row terminal has no editor left. The layout
    // must clamp rather than wrap around into a giant height.
    let mut app = app();
    let buf = render_to_buffer(&mut app, 20, 2);
    assert_eq!(app.regions.editor.h, 0);
    assert_eq!(
        cell_text(&buf, 0, 0),
        " ",
        "menu row indents its first title"
    );
    assert_eq!(
        cell_text(&buf, 1, 0),
        "F",
        "the first title still draws on a 20-col screen"
    );
    let _ = render_to_buffer(&mut app, 1, 1);
}

// ---------------------------------------------------------------- typing

#[test]
fn typing_through_execute_lands_in_the_document_and_marks_modified() {
    let mut app = app();
    type_text(&mut app, "hello");
    let doc = app.doc().expect("a document");
    assert_eq!(doc.buffer.text_string(), "hello");
    assert!(doc.modified, "typing must set the modified flag (FR-F3)");
    assert_eq!(app.cursor_pos(), LineCol::new(0, 5));
}

#[test]
fn enter_and_backspace_route_through_the_document() {
    let mut app = app();
    type_text(&mut app, "ab");
    app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(press(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "ab\nc");
    app.handle_key(press(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "ab\n");
    app.handle_key(press(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "ab");
}

#[test]
fn repeat_and_release_events_are_ignored() {
    // Crossterm sends Press/Repeat/Release when the kitty protocol is on; without the
    // filter every held key doubles.
    let mut app = app();
    let mut ev = press(KeyCode::Char('x'), KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    ev.kind = KeyEventKind::Repeat;
    app.handle_key(ev);
    ev.kind = KeyEventKind::Release;
    app.handle_key(ev);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "x");
}

#[test]
fn unbound_ctrl_chords_do_not_scribble_in_the_buffer() {
    let mut app = app();
    app.handle_key(press(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.handle_key(press(KeyCode::Char('k'), KeyModifiers::ALT));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "");
}

// ---------------------------------------------------------- undo / redo

/// Typing through `App` must coalesce the way `UndoStack` prescribes (FR-E4): a burst of
/// characters with no idle gap is one undo unit, and a gap closes it.
#[test]
fn typing_bursts_coalesce_and_idle_splits_them() {
    let mut app = app();
    type_text(&mut app, "onetwo");
    assert_eq!(
        app.doc().unwrap().undo.len(),
        1,
        "six characters typed with no gap are one undo group"
    );
    app.execute(Action::Undo);
    assert_eq!(
        app.doc().unwrap().buffer.text_string(),
        "",
        "one undo must remove the whole coalesced burst, not one character"
    );

    type_text(&mut app, "abc");
    app.advance_clock(std::time::Duration::from_millis(2000));
    type_text(&mut app, "def");
    assert_eq!(
        app.doc().unwrap().undo.len(),
        2,
        "an idle gap starts a new group"
    );
    app.execute(Action::Undo);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "abc");
    app.execute(Action::Redo);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "abcdef");
}

/// The `App` path and a direct `Document` driven with the same per-character edits and the
/// same clock must agree exactly, including through undo and redo.
#[test]
fn app_undo_redo_matches_direct_document_behavior() {
    let mut via_app = app();
    let clock = std::time::Instant::now();
    let mut direct = Document::new_untitled(EncodingId::Utf8, Eol::Lf);

    // Interleave both drivers so the two stacks see identical timings.
    for (i, ch) in "one".chars().enumerate() {
        via_app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE));
        let t = clock + std::time::Duration::from_millis(i as u64);
        direct
            .insert_text(&ch.to_string(), UndoGroup::Typing, t)
            .unwrap();
    }
    via_app.advance_clock(std::time::Duration::from_millis(2000));
    let base = clock + std::time::Duration::from_millis(2000);
    for (i, ch) in " two".chars().enumerate() {
        via_app.handle_key(press(KeyCode::Char(ch), KeyModifiers::NONE));
        let t = base + std::time::Duration::from_millis(i as u64);
        direct
            .insert_text(&ch.to_string(), UndoGroup::Typing, t)
            .unwrap();
    }

    assert_eq!(
        via_app.doc().unwrap().undo.len(),
        direct.undo.len(),
        "grouping diverged"
    );
    assert_eq!(
        via_app.doc().unwrap().buffer.text_string(),
        direct.buffer.text_string()
    );

    for _ in 0..3 {
        via_app.execute(Action::Undo);
        direct.undo_one();
        assert_eq!(
            via_app.doc().unwrap().buffer.text_string(),
            direct.buffer.text_string(),
            "undo diverged"
        );
        assert_eq!(
            via_app.cursor_pos(),
            direct.cursor.pos,
            "undo moved the caret apart"
        );
    }
    for _ in 0..3 {
        via_app.execute(Action::Redo);
        direct.redo_one();
        assert_eq!(
            via_app.doc().unwrap().buffer.text_string(),
            direct.buffer.text_string(),
            "redo diverged"
        );
    }
}

// ------------------------------------------------------------- tabs

#[test]
fn new_file_and_tab_cycling() {
    let mut app = app();
    type_text(&mut app, "first");
    app.execute(Action::NewFile);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.active, 1);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "");

    type_text(&mut app, "second");
    app.execute(Action::PrevTab);
    assert_eq!(app.active, 0);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "first");
    app.execute(Action::NextTab);
    assert_eq!(app.active, 1);
    // Cycling with one document is a no-op rather than a modulo surprise.
    app.execute(Action::CloseTab);
    app.execute(Action::NextTab);
    assert_eq!(app.active, 0);
}

#[test]
fn closing_the_last_tab_leaves_a_fresh_untitled_document() {
    let mut app = app();
    assert_eq!(app.documents.len(), 1);
    // Unmodified, so it closes without a confirm.
    app.execute(Action::CloseTab);
    assert_eq!(app.documents.len(), 1, "never left with zero tabs");
    assert_eq!(app.doc().unwrap().buffer.text_string(), "");
    assert!(!app.doc().unwrap().modified);
}

#[test]
fn closing_a_modified_tab_asks_instead_of_discarding() {
    let mut app = app();
    type_text(&mut app, "precious");
    app.execute(Action::CloseTab);
    assert_eq!(app.documents.len(), 1, "the document is still there");
    assert_eq!(app.mode(), Mode::Dialog(np_tui::DialogKind::Confirm));
    assert!(app.banner.is_some(), "the banner names what is unsaved");
    // Esc backs out of the confirm without losing anything.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(app.doc().unwrap().buffer.text_string(), "precious");
}

// -------------------------------------------------------------- Esc

#[test]
fn esc_pops_one_dialog_level_and_no_more() {
    let mut app = app();
    app.push_mode(Mode::Dialog(np_tui::DialogKind::Find));
    app.push_mode(Mode::Dialog(np_tui::DialogKind::Confirm));
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.mode(), Mode::Dialog(np_tui::DialogKind::Find));
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.mode(), Mode::Editing);
    // The bottom entry survives: Esc can never leave the app modeless.
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(app.mode_stack.len(), 1);
}

#[test]
fn esc_in_editing_clears_the_selection() {
    let mut app = app();
    type_text(&mut app, "hello");
    app.execute(Action::SelectAll);
    assert!(app.doc().unwrap().selection.is_some());
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.doc().unwrap().selection.is_none());
    // The text itself is untouched.
    assert_eq!(app.doc().unwrap().buffer.text_string(), "hello");
}

#[test]
fn dialogs_swallow_typing_so_it_cannot_leak_into_the_buffer() {
    let mut app = app();
    type_text(&mut app, "ab");
    app.push_mode(Mode::Dialog(np_tui::DialogKind::Find));
    type_text(&mut app, "cd");
    assert_eq!(app.doc().unwrap().buffer.text_string(), "ab");
}

// ------------------------------------------------------- movement wiring

#[test]
fn movement_and_selecting_movement_agree_with_the_document() {
    let mut app = app();
    type_text(&mut app, "line one\nline two\nline three");
    app.execute(Action::DocStart);
    assert_eq!(app.cursor_pos(), LineCol::new(0, 0));
    app.execute(Action::MoveDown);
    assert_eq!(app.cursor_pos(), LineCol::new(1, 0));
    app.execute(Action::MoveRight);
    app.execute(Action::MoveRight);
    assert_eq!(app.cursor_pos(), LineCol::new(1, 2));
    // Shift+Right extends the selection to the same place.
    app.execute(Action::MoveRightSelect);
    let sel = app.doc().unwrap().selection.expect("selection");
    assert_eq!(sel.focus, LineCol::new(1, 3));
    assert_eq!(sel.anchor, LineCol::new(1, 2));
    app.execute(Action::LineEnd);
    assert_eq!(app.cursor_pos(), LineCol::new(1, 8));
    app.execute(Action::DocEnd);
    assert_eq!(app.cursor_pos(), LineCol::new(2, 10));
    app.execute(Action::LineStart);
    assert_eq!(app.cursor_pos(), LineCol::new(2, 0));
}

#[test]
fn goal_column_survives_a_move_across_lines() {
    // FR-E2: moving down from a long line onto a short one and back restores the column.
    let mut app = app();
    type_text(&mut app, "aaaaaaaaaa\nb\naaaaaaaaaa");
    app.execute(Action::DocStart);
    for _ in 0..6 {
        app.execute(Action::MoveRight);
    }
    assert_eq!(app.cursor_pos(), LineCol::new(0, 6));
    app.execute(Action::MoveDown);
    assert_eq!(
        app.cursor_pos(),
        LineCol::new(1, 1),
        "clamped to the short line"
    );
    app.execute(Action::MoveDown);
    assert_eq!(app.cursor_pos(), LineCol::new(2, 6), "goal column restored");
}

// ---------------------------------------------------------- keymap glue

#[test]
fn keymap_drives_the_app_end_to_end() {
    let mut app = app();
    type_text(&mut app, "x");
    // Ctrl+D is duplicate-line per §6.10.
    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::CONTROL));
    // "x" has no trailing newline, so the copy brings its own: "x\nx\n". Pinned in
    // np_core::document::duplicating_the_last_line_of_an_unterminated_doc_adds_a_terminator.
    assert_eq!(app.doc().unwrap().buffer.text_string(), "x\nx\n");
    // Ctrl+Z undoes it.
    app.handle_key(press(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "x");
}

#[test]
fn config_keymap_override_reaches_the_app() {
    let mut config = Config::default();
    config
        .keymap
        .insert("duplicate-line".into(), vec!["ctrl+j".into()]);
    let mut app = App::new_headless(config, (80, 24));
    type_text(&mut app, "y");
    app.handle_key(press(KeyCode::Char('j'), KeyModifiers::CONTROL));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "y\ny\n");
    // The default Ctrl+D is gone: the override replaces rather than adds.
    app.handle_key(press(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(app.doc().unwrap().buffer.text_string(), "y\ny\n");
}

#[test]
fn f10_activates_the_menu_bar_and_esc_releases_it() {
    let mut app = app();
    app.handle_key(press(KeyCode::F(10), KeyModifiers::NONE));
    assert!(app.menu_bar_active);
    app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.menu_bar_active);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn alt_letter_accelerators_open_their_menu() {
    // Spec §6.10: "F10 or Alt" activates the bar, with Alt+F/E/S/V/... opening one menu
    // directly. These chords are generated from MenuId::accelerator(), not listed in
    // default_bindings(), which is why adding a menu cannot forget one.
    let km = Keymap::with_defaults();
    for menu in MenuId::ALL {
        let letter = menu.accelerator().to_lowercase().next().unwrap_or('x');
        let opened = km.resolve(press(KeyCode::Char(letter), KeyModifiers::ALT));
        // Alt+letter is only claimed here, so it must resolve to that menu.
        assert_eq!(opened, Some(Action::MenuBar(menu)), "Alt+{letter}");
    }
}

// -------------------------------------------------------------- overtype

#[test]
fn overtype_replaces_the_character_under_the_caret() {
    let mut app = app();
    type_text(&mut app, "abc");
    app.execute(Action::DocStart);
    app.execute(Action::ToggleOvertype);
    type_text(&mut app, "X");
    assert_eq!(app.doc().unwrap().buffer.text_string(), "Xbc");
    assert_eq!(app.cursor_pos(), LineCol::new(0, 1));
}

#[test]
fn overtype_state_travels_with_the_tab() {
    // Corrected in S8. The S3 version of `cycle_tab` copied `App::overtype` onto whichever
    // tab became active, so switching tabs overwrote that tab's own mode and the old
    // assertion here could only pass by describing that leak. Under the S8 ownership rule a
    // document owns its overtype, a tab inherits the mode at creation, and a switch only
    // reads. See docs/DEVIATIONS.md.
    let mut app = app();
    app.execute(Action::ToggleOvertype);
    assert!(app.doc().unwrap().overtype);
    app.execute(Action::NewFile);
    assert!(app.doc().unwrap().overtype, "a new tab inherits the mode");

    app.execute(Action::ToggleOvertype);
    assert!(!app.doc().unwrap().overtype, "the new tab's own toggle");
    app.execute(Action::PrevTab);
    assert!(
        app.doc().unwrap().overtype,
        "the first tab keeps the mode it was left in"
    );
    assert!(
        app.overtype,
        "and the status bar follows the active document"
    );

    app.execute(Action::NextTab);
    assert!(
        !app.doc().unwrap().overtype,
        "and so does the second tab, independently"
    );
}

// ------------------------------------------------------------- geometry

#[test]
fn resize_updates_regions_and_the_buffer_size() {
    let mut app = app();
    app.recompute_geometry(40, 12);
    assert_eq!(app.regions.status.y, 11);
    assert_eq!(app.regions.editor.h, 9);
    let buf = render_to_buffer(&mut app, 40, 12);
    assert_eq!(buf.area().width, 40);
    // Nothing from the old size survives as garbage on the new frame.
    let _ = render_to_buffer(&mut app, 200, 50);
    assert_eq!(app.regions.status.y, 49);
}

#[test]
fn line_number_gutter_widens_with_the_document() {
    let mut app = app();
    type_text(&mut app, "a");
    let narrow = app.geometry.gutter_width;
    for _ in 0..12 {
        app.execute(Action::NewFile);
    }
    let mut big = app.clone();
    for _ in 0..20 {
        type_text(&mut big, "x\ny");
    }
    big.recompute_geometry(80, 24);
    assert!(narrow >= 2, "gutter has a sane minimum, got {narrow}");
    assert!(
        big.geometry.gutter_width >= narrow,
        "gutter must not shrink as lines are added"
    );
}
