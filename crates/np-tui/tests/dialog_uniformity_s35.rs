//! Dialogs must answer the mouse and the keyboard the same way (S35).
//!
//! Three defects, one shape: a dialog had two ways to say "this one" and the two devices used
//! different ones.
//!
//! 1. `[Open]` read the path field and ignored the highlighted row, so selecting a file with the
//!    mouse and pressing the button reset the selection to the top and opened nothing.
//! 2. A Recent Files row dispatched `Action::RecentFiles` -- "open the Recent Files dialog" --
//!    because that is what the File menu's header runs. The path lived only in the row's label.
//!    Clicking worked (the hit-test reads the label); Enter reopened the list on itself.
//! 3. `DialogInput::route` delivered a character only when the list did *not* have focus, and a
//!    click gives the list focus. So after any click you could not type: the path field was
//!    unreachable without a Tab, and the Language/Theme picker's `D::Char` filter arm was dead code
//!    after touching the list it filters.
//!
//! Each test here drives the *pair* -- mouse gesture then keyboard gesture -- because every defect
//! was invisible to single-device tests.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, Mode};
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

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    }
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

/// The first listing row of the open dialog, as a clickable point. Taken from the focus items so
/// the test cannot drift when a dialog is re-centred or resized.
fn first_row_point(app: &App) -> (u16, u16) {
    let items = app.dialog_focus_items();
    let item = items
        .iter()
        .find(|it| format!("{:?}", it.id).starts_with("ListRow"))
        .expect("dialog has list rows");
    (item.area.x + 3, item.area.y)
}

fn recent_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["first.txt", "second.txt", "third.txt"] {
        fs::write(dir.path().join(name), "x\n").expect("write");
    }
    dir
}

// ------------------------------------------------------------ Recent Files

#[test]
fn enter_on_a_recent_files_row_opens_that_file() {
    // The row's action named the dialog rather than the file, so Enter re-opened the list the user
    // was already looking at -- indistinguishable from a dead key.
    let dir = recent_dir();
    let mut app = app();
    for name in ["first.txt", "second.txt", "third.txt"] {
        app.open_path(&dir.path().join(name));
    }
    app.execute(Action::RecentFiles);
    assert!(matches!(app.mode(), Mode::Dialog(DialogKind::RecentFiles)));
    let rows = np_tui::ui::dialogs::choice_rows(&app);
    assert_eq!(rows.len(), 3, "three files should be listed");

    // Pick a row by the label actually on screen rather than by a guessed index, so the test says
    // "the row I highlighted opened" instead of "the list is ordered the way I assumed".
    let wanted = 1usize;
    let expected = std::path::Path::new(&rows[wanted].0)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .expect("row label is a path");
    // Reach the row the way a user does: Down from row 0. Setting `dialog_focus` directly was the
    // tempting shortcut and is wrong -- it writes one of the two fields the framework reconciles,
    // so `focused_id` keeps answering the stale row and the test would pass while proving nothing.
    for _ in 0..wanted {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.mode(), Mode::Editing, "Enter must close the list");
    let active = app
        .doc()
        .and_then(|d| d.path.clone())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    assert_eq!(
        active.as_deref(),
        Some(expected.as_str()),
        "the highlighted row must be the file that opened"
    );
}

#[test]
fn clicking_a_recent_files_row_still_opens_it() {
    // The pointer path was never broken; asserted so a "fix" on the keyboard side cannot quietly
    // move the divergence rather than remove it.
    let dir = recent_dir();
    let mut app = app();
    for name in ["first.txt", "second.txt", "third.txt"] {
        app.open_path(&dir.path().join(name));
    }
    app.execute(Action::RecentFiles);
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing);
    let active = app
        .doc()
        .and_then(|d| d.path.clone())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    assert_eq!(active.as_deref(), Some("third.txt"), "row 0 is the newest");
}

// ------------------------------------------------------- typing after a click

#[test]
fn a_character_reaches_the_path_field_after_clicking_the_listing() {
    // Before the routing fix this key was dropped on the floor: `route` only passed a character
    // when the list did not have focus, and a click gives it focus.
    let dir = recent_dir();
    let mut app = app();
    app.open_file_picker(np_tui::app::FilePurpose::Open);
    app.file_input
        .set_text(format!("{}/", dir.path().display()));
    app.refresh_file_entries();

    let layout = np_tui::ui::dialogs::file_picker_layout(&app);
    let area = layout.list_area();
    click(&mut app, area.x + 3, area.y + 1);
    assert!(app.list_focused(), "the click focuses the listing");

    // Clear the field and type a fresh path, which is what a user reaching for the keyboard after
    // a click is usually doing.
    press(&mut app, KeyCode::End);
    for _ in 0..200 {
        press(&mut app, KeyCode::Backspace);
    }
    let target = dir.path().join("second.txt");
    type_str(&mut app, &target.display().to_string());

    assert_eq!(
        app.file_input.text(),
        target.display().to_string(),
        "typing must reach the field even though the list had focus"
    );
    assert!(
        !app.list_focused(),
        "typing must return focus to the field, or the next Enter acts on the old row"
    );
}

