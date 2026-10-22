//! S13: the view features driven through the app -- §6.9 rows 11-14, the view toggles, fold
//! and bookmark actions, and the row-based scroll math.
//!
//! The layout engine itself is tested in `np-core` (`view_s13.rs`, `folding_s13.rs`). What this
//! file covers is the wiring: that a click on a gutter marker toggles the thing drawn at that
//! cell, that a scrollbar thumb drag lands where the thumb was painted, and that the toggles and
//! fold/bookmark actions actually change state rather than falling into a `_ => {}`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Buffer, Config, LineCol};
use np_tui::actions::Action;
use np_tui::app::ClickState;
use np_tui::mouse::{self, Hit, MouseIntent};
use np_tui::App;

const W: u16 = 80;
const H: u16 = 24;

fn app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.recompute_geometry(W, H);
    app
}

/// App with `text` in the active tab and folds detected for `language`.
fn app_with(text: &str, language: &str) -> App {
    let mut app = app();
    {
        let doc = app.doc_mut().expect("document");
        doc.buffer = Buffer::from_str_lossy(text);
        doc.set_cursor(LineCol::new(0, 0), false);
        doc.language = np_core::LanguageId::Named(language.into());
    }
    app.recompute_geometry(W, H);
    np_tui::view::refresh_folds(&mut app);
    app.recompute_geometry(W, H);
    app
}

/// A brace-shaped document with a fold on line 0 and one on line 5.
const BRACES: &str =
    "fn a() {\n    one();\n    two();\n}\n\nfn b() {\n    three();\n    four();\n}\n";

fn ev(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn down(column: u16, row: u16) -> MouseEvent {
    ev(MouseEventKind::Down(MouseButton::Left), column, row)
}

fn drag(column: u16, row: u16) -> MouseEvent {
    ev(MouseEventKind::Drag(MouseButton::Left), column, row)
}

fn route(app: &App, e: &MouseEvent) -> MouseIntent {
    mouse::route_with_click(e, app, &ClickState::default(), Instant::now()).0
}

/// Screen row showing document line `line`, asking the layout rather than assuming one row
/// per line -- which is the whole point once wrap is on.
fn screen_row_of_line(app: &App, line: usize) -> u16 {
    let doc = app.doc().expect("document");
    let rows = doc.visible_rows(&app.geometry);
    let offset = rows
        .iter()
        .position(|r| r.line == line)
        .expect("line is visible");
    app.geometry.text_area.y + offset as u16
}

/// Render once so `App` has published its gutter geometry, then return the gutter layout.
fn gutter_of(app: &App) -> np_tui::ui::gutter::Gutter {
    np_tui::mouse::gutter_layout(app)
}

/// The painted text of a screen row, gutter excluded.
///
/// Reading the whole terminal row would make every render assertion depend on the gutter's
/// width, which is exactly the thing S13 made variable -- a fold column appears the moment a
/// document has a fold and shifts the text right by a cell. Asserting on the text area alone
/// keeps these tests about what was painted rather than about where the strip ends.
fn painted_text(app: &App, buf: &ratatui::buffer::Buffer, screen_row: u16) -> String {
    let from = app.geometry.text_area.x;
    np_tui::ui::row_text(buf, screen_row, from, W)
        .trim_end()
        .to_string()
}

// ------------------------------------------------------- row 11: gutter select

#[test]
fn row_11_click_a_line_number_selects_that_line() {
    let app = app_with("one\ntwo\nthree\n", "");
    let g = gutter_of(&app);
    let x = g.number_x();
    let row = screen_row_of_line(&app, 1);
    assert_eq!(
        route(&app, &down(x, row)),
        MouseIntent::SelectLine {
            line: 1,
            keep_caret: true
        }
    );
}

#[test]
fn row_11_click_in_the_gutter_does_not_move_the_caret() {
    let mut app = app_with("one\ntwo\nthree\n", "");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(0, 2), false);
    let g = gutter_of(&app);
    let row = screen_row_of_line(&app, 2);
    app.handle_mouse(&down(g.number_x(), row));
    let doc = app.doc().expect("doc");
    assert_eq!(doc.cursor.pos, LineCol::new(0, 2), "caret stays put");
    let sel = doc.selection.expect("line selected");
    assert_eq!(sel.anchor, LineCol::new(2, 0));
    // Ropey counts the final terminator as one more (empty) line, so "three\n" is four lines
    // and the selection runs to column 0 of that phantom line -- which is the whole of line 2
    // including its newline, so cutting leaves no empty line behind.
    assert_eq!(sel.focus, LineCol::new(3, 0), "through the line terminator");
}

