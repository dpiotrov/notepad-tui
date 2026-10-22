//! S22: opening a list item by mouse.
//!
//! Reported as: "Search in files results is scrollable, but not selectable by mouse" and "opening
//! a list element should be done by double-click, not one -- it is more reliable and more familiar
//! to computer users".
//!
//! # Why the results looked dead
//!
//! A click on a Find in Files result selected it. It did that correctly all along. But the row
//! already looked selected, so a click produced no visible change and the panel read as
//! unresponsive. Nothing opened, because `SelectFindHit` was written and the double-click that the
//! code comment promised was never implemented.
//!
//! # Why single-click-to-open is the wrong default
//!
//! In a *file* listing especially, opening on the first click means the pointer brushes a
//! neighbouring name on the way to a button and a file turns up in a new tab. Selecting on press
//! and opening on double-click is what every desktop file list does, it keeps the press's own
//! feedback (the row highlights), and it makes drag-to-scan safe.
//!
//! # Why the click count is keyed by cell and not by row
//!
//! Selecting a row re-centres the list's window around the cursor, so between the two clicks of a
//! deliberate double-click the entry under a fixed cell can change. Keying the count by row index
//! made a double-click on `d29` at the top of a scrolled list resolve to whatever slid into that
//! cell, and the picker went to `d19`. The count follows the pointer the user is repeating; *which*
//! entry opens is resolved from the press and carried on the intent.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, FilePurpose, Mode};

const W: u16 = 100;
const H: u16 = 30;

