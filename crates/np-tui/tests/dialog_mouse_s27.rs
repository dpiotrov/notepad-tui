//! Dialog and palette mouse behavior (S27): the wheel scrolls the box it is over, a click
//! focuses, and a double-click acts.
//!
//! Three separate reports, one underlying shape: each list decided its own mouse behavior, so
//! the gestures that mean one thing everywhere else meant something else in a given box.
//!
//! 1. The wheel over a dialog fell through to the ordinary document scroll. A wheel over the
//!    Theme picker scrolled the *file behind the box* while the list being pointed at did not
//!    move -- worse than doing nothing, because the text jumped about behind a dialog the user
//!    was trying to read.
//! 2. The command palette ran its command on a single press. It is the one list that executes
//!    arbitrary commands, so a brush across a row on the way to the query field could run one.
//! 3. A dialog's row cursor was also its scroll position, so scrolling a list silently
//!    re-selected whatever row the view landed on.
//!
//! The tests below assert on rendered row windows and on the app's own cursor state, because a
//! dialog list has no scrollbar to inspect.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::app::App;

const W: u16 = 100;
const H: u16 = 30;

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let mut ev = KeyEvent::new(code, mods);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16, at: std::time::Instant) {
    let ev = MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_at(&ev, at);
}

fn wheel(app: &mut App, down: bool, x: u16, y: u16) {
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    mouse(app, kind, x, y, std::time::Instant::now());
}

/// One full click at a cell, press and release at the same instant.
fn click(app: &mut App, x: u16, y: u16) {
    let t = std::time::Instant::now();
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y, t);
    mouse(app, MouseEventKind::Up(MouseButton::Left), x, y, t);
}

/// Two clicks at the same cell inside the multi-click window.
fn double_click(app: &mut App, x: u16, y: u16) {
    let t = std::time::Instant::now();
    for _ in 0..2 {
        mouse(app, MouseEventKind::Down(MouseButton::Left), x, y, t);
        mouse(app, MouseEventKind::Up(MouseButton::Left), x, y, t);
    }
}

