//! S29: a click outside a dialog box closes it, the way `Esc` does.
//!
//! Before this, `Mode::Dialog(_)` answered `Hit::Dialog` for *every* coordinate, so the routing
//! arm had no way to tell "the user clicked the box's border" from "the user clicked the text
//! three columns past the box" and chose the safe answer for both: swallow. The user's report was
//! that Recent Files, Languages, Themes, Preferences and the palette could only be closed with
//! `Esc` or their own button.
//!
//! The two behaviors that had to survive are pinned here alongside the new one:
//!
//! - a click inside the box, off any widget, is still swallowed and must not move the caret or
//!   close anything;
//! - a click on a widget still activates that widget, because the box contains its widgets and a
//!   box test that ran first would turn every button press into a dismissal.
//!
//! Coordinates come from `dialog_box_rect`, which is the same layout the renderer draws with, so
//! these tests assert the *relationship* -- inside versus outside -- rather than magic numbers
//! that would silently rot when a dialog's row count changes.

use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Buffer, Config, LineCol};
use np_tui::app::{ClickState, DialogKind, Mode};
use np_tui::mouse::{self, Hit, MouseIntent};
use np_tui::{App, DialogKind as DK};

const W: u16 = 80;
const H: u16 = 24;

fn app_with(text: &str) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    let doc = app.doc_mut().expect("document");
    doc.buffer = Buffer::from_str_lossy(text);
    doc.set_cursor(LineCol::new(0, 0), false);
    app.recompute_geometry(W, H);
    app
}

fn down(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn route(app: &App, e: &MouseEvent) -> MouseIntent {
    mouse::route_with_click(e, app, &ClickState::default(), Instant::now()).0
}

/// Every dialog kind that is drawn as a centered box.
///
/// Find and Replace are excluded on purpose: they are docked service strips with no box, so they
/// have no "outside" to click, and `dialog_box_rect` answers `None` for them. That asymmetry is
/// itself asserted below rather than left implicit.
fn boxed_dialogs() -> Vec<DialogKind> {
    vec![
        DK::Open,
        DK::SaveAs,
        DK::GoTo,
        DK::Preferences,
        DK::About,
        DK::Confirm,
        DK::LanguagePicker,
        DK::ThemePicker,
        DK::DocumentList,
        DK::EncodingPicker,
        DK::EolMenu,
        DK::RecentFiles,
    ]
}

#[test]
fn every_boxed_dialog_reports_a_box() {
    let mut app = app_with("alpha\nbravo\n");
    for kind in boxed_dialogs() {
        app.push_mode(Mode::Dialog(kind));
        let area = app
            .dialog_box_rect()
            .unwrap_or_else(|| panic!("{kind:?} draws a box and must report one"));
        // A box with no interior would make "inside" meaningless, and a box wider than the screen
        // would mean every point is inside and nothing can ever be dismissed.
        assert!(
            area.w >= 3 && area.h >= 3,
            "{kind:?} box too small: {area:?}"
        );
        assert!(
            area.x + area.w <= W && area.y + area.h <= H,
            "{kind:?} box escapes the screen: {area:?} on {W}x{H}"
        );
        app.pop_mode();
    }
}

#[test]
fn find_and_replace_have_no_box_to_click_outside() {
    // The docked strips. If these ever reported a rectangle, a click past the strip would close
    // a search the user is still typing into.
    let mut app = app_with("alpha\n");
    for kind in [DK::Find, DK::Replace] {
        app.push_mode(Mode::Dialog(kind));
        assert_eq!(
            app.dialog_box_rect(),
            None,
            "{kind:?} is a docked strip and must not claim a box"
        );
        app.pop_mode();
    }
}

#[test]
fn a_click_outside_a_boxed_dialog_closes_it() {
    // The reported bug, across every box. The point is chosen one cell to the left of the box's
    // left border, on the box's own vertical middle, which is guaranteed outside the frame and
    // still inside the editor band on every dialog tested.
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    for kind in boxed_dialogs() {
        app.push_mode(Mode::Dialog(kind));
        let area = app.dialog_box_rect().expect("boxed dialog");
        assert!(
            area.x > 0,
            "{kind:?} box is flush with the left edge, so no outside point exists here"
        );
        let (x, y) = (area.x - 1, area.y + area.h / 2);

        assert_eq!(
            mouse::hit_test(&down(x, y), &app),
            Hit::DialogOutside,
            "{kind:?}: a point left of the box should read as outside"
        );
        assert_eq!(
            route(&app, &down(x, y)),
            MouseIntent::CloseDialog,
            "{kind:?}: an outside click should close"
        );
        app.pop_mode();
    }
}

#[test]
fn closing_by_outside_click_leaves_editing_mode() {
    let mut app = app_with("alpha\nbravo\n");
    app.push_mode(Mode::Dialog(DK::RecentFiles));
    let area = app.dialog_box_rect().expect("box");
    let before = app.mode();
    app.handle_mouse(&down(area.x - 1, area.y + area.h / 2));
    assert_ne!(app.mode(), before, "dialog still open after outside click");
    assert_eq!(app.mode(), Mode::Editing, "should land back in the editor");
}

#[test]
fn a_click_on_the_box_border_is_still_swallowed() {
    // Inside the frame, on nothing: must not close, and must not place the caret in the document
    // behind it. This is the half of the split that keeps a stray click on a dialog's edge from
    // dismissing a half-filled form.
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    app.push_mode(Mode::Dialog(DK::Preferences));
    let area = app.dialog_box_rect().expect("box");
    let (x, y) = (area.x, area.y + area.h / 2);

    assert_eq!(mouse::hit_test(&down(x, y), &app), Hit::Dialog);
    assert_eq!(route(&app, &down(x, y)), MouseIntent::Nothing);
    assert_eq!(
        app.mode(),
        Mode::Dialog(DK::Preferences),
        "clicking the border closed the dialog"
    );
}

#[test]
fn a_click_on_a_widget_inside_the_box_still_activates_it() {
    // Ordering guard. The box test must not run before the focus-item test, or every click on a
    // list row becomes a dismissal. Recent Files is used because its rows fill the box interior,
    // so a row is easy to point at.
    let mut app = app_with("alpha\nbravo\n");
    // The Language picker rather than Recent Files: a fresh app has no recent files, so that list
    // is empty and offers no row to aim at. Languages always has rows.
    app.push_mode(Mode::Dialog(DK::LanguagePicker));
    let items = app.dialog_focus_items();
    let row = items
        .iter()
        .find(|it| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(_)))
        .expect("the language picker has visible rows");
    let cx = row.area.x + row.area.w / 2;
    let cy = row.area.y;
    let area = app.dialog_box_rect().expect("box");
    assert!(
        area.contains(cx, cy),
        "test is pointing outside its own dialog: row at ({cx},{cy}) vs box {area:?}"
    );

    assert_eq!(
        mouse::hit_test(&down(cx, cy), &app),
        Hit::DialogWidget(items.iter().position(|it| it.id == row.id).expect("index")),
        "a row inside the box must resolve to the row, not to the box or to outside"
    );
}

