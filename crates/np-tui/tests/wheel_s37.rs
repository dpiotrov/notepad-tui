//! The mouse wheel over a dialog list (S37).
//!
//! A dialog list used to have no scroll offset of its own: the first visible row was derived from
//! the row cursor, so the only way to scroll was to move the selection. To stop a scroll silently
//! re-selecting a row, the wheel parked the cursor on the *last* row of the window it wanted and
//! let a separate field re-state the real choice. That produced the two reports this file answers:
//!
//! 1. **The highlight jumped to the bottom of the box.** The renderer draws the selection bar at
//!    the cursor, so a cursor parked at the window's last row paints the highlight at the bottom,
//!    whichever way the wheel turned.
//! 2. **The wheel scrolled once and died.** The derivation answers 0 for *every* cursor below the
//!    window height, so it cannot be inverted; the next notch computed a window the cursor already
//!    produced. A Theme list with 14 rows in a 14-row window moved on the first notch and then not
//!    at all, in either direction.
//!
//! The fix gives a dialog list its own `dialog_scroll`. These tests assert on the *rendered* frame
//! rather than on state, because every earlier test here passed while the highlight was visibly in
//! the wrong place.

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

fn wheel(app: &mut App, up: bool, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: 50,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn click(app: &mut App, y: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_mouse(&MouseEvent {
            kind,
            column: 50,
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

/// Screen rows of the open dialog's list rows, in order.
fn list_rows(app: &App) -> Vec<u16> {
    app.dialog_focus_items()
        .iter()
        .filter(|it| format!("{:?}", it.id).starts_with("ListRow"))
        .map(|it| it.area.y)
        .collect()
}

/// Which screen row carries the selection bar, and the text drawn on it.
///
/// Read off the frame the terminal would show. State assertions let the parked-cursor bug through
/// because the state was self-consistent; only the picture says where the user sees the highlight.
fn drawn_selection(app: &mut App) -> (u16, String) {
    let palette = app.palette();
    let (r, g, b) = palette.highlight_bg;
    let bar = ratatui::style::Color::Rgb(r, g, b);
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    for y in list_rows(app) {
        let wide = (0..W)
            .filter(|x| buf[(*x, y)].style().bg == Some(bar))
            .count()
            > 10;
        if wide {
            let text: String = (0..W).map(|x| buf[(x, y)].symbol()).collect();
            return (y, text.trim().trim_matches('│').trim().to_string());
        }
    }
    panic!("no selection bar drawn inside the list rows");
}

/// A directory with enough entries that the file picker's list scrolls.
fn scrollable_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for n in 0..26 {
        fs::write(dir.path().join(format!("file{n:02}.txt")), "x\n").expect("write");
    }
    fs::write(dir.path().join("seed.txt"), "x\n").expect("write");
    dir
}

fn picker_in(dir: &std::path::Path) -> App {
    let mut app = app();
    app.open_path(&dir.join("seed.txt"));
    app.open_file_picker(FilePurpose::Open);
    app
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

// ------------------------------------------------------- the highlight stays put

#[test]
fn the_wheel_does_not_move_the_highlight_to_the_bottom_of_the_box() {
    // The reported gesture. With no prior choice the highlight keeps the same *screen* row as the
    // window slides -- that is what a wheel over a list looks like everywhere else.
    let mut app = language_picker();
    let rows = list_rows(&app);
    let (y0, _) = drawn_selection(&mut app);
    assert!(
        y0 > rows[0] && y0 < *rows.last().expect("list rows"),
        "the picker opens with its highlight somewhere in the middle, so a jump is visible"
    );
    for _ in 0..4 {
        wheel(&mut app, false, rows[0] + 1);
        let (y, _) = drawn_selection(&mut app);
        assert_eq!(
            y, y0,
            "the highlight must hold its screen row while the list scrolls"
        );
    }
    for _ in 0..4 {
        wheel(&mut app, true, rows[0] + 1);
        let (y, _) = drawn_selection(&mut app);
        assert_eq!(y, y0, "and in the other direction too");
    }
}

#[test]
fn one_wheel_notch_scrolls_the_same_amount_in_both_directions() {
    let mut app = language_picker();
    let rows = list_rows(&app);
    let start = app.dialog_scroll;
    let mut downs = Vec::new();
    for _ in 0..4 {
        wheel(&mut app, false, rows[0] + 1);
        downs.push(app.dialog_scroll);
    }
    let mut ups = Vec::new();
    for _ in 0..4 {
        wheel(&mut app, true, rows[0] + 1);
        ups.push(app.dialog_scroll);
    }
    assert_eq!(
        app.dialog_scroll, start,
        "four down and four up must land where we started"
    );
    // Per-notch distance, not absolute position: the up sequence runs from a different place.
    let mut down_steps = vec![start as i64];
    down_steps.extend(downs.iter().map(|v| *v as i64));
    let mut up_steps = vec![*downs.last().expect("the bottom of the down run") as i64];
    up_steps.extend(ups.iter().map(|v| *v as i64));
    let fwd: Vec<i64> = down_steps.windows(2).map(|w| w[1] - w[0]).collect();
    let back: Vec<i64> = up_steps.windows(2).map(|w| w[0] - w[1]).collect();
    assert!(
        fwd.iter().all(|s| *s > 0) && back.iter().all(|s| *s > 0),
        "every notch must move the window: {downs:?} then {ups:?}"
    );
    assert_eq!(fwd, back, "one notch is one distance, both ways");
}

#[test]
fn the_wheel_keeps_scrolling_after_the_first_notch() {
    // The dead zone. The old derivation answered 0 for every cursor below the window height, so a
    // list whose rows nearly all fitted moved once and then never again.
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    let mut seen = vec![app.dialog_scroll];
    for _ in 0..6 {
        wheel(&mut app, false, rows[0] + 1);
        let now = app.dialog_scroll;
        assert_ne!(
            now,
            *seen.last().expect("a previous window"),
            "the wheel must keep moving the window, not stall after one notch (saw {seen:?})"
        );
        seen.push(now);
    }
}

#[test]
fn the_wheel_reaches_the_last_row_and_stops_there() {
    // A control that cannot reach its own end does not do what it is pointed at.
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    let count = app.dialog_row_count();
    let visible = rows.len();
    for _ in 0..30 {
        wheel(&mut app, false, rows[0] + 1);
    }
    assert_eq!(
        app.dialog_scroll,
        count - visible,
        "the window must reach the last row and clamp there"
    );
    // What must be true is that the last entry is *on screen*. The highlight sits wherever the
    // cursor was carried to, and asserting on it here would restate a different promise.
    let last = app
        .file_entries
        .last()
        .map(|e| e.name.clone())
        .expect("entries");
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let screen: String = list_rows(&app)
        .iter()
        .map(|&y| (0..W).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect();
    assert!(
        screen.contains(&last),
        "the last entry {last:?} should be visible at the end of the list: {screen}"
    );
    // And back to the top.
    for _ in 0..30 {
        wheel(&mut app, true, rows[0] + 1);
    }
    assert_eq!(
        app.dialog_scroll, 0,
        "scrolling up must return to the first row"
    );
}

// ------------------------------------------------------- a chosen row stays chosen

#[test]
fn a_row_chosen_by_click_stays_the_row_that_enter_opens_after_scrolling() {
    // The wheel holds a deliberate choice. It cannot keep the *highlight* on a row that has
    // scrolled off the edge -- there is nowhere to draw it -- but Enter must still open that row,
    // or a scroll down a long list silently changes the user's selection.
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    click(&mut app, rows[3]);
    let chosen = app
        .file_entries
        .get(app.list_cursor())
        .map(|e| e.name.clone())
        .expect("the clicked row names a file");

    for _ in 0..4 {
        wheel(&mut app, false, rows[0] + 1);
    }
    assert!(
        app.dialog_scroll > 3,
        "the chosen row should have scrolled off the top for this to mean anything"
    );
    let accept = app
        .picker_accept_row()
        .and_then(|r| app.file_entries.get(r))
        .map(|e| e.name.clone());
    assert_eq!(
        accept.as_deref(),
        Some(chosen.as_str()),
        "Enter must still open the row that was clicked, not whichever row the cursor was parked on"
    );

    press(&mut app, KeyCode::Enter);
    let opened = app
        .doc()
        .and_then(|d| d.path.clone())
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    assert_eq!(
        opened.as_deref(),
        Some(chosen.as_str()),
        "and it must actually open that file"
    );
    assert_eq!(app.mode(), Mode::Editing, "and close the dialog");
}

#[test]
fn scrolling_back_restores_the_highlighted_choice() {
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    click(&mut app, rows[3]);
    let (y_before, label_before) = drawn_selection(&mut app);
    for _ in 0..2 {
        wheel(&mut app, false, rows[0] + 1);
    }
    for _ in 0..2 {
        wheel(&mut app, true, rows[0] + 1);
    }
    let (y_after, label_after) = drawn_selection(&mut app);
    assert_eq!(
        label_after, label_before,
        "the same row must be highlighted after scrolling away and back"
    );
    assert_eq!(y_after, y_before, "and on the same screen row");
}

// ------------------------------------------------------- one window for both devices

#[test]
fn the_drawn_row_and_the_clickable_row_are_the_same_row() {
    // The renderer and the hit-test now read one offset. A row highlighted at one screen position
    // and hit-tested at another is the class of bug this whole area keeps producing.
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    for _ in 0..3 {
        wheel(&mut app, false, rows[0] + 1);
    }
    let (bar_y, bar_label) = drawn_selection(&mut app);
    let items = app.dialog_focus_items();
    let item = items
        .iter()
        .find(|it| it.area.y == bar_y)
        .expect("the highlighted row is a hit-test row");
    let row_index = match format!("{:?}", item.id).strip_prefix("ListRow(") {
        Some(rest) => rest
            .trim_end_matches(')')
            .parse::<usize>()
            .expect("a row number"),
        None => panic!("the highlighted item is not a list row: {:?}", item.id),
    };
    let name = app
        .file_entries
        .get(row_index)
        .map(|e| e.name.clone())
        .expect("a row in range");
    assert!(
        bar_label.contains(&name),
        "screen row {bar_y} draws {bar_label:?} but clicking it would act on row {row_index} = {name:?}"
    );
}

#[test]
fn a_reopened_dialog_starts_at_the_top_of_its_listing() {
    // The window is state, and stale window state shows a user rows of a directory they never
    // scrolled through.
    let dir = scrollable_dir();
    let mut app = picker_in(dir.path());
    let rows = list_rows(&app);
    for _ in 0..5 {
        wheel(&mut app, false, rows[0] + 1);
    }
    assert!(app.dialog_scroll > 0, "precondition: scrolled");
    press(&mut app, KeyCode::Esc);
    app.open_file_picker(FilePurpose::Open);
    assert_eq!(app.dialog_scroll, 0, "a fresh dialog opens at the top");
}

// ------------------------------------------------------- keyboard still owns the window

#[test]
fn arrow_keys_scroll_the_window_instead_of_walking_off_screen() {
    // The cursor has to stay drawn. An arrow past the bottom used to move it out of the window,
    // where the focus resolution stops honouring it -- the list looked frozen.
    let mut app = language_picker();
    let rows = list_rows(&app);
    let first = rows[0];
    for _ in 0..40 {
        press(&mut app, KeyCode::Down);
        let (y, _) = drawn_selection(&mut app);
        assert!(
            y >= first && y <= *rows.last().expect("list rows"),
            "the highlight must stay inside the listing while arrowing (row {y})"
        );
    }
    assert!(
        app.dialog_scroll > 0,
        "arrowing 40 rows in a 76-row list must have scrolled the window"
    );
}

#[test]
fn preferences_can_reach_its_last_row() {
    // Preferences advertised one more clickable row than it drew, because the frame's inner area
    // includes the hint line at the bottom. The last setting could be selected but never shown.
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("s.txt"), "x\n").expect("write");
    let mut app = app();
    app.open_path(&dir.path().join("s.txt"));
    app.execute(Action::Preferences);
    let count = app.dialog_row_count();
    let last_index = count - 1;
    let mut reached = false;
    for _ in 0..count {
        press(&mut app, KeyCode::Down);
        let items = app.dialog_focus_items();
        let visible: Vec<usize> = items
            .iter()
            .filter_map(|it| {
                format!("{:?}", it.id)
                    .strip_prefix("ListRow(")
                    .and_then(|r| r.trim_end_matches(')').parse::<usize>().ok())
            })
            .collect();
        if visible.contains(&last_index) {
            reached = true;
            break;
        }
    }
    assert!(
        reached,
        "the last preference row must be scrollable into view (window rows never reached {last_index})"
    );
    // And the highlight is on that row: read the row the bar is drawn over back out of the hit
    // test rather than matching label text, which is how the S35/S36 tests name a row.
    let (bar_y, _) = drawn_selection(&mut app);
    let items = app.dialog_focus_items();
    let id = items
        .iter()
        .find(|it| it.area.y == bar_y)
        .map(|it| format!("{:?}", it.id))
        .expect("the highlighted row is a hit-test row");
    assert_eq!(
        id,
        format!("ListRow({last_index})"),
        "the highlight should be on the last preference row"
    );
}

#[test]
fn the_picker_still_applies_the_highlighted_theme_after_a_wheel() {
    // The accept path and the highlight must agree after a scroll, which is the promise the old
    // parked cursor broke.
    let mut app = app();
    app.execute(Action::ThemePicker);
    let rows = list_rows(&app);
    wheel(&mut app, false, rows[0] + 1);
    let (_, label) = drawn_selection(&mut app);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.highlight.theme_name(),
        label,
        "Enter must apply the theme that was highlighted"
    );
}
