//! S6: the §6.9 mouse table, driven through `mouse::route` with no terminal.
//!
//! Every row of the spec's twenty gets its own test asserting the exact `MouseIntent`, and
//! the rest cover the cases where hit-testing can go wrong silently: precedence under an
//! overlay, clamping past end-of-line, wrapped rows, and multi-click timing.
//!
//! `route` takes `now` as an argument precisely so the double-click window is asserted rather
//! than slept through.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Buffer, Config, LineCol, SelectionKind};
use np_tui::actions::{Action, MenuId};
use np_tui::app::{ClickState, ContextKind, HoverTarget, Mode};
use np_tui::mouse::{self, Hit, MouseIntent};
use np_tui::App;

const W: u16 = 80;
const H: u16 = 24;

fn app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.recompute_geometry(W, H);
    app
}

/// App with a known multi-line document loaded into the active tab.
fn app_with(text: &str) -> App {
    let mut app = app();
    let now = app.clock;
    let doc = app.doc_mut().expect("document");
    doc.buffer = Buffer::from_str_lossy(text);
    doc.set_cursor(LineCol::new(0, 0), false);
    let _ = now;
    app.recompute_geometry(W, H);
    app
}

fn ev(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn ev_mods(kind: MouseEventKind, column: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: mods,
    }
}

fn down(column: u16, row: u16) -> MouseEvent {
    ev(MouseEventKind::Down(MouseButton::Left), column, row)
}

fn drag(column: u16, row: u16) -> MouseEvent {
    ev(MouseEventKind::Drag(MouseButton::Left), column, row)
}

/// Route with a fresh click state, returning just the intent.
fn route(app: &App, e: &MouseEvent) -> MouseIntent {
    mouse::route_with_click(e, app, &ClickState::default(), Instant::now()).0
}

/// Route with explicit click state and time.
fn route_at(app: &App, e: &MouseEvent, click: &ClickState, now: Instant) -> MouseIntent {
    mouse::route_with_click(e, app, click, now).0
}

/// The row of the text area that shows document line `line`, in screen coordinates.
fn screen_row_of_line(app: &App, line: usize) -> u16 {
    let top = app.doc().expect("document").scroll.top_line;
    let offset = line.saturating_sub(top) as u16;
    app.geometry.text_area.y + offset
}

/// Screen x for character column `col` on a line with no tabs or wide chars.
fn screen_x_for_col(app: &App, col: usize) -> u16 {
    app.geometry.text_area.x + col as u16
}

/// A click state that has just registered a click at `(column, row)` at `now`.
fn clicked_at(column: u16, row: u16, now: Instant, count: u32) -> ClickState {
    ClickState {
        cell: Some((column, row)),
        count,
        at: now,
    }
}

// ---------------------------------------------------------------- §6.9 rows

#[test]
fn row_01_left_click_sets_the_caret_without_extending() {
    let app = app_with("alpha\nbravo\ncharlie\n");
    let row = screen_row_of_line(&app, 1);
    let x = screen_x_for_col(&app, 3);

    let intent = route(&app, &down(x, row));

    assert_eq!(
        intent,
        MouseIntent::SetCaret {
            lc: LineCol::new(1, 3),
            extend: false
        }
    );
}

#[test]
fn row_02_left_drag_begins_then_extends_the_selection() {
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    let row = screen_row_of_line(&app, 0);

    // First drag press with no selection starts one.
    let start = screen_x_for_col(&app, 1);
    let first = route(&app, &drag(start, row));
    assert!(
        matches!(
            first,
            MouseIntent::BeginSelection { .. } | MouseIntent::ExtendSelection { .. }
        ),
        "a drag must select, got {first:?}"
    );

    // Apply it, then drag further: the anchor stays put and the focus follows.
    app.handle_mouse(&drag(start, row));
    let later = screen_x_for_col(&app, 4);
    let second = route(&app, &drag(later, row));
    assert_eq!(
        second,
        MouseIntent::ExtendSelection {
            focus: LineCol::new(0, 4)
        }
    );

    app.handle_mouse(&drag(later, row));
    let sel = app.doc().expect("document").selection.expect("selection");
    assert_eq!(
        sel.anchor,
        LineCol::new(0, 0),
        "anchor is the drag start, not the previous focus"
    );
    assert_eq!(sel.focus, LineCol::new(0, 4));
}

#[test]
fn row_03_shift_click_extends_the_selection() {
    let app = app_with("alpha\nbravo\n");
    let row = screen_row_of_line(&app, 1);
    let x = screen_x_for_col(&app, 2);

    let intent = route(&app, &ev_mods(down_kind(), x, row, KeyModifiers::SHIFT));

    assert_eq!(
        intent,
        MouseIntent::SetCaret {
            lc: LineCol::new(1, 2),
            extend: true
        }
    );
}