#[test]
fn row_11_drag_from_3_to_7_selects_five_lines() {
    let mut app = app_with("a\nb\nc\nd\ne\nf\ng\nh\n", "");
    let g = gutter_of(&app);
    let x = g.number_x();
    app.handle_mouse(&down(x, screen_row_of_line(&app, 2)));
    app.handle_mouse(&drag(x, screen_row_of_line(&app, 6)));
    let sel = app.doc().expect("doc").selection.expect("drag selected");
    assert_eq!(sel.anchor, LineCol::new(2, 0));
    assert_eq!(
        sel.focus,
        LineCol::new(7, 0),
        "through the end of line 6, so five whole lines"
    );
}

#[test]
fn row_11_drag_upwards_selects_the_same_range() {
    let mut app = app_with("a\nb\nc\nd\ne\nf\ng\nh\n", "");
    let g = gutter_of(&app);
    let x = g.number_x();
    app.handle_mouse(&down(x, screen_row_of_line(&app, 6)));
    app.handle_mouse(&drag(x, screen_row_of_line(&app, 2)));
    let sel = app.doc().expect("doc").selection.expect("drag selected");
    assert_eq!(
        sel.anchor,
        LineCol::new(2, 0),
        "anchor is the top of the range"
    );
    assert_eq!(sel.focus, LineCol::new(7, 0));
}

#[test]
fn row_11_a_gutter_drag_leaves_the_caret_alone() {
    let mut app = app_with("a\nb\nc\nd\ne\nf\ng\nh\n", "");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(0, 1), false);
    let g = gutter_of(&app);
    let x = g.number_x();
    app.handle_mouse(&down(x, screen_row_of_line(&app, 3)));
    app.handle_mouse(&drag(x, screen_row_of_line(&app, 5)));
    assert_eq!(app.doc().expect("doc").cursor.pos, LineCol::new(0, 1));
}

#[test]
fn row_11_a_text_click_clears_the_gutter_drag_anchor() {
    // Otherwise a gutter drag much later would extend from a line the user stopped caring
    // about, selecting a range that has nothing to do with the pointer.
    let mut app = app_with("a\nb\nc\nd\ne\nf\ng\nh\n", "");
    let g = gutter_of(&app);
    app.handle_mouse(&down(g.number_x(), screen_row_of_line(&app, 3)));
    let text_x = app.geometry.text_area.x + 1;
    app.handle_mouse(&down(text_x, screen_row_of_line(&app, 7)));
    app.handle_mouse(&drag(text_x, screen_row_of_line(&app, 7)));
    // Now a fresh gutter drag must anchor at its own press line, not at 3.
    app.handle_mouse(&down(g.number_x(), screen_row_of_line(&app, 5)));
    app.handle_mouse(&drag(g.number_x(), screen_row_of_line(&app, 6)));
    let sel = app.doc().expect("doc").selection.expect("selected");
    assert_eq!(sel.anchor, LineCol::new(5, 0), "anchored at the new press");
}

// ----------------------------------------------------- row 12: fold markers

#[test]
fn row_12_the_fold_column_hits_the_fold_on_that_row() {
    let app = app_with(BRACES, "Rust");
    let g = gutter_of(&app);
    let fold_col = g.fold_col().expect("document has folds");
    let row = screen_row_of_line(&app, 5);
    assert_eq!(
        mouse::hit_test(&down(fold_col, row), &app),
        Hit::GutterFold(5)
    );
    assert_eq!(
        route(&app, &down(fold_col, row)),
        MouseIntent::ToggleFold(5)
    );
}

#[test]
fn row_12_clicking_the_fold_marker_collapses_only_that_fold() {
    let mut app = app_with(BRACES, "Rust");
    let headers: Vec<usize> = app
        .doc()
        .expect("doc")
        .folds
        .iter()
        .map(|f| f.start)
        .collect();
    assert!(headers.contains(&0) && headers.contains(&5), "{headers:?}");
    let g = gutter_of(&app);
    let fold_col = g.fold_col().expect("fold column");
    app.handle_mouse(&down(fold_col, screen_row_of_line(&app, 5)));
    let folds = &app.doc().expect("doc").folds;
    let closed: Vec<usize> = folds
        .iter()
        .filter(|f| f.collapsed)
        .map(|f| f.start)
        .collect();
    assert_eq!(
        closed,
        vec![5],
        "only the clicked fold collapsed: {folds:?}"
    );
}

#[test]
fn row_12_a_collapsed_fold_hides_its_body_rows() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(0);
    let rows: Vec<usize> = app
        .doc()
        .expect("doc")
        .visible_rows(&app.geometry)
        .iter()
        .map(|r| r.line)
        .collect();
    assert!(rows.contains(&0), "the header stays");
    assert!(!rows.contains(&1) && !rows.contains(&2), "{rows:?}");
    assert!(rows.contains(&5), "the second function is untouched");
}

#[test]
fn row_12_clicking_a_fold_marker_twice_reopens_it() {
    let mut app = app_with(BRACES, "Rust");
    let g = gutter_of(&app);
    let fold_col = g.fold_col().expect("fold column");
    let row = screen_row_of_line(&app, 0);
    app.handle_mouse(&down(fold_col, row));
    app.handle_mouse(&down(fold_col, row));
    assert!(
        app.doc()
            .expect("doc")
            .folds
            .iter()
            .all(|f| !f.collapsed || f.start != 0),
        "back to open"
    );
}

