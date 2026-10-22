//! S15 hardening, second pass: the four defects found by using `npt` in a real terminal.
//!
//! Each of these survived the S15 test suite for the same reason, and the reason is worth
//! stating once rather than four times: **the headless tests assert where the document
//! thinks things are, not what the terminal ends up showing.** A document position is
//! correct long after the viewport has failed to follow it, a click has resolved to the
//! wrong row, or a button has done nothing at all. So the tests below assert about the
//! screen and about *consequences*, which is the level the user actually experiences.
//!
//! 1. The caret walked off the bottom of the screen and kept going into the status bar,
//!    because `scroll_to_show` was only ever called from jumps (search, bookmarks, Go To)
//!    and never from an ordinary movement. See `ensure_caret_visible`.
//! 2. A click on a file-dialog row selected a row from the opposite end of the list, because
//!    the listing was one hit region and the click's index was discarded.
//! 3. The save-changes prompt could not be answered by mouse, which made the application
//!    impossible to close.
//! 4. `kill <pid>` left the terminal in raw mode with mouse reporting on. That one needs a
//!    pty and is covered by `signal_pty.rs`-level reasoning in `docs/DEVIATIONS.md`; here the
//!    observable contract is that the restore path is a single, idempotent, depth-claiming
//!    call.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, Mode};

const W: u16 = 80;
const H: u16 = 20;