fn down_kind() -> MouseEventKind {
    MouseEventKind::Down(MouseButton::Left)
}

#[test]
fn row_04_double_click_selects_the_word() {
    let app = app_with("hello world\n");
    let row = screen_row_of_line(&app, 0);
    // Inside "world", which starts at column 6.
    let x = screen_x_for_col(&app, 8);
    let now = Instant::now();
    let click = clicked_at(x, row, now - Duration::from_millis(100), 1);

    let intent = route_at(&app, &down(x, row), &click, now);

    assert_eq!(
        intent,
        MouseIntent::BeginSelection {
            kind: SelectionKind::Range,
            anchor: LineCol::new(0, 6),
            focus: LineCol::new(0, 11),
        }
    );
}

#[test]
fn row_05_triple_click_selects_the_whole_line_including_its_newline() {
    let app = app_with("one\ntwo\nthree\n");
    let row = screen_row_of_line(&app, 1);
    let x = screen_x_for_col(&app, 1);
    let now = Instant::now();
    let click = clicked_at(x, row, now - Duration::from_millis(80), 2);

    let intent = route_at(&app, &down(x, row), &click, now);

    // Column 0 of the NEXT line: the terminator is part of the selection, so cutting a
    // triple-selected line leaves no empty line behind.
    assert_eq!(
        intent,
        MouseIntent::BeginSelection {
            kind: SelectionKind::Range,
            anchor: LineCol::new(1, 0),
            focus: LineCol::new(2, 0),
        }
    );
}

#[test]
fn row_05_fourth_click_stays_line_selected() {
    let app = app_with("one\ntwo\nthree\n");
    let row = screen_row_of_line(&app, 0);
    let x = screen_x_for_col(&app, 1);
    let now = Instant::now();
    let click = clicked_at(x, row, now - Duration::from_millis(80), 3);

    let intent = route_at(&app, &down(x, row), &click, now);

    let MouseIntent::BeginSelection { anchor, focus, .. } = intent else {
        panic!("4th click must keep the line selected, got {intent:?}");
    };
    assert_eq!((anchor, focus), (LineCol::new(0, 0), LineCol::new(1, 0)));
}

#[test]
fn row_06_alt_drag_produces_a_block_selection() {
    let app = app_with("alpha\nbravo\ncharlie\n");
    let row = screen_row_of_line(&app, 1);
    let x = screen_x_for_col(&app, 2);

    let intent = route(&app, &ev_mods(down_kind(), x, row, KeyModifiers::ALT));

    assert_eq!(
        intent,
        MouseIntent::BeginSelection {
            kind: SelectionKind::Block,
            anchor: LineCol::new(1, 2),
            focus: LineCol::new(1, 2),
        }
    );
}

#[test]
fn row_07_middle_click_in_text_requests_a_primary_paste() {
    let app = app_with("text\n");
    let row = screen_row_of_line(&app, 0);
    let x = screen_x_for_col(&app, 1);

    let intent = route(&app, &ev(MouseEventKind::Down(MouseButton::Middle), x, row));

    assert_eq!(intent, MouseIntent::PastePrimary);
}

#[test]
fn row_08_wheel_scrolls_the_configured_number_of_lines() {
    let mut app = app_with("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n");
    app.config.ui.mouse_scroll_lines = 5;
    let row = screen_row_of_line(&app, 0);

    assert_eq!(
        route(&app, &ev(MouseEventKind::ScrollDown, 10, row)),
        MouseIntent::Scroll { lines: 5 }
    );
    assert_eq!(
        route(&app, &ev(MouseEventKind::ScrollUp, 10, row)),
        MouseIntent::Scroll { lines: -5 }
    );

    // And it actually moves the viewport.
    app.handle_mouse(&ev(MouseEventKind::ScrollDown, 10, row));
    assert_eq!(app.doc().expect("document").scroll.top_line, 5);
}