fn press(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn release(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

/// One click: press then release at the same cell, as a terminal delivers them.
fn click(app: &mut App, x: u16, y: u16) {
    press(app, x, y);
    release(app, x, y);
}

/// Two clicks at the same cell, which is what opens a list row since S22.
///
/// No clock advance between them, so the second lands inside `CLICK_MULTI_WINDOW` -- the same
/// reason two real clicks within 250 ms count as one double-click.
fn double_click(app: &mut App, x: u16, y: u16) {
    click(app, x, y);
    click(app, x, y);
}

fn char(app: &mut App, c: char) {
    let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn doc_app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/s22-current.txt".into(),
        "the current buffer\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

/// A tree of 40 files each holding one `needle`, in a directory unique to this call.
///
/// Unique per call rather than per process: these tests run in parallel and each asserts an exact
/// hit count, so a shared file could be rewritten while another test was walking it.
fn hit_tree() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(format!("/tmp/notepad-tui-s22-{}-{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let text: String = (0..40)
        .map(|i| format!("needle {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(dir.join("hits.txt"), text);
    dir
}

/// A finished Find in Files search over a 40-hit tree.
fn fif_app() -> App {
    let mut app = doc_app();
    app.execute(Action::FindInFiles);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    for c in "needle".chars() {
        char(&mut app, c);
    }
    app.fif.dir = hit_tree().to_string_lossy().into_owned();
    assert!(np_tui::fif::start(&mut app).is_some());
    np_tui::fif::drain(&mut app, 100_000);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert_eq!(app.fif.hits.len(), 40, "the fixture should have 40 hits");
    app
}

/// A file picker listing `d00/` .. `d39/` plus `..`.
fn picker() -> (App, std::path::PathBuf) {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(format!("/tmp/notepad-tui-s22p-{}-{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for i in 0..40 {
        let _ = std::fs::create_dir_all(dir.join(format!("d{i:02}")));
    }
    let mut app = doc_app();
    app.last_dir = Some(dir.clone());
    app.open_file_picker(FilePurpose::Open);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    (app, dir)
}

fn list_area(app: &App) -> np_core::RectCore {
    let layout = np_tui::ui::layout::FilePicker::new(app);
    layout.list_area()
}

fn active_path(app: &App) -> String {
    app.doc()
        .and_then(|d| d.path.as_ref())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ------------------------------------------------------------ Find in Files

#[test]
fn a_single_click_on_a_result_selects_it_and_opens_nothing() {
    let mut app = fif_app();
    let docs_before = app.documents.len();
    let current = active_path(&app);
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    let row = panel.y + 4 + 3;
    let want = np_tui::ui::find_in_files::row_index_at(&app, row).expect("a result row");

    click(&mut app, panel.x + 10, row);
    assert_eq!(app.fif.selected, want, "a click should select the row");
    assert_eq!(
        app.documents.len(),
        docs_before,
        "a single click must not open a file"
    );
    assert_eq!(active_path(&app), current, "and must not change the buffer");
}

#[test]
fn a_double_click_on_a_result_opens_that_file() {
    let mut app = fif_app();
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    let row = panel.y + 4 + 3;
    let want = np_tui::ui::find_in_files::row_index_at(&app, row).expect("a result row");
    let want_path = app.fif.hits[want].path.clone();

    double_click(&mut app, panel.x + 10, row);
    assert_eq!(
        app.fif.selected, want,
        "the row opened should still be the one clicked"
    );
    assert_eq!(
        active_path(&app),
        want_path.to_string_lossy().as_ref(),
        "a double-click should open the file under the pointer"
    );
}

#[test]
fn opening_a_result_leaves_the_panel_up_so_a_second_hit_can_be_opened() {
    let mut app = fif_app();
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    double_click(&mut app, panel.x + 10, panel.y + 4);
    let after_first = app.documents.len();
    // A different row, so no multi-click carry-over is involved.
    let row2 = panel.y + 4 + 5;
    double_click(&mut app, panel.x + 10, row2);
    assert_eq!(
        app.documents.len(),
        after_first + 1,
        "the panel must stay usable after a double-click"
    );
}

#[test]
fn two_clicks_on_different_rows_never_open() {
    // The multi-click count must reset when the pointer moves, or scanning a list by clicking
    // down it would open files the user never meant to open.
    let mut app = fif_app();
    let docs_before = app.documents.len();
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    click(&mut app, panel.x + 10, panel.y + 4);
    click(&mut app, panel.x + 10, panel.y + 5);
    click(&mut app, panel.x + 10, panel.y + 6);
    assert_eq!(
        app.documents.len(),
        docs_before,
        "clicking down a list is scanning, not opening"
    );
}

// ------------------------------------------------------------ file picker

#[test]
fn a_single_click_in_the_file_picker_selects_and_does_not_descend() {
    let (mut app, dir) = picker();
    let area = list_area(&app);
    let row = area.y + 2;
    let before = {
        let text: String = app.file_input.text().into();
        text
    };

    click(&mut app, area.x + 3, row);
    assert!(
        app.file_selected > 0,
        "the click should have moved the row cursor"
    );
    assert_eq!(
        app.file_input.text(),
        before,
        "a single click must select, not open or descend"
    );
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.as_path()),
        "and must not change the listed directory"
    );
}

#[test]
fn a_double_click_on_a_directory_row_descends_into_it() {
    let (mut app, dir) = picker();
    let area = list_area(&app);
    let row = area.y + 2;
    // Resolve which entry the row draws before touching the app mutatively, so the assertion below
    // cannot be influenced by what the click did to the row cursor.
    let items = app.dialog_focus_items();
    let idx = np_tui::ui::framework::focus_at(&items, area.x + 3, row)
        .and_then(|i| app.focus_child_row(i))
        .expect("the row is a list row");
    let name = app.file_entries[idx].name.clone();
    assert!(name.ends_with('/'), "fixture row should be a directory");
    let want = dir.join(name.trim_end_matches('/'));

    double_click(&mut app, area.x + 3, row);
    assert_eq!(
        app.last_dir.as_deref(),
        Some(want.as_path()),
        "expected to descend into {name:?}, picker is at {:?}",
        app.last_dir
    );
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Open));
}

#[test]
fn a_double_click_on_the_parent_row_goes_up_one_level() {
    // A fresh picker is already listing `dir`, so `..` is a real move -- up to `/tmp`'s child of
    // the temp root -- and no descend is needed to set the scene. Descending first was tempting and
    // wrong twice over: the subdirectory is empty, and the descend's own double-click leaves the
    // multi-click count armed on its cell, so the next click looks like a second click.
    let (mut app, dir) = picker();
    let area = list_area(&app);
    assert_eq!(app.file_entries[0].name, "../");

    // A single click on `..` selects it and goes nowhere.
    click(&mut app, area.x + 3, area.y);
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.as_path()),
        "a single click on `..` must not go up"
    );

    // A second click on the same cell now opens it, and the listing moves up exactly one level.
    press(&mut app, area.x + 3, area.y);
    release(&mut app, area.x + 3, area.y);
    assert_eq!(
        app.last_dir.as_deref(),
        dir.parent(),
        "a double-click on `..` goes up exactly one level"
    );
}

#[test]
fn a_dialog_button_acts_on_one_press_and_never_waits_for_a_double() {
    // The picker's two buttons are "Open" (0) and "toggle hidden" (1). Neither is a row, so
    // neither should be held back for a second click -- that deferral belongs to list rows only,
    // and applying it to buttons would make every dialog feel broken.
    //
    // "toggle hidden" is the cleaner thing to assert because it is idempotent to observe and has no
    // side effect on the mode: the dialog stays open either way, so the only question is whether the
    // flag flipped on the first press.
    let (mut app, _dir) = picker();
    let items = app.dialog_focus_items();
    let button = items
        .iter()
        .rev()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::Button(_)))
        .expect("the picker has buttons");
    let (x, y) = (button.area.x + 1, button.area.y);
    drop(items);

    let before = app.show_hidden;
    press(&mut app, x, y);
    assert_ne!(
        app.show_hidden, before,
        "a button must act on the first press, not wait for a second click"
    );
}