#[test]
fn the_picker_filter_still_works_after_clicking_the_list() {
    // The Language/Theme picker's `D::Char` arm was unreachable after any click, for the same
    // router reason. A filter you can type into only until you touch the list it filters is not a
    // filter.
    let mut app = app();
    app.execute(Action::ThemePicker);
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    assert!(app.list_focused());

    type_str(&mut app, "drac");
    assert_eq!(
        app.theme_query, "drac",
        "the keystrokes must reach the filter"
    );
    let names = app.picker_candidates(DialogKind::ThemePicker);
    assert!(
        names.iter().any(|n| n == "Dracula"),
        "filtering to Dracula should leave it visible: {names:?}"
    );
}

#[test]
fn typing_in_the_picker_filter_moves_focus_off_the_row() {
    // Direct assertion of the focus move, independent of whether the filter works. A mutation that
    // keeps filtering functional but leaves focus on the row passes every behavioural test, because
    // nothing else reads `focused_id` here -- and it is exactly the disagreement between highlight
    // and answer that caused the reported bugs.
    let mut app = app();
    app.execute(Action::LanguagePicker);
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    assert!(app.list_focused(), "the click focuses the listing");
    type_str(&mut app, "ru");
    assert!(
        !app.list_focused(),
        "focus must return to the query field: {:?}",
        app.focused_id()
    );
    assert_eq!(app.lang_query, "ru");
}

#[test]
fn backspace_after_a_click_edits_the_field_rather_than_the_list() {
    // Backspace used to be gated on `!list_focused` too, so a typo in the path could not be
    // corrected after a click without leaving the listing first.
    let dir = recent_dir();
    let mut app = app();
    app.open_file_picker(np_tui::app::FilePurpose::Open);
    let before = app.file_input.text().to_string();
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    press(&mut app, KeyCode::Backspace);
    assert!(
        app.file_input.text().len() < before.len(),
        "Backspace must edit the path: {:?} -> {:?}",
        before,
        app.file_input.text()
    );
    let _ = dir;
}

#[test]
fn space_on_a_focused_list_row_is_not_typable_text() {
    // The one character with two meanings. Keeping it bound to the list is deliberate: a dialog
    // with checkboxes needs Space to toggle, and the routing change had to preserve that.
    let mut app = app();
    app.execute(Action::ThemePicker);
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    press(&mut app, KeyCode::Char(' '));
    assert!(
        app.theme_query.is_empty(),
        "Space must not leak into the query while a list row has focus"
    );
}

#[test]
fn caret_keys_do_not_steal_focus_from_the_listing() {
    // Home/End are claimed by the list while it has focus (jump to first/last row). They are not
    // field edits, so they must not drag focus back and strand the highlight.
    let mut app = app();
    app.execute(Action::ThemePicker);
    let (x, y) = first_row_point(&app);
    click(&mut app, x, y);
    press(&mut app, KeyCode::End);
    assert!(
        app.list_focused(),
        "End on a focused list moves the row cursor and keeps focus there"
    );
    assert!(
        app.theme_query.is_empty(),
        "a caret key is not a keystroke into the filter"
    );
}

// ------------------------------------------------- S36: dialog open state

/// The open state the reported bug was about: does the row the renderer highlights equal the row
/// the accept path will act on? A mismatch means the first click on a freshly opened list moves
/// the highlight, which reads as "the selection jumped".
fn assert_opens_agreeing(app: &App, what: &str) {
    let field = match app.mode() {
        Mode::Dialog(DialogKind::Open) | Mode::Dialog(DialogKind::SaveAs) => app.file_selected,
        Mode::Dialog(DialogKind::LanguagePicker) => app.lang_selected,
        Mode::Dialog(DialogKind::ThemePicker) => app.theme_selected,
        Mode::Dialog(DialogKind::DocumentList) => app.document_list_selected,
        Mode::Dialog(DialogKind::Preferences) => app.prefs_selected,
        Mode::Dialog(DialogKind::EncodingPicker)
        | Mode::Dialog(DialogKind::EolMenu)
        | Mode::Dialog(DialogKind::RecentFiles) => app.choice_cursor(),
        other => panic!("not a list dialog: {other:?}"),
    };
    assert_eq!(
        app.list_cursor(),
        field,
        "{what}: highlight (renderer reads {field}) and accept path (reads {}) disagree on open",
        app.list_cursor()
    );
}

#[test]
fn every_list_dialog_opens_with_its_highlight_on_the_row_it_will_accept() {
    // The pickers and the Document List pushed their mode after writing only their own cursor
    // field, skipping `reset_dialog_focus()` -- so the shared row cursor kept whatever row the
    // *previous* dialog had left, and the two answers to "this one" diverged from frame one.
    let dir = recent_dir();
    let cases: [(&str, Action); 8] = [
        ("Language picker", Action::LanguagePicker),
        ("Theme picker", Action::ThemePicker),
        ("Open dialog", Action::OpenFile),
        ("Recent Files", Action::RecentFiles),
        ("Document List", Action::DocumentList),
        ("Encoding menu", Action::EncodingMenu),
        ("EOL menu", Action::EolMenu),
        ("Preferences", Action::Preferences),
    ];
    for (what, action) in cases {
        let mut app = app();
        for name in ["first.txt", "second.txt", "third.txt"] {
            app.open_path(&dir.path().join(name));
        }
        app.execute(action);
        assert!(matches!(app.mode(), Mode::Dialog(_)), "{what} should open");
        assert_opens_agreeing(&app, what);
    }
}