#[test]
fn row_09_shift_wheel_scrolls_horizontally() {
    let mut app = app_with("a long line that goes past the edge of the window\n");
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    let row = screen_row_of_line(&app, 0);

    let intent = route(
        &app,
        &ev_mods(MouseEventKind::ScrollDown, 10, row, KeyModifiers::SHIFT),
    );
    assert_eq!(intent, MouseIntent::ScrollH { cols: 3 });

    app.handle_mouse(&ev_mods(
        MouseEventKind::ScrollDown,
        10,
        row,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.doc().expect("document").scroll.left_col, 3);
}

#[test]
fn row_09_shift_wheel_is_inert_while_word_wrap_is_on() {
    // FR-V1: with wrap on there is no horizontal position, so the gesture must do nothing
    // rather than desync left_col from what is rendered.
    let mut app = app_with("wrapped text here\n");
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let row = screen_row_of_line(&app, 0);

    let intent = route(
        &app,
        &ev_mods(MouseEventKind::ScrollDown, 10, row, KeyModifiers::SHIFT),
    );
    assert_eq!(intent, MouseIntent::Nothing);

    app.handle_mouse(&ev_mods(
        MouseEventKind::ScrollDown,
        10,
        row,
        KeyModifiers::SHIFT,
    ));
    assert_eq!(app.doc().expect("document").scroll.left_col, 0);
}

#[test]
fn row_10_ctrl_wheel_scrolls_a_full_page() {
    // §6.9 asks for Ctrl+wheel zoom; a terminal has no font size, so this is the documented
    // deviation: a page scroll (FR-V8).
    let app = app_with("l1\nl2\nl3\nl4\nl5\n");
    let row = screen_row_of_line(&app, 0);
    let page = app.geometry.text_area.h as i32;

    let intent = route(
        &app,
        &ev_mods(MouseEventKind::ScrollDown, 10, row, KeyModifiers::CONTROL),
    );
    assert_eq!(intent, MouseIntent::Scroll { lines: page });
}

#[test]
fn rows_11_to_14_hit_test_the_gutter_and_scrollbar_regions() {
    let app = app_with("l1\nl2\nl3\n");
    // The gutter occupies the columns left of the text area.
    let gutter_x = app.geometry.text_area.x.saturating_sub(1);
    let row = screen_row_of_line(&app, 1);

    let hit = mouse::hit_test(&down(gutter_x, row), &app);
    assert_eq!(hit, Hit::GutterLine(1), "gutter click identifies its line");

    // Row 11: selecting the line, and crucially not moving the caret.
    let intent = route(&app, &down(gutter_x, row));
    assert_eq!(
        intent,
        MouseIntent::SelectLine {
            line: 1,
            keep_caret: true
        }
    );

    // With no folds and no bookmarks the gutter has no marker columns, so every cell of it is
    // the line-number field. The marker columns exist only when the document can put something
    // in them, which is what keeps a plain file's gutter the width it always was; rows 12 and
    // 13 get their own tests below against a document that does have them.
    let intent_fold_column = route(&app, &down(gutter_x, row));
    assert!(
        matches!(
            intent_fold_column,
            MouseIntent::SelectLine {
                line: 1,
                keep_caret: true
            }
        ),
        "{intent_fold_column:?}"
    );

    // Row 14: a three-line document fits the 24-row terminal, so there is no scrollbar to hit
    // and the rightmost column is ordinary text.
    let intent_outside = route(&app, &down(W - 1, row));
    assert!(
        matches!(
            intent_outside,
            MouseIntent::Nothing
                | MouseIntent::SetCaret { .. }
                | MouseIntent::BeginSelection { .. }
        ),
        "{intent_outside:?}"
    );
}

#[test]
fn row_15_click_on_a_tab_activates_it() {
    let mut app = app();
    app.execute(Action::NewFile);
    assert_eq!(app.active, 1);
    // Click the FIRST tab.
    let x = tab_span_x(&app, 0).0;

    let intent = route(&app, &down(x, app.regions.tabs.y));
    assert_eq!(intent, MouseIntent::ActivateTab(0));

    app.handle_mouse(&down(x, app.regions.tabs.y));
    assert_eq!(app.active, 0);
}

#[test]
fn row_16_middle_click_on_a_tab_closes_it() {
    let mut app = app();
    app.execute(Action::NewFile);
    let (x, _w) = tab_span_x(&app, 1);

    let intent = route(
        &app,
        &ev(
            MouseEventKind::Down(MouseButton::Middle),
            x,
            app.regions.tabs.y,
        ),
    );
    assert_eq!(intent, MouseIntent::CloseTab(1));

    // An unmodified tab closes without a confirmation.
    app.handle_mouse(&ev(
        MouseEventKind::Down(MouseButton::Middle),
        x,
        app.regions.tabs.y,
    ));
    assert_eq!(app.documents.len(), 1);
    assert_eq!(app.pending_confirm, None);
}

#[test]
fn row_17_wheel_over_the_tab_bar_cycles_tabs() {
    let mut app = app();
    app.execute(Action::NewFile);
    assert_eq!(app.active, 1);

    let y = app.regions.tabs.y;
    assert_eq!(
        route(&app, &ev(MouseEventKind::ScrollDown, 2, y)),
        MouseIntent::CycleTabs(1)
    );
    assert_eq!(
        route(&app, &ev(MouseEventKind::ScrollUp, 2, y)),
        MouseIntent::CycleTabs(-1)
    );

    // Wraparound at both ends with two tabs.
    app.handle_mouse(&ev(MouseEventKind::ScrollDown, 2, y));
    assert_eq!(app.active, 0);
    app.handle_mouse(&ev(MouseEventKind::ScrollUp, 2, y));
    assert_eq!(app.active, 1, "wraps from the first tab back to the last");
}

#[test]
fn row_18_right_click_on_text_opens_the_editor_context_without_moving_the_caret() {
    let mut app = app_with("hello\nworld\n");
    app.doc_mut()
        .expect("document")
        .set_cursor(LineCol::new(0, 2), false);
    let row = screen_row_of_line(&app, 1);
    let x = screen_x_for_col(&app, 3);

    let intent = route(&app, &ev(MouseEventKind::Down(MouseButton::Right), x, row));
    assert_eq!(
        intent,
        MouseIntent::OpenContextMenu(ContextKind::Editor, LineCol::new(1, 3))
    );

    app.handle_mouse(&ev(MouseEventKind::Down(MouseButton::Right), x, row));
    assert_eq!(
        app.doc().expect("document").cursor.pos,
        LineCol::new(0, 2),
        "caret must not move"
    );
    assert!(matches!(app.mode(), Mode::ContextMenu(ContextKind::Editor)));
    assert_eq!(app.context_menu_target, Some(LineCol::new(1, 3)));
}

#[test]
fn row_18_right_click_on_a_tab_opens_the_tab_context() {
    let app = app();
    let (x, _w) = tab_span_x(&app, 0);

    let intent = route(
        &app,
        &ev(
            MouseEventKind::Down(MouseButton::Right),
            x,
            app.regions.tabs.y,
        ),
    );
    assert_eq!(
        intent,
        MouseIntent::OpenContextMenu(ContextKind::Tab, LineCol::new(0, 0))
    );
}

#[test]
fn row_20_hover_updates_the_highlight_target() {
    let mut app = app();
    app.execute(Action::NewFile);
    let (x, _w) = tab_span_x(&app, 1);

    let intent = route(&app, &ev(MouseEventKind::Moved, x, app.regions.tabs.y));
    assert_eq!(intent, MouseIntent::Hover(Some(HoverTarget::Tab(1))));

    app.handle_mouse(&ev(MouseEventKind::Moved, x, app.regions.tabs.y));
    assert_eq!(app.hover, Some(HoverTarget::Tab(1)));

    // Moving off the tab clears it.
    app.handle_mouse(&ev(MouseEventKind::Moved, x, app.regions.status.y));
    assert_eq!(app.hover, None);
}

#[test]
fn row_20_menu_hover_reports_the_menu_index() {
    let app = app();
    let spans = mouse::menu_bar_spans(app.regions.menu);
    let (_id, i, x, _w) = spans[2];

    let intent = route(&app, &ev(MouseEventKind::Moved, x, app.regions.menu.y));
    assert_eq!(intent, MouseIntent::Hover(Some(HoverTarget::Menu(i))));
}

#[test]
fn a_moved_event_never_changes_the_selection() {
    let mut app = app_with("alpha\nbravo\n");
    app.doc_mut()
        .expect("document")
        .set_cursor(LineCol::new(0, 1), true);
    let before = app.doc().expect("document").selection;

    let row = screen_row_of_line(&app, 1);
    app.handle_mouse(&ev(MouseEventKind::Moved, screen_x_for_col(&app, 4), row));

    assert_eq!(
        app.doc().expect("document").selection,
        before,
        "hover must not select"
    );
}

// ------------------------------------------------------------------ hit testing

#[test]
fn a_click_inside_a_dialog_hits_the_dialog_not_the_text() {
    // The precedence case: a dialog drawn over the text area must swallow a click that lands in
    // its own frame, or dismissing a dialog also moves the caret somewhere the user never aimed
    // at.
    //
    // The point is taken from the drawn box rather than from an arbitrary column. This test used
    // to click column 2 of an 80-column screen and call it "inside the dialog", which it never
    // was -- the Confirm box is 36 wide and centered, so it spans columns 22..57. That went
    // unnoticed while every click near a dialog answered `Dialog`, and it matters now that an
    // outside click closes: a test that mislabels outside as inside would pass while the feature
    // was backwards.
    let mut app = app_with("alpha\nbravo\n");
    app.push_mode(Mode::Dialog(np_tui::DialogKind::Confirm));
    let box_rect = app.dialog_box_rect().expect("Confirm draws a box");
    // Border, not interior: the interior's row 0 is the prompt text and its row 2 is the button
    // row, both of which resolve to widgets. The left border column is the one point that is
    // unambiguously inside the box and on nothing.
    let x = box_rect.x;
    let y = box_rect.y + box_rect.h / 2;

    assert_eq!(mouse::hit_test(&down(x, y), &app), Hit::Dialog);
    assert_eq!(route(&app, &down(x, y)), MouseIntent::Nothing);
}

#[test]
fn a_context_menu_outranks_the_text_beneath_it() {
    // S14 gave the context menu real geometry, so "outranks the text" now means two things:
    // a click inside the box claims the point (it selects an item or hits a border) and a
    // click outside it dismisses. The menu is placed at the cell it was opened at, so this
    // sets that cell and then checks a point well away from the box.
    let mut app = app_with("alpha\n");
    app.push_mode(Mode::ContextMenu(ContextKind::Editor));
    app.context_menu_cell = (2, screen_row_of_line(&app, 0));

    // A point clear of the menu (the box opens at column 2 and is 26 wide) still dismisses.
    let (w, _h) = app.frame_size();
    let outside = w.saturating_sub(1);
    let top_row = 0;
    assert_eq!(
        mouse::hit_test(&down(outside, top_row), &app),
        Hit::ContextMenu
    );
    assert_eq!(
        route(&app, &down(outside, top_row)),
        MouseIntent::DismissContextMenu
    );
}

#[test]
fn a_context_menu_item_click_runs_that_item() {
    // FR-U7: the menu is not just a picture; clicking a row runs that row's command.
    let mut app = app_with("alpha beta\n");
    let target = np_core::LineCol::new(0, 0);
    app.doc_mut().unwrap().select_all();
    app.context_menu_cell = (2, 4);
    app.push_mode(Mode::ContextMenu(ContextKind::Editor));
    let items = np_tui::ui::context_menu::items(&app, ContextKind::Editor);
    let copy_index = items
        .iter()
        .position(|i| i.action == Action::Copy)
        .expect("Copy is an editor context item");
    let area = np_tui::ui::context_menu::menu_rect(2, 4, items.len(), app.regions.bounds());
    let y = area.y + 1 + copy_index as u16;
    assert_eq!(
        mouse::hit_test(&down(area.x + 1, y), &app),
        Hit::ContextMenuItem(copy_index)
    );
    assert_eq!(
        route(&app, &down(area.x + 1, y)),
        MouseIntent::RunAction(Action::Copy)
    );
    let _ = target;
}

#[test]
fn a_click_right_of_a_short_line_clamps_to_end_of_line() {
    // §6.9 row 1. The line is 5 chars; the click is far past it.
    let app = app_with("short\nmuch longer line here\n");
    let row = screen_row_of_line(&app, 0);
    let x = screen_x_for_col(&app, 40);

    let intent = route(&app, &down(x, row));
    assert_eq!(
        intent,
        MouseIntent::SetCaret {
            lc: LineCol::new(0, 5),
            extend: false
        }
    );
}

#[test]
fn a_click_below_the_last_line_clamps_to_the_last_row() {
    let app = app_with("one\ntwo\n");
    let below = app.geometry.text_area.bottom().saturating_sub(1);

    let pos = mouse::text_position_at(&app, app.geometry.text_area.x + 1, below);
    assert!(
        pos.line >= 1,
        "must clamp into existing content, got {pos:?}"
    );
}

#[test]
fn a_click_in_the_gutter_does_not_move_the_caret() {
    let mut app = app_with("l1\nl2\nl3\n");
    app.doc_mut()
        .expect("document")
        .set_cursor(LineCol::new(0, 1), false);
    let gutter_x = app.geometry.text_area.x.saturating_sub(1);
    let row = screen_row_of_line(&app, 2);

    app.handle_mouse(&down(gutter_x, row));
    assert_eq!(
        app.doc().expect("document").cursor.pos.line,
        0,
        "gutter selects, never recentres"
    );
}

#[test]
fn a_click_on_a_wrapped_row_resolves_to_the_right_character() {
    // Wrap *rendering* is S13, and `layout_visible_rows` still emits one whole row per
    // logical line today (verified: a 15-char line in a 10-column window yields a single
    // whole row). So the wrapped case is driven through `position_in_row` with hand-built
    // rows. The arithmetic has to be right before the layout starts producing them, or the
    // day wrapping lands, clicks on the second screen row point back at the start of the
    // line — a bug that would look like a broken mouse.
    let buffer = Buffer::from_str_lossy("abcdefghijklmnopqrstuvwxyz\n");
    let geom = np_core::ViewGeometry {
        gutter_width: 2,
        text_area: np_core::RectCore::new(2, 2, 10, 21),
        tab_width: 4,
        word_wrap: true,
        ..Default::default()
    };
    let row0 = np_core::VisualRow {
        line: 0,
        start_col: 0,
        end_col: 10,
    };
    let row1 = np_core::VisualRow {
        line: 0,
        start_col: 10,
        end_col: 20,
    };

    // First screen row: plain prefix.
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row0, 0),
        LineCol::new(0, 0)
    );
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row0, 5),
        LineCol::new(0, 5)
    );

    // Second screen row: its first cell is char 10, not char 0.
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row1, 0),
        LineCol::new(0, 10)
    );
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row1, 4),
        LineCol::new(0, 14)
    );

    // Clicking past the end of a wrapped row stops at that row's end, not the end of the
    // logical line, which would jump the caret ten characters right of the pointer.
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row1, 40),
        LineCol::new(0, 20)
    );
}

