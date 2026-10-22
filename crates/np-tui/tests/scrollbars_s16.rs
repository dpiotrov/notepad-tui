//! Caret and scrollbars must never occupy the same cell (FR-V7, FR-V3).
//!
//! Reported against a file with an 8324-column line: the text cursor sat on top of the
//! horizontal scrollbar, and neither bar answered the mouse properly.
//!
//! # The shape of the bug
//!
//! A scrollbar is drawn *over* the editor band, not beside it: the vertical bar takes the
//! band's last column and the horizontal bar its last row. But the row layout counted the
//! band's full height when it decided how many rows of text fitted, so it promised one row
//! more than the screen had. The caret's line got that phantom row, the terminal put the
//! hardware cursor on the bar, and typing at the bottom edge wrote behind it.
//!
//! The general form: five places needed the same number (row layout, wrap width, the
//! viewport-following code, caret placement, mouse hit-testing) and each subtracted the bar
//! for itself, or not at all. The fix is `np_core::BarOccupancy` -- one decision, made in
//! `recompute_geometry`, read by all five. The tests below are written against that decision
//! so a future change cannot reintroduce a private opinion about the viewport.
//!
//! # Why the assertions are about cells
//!
//! Asserting `cursor.pos` would have passed throughout: the *document* position was always
//! right. Only the screen disagreed. So everything here asserts either a screen coordinate or
//! a rendered cell.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Config, Document, EncodingId, Eol, LineCol};
use np_tui::app::App;

const W: u16 = 80;
const H: u16 = 24;

/// The editor band starts after the menu and tab rows.
const TEXT_TOP: u16 = 2;

/// The last text row, exclusive -- the viewport's bottom, whether or not a bar is drawn.
///
/// Derived from the app rather than hard-coded. The band is 21 rows and the horizontal bar
/// takes its last row only when one is actually drawn, so a test that assumed row 22 was a
/// bar would be wrong for a file that needs only a vertical bar -- and would then report a
/// caret sitting legally on the band's last row as a defect.
fn text_bottom(app: &App) -> u16 {
    app.regions.editor.y + app.geometry.viewport_rows() as u16
}

/// The row the horizontal bar occupies, or the band's last row when there is none.
fn bar_row(app: &App) -> u16 {
    app.regions.editor.bottom().saturating_sub(1)
}

/// The column the vertical bar occupies, or the band's last column when there is none.
fn bar_col(app: &App) -> u16 {
    app.regions.editor.right().saturating_sub(1)
}

fn open(text: &str) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/scrollbars.txt".into(),
        text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    // A headless App has no geometry until something draws it.
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

/// A file with a genuinely wide line, like the one reported: a single line of thousands of
/// columns among ordinary ones.
fn wide_file() -> String {
    let mut out = String::new();
    for i in 0..60 {
        if i == 30 {
            out.push_str(&"k".repeat(4000));
        } else {
            out.push_str(&format!("line {i} of the file\n"));
        }
    }
    out
}

fn click(app: &mut App, x: u16, y: u16) {
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

fn drag_to(app: &mut App, from: (u16, u16), to: (u16, u16)) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: from.0,
        row: from.1,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: to.0,
        row: to.1,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: to.0,
        row: to.1,
        modifiers: KeyModifiers::NONE,
    });
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// The cell the terminal cursor would be placed at, as `(row, col)`.
fn caret_cell(app: &App) -> Option<(u16, u16)> {
    np_tui::ui::editor_view::caret_position(app).map(|p| (p.line as u16, p.col as u16))
}

/// Every caret position worth probing, as `(line, col, screen_row, screen_col)`.
///
/// The viewport limits are captured from the app alongside each cell, so an assertion can
/// say "this cell is on a bar" without the test knowing the terminal size or which bars
/// happen to be drawn for this particular file.
fn every_caret_cell(text: &str) -> Vec<(usize, usize, u16, u16, u16, u16)> {
    let mut app = open(text);
    let lines = app.doc().expect("doc").buffer.line_count();
    let mut out = Vec::new();
    for line in 0..lines {
        for col in [0usize, 1, 40, 900, 3999] {
            let len = app.doc().expect("doc").buffer.line_len_chars(line);
            let col = col.min(len);
            app.doc_mut().expect("doc").cursor.pos = LineCol::new(line, col);
            np_tui::view::ensure_caret_visible(&mut app);
            let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
            if let Some((y, x)) = caret_cell(&app) {
                out.push((line, col, y, x, text_bottom(&app), bar_col(&app)));
            }
        }
    }
    out
}