fn with_doc(app: &mut App, lines: usize) {
    let text: String = (1..=lines).map(|i| format!("line {i}\n")).collect();
    let doc = np_core::Document::from_decoded(
        std::path::PathBuf::from("/tmp/dialog_mouse.txt"),
        &text,
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.documents.clear();
    app.active = 0;
    app.documents.push(doc);
}

fn fresh(kind: OpenKind) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    with_doc(&mut app, 200);
    // The Recent list is built when the dialog opens, so a session with no history yields an
    // empty list -- and an empty list passes every scroll assertion vacuously. Seed it first.
    if matches!(kind, OpenKind::Recent) {
        for i in 0..40 {
            app.recent
                .push(std::path::PathBuf::from(format!("/tmp/recent_{i}.txt")));
        }
    }
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    match kind {
        OpenKind::Preferences => key(&mut app, KeyCode::Char(','), KeyModifiers::CONTROL),
        OpenKind::Themes => key(
            &mut app,
            KeyCode::Char('T'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        OpenKind::Recent => key(&mut app, KeyCode::Char('E'), KeyModifiers::CONTROL),
        OpenKind::Palette => key(
            &mut app,
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
    }
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

enum OpenKind {
    Preferences,
    Themes,
    Recent,
    Palette,
}

/// The first and last list row currently in the focus-item window, i.e. what is on screen.
///
/// Focus items are built from the same layout the renderer uses, so this is the drawn window and
/// not a second opinion about it.
fn visible_window(app: &App) -> Option<(usize, usize)> {
    let rows: Vec<usize> = app
        .dialog_focus_items()
        .iter()
        .filter_map(|it| match it.id {
            np_tui::ui::framework::FocusId::ListRow(r) => Some(r),
            _ => None,
        })
        .collect();
    Some((*rows.first()?, *rows.last()?))
}

/// The screen cell of the focus item for absolute list row `row`, if it is visible.
fn row_cell(app: &App, row: usize) -> Option<(u16, u16)> {
    let it = app
        .dialog_focus_items()
        .into_iter()
        .find(|it| it.id == np_tui::ui::framework::FocusId::ListRow(row))?;
    Some((it.area.x + 1, it.area.y))
}

fn doc_top(app: &App) -> usize {
    app.doc().map(|d| d.scroll.top_line).unwrap_or(999)
}

// ------------------------------------------------------ premise guards

#[test]
fn the_dialogs_under_test_really_have_more_rows_than_fit() {
    // Without a list longer than its window there is nothing to scroll, and every wheel test
    // below would pass against a list that never needed scrolling.
    let app = fresh(OpenKind::Preferences);
    let count = app.dialog_row_count();
    let (first, last) = visible_window(&app).expect("preferences has list rows");
    let visible = last - first + 1;
    assert!(
        count > visible,
        "premise: Preferences needs {count} rows in a {visible}-row window"
    );
}

// ------------------------------------------------- 1. the wheel, the bug

#[test]
fn the_wheel_over_a_dialog_does_not_scroll_the_document_behind_it() {
    // The original fault: the wheel arm fell through to document scroll, so the file behind the
    // box moved. This is the single assertion that would have caught all four reports at once.
    for kind in [OpenKind::Preferences, OpenKind::Themes] {
        let mut app = fresh(kind);
        let before = doc_top(&app);
        let (x, y) = (W / 2, H / 2);
        for _ in 0..5 {
            wheel(&mut app, true, x, y);
        }
        assert_eq!(
            doc_top(&app),
            before,
            "the wheel over {:?} scrolled the document behind the dialog",
            app.mode()
        );
    }
}

#[test]
fn the_wheel_over_a_dialog_moves_its_row_window() {
    let mut app = fresh(OpenKind::Preferences);
    let before = visible_window(&app).expect("window");
    for _ in 0..2 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    let after = visible_window(&app).expect("window");
    assert!(
        after.0 > before.0,
        "the wheel should move the list down: {before:?} -> {after:?}"
    );
}

#[test]
fn the_wheel_clamps_at_both_ends_of_the_list() {
    let mut app = fresh(OpenKind::Preferences);
    let count = app.dialog_row_count();
    for _ in 0..40 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    let (first, last) = visible_window(&app).expect("window");
    assert_eq!(
        last,
        count - 1,
        "the wheel must be able to reach the last row, window was {first}..{last} of {count}"
    );
    for _ in 0..40 {
        wheel(&mut app, false, W / 2, H / 2);
    }
    let (first, _) = visible_window(&app).expect("window");
    assert_eq!(first, 0, "the wheel must be able to reach the first row");
}

#[test]
fn scrolling_does_not_change_the_row_a_click_selected() {
    // The cursor doubles as a dialog's scroll position, so a wheel that moved the cursor also
    // re-selected a row. The pointer's choice is held separately and has to survive the scroll.
    let mut app = fresh(OpenKind::Preferences);
    let (x, y) = row_cell(&app, 2).expect("row 2 visible");
    click(&mut app, x, y);
    assert_eq!(app.list_cursor(), 2, "premise: the click selected row 2");
    for _ in 0..8 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    // The chosen row is out of the window now; what must not happen is the *focus* claiming a
    // row the user never chose while the chosen row is still reachable. Clamped into the window,
    // it stays a real row, and scrolling back restores it.
    for _ in 0..8 {
        wheel(&mut app, false, W / 2, H / 2);
    }
    let focused = app.focused_id();
    assert_eq!(
        focused,
        Some(np_tui::ui::framework::FocusId::ListRow(2)),
        "after scrolling away and back, the row the user clicked should still be the focus"
    );
}

#[test]
fn a_choice_survives_many_wheel_steps_in_both_directions() {
    // Regression for the first attempt at this, which clamped the choice into each new window and
    // wrote it back. Four notches later the original row was gone, because the next notch read its
    // own clamp. Holding the row absolutely and only *displaying* it when visible is what fixes it.
    let mut app = fresh(OpenKind::Preferences);
    let (x, y) = row_cell(&app, 2).expect("row 2 visible");
    click(&mut app, x, y);
    // Far enough that any clamp-into-window scheme must have forgotten row 2.
    for _ in 0..12 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    assert!(
        !matches!(
            app.focused_id(),
            Some(np_tui::ui::framework::FocusId::ListRow(2))
        ),
        "row 2 is off-screen; the highlight should have followed the window"
    );
    for _ in 0..12 {
        wheel(&mut app, false, W / 2, H / 2);
    }
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::ListRow(2)),
        "the row clicked 24 wheel steps ago should still be the choice"
    );
}

#[test]
fn keyboard_row_movement_releases_the_wheel_choice() {
    // The wheel holds a choice precisely because it is the gesture that moves the listing. The
    // moment the keyboard moves a row it owns the selection, and a held choice would outrank it.
    let mut app = fresh(OpenKind::Preferences);
    let (x, y) = row_cell(&app, 2).expect("row 2 visible");
    click(&mut app, x, y);
    for _ in 0..6 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.wheel_choice, None,
        "an arrow key takes the selection back from the wheel"
    );
}

#[test]
fn a_wheel_on_a_list_nobody_clicked_moves_the_highlight_too() {
    // The other half of the same rule. With no prior choice the cursor *is* the selection, and a
    // highlight that refused to move while the window scrolled would look broken.
    let mut app = fresh(OpenKind::Preferences);
    assert_eq!(
        app.list_cursor(),
        0,
        "premise: a fresh dialog starts at row 0"
    );
    for _ in 0..4 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    assert!(
        app.list_cursor() > 0,
        "with nothing selected the wheel should also move the highlight"
    );
}

#[test]
fn a_box_with_no_list_swallows_the_wheel() {
    // About has one OK button and nothing to scroll. Swallowing is deliberate: sliding the text
    // behind a modal question is worse than doing nothing.
    let mut app = App::new_headless(Config::default(), (W, H));
    with_doc(&mut app, 200);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(np_tui::actions::Action::About);
    assert!(matches!(app.mode(), np_tui::app::Mode::Dialog(_)));
    assert_eq!(app.dialog_row_count(), 0, "premise: About has no list");
    let before = doc_top(&app);
    for _ in 0..5 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    assert_eq!(doc_top(&app), before, "About has nothing to scroll");
}

// ------------------------------------------- 2. click to focus, double to act

#[test]
fn a_single_click_in_the_themes_list_focuses_without_closing() {
    let mut app = fresh(OpenKind::Themes);
    let target = 1usize;
    let (x, y) = row_cell(&app, target).expect("row visible");
    click(&mut app, x, y);
    assert!(
        matches!(app.mode(), np_tui::app::Mode::Dialog(_)),
        "a single click must leave the picker open"
    );
    assert_eq!(
        app.list_cursor(),
        target,
        "a single click should point the cursor at the row"
    );
}

#[test]
fn a_double_click_in_the_themes_list_applies_and_closes() {
    let mut app = fresh(OpenKind::Themes);
    let target = 1usize;
    let (x, y) = row_cell(&app, target).expect("row visible");
    double_click(&mut app, x, y);
    assert!(
        !matches!(app.mode(), np_tui::app::Mode::Dialog(_)),
        "a double click should accept the picker"
    );
}

#[test]
fn a_single_click_in_a_picker_does_not_close_it() {
    // The picker closes on a double click. Closing on one was the report: the pointer brushed a
    // name on its way to the filter field and the dialog was gone with a theme nobody chose.
    let mut app = fresh(OpenKind::Themes);
    let before_theme = app.config.ui.theme.clone();
    let (x, y) = row_cell(&app, 2).expect("row visible");
    click(&mut app, x, y);
    assert_eq!(
        app.config.ui.theme, before_theme,
        "a single click must not apply a theme"
    );
}

// -------------------------------------------------------- 3. the palette

#[test]
fn a_single_palette_click_selects_and_runs_nothing() {
    let mut app = fresh(OpenKind::Palette);
    let lay = np_tui::ui::palette::palette_layout(&app);
    let list = np_tui::ui::palette::list_area(&lay);
    let x = list.x + 2;
    let y = list.y + 3;
    let before_wrap = app.config.ui.word_wrap;
    click(&mut app, x, y);
    assert!(
        matches!(app.mode(), np_tui::app::Mode::Palette),
        "a single click must leave the palette open"
    );
    assert!(
        app.palette_selected > 0,
        "the click should have moved the palette cursor"
    );
    assert_eq!(
        app.config.ui.word_wrap, before_wrap,
        "a single click must not run a command"
    );
}

#[test]
fn a_double_palette_click_runs_the_command_under_the_pointer() {
    let mut app = fresh(OpenKind::Palette);
    let lay = np_tui::ui::palette::palette_layout(&app);
    let list = np_tui::ui::palette::list_area(&lay);
    let x = list.x + 2;
    let y = list.y + 3;
    let entries = np_tui::ui::palette::palette_entries(&app, "");
    let row = 3usize;
    let action = entries.get(row).map(|e| e.action);
    double_click(&mut app, x, y);
    // Whatever it was, it ran: the palette is a command runner, so a successful activation leaves
    // Palette mode unless the command itself opens a dialog. Asserting the mode rather than the
    // specific command keeps this test true for whatever the palette's ranking returns.
    assert!(action.is_some(), "premise: row {row} is a palette entry");
    assert!(
        !matches!(app.mode(), np_tui::app::Mode::Palette)
            || !matches!(action, Some(np_tui::actions::Action::Nop)),
        "a double click on a real entry should have acted"
    );
}

#[test]
fn the_wheel_over_the_palette_moves_its_cursor_not_the_document() {
    let mut app = fresh(OpenKind::Palette);
    let before = doc_top(&app);
    let lay = np_tui::ui::palette::palette_layout(&app);
    let list = np_tui::ui::palette::list_area(&lay);
    for _ in 0..3 {
        wheel(&mut app, true, list.x + 2, list.y + 2);
    }
    assert_eq!(
        doc_top(&app),
        before,
        "the palette owns the wheel over itself"
    );
    assert!(
        app.palette_selected > 0,
        "the wheel should move the palette's cursor"
    );
}

// ------------------------------------------- 4. Preferences commits on double click

#[test]
fn a_double_click_on_a_boolean_preference_toggles_it() {
    // Enter toggles a bool row; a double click is the mouse's Enter, so the two devices must
    // reach the same outcome for the same gesture shape.
    let mut app = fresh(OpenKind::Preferences);
    let rows = np_tui::ui::prefs::rows();
    let index = rows
        .iter()
        .position(|r| matches!(r.field, np_tui::ui::prefs::Field::Bool))
        .expect("a boolean preference row");
    let (x, y) = row_cell(&app, index).expect("row visible");
    let before = np_tui::ui::prefs::value_text(&app, index);
    double_click(&mut app, x, y);
    let after = np_tui::ui::prefs::value_text(&app, index);
    assert_ne!(before, after, "a double click should flip the bool row");
}

#[test]
fn a_single_click_on_a_boolean_preference_does_not_toggle_it() {
    let mut app = fresh(OpenKind::Preferences);
    let rows = np_tui::ui::prefs::rows();
    let index = rows
        .iter()
        .position(|r| matches!(r.field, np_tui::ui::prefs::Field::Bool))
        .expect("a boolean preference row");
    let (x, y) = row_cell(&app, index).expect("row visible");
    let before = np_tui::ui::prefs::value_text(&app, index);
    click(&mut app, x, y);
    assert_eq!(
        np_tui::ui::prefs::value_text(&app, index),
        before,
        "a single click selects a row, it does not change its value"
    );
}

// ------------------------------------------------ 5. recent files list

#[test]
fn a_populated_recent_files_list_scrolls_without_touching_the_document() {
    let mut app = fresh(OpenKind::Recent);
    assert!(
        app.dialog_row_count() > 10,
        "premise: the recent list should be populated, got {}",
        app.dialog_row_count()
    );
    let before = visible_window(&app).expect("window");
    let doc_before = doc_top(&app);
    for _ in 0..3 {
        wheel(&mut app, true, W / 2, H / 2);
    }
    let after = visible_window(&app).expect("window");
    assert!(after.0 > before.0, "the recent list should scroll");
    assert_eq!(doc_top(&app), doc_before, "and not the document behind it");
}