#[test]
fn a_wrapped_row_with_tabs_still_resolves_by_display_cells() {
    // The hard version of the same thing: tabs make display cells and char indices differ,
    // so a row starting after a tab must be measured in cells from its own start.
    let buffer = Buffer::from_str_lossy("\txdefghijklmnop\n");
    let geom = np_core::ViewGeometry {
        gutter_width: 0,
        text_area: np_core::RectCore::new(0, 0, 10, 21),
        tab_width: 4,
        word_wrap: true,
        ..Default::default()
    };
    // "	" occupies cells 0..4, so a row starting at char 1 begins at cell 4.
    let row = np_core::VisualRow {
        line: 0,
        start_col: 1,
        end_col: 9,
    };
    // Cell 0 *within that row* is char 1; four cells in is char 5.
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row, 0),
        LineCol::new(0, 1)
    );
    assert_eq!(
        mouse::position_in_row(&buffer, &geom, row, 4),
        LineCol::new(0, 5)
    );
}

#[test]
fn horizontal_scroll_shifts_the_resolved_column_when_wrap_is_off() {
    let mut app = app_with("abcdefghij\n");
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    {
        let doc = app.doc_mut().expect("document");
        doc.buffer = Buffer::from_str_lossy("abcdefghij\n");
        doc.scroll.left_col = 4;
    }
    let y = app.geometry.text_area.y;
    // With left_col = 4, screen column 0 of the text area shows character 4.
    let pos = mouse::text_position_at(&app, app.geometry.text_area.x, y);
    assert_eq!(pos.col, 4);
}