#[test]
fn row_12_a_fold_column_click_between_markers_is_a_no_op() {
    // The fold column spans every row, so most clicks in it land on a line with no fold. They
    // must not toggle the nearest fold.
    let mut app = app_with(BRACES, "Rust");
    let before = app.doc().expect("doc").folds.clone();
    let g = gutter_of(&app);
    let fold_col = g.fold_col().expect("fold column");
    app.handle_mouse(&down(fold_col, screen_row_of_line(&app, 4)));
    assert_eq!(app.doc().expect("doc").folds, before);
}

#[test]
fn row_12_hit_test_and_the_renderer_agree_on_the_fold_column() {
    // §6.9's rule, checked against actual painted cells rather than a second computation: the
    // glyph the router aims at is the glyph in the buffer at that coordinate.
    use ratatui::backend::TestBackend;
    let mut app = app_with(BRACES, "Rust");
    let mut terminal = ratatui::Terminal::new(TestBackend::new(W, H)).expect("terminal");
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let g = gutter_of(&app);
    let fold_col = g.fold_col().expect("fold column");
    let row = screen_row_of_line(&app, 0);
    let painted = np_tui::ui::row_text(terminal.backend().buffer(), row, fold_col, fold_col + 1);
    assert_eq!(painted, "\u{25bc}", "foldable line shows the open marker");
    app.handle_mouse(&down(fold_col, row));
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let after = np_tui::ui::row_text(terminal.backend().buffer(), row, fold_col, fold_col + 1);
    assert_eq!(after, "\u{25b6}", "collapsed line shows the closed marker");
}

// -------------------------------------------------- row 13: bookmark markers

#[test]
fn row_13_clicking_the_bookmark_column_toggles_only_that_bookmark() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").bookmarks = [2usize].into_iter().collect();
    app.recompute_geometry(W, H);
    let g = gutter_of(&app);
    let mark_col = g.bookmark_col().expect("bookmark column");
    app.handle_mouse(&down(mark_col, screen_row_of_line(&app, 5)));
    let marks: Vec<usize> = app.doc().expect("doc").bookmarks.iter().copied().collect();
    assert_eq!(marks, vec![2, 5], "existing bookmark untouched: {marks:?}");
    app.handle_mouse(&down(mark_col, screen_row_of_line(&app, 2)));
    let marks: Vec<usize> = app.doc().expect("doc").bookmarks.iter().copied().collect();
    assert_eq!(marks, vec![5], "clicked again removes it");
}

#[test]
fn row_13_the_bookmark_glyph_is_where_the_router_says_it_is() {
    use ratatui::backend::TestBackend;
    let mut app = app_with("a\nb\nc\n", "");
    app.doc_mut().expect("doc").bookmarks = [1usize].into_iter().collect();
    app.recompute_geometry(W, H);
    let mut terminal = ratatui::Terminal::new(TestBackend::new(W, H)).expect("terminal");
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let g = gutter_of(&app);
    let col = g.bookmark_col().expect("bookmark column");
    let painted = np_tui::ui::row_text(
        terminal.backend().buffer(),
        screen_row_of_line(&app, 1),
        col,
        col + 1,
    );
    assert_eq!(painted, "\u{25c6}");
}

#[test]
fn row_13_a_bookmark_click_never_moves_the_caret() {
    let mut app = app_with("a\nb\nc\n", "");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(0, 1), false);
    // No bookmarks yet, so the column does not exist; toggle via the action first, which is the
    // other half of the same rule.
    app.execute(Action::ToggleBookmark);
    assert!(app.doc().expect("doc").bookmarks.contains(&0));
    assert_eq!(app.doc().expect("doc").cursor.pos, LineCol::new(0, 1));
}

// ----------------------------------------------------------- row 14: scrollbars

