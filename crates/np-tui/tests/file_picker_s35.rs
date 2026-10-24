//! The picker's accept paths agree across devices (S35).
//!
//! # The report
//!
//! "In File dialog when I selected file by mouse and then press Open button, the file selection
//! line jumps to first position in the list."
//!
//! `[Open]` called `commit_file`, which read the path field and nothing else. In the Open dialog
//! that field holds the directory being listed, so the accept resolved to `EnterDir`, reset the
//! cursor to row 0, and re-read the listing with the dialog still open. The user watched the
//! highlight jump to the top and nothing open. Double-clicking the same row worked, because that
//! goes through `accept_clicked_file_row` and reads the row -- so two devices drove one button to
//! two different meanings, and the keyboard was the one ignoring the visible selection.
//!
//! # Why the fix is an intent flag rather than "a row is selected"
//!
//! The first attempt asked whether `file_selected` was in range. It always is: the picker opens with
//! the cursor on row 0, which is `../`. That variant made two existing tests fail -- type
//! `/tmp/notes.txt`, press Enter, and the untouched `../` highlight hijacked the typed path. A
//! selected row is not a chosen row, and the distinction has to be recorded rather than inferred.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::app::{App, DialogKind, FilePurpose, Mode};
use std::fs;

const W: u16 = 100;
const H: u16 = 30;

fn app() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn click(app: &mut App, x: u16, y: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_mouse(&MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
}

/// A directory whose listing is stable and long enough that row 0 is never the row under test.
fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["alpha.txt", "bravo.txt", "charlie.txt", "delta.txt"] {
        fs::write(dir.path().join(name), "x\n").expect("write");
    }
    dir
}

fn picker_at(dir: &std::path::Path, purpose: FilePurpose) -> App {
    let mut app = app();
    app.open_file_picker(purpose);
    app.file_input
        .set_text(format!("{}/", dir.display()).replace("//", "/"));
    app.refresh_file_entries();
    app
}

/// A point inside listing row `index`. The x comes from the layout rather than a constant: the
/// picker is centred, so its left edge moves with the terminal width, and a hardcoded column can
/// land outside the list (and on a button) without any assertion noticing.
fn click_row(app: &mut App, index: usize) {
    let area = np_tui::ui::dialogs::file_picker_layout(app).list_area();
    let (x, y) = (area.x + 3, area.y + index as u16);
    click(app, x, y);
}

fn button_pos(app: &App, which: u16) -> (u16, u16) {
    let layout = np_tui::ui::dialogs::file_picker_layout(app);
    let b = layout.button_row(which);
    (b.x + 1, b.y)
}

fn active_name(app: &App) -> Option<String> {
    app.doc()
        .and_then(|d| d.path.clone())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
}

#[test]
fn clicking_a_row_then_the_open_button_opens_that_file() {
    // The reported bug, end to end through the real mouse and button paths.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    // `alpha.txt` is row 1; row 0 is `../`.
    click_row(&mut app, 1);
    eprintln!(
        "DBG after click: sel={} chosen={} focused={:?}",
        app.file_selected,
        app.file_row_chosen,
        app.focused_id()
    );
    assert_eq!(app.file_selected, 1, "the click should select alpha.txt");
    let (bx, by) = button_pos(&app, 0);
    click(&mut app, bx, by);

    assert_eq!(app.mode(), Mode::Editing, "[Open] must close the dialog");
    assert_eq!(
        active_name(&app).as_deref(),
        Some("alpha.txt"),
        "[Open] must open the row the pointer selected"
    );
}

#[test]
fn the_open_button_does_not_reset_the_selection_it_was_about_to_use() {
    // The visible symptom: the highlight jumping to row 0. Asserted separately from the outcome
    // because the original code reset the cursor *and* opened nothing, and a fix that opened the
    // right file while still clobbering the cursor would pass the test above.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    click_row(&mut app, 2);
    assert_eq!(app.file_selected, 2);
    let (bx, by) = button_pos(&app, 0);
    click(&mut app, bx, by);
    // Dialog closed, so any cursor state is irrelevant -- but if it did NOT close, the selection
    // must not have been silently moved to the top.
    if app.mode() == Mode::Dialog(DialogKind::Open) {
        assert_ne!(
            app.file_selected, 0,
            "the selection jumped to row 0 instead of being used"
        );
    }
}

#[test]
fn arrow_key_selection_then_enter_opens_that_file() {
    // The keyboard half of the same gesture, so the two devices cannot diverge again.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(
        active_name(&app).as_deref(),
        Some("bravo.txt"),
        "two steps down from `../` is bravo.txt"
    );
}

#[test]
fn a_typed_path_still_opens_even_though_row_zero_is_highlighted() {
    // The constraint that the naive "a row is selected" rule violated. The picker always has a
    // cursor -- row 0, `../` -- so an untouched highlight must never outrank a typed path.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    let target = dir.path().join("delta.txt");
    app.file_input.set_text(target.display().to_string());
    app.refresh_file_entries();
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing, "the typed path must open");
    assert_eq!(active_name(&app).as_deref(), Some("delta.txt"));
}