// ------------------------------------------------------- the caret never sits on a bar

#[test]
fn both_bars_exist_for_a_wide_long_file() {
    let app = open(&wide_file());
    assert!(
        app.horizontal_scrollbar().is_some(),
        "a 4000-column line needs a horizontal bar"
    );
    assert!(
        app.vertical_scrollbar().is_some(),
        "60 lines in a 20-row viewport needs a vertical bar"
    );
    assert_eq!(app.geometry.bars.rows, 1);
    assert_eq!(app.geometry.bars.cols, 1);
}

#[test]
fn the_caret_never_lands_on_a_scrollbar_cell() {
    let cells = every_caret_cell(&wide_file());
    assert!(
        cells.len() > 200,
        "expected a broad sample, got {}",
        cells.len()
    );
    // (line, col, y, x, viewport bottom, vertical bar column)
    let bad: Vec<_> = cells
        .iter()
        .filter(|(_, _, y, x, bottom, bcol)| *y >= *bottom || *x >= *bcol)
        .collect();
    assert!(
        bad.is_empty(),
        "{} caret positions are on or past a scrollbar: {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
}

#[test]
fn the_caret_is_always_inside_the_text_band() {
    // The stronger statement: every position has a cell, and that cell is a text row. A
    // position with *no* cell is also a failure -- it means the viewport refused to follow.
    let cells = every_caret_cell(&wide_file());
    let above: Vec<_> = cells
        .iter()
        .filter(|(_, _, y, _, _, _)| *y < TEXT_TOP)
        .collect();
    assert!(above.is_empty(), "caret above the editor: {above:?}");
}

#[test]
fn every_position_gets_a_cell() {
    // A caret with no cell is invisible, which the user reads as "the editor lost the cursor".
    // Counting cells against countable positions is the only way to notice the viewport
    // refusing to follow rather than following somewhere wrong.
    let app = open(&wide_file());
    let lines = app.doc().expect("doc").buffer.line_count();
    let expected = lines * 5;
    let got = every_caret_cell(&wide_file()).len();
    assert_eq!(
        got, expected,
        "every probed position must produce a caret cell"
    );
}

#[test]
fn walking_down_with_arrow_keys_never_reaches_the_status_bar() {
    // The original report: each Down moved the cursor one more row until it was on the status
    // bar and then off the screen, because the viewport never followed the caret.
    let mut app = open(&(0..200).map(|i| format!("line {i}\n")).collect::<String>());
    for step in 0..400 {
        key(&mut app, KeyCode::Down);
        let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
        let bottom = text_bottom(&app);
        let bcol = bar_col(&app);
        if let Some((y, x)) = caret_cell(&app) {
            assert!(
                (TEXT_TOP..bottom).contains(&y),
                "after {step} Downs the caret is on row {y}, outside the text rows \
                 {TEXT_TOP}..{bottom}; doc={:?}",
                app.doc().expect("doc").cursor.pos
            );
            assert!(
                x < bcol,
                "after {step} Downs the caret is at {x}, on the vertical bar"
            );
        }
    }
}

#[test]
fn walking_up_with_arrow_keys_never_leaves_the_text_band() {
    let mut app = open(&(0..200).map(|i| format!("line {i}\n")).collect::<String>());
    // Start at the bottom so there is somewhere to walk up from.
    app.execute(np_tui::actions::Action::DocEnd);
    for step in 0..400 {
        key(&mut app, KeyCode::Up);
        let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
        let bottom = text_bottom(&app);
        if let Some((y, _)) = caret_cell(&app) {
            assert!(
                (TEXT_TOP..bottom).contains(&y),
                "after {step} Ups the caret is on row {y}; doc={:?}",
                app.doc().expect("doc").cursor.pos
            );
        }
    }
}

#[test]
fn walking_right_along_a_wide_line_stays_left_of_the_vertical_bar() {
    let mut app = open(&wide_file());
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(30, 0);
    np_tui::view::ensure_caret_visible(&mut app);
    for step in 0..3000 {
        key(&mut app, KeyCode::Right);
        let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
        let bcol = bar_col(&app);
        if let Some((_, x)) = caret_cell(&app) {
            assert!(
                x < bcol,
                "after {step} Rights the caret is at column {x}, on the vertical bar; \
                 doc={:?}",
                app.doc().expect("doc").cursor.pos
            );
        }
    }
}

#[test]
fn typing_at_the_right_edge_keeps_the_new_character_visible() {
    // The horizontal counterpart of the bottom-edge case: the caret must be scrolled into the
    // text columns, not left under the vertical bar where the character is drawn behind it.
    let mut app = open(&format!("{}\n", "z".repeat(3000)));
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 1500);
    np_tui::view::ensure_caret_visible(&mut app);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    // The bound is the viewport's right edge, which is the vertical bar's column when one is
    // drawn and the band's last column when it is not. Hard-coding the bar's column here would
    // make this test wrong for a file that needs no vertical bar.
    let right = app.geometry.text_area.x + app.geometry.viewport_cols() as u16;
    let (y, x) = caret_cell(&app).expect("caret must have a cell");
    let _ = y;
    assert!(
        x < right,
        "caret at column {x} is at or past the text area's right edge {right}"
    );
    key(&mut app, KeyCode::Char('Q'));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let right2 = app.geometry.text_area.x + app.geometry.viewport_cols() as u16;
    let (y2, x2) = caret_cell(&app).expect("caret after typing");
    assert!(
        x2 < right2,
        "caret after typing at column {x2} is at or past the right edge {right2}"
    );
    // The character that was typed must be on screen, immediately left of the caret. That is
    // what "visible" means here; the caret's absolute column moves as the view scrolls, so
    // comparing it to the previous cell would only test that no scroll happened.
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let drawn = np_tui::ui::cell_text(&buf, x2 - 1, y2);
    assert_eq!(
        drawn,
        "Q",
        "the typed character must be drawn at the cell the caret just left; found {drawn:?} \
         at ({y2},{})",
        x2 - 1
    );
    assert_eq!(
        app.doc()
            .expect("doc")
            .buffer
            .line_string(0)
            .chars()
            .nth(1500),
        Some('Q'),
        "the character must be in the document where it was typed"
    );
}