/// A document tall enough that the vertical bar exists.
fn tall_app() -> App {
    let text = (1..=100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app_with(&text, "")
}

fn vertical_track_x(app: &App) -> u16 {
    app.geometry.text_area.right().saturating_sub(1)
}

#[test]
fn row_14_the_rightmost_column_hit_tests_as_thumb_or_track() {
    let app = tall_app();
    let x = vertical_track_x(&app);
    let bar = app
        .vertical_scrollbar()
        .expect("a 100-line doc needs a bar");
    let (from, to) = bar.thumb_range();
    let thumb_row = app.regions.editor.y + from;
    let track_row = app.regions.editor.y + bar.track_len - 1;
    match mouse::hit_test(&down(x, thumb_row), &app) {
        Hit::ScrollbarThumb(kind, cell) => {
            assert_eq!(kind, np_tui::mouse::VerticalOrHorizontal::Vertical);
            assert_eq!(cell, thumb_row, "the hit carries the row clicked");
        }
        other => panic!("expected a thumb hit at the thumb's first row, got {other:?}"),
    }
    assert!(
        matches!(
            mouse::hit_test(&down(x, track_row), &app),
            Hit::ScrollbarTrack(np_tui::mouse::VerticalOrHorizontal::Vertical, _)
        ),
        "the bottom of the trough is track"
    );
    let _ = to;
}

/// Grab the thumb and drag it down `cells` trough cells, one event per cell.
///
/// A terminal reports every cell the pointer crosses, so this replays the gesture the way it
/// arrives rather than synthesizing a single event. The grab is at the thumb's *middle* rather
/// than its edge on purpose: grabbing at the first cell makes a correct implementation and a
/// buggy one agree on the first event, which is exactly the case that hides the grab-depth bug.
/// Grabbing mid-thumb means an implementation that forgets where the pointer took hold moves by
/// the thumb's half-height on the very first drag event, and the assertions below see it.
fn drag_thumb_down(app: &mut App, cells: u16) {
    let x = vertical_track_x(app);
    let bar = app.vertical_scrollbar().expect("bar");
    let grab_row = bar.track_start + bar.thumb_len / 2;
    app.handle_mouse(&down(x, grab_row));
    let last = (grab_row + cells).min(bar.track_start + bar.track_len - 1);
    for row in grab_row..=last {
        app.handle_mouse(&drag(x, row));
    }
    // A terminal keeps reporting the pointer's cell while it sits at the bottom edge, and the
    // implementation leans on that to reach the last rows (the thumb cannot be drawn past the
    // trough), so replay a couple of end-of-trough events the way a held drag would.
    for _ in 0..3 {
        app.handle_mouse(&drag(x, last));
    }
}

#[test]
fn row_14_dragging_the_thumb_to_the_bottom_shows_the_last_row() {
    let mut app = tall_app();
    let bar = app.vertical_scrollbar().expect("bar");
    // From the thumb's middle to the bottom of the trough: the whole reachable span.
    drag_thumb_down(&mut app, bar.track_len);
    let rows = app.doc().expect("doc").visible_rows(&app.geometry);
    let last = rows.last().expect("rows");
    assert_eq!(
        last.line,
        app.doc().expect("doc").buffer.line_count() - 1,
        "a thumb dragged to the bottom must show the document's last line, got {:?}",
        rows.iter().map(|r| r.line).collect::<Vec<_>>()
    );
}

#[test]
fn row_14_the_first_thumb_cell_shows_the_first_row() {
    let mut app = tall_app();
    let x = vertical_track_x(&app);
    // Scroll away first, then re-read the bar: the thumb has to be somewhere in the middle of
    // the trough for "aim at the top of the trough" to be a question with a non-trivial answer.
    // Reading the geometry before scrolling is the mistake that makes a test pass by accident --
    // the stale `bar` still says the thumb is home, and the assertion checks nothing.
    app.doc_mut().expect("doc").scroll.top_line = 60;
    let bar = app.vertical_scrollbar().expect("bar");
    assert!(bar.thumb_start > 0, "the thumb should have moved down");
    app.handle_mouse(&down(x, bar.track_start));
    assert_eq!(
        app.doc().expect("doc").scroll.top_line,
        0,
        "the top of the trough is the top of the document"
    );
}

#[test]
fn row_14_a_click_above_the_thumb_pages_up_one_screen() {
    let mut app = tall_app();
    app.doc_mut().expect("doc").scroll.top_line = 40;
    let x = vertical_track_x(&app);
    let bar = app.vertical_scrollbar().expect("bar");
    let above: u16 = app.regions.editor.y + bar.thumb_start / 2;
    if bar.is_thumb(x) && above >= app.regions.editor.y + bar.thumb_start {
        panic!("test premise: click must be above the thumb");
    }
    app.handle_mouse(&down(x, above));
    let top = app.doc().expect("doc").scroll.top_line;
    assert!(top < 40, "paged up, top is {top}");
    let floor = 40usize.saturating_sub(app.editor_height() * 2);
    assert!(top > floor, "one page, not a jump to the top: {top}");
}

#[test]
fn row_14_a_click_below_the_thumb_pages_down_one_screen() {
    let mut app = tall_app();
    let x = vertical_track_x(&app);
    let bar = app.vertical_scrollbar().expect("bar");
    let below = (app.regions.editor.y + bar.thumb_start + bar.thumb_len + 2)
        .min(app.regions.editor.y + bar.track_len - 1);
    app.handle_mouse(&down(x, below));
    assert!(app.doc().expect("doc").scroll.top_line > 0);
}

#[test]
fn row_14_no_scrollbar_exists_for_a_document_that_fits() {
    let app = app_with("a\nb\nc\n", "");
    assert!(app.vertical_scrollbar().is_none());
    assert!(app.horizontal_scrollbar().is_none());
}

#[test]
fn row_14_the_horizontal_bar_exists_for_a_wide_line_and_disappears_under_wrap() {
    let long = "x".repeat(300);
    let mut app = app_with(&long, "");
    assert!(
        app.horizontal_scrollbar().is_some(),
        "a 300-col line in an 80-col area needs a horizontal bar"
    );
    app.execute(Action::ToggleWordWrap);
    assert!(
        app.horizontal_scrollbar().is_none(),
        "FR-V1: no horizontal bar while wrapping"
    );
}

#[test]
fn row_14_thumb_drag_is_wrap_aware() {
    // The regression this guards: resolving the thumb against a *line* count instead of a row
    // count leaves the bottom of the trough somewhere near the top of a wrapped file. With 60
    // lines laid out as 180 rows, a line-based thumb tops out at offset 39 -- a fifth of the way
    // through. A row-based one gets to the last line the viewport can start at.
    let text = (1..=60)
        .map(|_| "word ".repeat(40))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = app_with(&text, "");
    app.execute(Action::ToggleWordWrap);
    let bar = app
        .vertical_scrollbar()
        .expect("wrapped rows overflow the viewport");
    drag_thumb_down(&mut app, bar.track_len);
    let doc = app.doc().expect("doc");
    let total = np_core::total_row_count(&doc.buffer, &app.geometry, &doc.folds);
    assert!(total > 150, "fixture should be about 180 rows, got {total}");
    assert!(
        doc.scroll.top_line >= 50,
        "a row-based thumb reaches the last page; a line-based one would stop near line 39. \
         top_line is {}",
        doc.scroll.top_line
    );
    // The documented limit, and the reason this asserts on `top_line` rather than on which lines
    // are painted: `scroll.top_line` names a line, so a viewport can only begin at a line's first
    // row. A 3-row line starting at the bottom of a 21-row screen paints one row and the rest
    // falls off the bottom -- those rows are reachable by scrolling up, not by scrolling further
    // down, and there is no scroll position that shows a line's last row without its first. The
    // trade is recorded in docs/DEVIATIONS.md.
    let rows = app.doc().expect("doc").visible_rows(&app.geometry);
    assert!(
        rows.len() <= app.editor_height(),
        "never more than a viewport of rows"
    );
    assert!(
        rows.first().expect("rows").line >= 50,
        "the last page starts in the document's last lines: {:?}",
        rows.iter().map(|r| r.line).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------- 10: the view toggles

#[test]
fn alt_z_toggles_word_wrap_and_forces_left_col_to_zero() {
    let mut app = app_with(&"y".repeat(300), "");
    app.doc_mut().expect("doc").scroll.left_col = 40;
    assert!(!app.config.ui.word_wrap);
    app.execute(Action::ToggleWordWrap);
    assert!(app.config.ui.word_wrap);
    assert_eq!(
        app.doc().expect("doc").scroll.left_col,
        0,
        "FR-V1: horizontal scroll is off while wrapped"
    );
    app.execute(Action::ToggleWordWrap);
    assert!(!app.config.ui.word_wrap);
}

#[test]
fn scroll_horizontal_is_inert_while_wrapped_and_returns_after() {
    let mut app = app_with(&"z".repeat(300), "");
    app.execute(Action::ToggleWordWrap);
    // Shift+wheel is the horizontal scroll gesture (§6.9 row 9).
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: app.geometry.text_area.x + 1,
        row: app.regions.editor.y,
        modifiers: KeyModifiers::SHIFT,
    };
    app.handle_mouse(&wheel);
    assert_eq!(
        app.doc().expect("doc").scroll.left_col,
        0,
        "inert while wrapped"
    );
    app.execute(Action::ToggleWordWrap);
    app.handle_mouse(&wheel);
    assert_eq!(
        app.doc().expect("doc").scroll.left_col,
        3,
        "the configured step applies again once wrap is off"
    );
}

#[test]
fn toggle_line_numbers_changes_the_gutter_width_and_the_text_area() {
    let mut app = app_with("a\nb\nc\n", "");
    let before = app.geometry;
    assert!(app.config.ui.line_numbers);
    app.execute(Action::ToggleLineNumbers);
    assert!(!app.config.ui.line_numbers);
    assert_eq!(app.geometry.gutter_width, 0, "no numbers, no gutter");
    assert_eq!(app.geometry.text_area.x, 0);
    app.execute(Action::ToggleLineNumbers);
    assert_eq!(app.geometry.gutter_width, before.gutter_width);
}

#[test]
fn toggle_whitespace_and_eol_marks_flip_their_config_flags() {
    let mut app = app_with("a\n", "");
    assert!(!app.config.ui.show_whitespace);
    assert!(!app.config.ui.show_eol);
    app.execute(Action::ToggleWhitespace);
    app.execute(Action::ToggleEolMarks);
    assert!(app.config.ui.show_whitespace);
    assert!(app.config.ui.show_eol);
    app.execute(Action::ToggleWhitespace);
    app.execute(Action::ToggleEolMarks);
    assert!(!app.config.ui.show_whitespace);
    assert!(!app.config.ui.show_eol);
}

#[test]
fn a_view_toggle_leaves_the_editor_usable_afterwards() {
    // The toggles recompute geometry, which is the layout every other subsystem reads. If a
    // toggle left it stale, typing would land at the wrong place and no test below would say so.
    let mut app = app_with("hello\nworld\n", "");
    app.execute(Action::ToggleWordWrap);
    app.execute(Action::ToggleLineNumbers);
    app.execute(Action::ToggleLineNumbers);
    app.execute(Action::ToggleWordWrap);
    for ch in "xy".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert!(app
        .doc()
        .expect("doc")
        .buffer
        .line_string(0)
        .starts_with("xyhello"));
}

// ------------------------------------------------- fold and bookmark actions

#[test]
fn fold_all_then_unfold_all() {
    let mut app = app_with(BRACES, "Rust");
    app.execute(Action::FoldAll);
    let folds = app.doc().expect("doc").folds.clone();
    assert!(!folds.is_empty());
    assert!(
        folds.iter().all(|f| f.collapsed),
        "every fold closed: {folds:?}"
    );
    let rows: Vec<usize> = app
        .doc()
        .expect("doc")
        .visible_rows(&app.geometry)
        .iter()
        .map(|r| r.line)
        .collect();
    // Fold All closes both blocks, so what is left is the two headers, the blank between them
    // (line 4, inside neither fold), the line after the second header's body and the tail.
    assert_eq!(
        rows,
        vec![0, 4, 5, 9],
        "headers and the lines no fold covers: {rows:?}"
    );
    app.execute(Action::UnfoldAll);
    assert!(app.doc().expect("doc").folds.iter().all(|f| !f.collapsed));
}

#[test]
fn toggle_fold_on_the_caret_line_uses_the_fold_there() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(0, 0), false);
    app.execute(Action::ToggleFold);
    assert!(app
        .doc()
        .expect("doc")
        .fold_at(0)
        .is_some_and(|f| f.collapsed));
}