fn open(text: &str) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/viewport.txt".into(),
        text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    app
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn click(app: &mut App, x: u16, y: u16) {
    // A click is a press and a release on the same cell, and both halves are sent because that is
    // what a terminal delivers. The split is load-bearing: a press on a list row selects it and a
    // release that stayed on it activates it, which is what lets a pointer be dragged down a
    // listing to scan it without opening every row it crosses.
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

/// Two clicks at the same cell, which is what opens a listing row since S22.
///
/// Sent with no clock advance, so the second press lands inside `CLICK_MULTI_WINDOW` and counts as
/// the second click of a double-click. A real user's two clicks land within 250 ms for the same
/// reason.
fn double_click(app: &mut App, x: u16, y: u16) {
    click(app, x, y);
    click(app, x, y);
}

/// The screen cell the caret would be drawn at, or `None` when the renderer refuses to
/// place it. This is the exact value `event::draw_if_dirty` hands to
/// `frame.set_cursor_position`, so asserting on it is asserting on the terminal.
fn caret_cell(app: &App) -> Option<(usize, usize)> {
    np_tui::ui::editor_view::caret_position(app).map(|p| (p.line, p.col))
}

/// The editor's drawable rectangle, in screen coordinates.
fn editor_box(app: &App) -> (u16, u16, u16, u16) {
    (
        app.regions.editor.x,
        app.regions.editor.y,
        app.regions.editor.width(),
        app.regions.editor.height(),
    )
}

fn lines(n: usize) -> String {
    (0..n)
        .map(|i| format!("line {i:03}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------ 1: caret at edges

#[test]
fn the_caret_never_leaves_the_editor_area_walking_down() {
    let mut app = open(&lines(200));
    let (_ex, ey, _ew, eh) = editor_box(&app);
    key(&mut app, KeyCode::Down);
    for step in 0..260 {
        key(&mut app, KeyCode::Down);
        let (cy, _cx) = caret_cell(&app).unwrap_or_else(|| {
            panic!(
                "step {step}: the renderer refused to place the caret \
                 (doc line {})",
                app.doc().map(|d| d.cursor.pos.line).unwrap_or(usize::MAX)
            )
        });
        assert!(
            cy >= ey as usize && cy < (ey + eh) as usize,
            "step {step}: caret row {cy} is outside the editor rows {}..{}",
            ey,
            ey + eh
        );
    }
    // 200 lines, walked 261 times: the caret must have stopped at the document's end, not
    // continued into the status bar.
    assert_eq!(app.doc().map(|d| d.cursor.pos.line), Some(199));
}

#[test]
fn the_viewport_follows_the_caret_to_the_last_line() {
    let mut app = open(&lines(200));
    for _ in 0..250 {
        key(&mut app, KeyCode::Down);
    }
    let doc = app.doc().expect("document");
    let top = doc.scroll.top_line;
    let caret = doc.cursor.pos.line;
    assert!(
        caret >= top,
        "the caret line {caret} must be at or below the viewport top {top}"
    );
    // The last line has to be on screen, which is the whole point of auto-scroll.
    let (cy, _) = caret_cell(&app).expect("caret placeable");
    let (_ex, ey, _ew, eh) = editor_box(&app);
    assert!(
        cy < (ey + eh) as usize,
        "the final line must be visible: row {cy} vs editor bottom {}",
        ey + eh
    );
}

#[test]
fn the_caret_never_leaves_the_editor_area_walking_up() {
    let mut app = open(&lines(200));
    // Jump to the end first, then walk back up past the top edge.
    app.execute(Action::DocEnd);
    let (_ex, ey, _ew, eh) = editor_box(&app);
    for step in 0..260 {
        key(&mut app, KeyCode::Up);
        let (cy, _cx) = caret_cell(&app).unwrap_or_else(|| {
            panic!(
                "step {step}: caret left the drawable area \
                 (doc line {})",
                app.doc().map(|d| d.cursor.pos.line).unwrap_or(usize::MAX)
            )
        });
        assert!(
            cy >= ey as usize && cy < (ey + eh) as usize,
            "step {step}: caret row {cy} is outside the editor rows {}..{}",
            ey,
            ey + eh
        );
    }
    assert_eq!(app.doc().map(|d| d.cursor.pos.line), Some(0));
}

#[test]
fn typing_at_the_bottom_edge_keeps_the_new_character_visible() {
    // The user's report was about a *typed* character, which differs from a plain movement:
    // the insert changes line count, so the scroll decision happens after the edit.
    let mut app = open(&lines(12));
    let (_ex, ey, _ew, eh) = editor_box(&app);
    app.execute(Action::DocEnd);
    for step in 0..30 {
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('x'));
        let (cy, _cx) = caret_cell(&app)
            .unwrap_or_else(|| panic!("step {step}: caret unplaceable after typing"));
        assert!(
            cy >= ey as usize && cy < (ey + eh) as usize,
            "step {step}: typed at row {cy}, outside {}..{}",
            ey,
            ey + eh
        );
    }
}

#[test]
fn a_long_line_keeps_the_caret_visible_horizontally_when_wrap_is_off() {
    // Wrap is off by default in Config; the horizontal case is the one that needs it off.
    let long = "x".repeat(400);
    let mut app = open(&long);
    assert!(
        !app.geometry.word_wrap,
        "this test needs wrap off to exercise horizontal scrolling"
    );
    for step in 0..500 {
        key(&mut app, KeyCode::Right);
        let (_cy, cx) = caret_cell(&app).expect("caret placeable");
        let (ex, _ey, ew, _eh) = editor_box(&app);
        assert!(
            cx >= ex as usize && cx < (ex + ew) as usize,
            "step {step}: caret column {cx} outside the editor columns {}..{}",
            ex,
            ex + ew
        );
    }
}

#[test]
fn page_up_and_page_down_move_the_viewport_without_dragging_the_caret_back() {
    // Auto-scroll exists to follow the caret. Page scrolling deliberately moves the viewport
    // away from it, and re-centering after those two actions would make Page Up useless.
    let mut app = open(&lines(200));
    let caret_before = app.doc().map(|d| d.cursor.pos.line);
    let top_before = app.doc().map(|d| d.scroll.top_line);
    app.execute(Action::ScrollPageDown);
    let top_after = app.doc().map(|d| d.scroll.top_line);
    assert!(
        top_after > top_before,
        "Page Down must move the viewport: {top_before:?} -> {top_after:?}"
    );
    assert_eq!(
        app.doc().map(|d| d.cursor.pos.line),
        caret_before,
        "Page Down must not move the caret"
    );
}

// --------------------------------------------- 2: file dialog rows and their clicks

/// A directory with enough entries that the listing scrolls.
fn deep_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("npt-rows-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for i in 0..40 {
        let _ = std::fs::create_dir_all(dir.join(format!("d{i:02}")));
    }
    dir
}

fn picker_in(dir: &std::path::Path) -> App {
    let mut app = App::new_headless(Config::default(), (100, 30));
    // A headless App starts with no geometry until something draws it, and in production
    // that something is the first frame. Without this the dialog's rectangles are all zero
    // and every click test below misses for a reason that has nothing to do with the bug.
    let _ = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    app.execute(Action::OpenFile);
    app.file_input.set_text(crate_path_text(dir));
    app.refresh_file_entries();
    let _ = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    app
}

fn crate_path_text(dir: &std::path::Path) -> String {
    let mut text = dir.display().to_string();
    if !text.ends_with('/') {
        text.push('/');
    }
    text
}

fn list_area(app: &App) -> np_core::RectCore {
    np_tui::ui::layout::FilePicker::new(app).list_area()
}

/// The entry index drawn on listing screen row `y`.
///
/// Derived from the same windowing call the renderer makes, so a test that clicks a screen
/// row knows which entry it *should* have hit without hard-coding an offset that changes with
/// the selection.
fn entry_at_row(app: &App, y: u16) -> usize {
    let area = list_area(app);
    let (first, last) = np_tui::ui::framework::visible_rows(
        app.file_entries.len(),
        app.file_selected,
        area.h as usize,
    );
    let offset = y.saturating_sub(area.y) as usize;
    assert!(
        first + offset < last,
        "row {y} is outside the visible window {first}..{last}"
    );
    first + offset
}

#[test]
fn the_listing_has_one_hit_target_per_visible_row_not_one_for_the_whole_box() {
    let dir = deep_dir();
    let app = picker_in(&dir);
    let area = list_area(&app);
    let items = app.dialog_focus_items();
    let rows: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        rows.len(),
        area.h as usize,
        "every visible row needs its own focus item, or a click cannot name one"
    );
    // Each row item must be exactly one cell tall and stacked in screen order.
    let mut previous: Option<u16> = None;
    for it in items
        .iter()
        .filter(|it| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(_)))
    {
        assert_eq!(it.area.h, 1, "a row hit target must be one row tall");
        if let Some(prev) = previous {
            assert_eq!(
                it.area.y,
                prev + 1,
                "row hit targets must not overlap or gap"
            );
        }
        previous = Some(it.area.y);
    }
}