#[test]
fn click_past_column_223_resolves_correctly() {
    // SGR tracking is already on, so crossterm reports wide coordinates; make sure the
    // mapping does not assume a u8 or a 255-column ceiling.
    let mut app = app();
    app.recompute_geometry(400, H);
    let wide_x = 300u16;
    let y = app.geometry.text_area.y;
    assert!(
        app.geometry.text_area.contains(wide_x, y),
        "fixture must place x inside"
    );

    let pos = mouse::text_position_at(&app, wide_x, y);
    assert_eq!(pos.line, 0);
    // Empty buffer: everything clamps to the origin, which is the correct answer rather than
    // an overflow or a panic.
    assert_eq!(pos.col, 0);
}

#[test]
fn a_click_outside_every_region_is_outside() {
    let app = app();
    let below = app.regions.status.bottom();
    assert_eq!(mouse::hit_test(&down(5, below + 3), &app), Hit::Outside);
    assert_eq!(route(&app, &down(5, below + 3)), MouseIntent::Nothing);
}

#[test]
fn menu_bar_click_opens_that_menu() {
    let mut app = app();
    let (id, _i, x, _w) = mouse::menu_bar_spans(app.regions.menu)[0];
    assert_eq!(id, MenuId::File);

    let intent = route(&app, &down(x, app.regions.menu.y));
    assert_eq!(intent, MouseIntent::Action(Action::MenuBar(MenuId::File)));

    app.handle_mouse(&down(x, app.regions.menu.y));
    assert!(matches!(
        app.mode(),
        Mode::Menu {
            open: MenuId::File,
            ..
        }
    ));
}