#[test]
fn toggle_fold_inside_a_collapsed_block_reopens_it() {
    // The marker for a hidden line is not on screen, so the caret key has to reach the fold
    // containing the caret or the block can only be opened by scrolling to its header.
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(0);
    let hidden = LineCol::new(2, 0);
    assert!(app.doc().expect("doc").line_is_hidden(hidden.line));
    app.doc_mut().expect("doc").cursor.pos = hidden;
    app.execute(Action::ToggleFold);
    assert!(
        app.doc()
            .expect("doc")
            .fold_at(0)
            .is_some_and(|f| !f.collapsed),
        "reopened from inside"
    );
}

#[test]
fn editing_inside_a_collapsed_range_unfolds_it() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(0);
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 4);
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    let fold = app
        .doc()
        .expect("doc")
        .fold_at(0)
        .expect("fold still there");
    assert!(!fold.collapsed, "FR-V5: the edit opened the block");
    assert!(app.doc().expect("doc").buffer.line_string(1).contains('z'));
}

#[test]
fn backspace_inside_a_collapsed_range_unfolds_it_too() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(0);
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 4);
    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(!app.doc().expect("doc").fold_at(0).expect("fold").collapsed);
}

#[test]
fn bookmark_navigation_wraps_around() {
    let mut app = app_with("a\nb\nc\nd\ne\n", "");
    app.doc_mut().expect("doc").bookmarks = [1usize, 3].into_iter().collect();
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(4, 0), false);
    app.execute(Action::NextBookmark);
    assert_eq!(app.doc().expect("doc").cursor.pos.line, 1, "wrapped");
    app.execute(Action::PrevBookmark);
    assert_eq!(app.doc().expect("doc").cursor.pos.line, 3, "wrapped back");
}