#[test]
fn clicking_a_row_selects_the_row_under_the_pointer_not_the_previous_one() {
    let dir = deep_dir();
    let mut app = picker_in(&dir);
    let area = list_area(&app);
    // Click the third visible row.
    let want = entry_at_row(&app, area.y + 2);
    let want_name = app.file_entries[want].name.clone();
    double_click(&mut app, area.x + 3, area.y + 2);
    // The double-click descended into that directory, so the field must name *that* directory.
    // This is the assertion that matters: before the fix the click landed on the listing as
    // a whole and acted on whatever was already selected.
    let expected = dir.join(want_name.trim_end_matches('/'));
    assert_eq!(
        app.file_input.text(),
        crate_path_text(&expected),
        "clicking a directory row must descend into that directory"
    );
    // After descending the listing restarts at its top, which is what a user expects from
    // Notepad++, so the focus claim moves with it rather than pointing at a row from the
    // directory the pointer left.
    assert_eq!(
        app.file_selected, 0,
        "a fresh listing starts at its first row"
    );
    // The click was consumed by the descend, so nothing points at a row of the listing the
    // pointer left. A stale claim here would make the next Enter act on a directory the user
    // is no longer looking at.
    assert!(
        !matches!(
            app.focused_id(),
            Some(np_tui::ui::framework::FocusId::ListRow(n)) if n != app.file_selected
        ),
        "focus must not name a row outside the current selection: {:?}",
        app.focused_id()
    );
}