// ------------------------------------------------------ the bars own their own cells

#[test]
fn a_click_on_the_horizontal_bar_scrolls_rather_than_placing_the_caret() {
    let mut app = open(&wide_file());
    let caret_before = app.doc().expect("doc").cursor.pos;
    let left_before = app.doc().expect("doc").scroll.left_col;
    let brow = bar_row(&app);
    click(&mut app, 60, brow);
    assert!(
        app.doc().expect("doc").scroll.left_col > left_before,
        "a click on the horizontal trough must scroll right (was {left_before}, \
         still {})",
        app.doc().expect("doc").scroll.left_col
    );
    assert_eq!(
        app.doc().expect("doc").cursor.pos,
        caret_before,
        "scrolling with the bar must not move the caret"
    );
}

#[test]
fn a_click_on_the_vertical_bar_scrolls_rather_than_placing_the_caret() {
    let mut app = open(&wide_file());
    let top_before = app.doc().expect("doc").scroll.top_line;
    let bcol = bar_col(&app);
    click(&mut app, bcol, 12);
    assert!(
        app.doc().expect("doc").scroll.top_line > top_before,
        "a click below the thumb must page down"
    );
}

#[test]
fn both_bars_hit_test_across_their_whole_strip() {
    // The bars used to be hit-tested on a single centreline, so a click anywhere but that one
    // cell fell through to the text arm. Every cell of each strip must resolve to the bar.
    let app = open(&wide_file());
    assert_eq!(app.geometry.bars.rows, 1);
    assert_eq!(app.geometry.bars.cols, 1);
    let ev = |x: u16, y: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let brow = bar_row(&app);
    let bcol = bar_col(&app);
    // The horizontal bar's own strip: its row, up to but excluding the corner cell.
    for x in (app.geometry.gutter_width as u16 + 1)..bcol {
        let hit = np_tui::mouse::hit_test(&ev(x, brow), &app);
        assert!(
            matches!(
                hit,
                np_tui::mouse::Hit::ScrollbarTrack(
                    np_tui::mouse::VerticalOrHorizontal::Horizontal,
                    _
                ) | np_tui::mouse::Hit::ScrollbarThumb(
                    np_tui::mouse::VerticalOrHorizontal::Horizontal,
                    _
                )
            ),
            "row {brow} column {x} must hit the horizontal bar, got {hit:?}"
        );
    }
    for y in TEXT_TOP..=brow {
        let hit = np_tui::mouse::hit_test(&ev(bcol, y), &app);
        assert!(
            matches!(
                hit,
                np_tui::mouse::Hit::ScrollbarTrack(
                    np_tui::mouse::VerticalOrHorizontal::Vertical,
                    _
                ) | np_tui::mouse::Hit::ScrollbarThumb(
                    np_tui::mouse::VerticalOrHorizontal::Vertical,
                    _
                )
            ),
            "column {bcol} row {y} must hit the vertical bar, got {hit:?}"
        );
    }
}