#[test]
fn jumping_to_a_bookmark_inside_a_collapsed_range_unfolds_it() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").bookmarks = [2usize].into_iter().collect();
    app.doc_mut().expect("doc").toggle_fold_at(0);
    assert!(app.doc().expect("doc").line_is_hidden(2));
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(8, 0), false);
    app.execute(Action::NextBookmark);
    let doc = app.doc().expect("doc");
    assert_eq!(doc.cursor.pos.line, 2, "landed on the bookmark");
    assert!(
        doc.folds
            .iter()
            .filter(|f| f.start == 0)
            .all(|f| !f.collapsed),
        "the fold opened rather than the jump appearing broken"
    );
    let rows: Vec<usize> = doc
        .visible_rows(&app.geometry)
        .iter()
        .map(|r| r.line)
        .collect();
    assert!(rows.contains(&2), "and the line is actually on screen");
}

#[test]
fn clear_bookmarks_removes_every_one() {
    let mut app = app_with("a\nb\nc\n", "");
    app.doc_mut().expect("doc").bookmarks = [0usize, 2].into_iter().collect();
    app.execute(Action::ClearBookmarks);
    assert!(app.doc().expect("doc").bookmarks.is_empty());
}

#[test]
fn bookmark_and_fold_markers_on_adjacent_lines_both_render() {
    use ratatui::backend::TestBackend;
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").bookmarks = [1usize].into_iter().collect();
    app.doc_mut().expect("doc").toggle_fold_at(0);
    app.recompute_geometry(W, H);
    let mut terminal = ratatui::Terminal::new(TestBackend::new(W, H)).expect("terminal");
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let g = gutter_of(&app);
    let buf = terminal.backend().buffer();
    let row = screen_row_of_line(&app, 0);
    let fold_glyph = np_tui::ui::row_text(
        buf,
        row,
        g.fold_col().expect("fold"),
        g.fold_col().unwrap() + 1,
    );
    assert_eq!(fold_glyph, "\u{25b6}", "closed marker on line 0");
    // Line 1 is inside the collapsed range, so it is not on screen; the bookmark lives on a line
    // the fold hides, which is the honest rendering. Put it where it can be seen instead and
    // check the two columns do not collide.
    app.doc_mut().expect("doc").bookmarks = [5usize].into_iter().collect();
    app.recompute_geometry(W, H);
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let row = screen_row_of_line(&app, 5);
    assert_eq!(
        np_tui::ui::row_text(
            buf,
            row,
            g.bookmark_col().expect("bkm"),
            g.bookmark_col().unwrap() + 1
        ),
        "\u{25c6}"
    );
    assert_eq!(
        np_tui::ui::row_text(buf, row, g.fold_col().unwrap(), g.fold_col().unwrap() + 1),
        "\u{25bc}",
        "the fold marker is still there on the same row"
    );
    assert!(
        g.bookmark_col().unwrap() < g.fold_col().unwrap(),
        "bookmark column is outside the fold column"
    );
}