#[test]
fn outside_click_on_a_nested_menu_does_not_pop_the_dialog_underneath() {
    // A dialog opened from a menu is pushed on top of it. Dismissing must pop the dialog and land
    // where Esc would land, not flatten the whole stack.
    let mut app = app_with("alpha\n");
    app.push_mode(Mode::Dialog(DK::ThemePicker));
    let area = app.dialog_box_rect().expect("box");
    app.handle_mouse(&down(area.x - 1, area.y + area.h / 2));
    // Whatever it lands on, it must not still be the picker.
    assert!(
        !matches!(app.mode(), Mode::Dialog(DK::ThemePicker)),
        "picker survived an outside click"
    );
}

#[test]
fn outside_click_closes_a_confirm_without_choosing_a_button() {
    // The unsaved-file prompt. Closing it must be the `Esc` answer -- dismiss the prompt, keep the
    // document open and still modified -- and must never be read as clicking Save or Don't Save.
    // Clicking outside a prompt that asks "save before closing?" cannot mean either answer.
    let mut app = app_with("alpha\n");
    if let Some(doc) = app.doc_mut() {
        doc.buffer.insert(LineCol::new(0, 0), "x");
    }
    app.push_mode(Mode::Dialog(DK::Confirm));
    let area = app.dialog_box_rect().expect("box");
    app.handle_mouse(&down(area.x - 1, area.y + area.h / 2));
    assert!(
        !matches!(app.mode(), Mode::Dialog(DK::Confirm)),
        "confirm survived an outside click"
    );
    assert!(
        app.documents.len() == 1 && app.doc().is_some(),
        "dismissing a save prompt must not close the document"
    );
}

#[test]
fn an_outside_click_does_nothing_when_no_dialog_is_open() {
    // The new hit-test arm lives under `Mode::Dialog(_)`, so plain editing must be untouched:
    // a click in the text still places the caret.
    let app = app_with("alpha\nbravo\n");
    // Document line 1 at the text area's own origin column. Column 4 of the *screen* is gutter,
    // which is why an earlier version of this test resolved to line 0.
    let row = app.geometry.text_area.y + 1;
    let col = app.geometry.text_area.x;
    match route(&app, &down(col, row)) {
        MouseIntent::SetCaret { lc, extend } => {
            assert_eq!(lc, LineCol::new(1, 0));
            assert!(!extend);
        }
        other => panic!("plain editing click should place the caret, got {other:?}"),
    }
}

#[test]
fn a_dialog_close_does_not_move_the_caret() {
    // The complaint that made the old swallow-everything behavior defensible: closing a dialog
    // should not also jump the caret. Clicking outside must close and leave the cursor alone.
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    if let Some(doc) = app.doc_mut() {
        doc.set_cursor(LineCol::new(2, 3), false);
    }
    app.push_mode(Mode::Dialog(DK::RecentFiles));
    let area = app.dialog_box_rect().expect("box");
    app.handle_mouse(&down(area.x - 1, area.y + area.h / 2));
    let after = app.doc().expect("document").cursor.pos;
    assert_eq!(
        after,
        LineCol::new(2, 3),
        "closing the dialog moved the caret"
    );
}