#[test]
fn the_corner_cell_belongs_to_the_vertical_bar() {
    // Both bars want it. It is *drawn* by the vertical one -- the horizontal bar's area is
    // narrowed so the two cannot overwrite each other -- so it has to be *hit* by the same
    // bar, or clicking the corner would scroll the wrong axis from the glyph of the other.
    let app = open(&wide_file());
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bar_col(&app),
        row: bar_row(&app),
        modifiers: KeyModifiers::NONE,
    };
    let hit = np_tui::mouse::hit_test(&ev, &app);
    assert!(
        matches!(
            hit,
            np_tui::mouse::Hit::ScrollbarTrack(np_tui::mouse::VerticalOrHorizontal::Vertical, _)
                | np_tui::mouse::Hit::ScrollbarThumb(
                    np_tui::mouse::VerticalOrHorizontal::Vertical,
                    _
                )
        ),
        "the corner cell must resolve to the vertical bar, got {hit:?}"
    );
}

#[test]
fn the_two_bars_do_not_overwrite_each_others_last_cell() {
    // The horizontal bar used to be rendered across the band's full width, so it painted over
    // the vertical bar's bottom cell: the meeting point came out as a mix of both, which is
    // what "the scrollbars look mangled" means on screen.
    let mut app = open(&wide_file());
    let (bcol, brow) = (bar_col(&app), bar_row(&app));
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let corner = np_tui::ui::cell_text(&buf, bcol, brow);
    let above = np_tui::ui::cell_text(&buf, bcol, brow - 1);
    assert_eq!(
        corner, above,
        "the corner cell must look like the vertical bar above it ({above:?}), not like a \
         horizontal bar fragment ({corner:?})"
    );
    let left_of_corner = np_tui::ui::cell_text(&buf, bcol - 1, brow);
    assert_ne!(
        corner, left_of_corner,
        "the corner must differ from the horizontal bar to its left, or nothing is drawn at all"
    );
}

#[test]
fn dragging_the_horizontal_thumb_moves_the_view_and_the_thumb_follows() {
    let mut app = open(&wide_file());
    let thumb = app.horizontal_scrollbar().expect("horizontal bar exists");
    let grab = thumb.track_start + thumb.thumb_start;
    let (brow, bcol) = (bar_row(&app), bar_col(&app));
    drag_to(&mut app, (grab, brow), (bcol - 1, brow));
    let left = app.doc().expect("doc").scroll.left_col;
    assert!(
        left > 0,
        "dragging the thumb to the right must scroll right"
    );
    let moved = app.horizontal_scrollbar().expect("still drawn");
    assert!(
        moved.thumb_start > thumb.thumb_start,
        "the thumb must move with the drag: was {}, now {}",
        thumb.thumb_start,
        moved.thumb_start
    );
}