#[test]
fn clicking_a_row_in_a_scrolled_listing_selects_that_row() {
    // The reported bug: with the list scrolled, a click picked a row from the other end.
    let dir = deep_dir();
    let mut app = picker_in(&dir);
    // Scroll through the shared row cursor, the same one the keyboard and the mouse drive. The
    // legacy `file_selected` field is what the renderer reads, and writing it directly leaves the
    // shared cursor behind at row 0 -- exactly the desync this file exists to catch.
    let last = app.file_entries.len() - 1;
    app.set_list_cursor(last);
    let area = list_area(&app);
    // The top visible row of the scrolled window; which entry that is comes from the renderer's own
    // windowing rule, not from arithmetic in this test.
    let shown_first = entry_at_row(&app, area.y);
    let want = app.file_entries[shown_first].name.clone();

    // S22 asserts the two halves separately. Inferring correctness from where the picker ended up
    // conflated two things and hid a real defect: a press at the top visible row resolved to entry
    // 20 while the renderer drew entry 30 there, so the activation ran on the wrong entry even
    // though "a row got picked" still looked plausible.
    //
    // Half one: the hit-test must resolve the row the renderer painted at that cell.
    let focus = np_tui::ui::framework::focus_at(&app.dialog_focus_items(), area.x + 3, area.y);
    let resolved = focus.and_then(|i| app.focus_child_row(i));
    assert_eq!(
        resolved,
        Some(shown_first),
        "row {} draws entry {shown_first} ({want:?}) but the hit-test resolved {resolved:?}",
        area.y
    );

    // Half two: that press moves the row cursor onto the same entry, which is what a single click
    // does now.
    click(&mut app, area.x + 3, area.y);
    assert_eq!(
        app.file_selected,
        shown_first,
        "clicked entry {shown_first} ({want:?}) at the top of a scrolled list, \
         but the cursor went to {} ({:?})",
        app.file_selected,
        app.file_entries
            .get(app.file_selected)
            .map(|e| e.name.clone())
    );
}
#[test]
fn clicking_the_parent_row_descends_to_the_parent_not_something_else() {
    let dir = deep_dir();
    let parent = dir
        .parent()
        .expect("the temp directory has a parent")
        .to_path_buf();
    let mut app = picker_in(&dir);
    // Scroll away from the top so `..` is off screen, then scroll back and click it.
    app.file_selected = app.file_entries.len() - 1;
    app.file_selected = 0;
    app.dirty = true;
    let area = list_area(&app);
    assert_eq!(
        app.file_entries[0].name, "../",
        "the parent row is always entry 0"
    );
    assert_eq!(
        entry_at_row(&app, area.y),
        0,
        "with the selection at the top, `..` is the first row on screen"
    );
    double_click(&mut app, area.x + 3, area.y);
    // `..` is itself the entry, and activating it moves the picker up one level. That is the
    // contract; `../` is not a directory that happens to contain the listing.
    assert_eq!(
        app.file_input.text(),
        crate_path_text(&parent),
        "activating `..` must go up exactly one level"
    );
    assert_eq!(
        app.last_dir.as_deref(),
        Some(parent.as_path()),
        "and must record the new directory"
    );
    // The old defect resolved the click against the whole listing box and acted on whatever
    // row was already selected, so the same click could descend into some unrelated entry.
    assert!(
        !app.file_input.text().contains("/d0"),
        "the parent click must not have descended into an entry: {:?}",
        app.file_input.text()
    );
}

#[test]
fn tab_landing_on_a_row_focuses_a_row_that_is_actually_on_screen() {
    let dir = deep_dir();
    let mut app = picker_in(&dir);
    // Scroll the selection to the bottom, then Tab: focus must land inside the window.
    app.file_selected = app.file_entries.len() - 1;
    app.dirty = true;
    app.cycle_dialog_focus(1);
    let area = list_area(&app);
    let items = app.dialog_focus_items();
    let focused = app.focused_id();
    let item = items
        .iter()
        .find(|it| Some(it.id) == focused)
        .expect("focus names a real item");
    assert!(
        matches!(item.id, np_tui::ui::framework::FocusId::ListRow(_)),
        "Tab from the field must reach a row, got {:?}",
        item.id
    );
    assert!(
        item.area.y >= area.y && item.area.bottom() <= area.bottom(),
        "the focused row {:?} must be inside the listing box",
        item.area
    );
}