#[test]
fn typing_after_a_click_gives_the_decision_back_to_the_field() {
    // Pointing then typing is the user changing their mind: the keystroke, not the stale
    // highlight, is the current intent. This also covers the routing fix -- before it, a character
    // was swallowed outright once the listing had focus, so this gesture was not "the field wins",
    // it was "nothing happens at all".
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    click_row(&mut app, 1);
    assert!(app.file_row_chosen, "a click is a choice");

    // Replace the field's contents by typing, the way a person correcting a path does.
    press(&mut app, KeyCode::End);
    for _ in 0..200 {
        press(&mut app, KeyCode::Backspace);
    }
    let target = dir.path().join("charlie.txt");
    for c in target.display().to_string().chars() {
        let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    }
    let text = app.file_input.text().to_string();
    assert_eq!(
        text,
        target.display().to_string(),
        "typing must reach the field"
    );
    assert!(
        !app.file_row_chosen,
        "editing the path releases the row claim: field is {text}"
    );
    assert!(
        !app.list_focused(),
        "typing must bring focus back to the field"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        active_name(&app).as_deref(),
        Some("charlie.txt"),
        "the typed path wins over the earlier click; field was {text}"
    );
}

#[test]
fn typing_in_the_path_field_moves_focus_off_the_clicked_row() {
    // The focus move asserted on its own. Without it, a build that clears the row claim via the
    // listing re-read but leaves focus sitting on the old row still opens the right file from the
    // keyboard, and the defect only shows up the next time Enter is pressed -- which is the exact
    // highlight-versus-answer disagreement behind every bug in this file.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    click_row(&mut app, 1);
    assert!(app.list_focused(), "the click focuses the listing");
    press(&mut app, KeyCode::Char('x'));
    assert!(
        !app.list_focused(),
        "a keystroke must return focus to the field: {:?}",
        app.focused_id()
    );
    assert!(
        app.file_input.text().ends_with('x'),
        "and the character must land in the path"
    );
}

#[test]
fn moving_the_caret_inside_the_field_keeps_the_row_choice() {
    // Home/End move the caret without changing the path, so they must not cost the user their
    // selection: a gesture that changes nothing should cancel nothing.
    //
    // The field is left pointing at the real directory so the listing stays populated -- typing a
    // partial name into it would filter the listing to just `../` and the click below would land
    // on a button instead of a row, which is exactly the kind of invalid setup that lets a test
    // pass without exercising anything.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    click_row(&mut app, 1);
    assert!(
        app.file_row_chosen && app.file_selected == 1,
        "the click should select the first file row"
    );

    // Tab into the path field, then move the caret. Focus must be in the field for Home/End to be
    // caret keys at all -- while the listing has focus they jump to the first and last row.
    press(&mut app, KeyCode::Tab);
    let text = app.file_input.text().to_string();
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::End);
    assert_eq!(
        app.file_input.text(),
        text,
        "a caret move must not change the path"
    );
    assert!(
        app.file_row_chosen,
        "a caret move must not release the row claim"
    );
}

#[test]
fn descending_clears_the_choice_made_in_the_previous_directory() {
    // Row numbers do not survive a listing re-read: row 2 of the old directory is unrelated to
    // row 2 of the new one.
    let dir = tree();
    fs::create_dir(dir.path().join("sub")).expect("mkdir");
    fs::write(dir.path().join("sub/nested.txt"), "n\n").expect("write");
    let mut app = picker_at(dir.path(), FilePurpose::Open);
    click_row(&mut app, 1);
    assert!(app.file_row_chosen);
    // Descend via `..`-adjacent navigation: point the field at the subdirectory and re-read.
    app.file_input
        .set_text(format!("{}/sub/", dir.path().display()));
    app.refresh_file_entries();
    assert!(
        !app.file_row_chosen,
        "a fresh listing must not inherit a choice made against the old one"
    );
}

#[test]
fn save_as_prefers_a_typed_name_over_a_highlighted_row() {
    // In Save As the field is the answer, not the location. The intent flag handles this for free:
    // typing clears it, so a name typed over a highlighted row saves the typed name.
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::SaveAs);
    click_row(&mut app, 1);
    let target = dir.path().join("brand-new.txt");
    app.file_input.set_text(target.display().to_string());
    app.refresh_file_entries();
    assert_eq!(app.picker_accept_row(), None, "the typed name wins");
    press(&mut app, KeyCode::Enter);
    assert!(target.exists(), "the typed name must be what got written");
    assert_eq!(active_name(&app).as_deref(), Some("brand-new.txt"));
}

#[test]
fn save_as_accepts_a_row_when_nothing_was_typed() {
    let dir = tree();
    let mut app = picker_at(dir.path(), FilePurpose::SaveAs);
    click_row(&mut app, 2);
    assert_eq!(
        app.picker_accept_row(),
        Some(2),
        "a chosen row with an untouched directory field is a real choice"
    );
}