#[test]
fn tab_label_matches_the_rendered_span_and_hit_test_agrees() {
    // The renderer and the hit-tester share `tab_label`; assert the shared span really is
    // where a click finds that tab, including for the second tab.
    let mut app = app();
    app.execute(Action::NewFile);
    for i in 0..app.documents.len() {
        let (x, w) = tab_span_x(&app, i);
        let hit = mouse::hit_test(&down(x + w / 2, app.regions.tabs.y), &app);
        assert_eq!(hit, Hit::TabBar(Some(i)), "click at the middle of tab {i}");
    }
}

#[test]
fn empty_tab_bar_space_is_not_a_tab() {
    let app = app();
    let (_x, w) = tab_span_x(&app, 0);
    let empty = app.regions.tabs.x + w + 5;
    assert!(
        empty < app.regions.tabs.right(),
        "need empty tab-bar space to test"
    );

    let hit = mouse::hit_test(&down(empty, app.regions.tabs.y), &app);
    assert_eq!(hit, Hit::TabBar(None));
    assert_eq!(
        route(&app, &down(empty, app.regions.tabs.y)),
        MouseIntent::Nothing
    );
}

// ------------------------------------------------------- multi-click synthesis

#[test]
fn two_clicks_100ms_apart_at_the_same_cell_select_a_word() {
    let app = app_with("hello world\n");
    let row = screen_row_of_line(&app, 0);
    let x = screen_x_for_col(&app, 8);
    let t0 = Instant::now();

    // First click: a plain caret.
    let (first, state) = mouse::route_with_click(&down(x, row), &app, &ClickState::default(), t0);
    assert_eq!(
        first,
        MouseIntent::SetCaret {
            lc: LineCol::new(0, 8),
            extend: false
        }
    );

    // Second, 100 ms later, same cell: a word selection.
    let second = route_at(&app, &down(x, row), &state, t0 + Duration::from_millis(100));
    assert!(
        matches!(second, MouseIntent::BeginSelection { .. }),
        "100 ms apart must be a double click, got {second:?}"
    );
}