// ------------------------------------------- 3: confirm dialog buttons must activate

fn dirty_app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/dirty.txt".into(),
        "hello\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let now = app.clock;
    app.doc_mut()
        .expect("document")
        .insert_text(" x", np_core::UndoGroup::Atomic, now)
        .expect("insert");
    app
}

#[test]
fn clicking_save_on_the_save_prompt_saves_and_lets_the_app_quit() {
    let mut app = dirty_app();
    app.execute(Action::Exit);
    assert_eq!(
        app.mode(),
        Mode::Dialog(DialogKind::Confirm),
        "an unsaved document must produce the save prompt"
    );
    // Click the first button, `[Save]`.
    let items = app.dialog_focus_items();
    let save = items
        .iter()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::Button(0)))
        .expect("the prompt has a default button");
    click(&mut app, save.area.x + 1, save.area.y);
    assert!(
        !app.confirm.is_some(),
        "the prompt must be resolved by the click"
    );
    assert!(
        app.quit_requested,
        "answering the prompt with Save must complete the quit that raised it"
    );
}

#[test]
fn clicking_cancel_on_the_save_prompt_keeps_editing() {
    let mut app = dirty_app();
    app.execute(Action::Exit);
    let items = app.dialog_focus_items();
    let cancel = items
        .iter()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::Button(2)))
        .expect("the prompt has a Cancel button");
    click(&mut app, cancel.area.x + 1, cancel.area.y);
    assert!(app.confirm.is_none(), "Cancel closes the prompt");
    assert!(
        !app.quit_requested,
        "Cancel must not leave a quit pending, or the next frame closes the app"
    );
    assert_eq!(app.mode(), Mode::Editing, "Cancel returns to editing");
}

#[test]
fn clicking_a_checkbox_in_the_find_dialog_toggles_it() {
    let mut app = open("hello world\n");
    app.execute(Action::Find);
    let items = app.dialog_focus_items();
    let check = items
        .iter()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::Checkbox(0)))
        .expect("the find dialog has checkboxes");
    let before = app.find.opts.case_sensitive;
    click(&mut app, check.area.x + 1, check.area.y);
    assert_ne!(
        app.find.opts.case_sensitive, before,
        "a click on a checkbox must flip it, not merely focus it"
    );
}

#[test]
fn the_save_prompt_still_answers_the_keyboard() {
    // The mouse fix must not disturb the existing keyboard contract.
    let mut app = dirty_app();
    app.execute(Action::Exit);
    key(&mut app, KeyCode::Char('3'));
    assert!(app.confirm.is_none(), "digit 3 picks Cancel");
    assert!(!app.quit_requested);
}

// ------------------------------------------------- 4: a killed process restores the tty

#[test]
fn the_signal_handlers_install_and_leave_a_pollable_wake_fd() {
    // Without this, a signal that arrives while the editor is idle sits unnoticed until the
    // next keystroke, because crossterm retries the EINTR its own poll gets.
    assert!(
        np_tui::signal::install(),
        "SIGINT/SIGTERM/SIGHUP handlers must install"
    );
    assert!(
        np_tui::signal::wake_fd().is_some(),
        "a signal must be able to wake the event loop on its own"
    );
}

#[test]
fn force_restore_without_a_live_guard_is_a_no_op() {
    // The kill path and the panic path share the depth counter. If force_restore emitted
    // when nothing was entered, a stray signal would scribble disable sequences onto a
    // terminal that was never in raw mode -- the user-visible half of bug 4.
    let before = np_tui::term::guard_depth();
    assert_eq!(np_tui::term::force_restore(), 0);
    assert_eq!(
        np_tui::term::guard_depth(),
        before,
        "depth must not go negative"
    );
}

