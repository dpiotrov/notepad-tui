//! Mouse support for the Keyboard Help overlay, and wheel travel past the ends of a dialog list
//! (S38).
//!
//! Two gaps in the wheel work of S37:
//!
//! 1. **The cheatsheet ignored the mouse entirely.** The wheel handler already routed an overlay
//!    scroll here, but `scroll_dialog_list` tested for `Mode::Dialog` and fell straight through, so
//!    nothing moved. Hit-testing answered `Hit::Dialog` for every point on screen, so a click past
//!    the box was swallowed too -- Esc was the only way out of a box whose whole purpose is to be
//!    looked at and dismissed.
//! 2. **A dialog list stopped responding at its ends.** The wheel pans the window, and once the
//!    window is against the first or last row there is nothing left to pan, so further notches did
//!    nothing at all. The user asked for the selection to take over the remaining travel: one item
//!    per notch, and the highlight on the first row for the first upward notch and the last row for
//!    the last downward one.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, FilePurpose, Mode};
use std::fs;

const W: u16 = 100;
const H: u16 = 30;

fn app() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn wheel(at: &mut App, up: bool, x: u16, y: u16) {
    at.handle_mouse(&MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn click(at: &mut App, x: u16, y: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        at.handle_mouse(&MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn list_rows(app: &App) -> Vec<u16> {
    app.dialog_focus_items()
        .iter()
        .filter(|it| format!("{:?}", it.id).starts_with("ListRow"))
        .map(|it| it.area.y)
        .collect()
}

/// The screen row carrying the selection bar, read off the frame.
fn drawn_selection(app: &mut App) -> (u16, String) {
    let palette = app.palette();
    let (r, g, b) = palette.highlight_bg;
    let bar = ratatui::style::Color::Rgb(r, g, b);
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    for y in list_rows(app) {
        if (0..W)
            .filter(|x| buf[(*x, y)].style().bg == Some(bar))
            .count()
            > 10
        {
            let text: String = (0..W).map(|x| buf[(x, y)].symbol()).collect();
            return (y, text.trim().trim_matches('│').trim().to_string());
        }
    }
    panic!("no selection bar drawn inside the list rows");
}

/// The text drawn inside the help box, so a scroll can be observed rather than inferred.
fn help_screen(app: &mut App) -> String {
    let area = np_tui::ui::help::layout(app).area;
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn language_picker() -> App {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe.cpp");
    fs::write(&path, "int main() {}\n").expect("write");
    let mut app = app();
    app.open_path(&path);
    app.execute(Action::LanguagePicker);
    app
}

fn scrollable_picker() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().expect("tempdir");
    for n in 0..26 {
        fs::write(dir.path().join(format!("file{n:02}.txt")), "x\n").expect("write");
    }
    fs::write(dir.path().join("seed.txt"), "x\n").expect("write");
    let mut app = app();
    app.open_path(&dir.path().join("seed.txt"));
    app.open_file_picker(FilePurpose::Open);
    (dir, app)
}

// ------------------------------------------------------- the help overlay and the mouse

#[test]
fn the_help_overlay_scrolls_under_the_wheel() {
    let mut app = app();
    app.execute(Action::HelpOverlay);
    assert_eq!(app.mode(), Mode::HelpOverlay);
    let before = help_screen(&mut app);
    assert_eq!(app.help_scroll, 0, "the cheatsheet opens at the top");
    wheel(&mut app, false, 50, 12);
    wheel(&mut app, false, 50, 12);
    assert!(app.help_scroll > 0, "the wheel must scroll the cheatsheet");
    assert_ne!(
        help_screen(&mut app),
        before,
        "and the box must actually show different lines"
    );
}

#[test]
fn the_help_overlay_wheel_returns_to_the_top_and_clamps() {
    let mut app = app();
    app.execute(Action::HelpOverlay);
    for _ in 0..40 {
        wheel(&mut app, false, 50, 12);
    }
    let count = np_tui::ui::help::help_lines(&app).len();
    let visible = np_tui::ui::help::visible_lines(&app);
    assert!(
        count > visible,
        "the cheatsheet must be longer than the box to mean anything"
    );
    assert_eq!(
        app.help_scroll,
        count - visible,
        "the wheel must reach the last line and stop there"
    );
    let bottom = help_screen(&mut app);
    for _ in 0..60 {
        wheel(&mut app, true, 50, 12);
    }
    assert_eq!(app.help_scroll, 0, "and back to the first line");
    assert_ne!(
        help_screen(&mut app),
        bottom,
        "showing the top of the cheatsheet"
    );
}

#[test]
fn a_click_outside_the_help_overlay_closes_it() {
    let mut app = app();
    app.execute(Action::HelpOverlay);
    let area = np_tui::ui::help::layout(&app).area;
    // A point to the left of the drawn box. The box is centred and narrower than the screen, so
    // this is real backdrop rather than a corner of the frame.
    assert!(area.x > 2, "the test needs backdrop to the left of the box");
    click(&mut app, 2, area.y + 2);
    assert_eq!(
        app.mode(),
        Mode::Editing,
        "a click past the edge dismisses a box, whatever box it is"
    );
}

#[test]
fn a_click_inside_the_help_overlay_leaves_it_open() {
    // The other half of the dismissal rule: inside the frame the press belongs to the box and is
    // swallowed, or every click on the cheatsheet would close it.
    let mut app = app();
    app.execute(Action::HelpOverlay);
    let area = np_tui::ui::help::layout(&app).area;
    click(&mut app, area.x + area.w / 2, area.y + area.h / 2);
    assert_eq!(
        app.mode(),
        Mode::HelpOverlay,
        "a click inside the box is not a dismissal"
    );
}

#[test]
fn the_help_overlay_wheel_does_not_scroll_the_document_behind_it() {
    // The gesture belongs to the box the pointer is over. Scrolling the file behind a cheatsheet
    // would make the overlay pointless and the document wrong.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("big.txt");
    fs::write(&path, "line\n".repeat(200)).expect("write");
    let mut app = app();
    app.open_path(&path);
    app.execute(Action::HelpOverlay);
    let before = app.doc().map(|d| d.scroll.top_line).unwrap_or(0);
    for _ in 0..6 {
        wheel(&mut app, false, 50, 12);
    }
    let after = app.doc().map(|d| d.scroll.top_line).unwrap_or(0);
    assert_eq!(
        before, after,
        "the document behind the overlay must not move"
    );
    assert!(app.help_scroll > 0, "the overlay moved instead");
}

// ------------------------------------- wheel travel continues into the selection at the ends

#[test]
fn at_the_bottom_of_a_list_the_selection_steps_one_item_per_notch() {
    let mut app = language_picker();
    let rows = list_rows(&app);
    let count = app.dialog_row_count();
    for _ in 0..40 {
        wheel(&mut app, false, 50, rows[0] + 1);
    }
    assert!(
        count > rows.len(),
        "the list must scroll for this to be about the end"
    );
    // Past the end, the selection keeps travelling and lands on the very last row.
    let mut seen = vec![app.list_cursor()];
    for _ in 0..30 {
        wheel(&mut app, false, 50, rows[0] + 1);
        let now = app.list_cursor();
        if now != *seen.last().expect("a previous row") {
            assert!(
                now > *seen.last().expect("the previous row"),
                "downward travel only ever moves the selection down: {seen:?} then {now}"
            );
            seen.push(now);
        }
    }
    assert_eq!(
        *seen.last().expect("the final row"),
        count - 1,
        "repeated downward notches must reach the last item, not stop short: {seen:?}"
    );
}

#[test]
fn at_the_top_of_a_list_the_selection_steps_one_item_per_notch() {
    let mut app = language_picker();
    let rows = list_rows(&app);
    for _ in 0..40 {
        wheel(&mut app, true, 50, rows[0] + 1);
    }
    assert_eq!(
        app.dialog_scroll, 0,
        "precondition: the window is at the top"
    );
    let mut seen = vec![app.list_cursor()];
    for _ in 0..30 {
        wheel(&mut app, true, 50, rows[0] + 1);
        let now = app.list_cursor();
        if now != *seen.last().expect("a previous row") {
            assert!(
                now < *seen.last().expect("the previous row"),
                "upward travel only ever moves the selection up: {seen:?} then {now}"
            );
            seen.push(now);
        }
    }
    assert_eq!(
        *seen.last().expect("the final row"),
        0,
        "repeated upward notches must reach the first item: {seen:?}"
    );
}

#[test]
fn the_selection_lands_on_the_first_and_last_rows_at_the_ends() {
    // The corners, asserted on the picture: the highlight on the first row of the window at the
    // top of the list, and on the last entry at the bottom of it.
    let (_dir, mut app) = scrollable_picker();
    let rows = list_rows(&app);
    let count = app.dialog_row_count();
    for _ in 0..40 {
        wheel(&mut app, true, 50, rows[0] + 1);
    }
    for _ in 0..40 {
        wheel(&mut app, false, 50, rows[0] + 1);
    }
    assert_eq!(app.list_cursor(), count - 1, "the last entry is selected");
    let (y, label) = drawn_selection(&mut app);
    let last = app.file_entries[count - 1].name.clone();
    assert!(
        label.contains(&last),
        "the bar on screen row {y} must name the last entry {last:?}, got {label:?}"
    );
    assert_eq!(
        y,
        *rows.last().expect("last list row"),
        "at the bottom of the box"
    );
}

#[test]
fn wheel_travel_at_an_end_selects_a_row_that_enter_opens() {
    // The selection the wheel walked to must be the row the accept path acts on, or the gesture
    // moves a highlight that means nothing.
    let (_dir, mut app) = scrollable_picker();
    let rows = list_rows(&app);
    for _ in 0..40 {
        wheel(&mut app, true, 50, rows[0] + 1);
    }
    for _ in 0..40 {
        wheel(&mut app, false, 50, rows[0] + 1);
    }
    let wanted = app.file_entries[app.accept_row()].name.clone();
    press(&mut app, KeyCode::Enter);
    let opened = app
        .doc()
        .and_then(|d| d.path.clone())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    assert_eq!(
        opened.as_deref(),
        Some(wanted.as_str()),
        "Enter must open the row the wheel walked to"
    );
}

#[test]
fn the_wheel_still_pans_the_window_in_the_middle_of_a_list() {
    // The end-of-list rule must not eat the normal gesture.
    let mut app = language_picker();
    let rows = list_rows(&app);
    let (y0, _) = drawn_selection(&mut app);
    let start = app.dialog_scroll;
    wheel(&mut app, false, 50, rows[0] + 1);
    assert!(
        app.dialog_scroll > start,
        "mid-list the wheel pans the view"
    );
    let (y1, _) = drawn_selection(&mut app);
    assert_eq!(
        y1, y0,
        "and the highlight holds its screen row while it does"
    );
}

#[test]
fn a_chosen_row_still_survives_the_end_of_list_travel() {
    // S37's promise, re-checked against the new travel: holding a click choice outranks the
    // selection the wheel walks to at an end.
    let (_dir, mut app) = scrollable_picker();
    let rows = list_rows(&app);
    let items = app.dialog_focus_items();
    let target_y = items
        .iter()
        .find(|it| {
            format!("{:?}", it.id)
                .strip_prefix("ListRow(")
                .and_then(|r| r.trim_end_matches(')').parse::<usize>().ok())
                == Some(app.list_cursor() + 2)
        })
        .map(|it| it.area.y)
        .expect("the row two below the current one is on screen");
    let list_x = np_tui::ui::dialogs::file_picker_layout(&app).list_area().x;
    click(&mut app, list_x + 3, target_y);
    let chosen = app.file_entries[app.list_cursor()].name.clone();
    for _ in 0..40 {
        wheel(&mut app, false, 50, rows[0] + 1);
    }
    let accept = app
        .picker_accept_row()
        .and_then(|r| app.file_entries.get(r))
        .map(|e| e.name.clone());
    assert_eq!(
        accept.as_deref(),
        Some(chosen.as_str()),
        "a row the user clicked stays the row Enter opens, even after the wheel ran the list to its end"
    );
}