// ------------------------------------------------- 9: Ctrl+wheel page scroll

#[test]
fn ctrl_wheel_pages_by_screen_rows() {
    let mut app = tall_app();
    let row = screen_row_of_line(&app, 10);
    let e = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: app.geometry.text_area.x + 1,
        row,
        modifiers: KeyModifiers::CONTROL,
    };
    app.handle_mouse(&e);
    let top = app.doc().expect("doc").scroll.top_line;
    assert!(
        (app.editor_height() as i64 - top as i64).abs() <= 1,
        "one page down, top is {top}"
    );
}

#[test]
fn ctrl_wheel_at_the_bottom_does_not_scroll_past_the_last_page() {
    let mut app = tall_app();
    // The viewport is the text area, which is one row taller than the editor band here (the
    // banner row is reserved inside the band). Derive the last page from the layout rather than
    // from `editor_height`, which is the band.
    let last_page = app.doc().expect("doc").buffer.line_count()
        - app.doc().expect("doc").visible_rows(&app.geometry).len();
    app.doc_mut().expect("doc").scroll.top_line = last_page;
    let row = screen_row_of_line(&app, last_page + 1);
    let e = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: app.geometry.text_area.x + 1,
        row,
        modifiers: KeyModifiers::CONTROL,
    };
    app.handle_mouse(&e);
    // Already showing the last line, so a page down must not move the viewport or leave blank
    // space below the document.
    let rows = app.doc().expect("doc").visible_rows(&app.geometry);
    assert_eq!(
        rows.last().expect("rows").line,
        app.doc().expect("doc").buffer.line_count() - 1,
        "still showing the end of the document"
    );
}

#[test]
fn page_down_under_wrap_advances_rows_not_lines() {
    let text = (1..=60)
        .map(|_| "word ".repeat(40))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = app_with(&text, "");
    app.execute(Action::ToggleWordWrap);
    app.execute(Action::ScrollPageDown);
    let doc = app.doc().expect("doc");
    let rows = doc.visible_rows(&app.geometry);
    let top_row = np_tui::app::row_offset_of_top(doc, &app.geometry);
    assert!(
        top_row >= app.editor_height(),
        "a page of rows moved, top row offset is {top_row}"
    );
    assert!(
        doc.scroll.top_line < app.editor_height(),
        "and it is fewer *lines* than that, because each line is several rows: {}",
        doc.scroll.top_line
    );
    assert_eq!(rows.len(), app.editor_height(), "a full viewport of rows");
}

/// The words of the wrapped fixture, so a seam check can name them without restating the text.
fn text_words() -> impl Iterator<Item = &'static str> {
    "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega"
        .split_whitespace()
}

// ------------------------------------------------------------- snapshot-ish