#[test]
fn the_language_picker_opens_on_the_documents_language() {
    // A C++ file opening a 76-entry alphabetic list: the highlight has to be on C++, and the
    // accept path has to agree with the highlight. Row 0 was the reported symptom.
    let dir = recent_dir();
    let path = dir.path().join("probe.cpp");
    fs::write(&path, "int main() {}\n").expect("write");
    let mut app = app();
    app.open_path(&path);
    app.execute(Action::LanguagePicker);

    let c_plus_plus = app
        .picker_candidates(DialogKind::LanguagePicker)
        .iter()
        .position(|n| n == "C++")
        .expect("C++ is an offered language");
    assert_ne!(
        c_plus_plus, 0,
        "the seed row must not be the top of the list"
    );
    assert_eq!(
        app.lang_selected, c_plus_plus,
        "the highlight should sit on the document's language"
    );
    assert_opens_agreeing(&app, "Language picker on a .cpp file");
}

#[test]
fn a_picker_reopens_on_the_current_selection_not_where_it_was_left() {
    // `reset_dialog_focus` copies the per-dialog field into the shared cursor. With the field
    // stale, a picker left on row 50 reopened 41 rows away from the language actually in use.
    let dir = recent_dir();
    let path = dir.path().join("probe.cpp");
    fs::write(&path, "int main() {}\n").expect("write");
    let mut app = app();
    app.open_path(&path);

    app.execute(Action::LanguagePicker);
    let seeded = app.lang_selected;
    press(&mut app, KeyCode::Down);
    for _ in 0..40 {
        press(&mut app, KeyCode::Down);
    }
    assert!(
        app.lang_selected > seeded,
        "the picker should have been left further down the list"
    );
    press(&mut app, KeyCode::Esc);

    app.execute(Action::LanguagePicker);
    assert_eq!(
        app.lang_selected, seeded,
        "reopen must land on the document's language, not the row the last visit scrolled to"
    );
    assert_opens_agreeing(&app, "Language picker reopen");
}

#[test]
fn filtering_a_picker_aims_the_cursor_at_the_best_match() {
    // The old code clamped the previous list's index into the filtered list. With the cursor on
    // row 63 of 76, typing "Ruby" clamped 63 to 2 -- "Ruby on Rails" -- and Enter applied that.
    // The filter ranks by score, so row 0 is the answer; the cursor belongs there.
    let dir = recent_dir();
    let path = dir.path().join("probe.cpp");
    fs::write(&path, "int main() {}\n").expect("write");
    let mut app = app();
    app.open_path(&path);
    app.execute(Action::LanguagePicker);
    for _ in 0..60 {
        press(&mut app, KeyCode::Down);
    }

    type_str(&mut app, "Ruby");
    let candidates = app.picker_candidates(DialogKind::LanguagePicker);
    assert!(
        candidates.iter().any(|n| n == "Ruby"),
        "Ruby should survive the filter: {candidates:?}"
    );
    assert_eq!(
        app.list_cursor(),
        0,
        "a filter re-ranks the list, so the cursor must point at row 0"
    );
    assert_opens_agreeing(&app, "Language picker after filtering");

    let best = candidates[0].clone();
    press(&mut app, KeyCode::Enter);
    let applied = app.doc().map(|d| d.language.clone()).unwrap_or_default();
    assert_eq!(
        best, "Ruby",
        "the best-ranked filter hit should be the exact name the user typed"
    );
    assert_eq!(
        applied,
        np_core::LanguageId::Named(best),
        "Enter after typing a language name must apply the best match, not a clamped neighbour"
    );
}

#[test]
fn the_document_list_opens_on_the_active_tab() {
    // Same class of bug on a different dialog: the highlight stayed on tab 0 while Enter activated
    // whichever row the previous dialog had left in the shared cursor.
    let dir = recent_dir();
    let mut app = app();
    for name in ["first.txt", "second.txt", "third.txt"] {
        app.open_path(&dir.path().join(name));
    }
    // Step back to a tab that is not the last, so "highlighted the active tab" cannot pass by
    // coincidence with a cursor that merely defaulted to one end of the list.
    app.execute(Action::PrevTab);
    let active = app.active;
    assert!(active > 0, "the test needs a tab that is not the first");

    app.execute(Action::DocumentList);
    assert_eq!(
        app.document_list_selected, active,
        "the list should highlight the tab in use"
    );
    assert_opens_agreeing(&app, "Document List");

    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.active, active,
        "Enter on the highlighted row keeps the tab in use"
    );
}