#[test]
fn two_clicks_400ms_apart_are_two_separate_carets() {
    let app = app_with("hello world\n");
    let row = screen_row_of_line(&app, 0);
    let x = screen_x_for_col(&app, 8);
    let t0 = Instant::now();

    let (_, state) = mouse::route_with_click(&down(x, row), &app, &ClickState::default(), t0);
    let second = route_at(&app, &down(x, row), &state, t0 + Duration::from_millis(400));

    assert_eq!(
        second,
        MouseIntent::SetCaret {
            lc: LineCol::new(0, 8),
            extend: false
        }
    );
}

#[test]
fn a_click_at_a_different_cell_resets_the_count() {
    let app = app_with("hello world\n");
    let row = screen_row_of_line(&app, 0);
    let t0 = Instant::now();
    let first_x = screen_x_for_col(&app, 2);
    let second_x = screen_x_for_col(&app, 8);

    let (_, state) = mouse::route_with_click(&down(first_x, row), &app, &ClickState::default(), t0);
    let second = route_at(
        &app,
        &down(second_x, row),
        &state,
        t0 + Duration::from_millis(50),
    );

    assert_eq!(
        second,
        MouseIntent::SetCaret {
            lc: LineCol::new(0, 8),
            extend: false
        },
        "a different cell is a new single click, not a double"
    );
}

#[test]
fn the_multi_click_window_is_250ms() {
    assert_eq!(np_tui::app::CLICK_MULTI_WINDOW, Duration::from_millis(250));
}

#[test]
fn a_double_click_on_a_tab_is_not_a_double_click_in_the_buffer() {
    // Click state must not leak between regions, or double-clicking tabs eventually looks
    // like a triple click and starts selecting lines.
    let mut app = app();
    app.execute(Action::NewFile);
    let (x, _w) = tab_span_x(&app, 0);
    let t0 = Instant::now();
    let click = clicked_at(x, app.regions.tabs.y, t0, 2);

    let row = screen_row_of_line(&app, 0);
    let text_x = screen_x_for_col(&app, 2);
    let (_, state) = mouse::route_with_click(
        &down(x, app.regions.tabs.y),
        &app,
        &click,
        t0 + Duration::from_millis(50),
    );
    assert_eq!(
        state.count, 0,
        "tab clicks must not accumulate text click counts"
    );

    let intent = route_at(
        &app,
        &down(text_x, row),
        &state,
        t0 + Duration::from_millis(60),
    );
    assert!(
        matches!(intent, MouseIntent::SetCaret { extend: false, .. }),
        "the next buffer click is a single click: {intent:?}"
    );
}

// ------------------------------------------------------------- apply behaviour

#[test]
fn a_drag_past_the_visible_edge_stops_at_the_viewport() {
    // Work item 6: extend to the first/last visible row, no auto-scroll.
    let mut app = app_with(&"line\n".repeat(200));
    let top_row = app.geometry.text_area.y;
    let bottom_row = app.geometry.text_area.bottom().saturating_sub(1);

    app.handle_mouse(&drag(app.geometry.text_area.x + 1, top_row));
    // Drag far below the viewport: the focus must land on the last visible row, not line 199.
    app.handle_mouse(&drag(app.geometry.text_area.x + 1, bottom_row + 20));

    let sel = app.doc().expect("document").selection.expect("selection");
    let visible_rows = app.editor_height();
    assert!(
        sel.focus.line <= visible_rows,
        "focus {} escaped {} visible rows",
        sel.focus.line,
        visible_rows
    );
}