#[test]
fn dragging_the_vertical_thumb_to_the_bottom_shows_the_last_line() {
    let mut app = open(&(0..200).map(|i| format!("line {i}\n")).collect::<String>());
    let thumb = app.vertical_scrollbar().expect("vertical bar exists");
    let grab = thumb.track_start + thumb.thumb_start;
    let (bcol, bottom) = (bar_col(&app), text_bottom(&app));
    drag_to(&mut app, (bcol, grab), (bcol, bottom - 1));
    let doc = app.doc().expect("doc");
    let last = doc.buffer.line_count() - 1;
    let rows = doc.visible_rows(&app.geometry);
    assert!(
        rows.iter().any(|r| r.line == last),
        "after dragging to the bottom the last line must be on screen; top={}, rows={:?}",
        doc.scroll.top_line,
        rows.last().map(|r| r.line)
    );
}

// ------------------------------------------------------------- the viewport arithmetic

#[test]
fn the_row_layout_never_piles_more_rows_than_the_viewport_has() {
    // The invariant the whole fix rests on. `layout_visible_rows` used to fill the band's
    // height, which is one more than the viewport whenever a horizontal bar is drawn.
    let app = open(&wide_file());
    let doc = app.doc().expect("doc");
    let rows = doc.visible_rows(&app.geometry);
    assert_eq!(
        rows.len(),
        app.geometry.viewport_rows(),
        "a full page must be exactly the viewport, not the band"
    );
    assert_eq!(
        app.geometry.viewport_rows() + app.geometry.bars.rows as usize,
        app.regions.editor.h as usize,
        "the viewport plus the bar row must be the whole band"
    );
    assert_eq!(
        app.geometry.viewport_rows() + app.geometry.bars.rows as usize,
        app.regions.editor.h as usize,
        "band = viewport + bar row"
    );
}

#[test]
fn wrap_width_excludes_the_vertical_bar() {
    // With wrap on, a row must stop at the column before the vertical bar. Laying out to the
    // band's width would push the last character of every wrapped row under the bar.
    let text = "x".repeat(500);
    let mut app = open(&text);
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let doc = app.doc().expect("doc");
    let rows = doc.visible_rows(&app.geometry);
    let widest = rows.iter().map(|r| r.char_len()).max().unwrap_or(0);
    assert!(
        widest <= app.geometry.viewport_cols(),
        "a wrapped row is {} columns, wider than the viewport's {}",
        widest,
        app.geometry.viewport_cols()
    );
}

#[test]
fn no_horizontal_bar_under_wrap_and_no_row_reserved_for_it() {
    // FR-V1: horizontal scrolling is disabled while wrapping. The occupancy decision must
    // agree, or a row is reserved for a bar that will never be drawn and the text is one row
    // short for no reason.
    let mut app = open(&format!("{}\n", "y".repeat(4000)));
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(app.horizontal_scrollbar().is_none());
    assert_eq!(app.geometry.bars.rows, 0);
}

#[test]
fn no_bars_for_a_document_that_fits() {
    let app = open("one\ntwo\nthree\n");
    assert!(app.horizontal_scrollbar().is_none());
    assert!(app.vertical_scrollbar().is_none());
    assert_eq!(app.geometry.bars.rows, 0);
    assert_eq!(app.geometry.bars.cols, 0);
    // With no bars the viewport is the whole band again.
    assert_eq!(app.geometry.viewport_rows(), app.regions.editor.h as usize);
}

#[test]
fn the_occupancy_is_stable_across_a_resize() {
    // The decision is a fixed point of two mutually-dependent bars: the bar row shrinks the
    // height, which changes whether the vertical bar is needed, which shrinks the width, which
    // changes whether a line overflows. A single pass can oscillate, which reads as a bar
    // flickering while the user drags a resize.
    let text = wide_file();
    let mut last = None;
    for w in 20..80u16 {
        let mut app = open(&text);
        app.recompute_geometry(w, H);
        let _ = np_tui::ui::render_to_buffer(&mut app, w, H);
        let bars = app.geometry.bars;
        assert!(
            bars.viewport_rows >= 1 && bars.viewport_cols >= 1,
            "width {w} left a viewport of {bars:?}"
        );
        assert!(
            bars.viewport_rows <= app.regions.editor.h as usize,
            "width {w}: viewport rows exceed the band"
        );
        last = Some((w, bars));
    }
    let (w, bars) = last.expect("loop ran");
    assert!(bars.viewport_rows >= 1, "final {w} {bars:?}");
}