#[test]
fn a_button_press_does_not_fire_twice() {
    // Buttons act on press. If the release also activated, a single click would toggle the flag and
    // toggle it straight back -- the exact double-fire that a Find checkbox hit in an earlier round.
    let (mut app, _dir) = picker();
    let items = app.dialog_focus_items();
    let button = items
        .iter()
        .rev()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::Button(_)))
        .expect("the picker has buttons");
    let (x, y) = (button.area.x + 1, button.area.y);
    drop(items);

    let before = app.show_hidden;
    click(&mut app, x, y);
    assert_ne!(
        app.show_hidden, before,
        "a full click must flip the flag once, not twice"
    );
}
#[test]
fn the_click_count_survives_a_release_so_the_second_click_is_recognised() {
    // The mechanism the whole feature rests on. A release clears `dialog_press`, so a count stored
    // there could never reach 2; it has to come from `ClickState`.
    let (mut app, _dir) = picker();
    let area = list_area(&app);
    let row = area.y + 2;
    click(&mut app, area.x + 3, row);
    assert_eq!(
        app.click.count, 1,
        "one click after a release still counts as one"
    );
    click(&mut app, area.x + 3, row);
    assert_eq!(
        app.click.count, 2,
        "the count must accumulate across a release, or nothing can ever open"
    );
}

#[test]
fn a_slow_second_click_is_not_a_double_click() {
    let (mut app, dir) = picker();
    let area = list_area(&app);
    let row = area.y + 2;
    click(&mut app, area.x + 3, row);
    // Push the recorded click outside the 250 ms window.
    app.click.at -= std::time::Duration::from_millis(400);
    click(&mut app, area.x + 3, row);
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.as_path()),
        "two deliberate separate clicks must not open"
    );
}

#[test]
fn every_dialog_listing_shares_the_gesture_not_just_the_file_picker() {
    // The point of putting the rule in the routing layer rather than in the picker: Recent Files,
    // the document list, the theme and language pickers and the encoding menus all resolve through
    // the same row-child arm, so they cannot drift into behaving differently from each other --
    // which is the same class of drift that made Settings mouse-less and Themes keyboard-less.
    let mut app = doc_app();
    for i in 0..3 {
        let path = std::path::PathBuf::from(format!(
            "/tmp/notepad-tui-s22doc-{}-{i}.txt",
            std::process::id()
        ));
        let _ = std::fs::write(&path, "x\n");
        app.open_document(Document::from_decoded(
            path,
            "x\n",
            EncodingId::Utf8,
            Eol::Lf,
            false,
        ));
    }
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::DocumentList);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(matches!(app.mode(), Mode::Dialog(DialogKind::DocumentList)));

    let items = app.dialog_focus_items();
    let row = items
        .iter()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(1)))
        .expect("the document list has rows");
    let (x, y) = (row.area.x + 1, row.area.y);
    drop(items);

    click(&mut app, x, y);
    assert!(
        matches!(app.mode(), Mode::Dialog(DialogKind::DocumentList)),
        "one click selects and leaves the list open"
    );
    assert_eq!(app.document_list_selected, 1);
    click(&mut app, x, y);
    assert_eq!(
        app.mode(),
        Mode::Editing,
        "the second click activates, exactly as it does in the file picker"
    );
}