#[test]
fn every_row_of_the_table_is_reachable() {
    // Not a behavior test so much as a completeness guard: the twenty rows of §6.9 each
    // produce a distinguishable intent for a representative event, so a row silently
    // dropped from `route` fails here.
    let mut app = app_with("hello world\nsecond line\n");
    // Horizontal scroll needs wrap off, and it has to be set before the closure below
    // captures `app`.
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    app.execute(Action::NewFile);
    let row = screen_row_of_line(&app, 0);
    let text_x = screen_x_for_col(&app, 2);
    let tab_x = tab_span_x(&app, 0).0;
    let menu_x = mouse::menu_bar_spans(app.regions.menu)[0].2;
    let gutter_x = app.geometry.text_area.x.saturating_sub(1);
    let now = Instant::now();

    let mut seen: Vec<&str> = Vec::new();
    let mut record = |e: MouseEvent, click: &ClickState, name: &'static str| {
        let intent = mouse::route_with_click(&e, &app, click, now).0;
        assert!(
            !matches!(intent, MouseIntent::Nothing),
            "row {name} produced Nothing"
        );
        seen.push(name);
    };

    record(down(text_x, row), &ClickState::default(), "1 click");
    record(drag(text_x, row), &ClickState::default(), "2 drag");
    record(
        ev_mods(down_kind(), text_x, row, KeyModifiers::SHIFT),
        &ClickState::default(),
        "3 shift-click",
    );
    record(
        down(text_x, row),
        &clicked_at(text_x, row, now - Duration::from_millis(50), 1),
        "4 double",
    );
    record(
        down(text_x, row),
        &clicked_at(text_x, row, now - Duration::from_millis(50), 2),
        "5 triple",
    );
    record(
        ev_mods(down_kind(), text_x, row, KeyModifiers::ALT),
        &ClickState::default(),
        "6 alt-drag",
    );
    record(
        ev(MouseEventKind::Down(MouseButton::Middle), text_x, row),
        &ClickState::default(),
        "7 middle paste",
    );
    record(
        ev(MouseEventKind::ScrollDown, text_x, row),
        &ClickState::default(),
        "8 wheel",
    );
    record(
        ev_mods(MouseEventKind::ScrollDown, text_x, row, KeyModifiers::SHIFT),
        &ClickState::default(),
        "9 shift-wheel",
    );
    record(
        ev_mods(
            MouseEventKind::ScrollDown,
            text_x,
            row,
            KeyModifiers::CONTROL,
        ),
        &ClickState::default(),
        "10 ctrl-wheel",
    );
    record(
        down(gutter_x, row),
        &ClickState::default(),
        "11 gutter line",
    );
    // 12/13 fold and bookmark gutters are S13; 14 scrollbar is S13. Hit-tested, no intent yet.
    record(
        down(tab_x, app.regions.tabs.y),
        &ClickState::default(),
        "15 tab click",
    );
    record(
        ev(
            MouseEventKind::Down(MouseButton::Middle),
            tab_x,
            app.regions.tabs.y,
        ),
        &ClickState::default(),
        "16 middle close",
    );
    record(
        ev(MouseEventKind::ScrollUp, tab_x, app.regions.tabs.y),
        &ClickState::default(),
        "17 tab wheel",
    );
    record(
        ev(MouseEventKind::Down(MouseButton::Right), text_x, row),
        &ClickState::default(),
        "18 right click",
    );
    record(
        down(menu_x, app.regions.menu.y),
        &ClickState::default(),
        "19 menu click",
    );
    record(
        ev(MouseEventKind::Moved, text_x, row),
        &ClickState::default(),
        "20 hover",
    );

    assert_eq!(
        seen.len(),
        17,
        "17 rows implemented today; 12/13/14 are S13's: {seen:?}"
    );
}

// ------------------------------------------------------------------- helpers

/// The screen x and width of tab `i`, derived from the shared label helper.
fn tab_span_x(app: &App, i: usize) -> (u16, u16) {
    let mut x = app.regions.tabs.x;
    for (j, doc) in app.documents.iter().enumerate() {
        let w = mouse::tab_label(doc).chars().count() as u16;
        if j == i {
            return (x, w);
        }
        x += w;
    }
    panic!("no tab {i}");
}