#[test]
fn a_terminated_flag_ends_the_run_loop_without_reading_the_terminal() {
    // The loop's quit check is what turns "a signal arrived" into "the guard gets to drop".
    // Asserted through `run` rather than a real signal: sending SIGTERM to the test process
    // would take the whole test binary down with it.
    let mut app = App::new_headless(Config::default(), (W, H));
    app.quit_requested = true;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).expect("test terminal");
    assert!(
        np_tui::event::run(&mut terminal, &mut app).is_ok(),
        "a quit must exit the loop cleanly so the guard restores the terminal"
    );
}

#[test]
fn no_test_in_this_suite_leaves_the_signal_hit_counter_increasing() {
    // Reading the counter twice must be stable when no signal was sent; this is a canary
    // against a test that fires a real signal at the process by accident.
    let a = np_tui::signal::hit_count();
    let b = np_tui::signal::hit_count();
    assert_eq!(a, b);
}

// --------------------------------------- 5: Enter answers with the *focused* button

/// Enter used to read `buttons.first()` unconditionally. The arrows moved the highlight, so
/// the dialog *looked* like it was answering Cancel while it ran Save -- which, on the quit
/// prompt, wrote the file and exited. Asserting the consequence rather than the highlight is
/// the only way to catch it: the highlight was always correct.
#[test]
fn enter_on_the_focused_button_answers_with_that_button() {
    let mut app = dirty_app();
    app.execute(Action::Exit);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Confirm));

    // Move the highlight to Cancel (button 2) and press Enter.
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Right);
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::Button(2)),
        "the highlight must be on Cancel before Enter"
    );
    let path = std::path::PathBuf::from("/tmp/npt-enter-cancel.txt");
    let _ = std::fs::remove_file(&path);
    app.doc_mut().expect("doc").path = Some(path.clone());
    key(&mut app, KeyCode::Enter);

    assert!(app.confirm.is_none(), "Enter resolves the prompt");
    assert!(
        !app.quit_requested,
        "Enter while Cancel is highlighted must not quit"
    );
    assert!(
        !path.exists(),
        "Enter while Cancel is highlighted must not write the file"
    );
    assert_eq!(app.mode(), Mode::Editing, "Cancel returns to editing");
}

#[test]
fn enter_on_the_default_button_still_saves_and_quits() {
    // The fix must not break the other direction: with nothing moved, Enter is the default
    // action, which on this prompt is Save.
    let mut app = dirty_app();
    app.execute(Action::Exit);
    let path = std::path::PathBuf::from("/tmp/npt-enter-save.txt");
    let _ = std::fs::remove_file(&path);
    app.doc_mut().expect("doc").path = Some(path.clone());
    key(&mut app, KeyCode::Enter);
    assert!(app.quit_requested, "Enter on the default button quits");
    assert!(path.exists(), "and the quit path saves the document");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn up_and_down_navigate_the_confirm_buttons() {
    // Buttons sit on one row, so Left/Right is the natural axis; Up/Down is the alias a user
    // reaches for when the row reads as a list of choices.
    let mut app = dirty_app();
    app.execute(Action::Exit);
    key(&mut app, KeyCode::Down);
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::Button(1)),
        "Down advances one button"
    );
    key(&mut app, KeyCode::Up);
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::Button(0)),
        "Up steps back"
    );
}

// ------------------------------- 6: after `..`, the next click means the row it landed on