#[test]
fn a_folded_view_snapshot_shows_the_closed_marker_and_skips_hidden_lines() {
    use ratatui::backend::TestBackend;
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(0);
    let mut terminal = ratatui::Terminal::new(TestBackend::new(W, H)).expect("terminal");
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let editor_y = app.regions.editor.y;
    let g = gutter_of(&app);
    let marker = np_tui::ui::row_text(
        buf,
        editor_y,
        g.fold_col().expect("fold column"),
        g.fold_col().unwrap() + 1,
    );
    assert_eq!(marker, "\u{25b6}", "the header row carries a closed marker");
    let first = painted_text(&app, buf, editor_y);
    assert!(first.contains("fn a()"), "{first:?}");
    // The line after the folded block is the blank between the two functions, so the second
    // screen row is legitimately empty. What matters is that the body is gone and that the row
    // *count* shrank by exactly the hidden lines.
    let second = painted_text(&app, buf, editor_y + 1);
    assert!(!second.contains("one()"), "folded body painted: {second:?}");
    let third = painted_text(&app, buf, editor_y + 2);
    assert_eq!(third, "fn b() {", "{third:?}");
    let shown = app
        .doc()
        .expect("doc")
        .visible_rows(&app.geometry)
        .iter()
        .map(|r| r.line)
        .collect::<Vec<_>>();
    assert_eq!(
        shown,
        vec![0, 4, 5, 6, 7, 8, 9],
        "the first block is hidden, everything from its closing brace on is not"
    );
}

#[test]
fn a_wrapped_view_snapshot_breaks_a_long_line_across_rows() {
    use ratatui::backend::TestBackend;
    let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega";
    let mut app = app_with(text, "");
    app.execute(Action::ToggleWordWrap);
    let mut terminal = ratatui::Terminal::new(TestBackend::new(W, H)).expect("terminal");
    terminal
        .draw(|frame| np_tui::ui::draw(&app, frame.buffer_mut()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let editor_y = app.regions.editor.y;
    let row0 = painted_text(&app, buf, editor_y);
    let row1 = painted_text(&app, buf, editor_y + 1);
    assert!(row0.starts_with("alpha"), "{row0:?}");
    assert!(!row1.is_empty(), "the wrap produced a second row");
    // A word boundary, not mid-word: the row before it must end at a whole word and this one
    // must begin at one. Checking the seam rather than naming the word keeps the assertion true
    // when the text area's width changes by a cell.
    let first_word = row1.split_whitespace().next().unwrap_or_default();
    assert!(
        text_words().any(|w| w == first_word),
        "the continuation row starts at a word boundary, not inside a word: {row1:?}"
    );
    assert!(
        !row0.ends_with(' ') && !row1.ends_with(' '),
        "wrapped rows do not carry the consumed space"
    );
}

#[test]
fn the_fold_state_survives_an_unrelated_edit() {
    let mut app = app_with(BRACES, "Rust");
    app.doc_mut().expect("doc").toggle_fold_at(5);
    // Type at the very top, outside the folded block.
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(0, 0), false);
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    np_tui::view::refresh_folds(&mut app);
    let closed: Vec<[usize; 2]> = app
        .doc()
        .expect("doc")
        .folds
        .iter()
        .filter(|f| f.collapsed)
        .map(|f| [f.start, f.end])
        .collect();
    // The fold is detected again from scratch here (a plain `refresh_folds`, the editing rule),
    // so what is being asserted is the merge: the closed flag followed the header one line down.
    let closed_headers: Vec<usize> = closed.iter().map(|[s, _]| *s).collect();
    assert_eq!(
        closed_headers.len(),
        1,
        "exactly one fold is still closed: {closed:?}"
    );
    // Which header it is depends on where the indent and brace detectors agree after the shift;
    // what must not change is that the closed flag survived and still names the second function's
    // block rather than the first one's.
    assert!(closed_headers[0] >= 5, "still the second block: {closed:?}");
}

#[test]
fn folds_and_bookmarks_persist_through_a_session_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f.rs");
    std::fs::write(&path, BRACES).expect("write");
    let mut app = app();
    app.session_path = Some(dir.path().join("session.toml"));
    let loaded = np_core::load(&path, None).expect("load");
    app.open_document(np_core::Document::from_loaded(&loaded));
    app.doc_mut().expect("doc").toggle_fold_at(0);
    app.doc_mut().expect("doc").bookmarks = [7usize].into_iter().collect();
    let (session, _) = app.capture_session();
    session
        .save(&dir.path().join("session.toml"))
        .expect("save session");

    let mut second = App::new_headless(Config::default(), (W, H));
    second.recompute_geometry(W, H);
    second.session_path = Some(dir.path().join("session.toml"));
    let loaded = np_core::Session::load(&dir.path().join("session.toml")).expect("load");
    second.restore_session(&loaded);
    let doc = second.doc().expect("restored");
    assert!(
        doc.folds.iter().any(|f| f.start == 0 && f.collapsed),
        "the collapsed fold came back: {:?}",
        doc.folds
    );
    assert!(doc.bookmarks.contains(&7), "and the bookmark");
    let rows: Vec<usize> = doc
        .visible_rows(&second.geometry)
        .iter()
        .map(|r| r.line)
        .collect();
    assert!(!rows.contains(&1), "and the view is still folded");
}