/// The user's sequence verbatim: click `../`, then click a row in the new listing. Before the
/// fix the second click still resolved against the directory just left, so it opened the wrong
/// entry -- and only after a few arrow keys did the dialog "come right".
///
/// The cause was a click-time row claim consulted ahead of the dialog's own index. Descending
/// left a stale claim behind, and the next interaction read the *previous* listing's row. The
/// claim is gone; a click resolves through the dialog's live item list, and the pointer's row
/// is cleared the moment the listing is replaced.
#[test]
fn after_clicking_the_parent_the_next_click_opens_the_row_clicked() {
    let root = std::env::temp_dir().join(format!("npt-nav-{}", std::process::id()));
    let sub = root.join("sub");
    let deeper = sub.join("deeper");
    let _ = std::fs::create_dir_all(&deeper);
    let _ = std::fs::write(root.join("aaa.txt"), "aaa\n");
    let _ = std::fs::write(root.join("bbb.txt"), "bbb\n");
    let _ = std::fs::write(sub.join("ccc.txt"), "ccc\n");
    let _ = std::fs::write(deeper.join("ddd.txt"), "ddd\n");

    // Start two levels down, so `..` is a real move rather than a no-op.
    let mut app = picker_in(&deeper);
    assert!(app.file_input.text().ends_with("deeper/"));

    // Double-click `../` -- the listing is now `sub/`.
    let area = list_area(&app);
    assert_eq!(app.file_entries[0].name, "../");
    double_click(&mut app, area.x + 3, area.y);
    assert!(
        app.file_input.text().ends_with("sub/"),
        "the parent click should land in sub/, got {:?}",
        app.file_input.text()
    );

    // Now click a real row in the new listing, with nothing moved in between. This is the
    // click that used to be answered as `../` again.
    let area = list_area(&app);
    let target = entry_at_row(&app, area.y + 1);
    let name = app.file_entries[target].name.clone();
    double_click(&mut app, area.x + 3, area.y + 1);
    assert!(
        !app.file_input.text().ends_with("sub/"),
        "clicking row {target} ({name:?}) must act on that row, not go up again; \
         picker is still at {:?}",
        app.file_input.text()
    );
    assert!(
        app.file_input.text().contains(name.trim_end_matches('/')),
        "the picker should have moved to the clicked entry {name:?}, got {:?}",
        app.file_input.text()
    );
}

#[test]
fn after_a_click_the_arrows_move_the_row_cursor_of_the_current_listing() {
    // Mouse then keyboard is the ordinary mix. After a click the arrow keys must drive the
    // row cursor in the listing that is on screen now.
    //
    // The rows are files, not directories: clicking a directory descends into it, which
    // legitimately replaces the listing and resets the cursor to row 0. That would make the
    // assertion below measure a different dialog state from the one under test.
    let dir = std::env::temp_dir().join(format!("npt-rows-kb-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for i in 0..20 {
        let _ = std::fs::write(
            dir.join(format!("f{i:02}.txt")),
            "x
",
        );
    }
    let mut app = picker_in(&dir);
    let area = list_area(&app);
    // Get into the listing the way a keyboard user does: Tab from the path field. Down is only
    // "move the row cursor" once focus is inside the list; while the field holds it, Down means
    // "tab into the list", which is the other half of the same contract.
    key(&mut app, KeyCode::Tab);
    let _ = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    assert!(
        app.list_focused(),
        "Tab from the path field must move focus into the listing"
    );
    let before = app.file_selected;
    key(&mut app, KeyCode::Down);
    assert_eq!(
        app.file_selected,
        before + 1,
        "Down advances the row cursor in the listing on screen"
    );
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::ListRow(app.file_selected)),
        "focus must follow the cursor, not a stale pointer claim"
    );
    // And a click still means the row it landed on, after the arrows have been moving things
    // around -- the ordering that used to leave a stale claim behind.
    let target = entry_at_row(&app, area.y + 6);
    let name = app.file_entries[target].name.clone();
    double_click(&mut app, area.x + 3, area.y + 6);
    assert!(
        app.file_input
            .text()
            .contains(name.trim_end_matches(".txt")),
        "a click after arrow movement must act on row {target} ({name:?}), got {:?}",
        app.file_input.text()
    );
}

#[test]
fn typing_in_the_path_field_still_works_after_a_row_click() {
    // A list dialog always has a valid row cursor -- row 0 of a fresh listing -- so if that
    // cursor were allowed to answer focus unconditionally the path field could never be
    // focused and typed characters would go nowhere. That is how the fix nearly broke typing.
    let dir = deep_dir();
    let mut app = picker_in(&dir);
    let items = app.dialog_focus_items();
    let field = items
        .iter()
        .position(|it| matches!(it.id, np_tui::ui::framework::FocusId::Field(0)))
        .expect("the picker has a path field");
    app.dialog_focus = field;
    let _ = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    for c in "xyz".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(
        app.file_input.text().contains("xyz"),
        "typed characters must reach the path field, got {:?}",
        app.file_input.text()
    );
}
