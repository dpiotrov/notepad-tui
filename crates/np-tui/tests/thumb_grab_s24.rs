//! S24: grabbing the vertical scrollbar thumb must not teleport the document.
//!
//! Reported as: "vertical scrollbar jumps when I try to click and move it by mouse. On click it
//! jumps upside and move with increased offset upper than mouse pointer. Horizontal scrollbar
//! work good."
//!
//! # Why the vertical bar jumped and the horizontal one seemed fine
//!
//! Both bars resolved a press the same way: the pressed *cell* stood for a proportional position
//! in the document, and the view went there. On the horizontal bar of a typical file that ratio is
//! close to 1:1, so the error is a column or two and invisible. The vertical bar of a long file is
//! nothing like 1:1 -- a 2000-line document in a 21-row viewport gives a thumb of **one cell** on a
//! trough of 21, so one cell is worth about 95 lines. Grabbing that thumb aimed the view at the
//! cell's proportion and yanked the document up to 84 lines, four screens, before the mouse had
//! moved. The pointer then sat below a thumb that had shot upward: exactly "jumps upside, moves
//! with an increased offset upper than the pointer".
//!
//! So the horizontal bar was not more correct. It was the same rule with a scale too fine to see.
//!
//! # The two gestures, and why they cannot share one rule
//!
//!  - **A press on the thumb holds.** The document does not move; the drag that follows moves it
//!    by the pointer's *travel*. This is what a grab means, and it is what every desktop scrollbar
//!    does.
//!  - **A press in the trough aims.** The cell stands for a position, so one click near the bottom
//!    of a long file goes near the bottom, and a drag from the trough still reaches either end.
//!
//! S17 deliberately made a thumb press aim, on the argument that a press which does nothing feels
//! inert and the view then lurches on the first drag event. That argument assumed the drag was
//! absolute too. With a travel-based drag the press is *meant* to do nothing: the hold and the
//! motion are two halves of one gesture.
//!
//! # What the tests refuse to accept
//!
//! Monotonicity is asserted as strictly as the no-jump rule. Two rounding bugs were found while
//! writing the fix and both show up only as a backwards step in a drag sequence -- 100, 109, 108
//! when the displacement was rounded on its own, and 100, 14 when the sum was floored and the
//! anchor's residue thrown away. A test that only checked "does not jump on press" passed against
//! both.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::app::{row_offset_of_top, App};

const W: u16 = 80;
const H: u16 = 24;

fn open(text: &str) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.documents.clear();
    app.active = 0;
    app.open_document(Document::from_decoded(
        "/tmp/thumb_grab.txt".into(),
        text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn lines(n: usize) -> String {
    (0..n).map(|i| format!("line {i:05}\n")).collect()
}

fn down(app: &mut App, x: u16, y: u16) {
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

fn drag(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn vbar_x(app: &App) -> u16 {
    app.regions.editor.right() - 1
}

fn top_offset(app: &App) -> usize {
    row_offset_of_top(app.doc().expect("doc"), &app.geometry)
}

fn set_top(app: &mut App, line: usize) {
    app.doc_mut().expect("doc").scroll.top_line = line;
    let _ = np_tui::ui::render_to_buffer(app, W, H);
}

/// The screen row of the thumb's first cell, re-read after `set_top` so the geometry is current.
fn thumb_row(app: &App) -> u16 {
    let bar = app
        .vertical_scrollbar()
        .expect("the vertical bar must be drawn");
    bar.track_start + bar.thumb_start
}

// ------------------------------------------------------ the press must not teleport anything

#[test]
fn pressing_the_thumb_does_not_move_the_document() {
    // The headline defect. A long file, so the thumb is one cell and the old proportional aim
    // moved the view by up to four screens.
    let mut app = open(&lines(2000));
    for start in [0usize, 500, 1000, 1900] {
        set_top(&mut app, start);
        let before = top_offset(&app);
        let y = thumb_row(&app);
        let x = vbar_x(&app);
        down(&mut app, x, y);
        assert_eq!(
            top_offset(&app),
            before,
            "grabbing the thumb at row {y} moved the view from {before} -- a grab must hold"
        );
    }
}

#[test]
fn the_test_premise_holds_that_aiming_at_the_cell_would_move_a_lot() {
    // A guard on the test above. If the thumb's own cell happened to resolve to the offset the
    // view already had, `pressing_the_thumb_does_not_move_the_document` would pass while proving
    // nothing -- the S17 lesson about a test that cannot fail.
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let bar = app.vertical_scrollbar().expect("bar");
    let y = bar.track_start + bar.thumb_start;
    // What the old rule computed: the pressed cell's proportional position.
    let span = (bar.track_len as i64 - 1).max(1);
    let max = app.max_viewport_row_offset() as i64;
    let aimed = (((y - bar.track_start) as i64) * max + span / 2) / span;
    let cell_worth = max / span;
    assert!(
        cell_worth >= 20,
        "premise: one trough cell must be worth many lines here, is {cell_worth}"
    );
    assert_ne!(
        aimed as usize, 1000,
        "premise: aiming at the thumb's cell must disagree with where the view is (both {aimed})"
    );
}

#[test]
fn pressing_the_thumb_does_not_move_it_either() {
    // The thumb is drawn from the view's offset, so holding the view holds the thumb. Asserted
    // separately because the user's words were about the *bar* jumping, not only the text.
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let before = app.vertical_scrollbar().expect("bar").thumb_start;
    let y = app.vertical_scrollbar().expect("bar").track_start + before;
    let x = vbar_x(&app);
    down(&mut app, x, y);
    assert_eq!(
        app.vertical_scrollbar().expect("bar").thumb_start,
        before,
        "the thumb slid under the pointer on contact"
    );
}

// ---------------------------------------------------------- the drag follows the pointer

#[test]
fn dragging_the_thumb_moves_the_view_in_the_direction_of_travel() {
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let before = top_offset(&app);
    let y = thumb_row(&app);
    let x = vbar_x(&app);
    down(&mut app, x, y);
    drag(&mut app, x, y + 1);
    assert!(
        top_offset(&app) > before,
        "a thumb dragged down must move the view down: {before} -> {}",
        top_offset(&app)
    );
}

#[test]
fn dragging_the_thumb_upwards_moves_the_view_upwards() {
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let before = top_offset(&app);
    let y = thumb_row(&app);
    let x = vbar_x(&app);
    down(&mut app, x, y);
    drag(&mut app, x, y.saturating_sub(2));
    assert!(
        top_offset(&app) < before,
        "a thumb dragged up must move the view up: {before} -> {}",
        top_offset(&app)
    );
}

#[test]
fn a_drag_event_at_the_grab_cell_changes_nothing() {
    // A terminal reports a drag event for the cell the button is already on, so a grab followed by
    // `Drag` at the same row is an ordinary thing to receive -- and the arithmetic case that has
    // to hold. Converting the travel to offsets and rounding the *sum* rather than the
    // displacement puts a bias on the anchor as well, and this is the only event that shows it:
    // the anchor of 100 answers 104, and the very first form of this fix moved the view ~100 rows
    // on contact through exactly this path.
    //
    // The press tests cannot see it, because a press records the grab without consulting the
    // scale, and the monotone drags cannot see it, because they start at one cell of travel.
    for (name, n) in [("short", 200usize), ("long", 2000)] {
        let mut app = open(&lines(n));
        set_top(&mut app, n / 2);
        let before = top_offset(&app);
        let x = vbar_x(&app);
        let y = thumb_row(&app);
        down(&mut app, x, y);
        drag(&mut app, x, y);
        assert_eq!(
            top_offset(&app),
            before,
            "{name} file: a drag event on the grab cell moved the view from {before}"
        );
    }
}

#[test]
fn a_thumb_drag_is_monotone_in_the_pointers_position() {
    // The rounding guard. Both rounding bugs found while writing the fix were invisible except as
    // a backwards step: 100, 109, 108 when the displacement was rounded alone, and 100, 14 when
    // the sum was floored. Neither is detectable from a single drag event.
    let mut app = open(&lines(200));
    set_top(&mut app, 100);
    let y = thumb_row(&app);
    let x = vbar_x(&app);
    down(&mut app, x, y);
    let mut previous = top_offset(&app);
    let mut seen = vec![previous];
    for dy in 1..=8u16 {
        drag(&mut app, x, y + dy);
        let now = top_offset(&app);
        seen.push(now);
        assert!(
            now >= previous,
            "drag cell {dy} moved the view backwards: {seen:?}"
        );
        previous = now;
    }
}

#[test]
fn a_thumb_drag_backwards_is_also_monotone() {
    let mut app = open(&lines(200));
    set_top(&mut app, 100);
    let y = thumb_row(&app);
    let x = vbar_x(&app);
    down(&mut app, x, y);
    let mut previous = top_offset(&app);
    let mut seen = vec![previous];
    for dy in 1..=8u16 {
        drag(&mut app, x, y.saturating_sub(dy));
        let now = top_offset(&app);
        seen.push(now);
        assert!(
            now <= previous,
            "dragging up, cell {dy} moved the view down: {seen:?}"
        );
        previous = now;
    }
}

#[test]
fn a_thumb_drag_still_reaches_the_end_of_the_document() {
    // The reason S17 abandoned travel-based dragging was reach: a delta model caps the view at
    // how far the pointer can travel. Anchoring to the *offset* rather than to the pressed cell
    // avoids that, because the pointer can travel the whole trough and the ends are answered
    // directly. This is the test that says the reason is still answered.
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let max = app.max_viewport_row_offset();
    let x = vbar_x(&app);
    let bottom = app.regions.editor.bottom() - 1;
    let y = thumb_row(&app);
    down(&mut app, x, y);
    for row in y..=bottom {
        drag(&mut app, x, row);
    }
    for _ in 0..3 {
        drag(&mut app, x, bottom);
    }
    assert_eq!(
        top_offset(&app),
        max,
        "a thumb dragged to the bottom of the trough must reach the last page"
    );
}

#[test]
fn a_thumb_drag_still_reaches_the_top_of_the_document() {
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let x = vbar_x(&app);
    let top = app.vertical_scrollbar().expect("bar").track_start;
    let y = thumb_row(&app);
    down(&mut app, x, y);
    for row in (top..=y).rev() {
        drag(&mut app, x, row);
    }
    for _ in 0..3 {
        drag(&mut app, x, top);
    }
    assert_eq!(top_offset(&app), 0, "dragged to the top must land on row 0");
}

// ----------------------------------------------- the trough keeps its aim, and its reach

#[test]
fn pressing_the_trough_still_aims_at_the_position_it_stands_for() {
    // The other half of the split. A press below the thumb in a long file must go a long way --
    // that is what makes a trough click useful, and a "hold everything" fix would kill it.
    let mut app = open(&lines(2000));
    set_top(&mut app, 0);
    let bar = app.vertical_scrollbar().expect("bar");
    let below = bar.track_start + bar.track_len - 2;
    assert!(!bar.is_thumb(below), "premise: the press is in the trough");
    let x = vbar_x(&app);
    down(&mut app, x, below);
    assert!(
        top_offset(&app) > app.max_viewport_row_offset() / 2,
        "a press near the bottom of the trough must aim past the middle, went to {}",
        top_offset(&app)
    );
}

#[test]
fn a_trough_drag_still_reaches_both_ends() {
    // S17's monotone-reach tests press in the trough, not on the thumb. Those must stay absolute,
    // and this is the shorter restatement of why the split is by grab site rather than global.
    let mut app = open(&lines(200));
    let max = app.max_viewport_row_offset();
    let bar = app.vertical_scrollbar().expect("bar");
    let x = vbar_x(&app);
    let start = bar.track_start;
    let len = bar.track_len;

    down(&mut app, x, start + len / 2);
    for cell in (len / 2)..len {
        drag(&mut app, x, start + cell);
    }
    release(&mut app, x, start + len - 1);
    assert_eq!(
        top_offset(&app),
        max,
        "a trough drag down must reach the end"
    );

    down(&mut app, x, start + len - 1);
    for cell in (0..len).rev() {
        drag(&mut app, x, start + cell);
    }
    release(&mut app, x, start);
    assert_eq!(top_offset(&app), 0, "a trough drag up must reach the top");
}

#[test]
fn a_trough_drag_lands_where_the_pointer_is_not_where_the_view_was() {
    // What actually distinguishes an aiming trough from a travel-based one. Both reach the ends,
    // so the reach tests pass either way -- measured as 15/15 green with the trough's absolute
    // mapping replaced by the thumb's relative one.
    //
    // The difference is the *middle*: a trough press aims at the pressed cell, so releasing the
    // drag half way down the trough must leave the view half way through the document regardless
    // of where it started. A relative drag would instead move it by the cells travelled from an
    // anchor, and a view starting at the top would land somewhere else entirely.
    let mut app = open(&lines(2000));
    set_top(&mut app, 0);
    let max = app.max_viewport_row_offset();
    let bar = app.vertical_scrollbar().expect("bar");
    let x = vbar_x(&app);
    let mid = bar.track_start + bar.track_len / 2;
    assert!(
        !bar.is_thumb(mid),
        "premise: the middle of the trough is not the thumb"
    );

    down(&mut app, x, mid);
    release(&mut app, x, mid);
    let aimed = top_offset(&app);
    // Half way down a 1980-offset trough is roughly half way through the document.
    assert!(
        aimed > max / 3 && aimed < (max * 2) / 3,
        "a press in the middle of the trough must aim near the middle, went to {aimed} of {max}"
    );

    let lower = bar.track_start + bar.track_len - 3;
    assert!(
        !bar.is_thumb(lower),
        "premise: the lower cell is in the trough"
    );
    drag(&mut app, x, lower);
    let moved_to = top_offset(&app);
    assert!(
        moved_to > aimed + max / 4,
        "dragging the pointer down the trough must aim at the new cell: {aimed} -> {moved_to}"
    );

    // One trough cell means one position, whatever the view was doing beforehand.
    let mut other = open(&lines(2000));
    set_top(&mut other, max);
    let x2 = vbar_x(&other);
    down(&mut other, x2, lower);
    assert_eq!(
        top_offset(&other),
        moved_to,
        "one trough cell must mean one position, whatever the view was doing beforehand"
    );
}

#[test]
fn a_trough_grab_anchors_on_the_cell_and_a_thumb_grab_on_the_view() {
    // The one place the two models are provably different, and it is not the drag.
    //
    // Feeding a trough drag through the thumb's relative rule is an *equivalent* mutation and no
    // test can catch it: a trough press records the pressed cell's own aimed offset as its anchor,
    // so `anchor + travel` reproduces `offset_at_cell` exactly. Verified numerically -- cells 10,
    // 14, 16 and 18 give 990, 1386, 1584 and 1782 under both formulas.
    //
    // The split therefore lives entirely in the *press*: a thumb press refuses to aim and anchors
    // on the view's current offset, a trough press aims and anchors on the cell. This asserts that
    // the two presses record different anchors, which is the whole of the difference.
    let mut trough = open(&lines(2000));
    set_top(&mut trough, 0);
    let bar = trough.vertical_scrollbar().expect("bar");
    let x = vbar_x(&trough);
    // A trough cell far from the view's current position.
    let cell = bar.track_start + bar.track_len - 3;
    assert!(!bar.is_thumb(cell), "premise: not the thumb");
    down(&mut trough, x, cell);
    let trough_grab = trough
        .scrollbar_drag
        .expect("a trough press records a grab");
    assert!(
        !trough_grab.on_thumb,
        "a trough press must record a trough grab"
    );
    assert!(
        trough_grab.start_offset > 0,
        "a trough press anchors on the cell it aimed at, not on the view's 0"
    );

    let mut thumb = open(&lines(2000));
    set_top(&mut thumb, 1000);
    let y = thumb_row(&thumb);
    let x = vbar_x(&thumb);
    down(&mut thumb, x, y);
    let thumb_grab = thumb.scrollbar_drag.expect("a thumb press records a grab");
    assert!(thumb_grab.on_thumb, "a thumb press records a thumb grab");
    assert_eq!(
        thumb_grab.start_offset, 1000,
        "a thumb press anchors on where the view is, not on a cell's proportion"
    );
}

#[test]
fn the_grab_remembers_that_it_started_on_the_thumb() {
    // The mechanism, asserted directly. Rebuilding the grab with `on_thumb: false` after the first
    // drag event hands the second event to the absolute rule, and the drag then alternates between
    // two models -- measured as 100, 113, 108, 117.
    let mut app = open(&lines(2000));
    set_top(&mut app, 1000);
    let y = thumb_row(&app);
    let x = vbar_x(&app);
    down(&mut app, x, y);
    assert!(
        app.scrollbar_drag.map(|g| g.on_thumb).unwrap_or(false),
        "a thumb press must record a thumb grab"
    );
    drag(&mut app, x, y + 1);
    assert!(
        app.scrollbar_drag.map(|g| g.on_thumb).unwrap_or(false),
        "the grab must still say thumb after a drag event"
    );
    drag(&mut app, x, y + 2);
    assert!(
        app.scrollbar_drag.map(|g| g.on_thumb).unwrap_or(false),
        "and after the next one too"
    );
}

// ------------------------------------------------------------- the horizontal twin

#[test]
fn pressing_the_horizontal_thumb_does_not_move_the_view() {
    // The user read the horizontal bar as working, but it ran the same rule at a finer scale. It
    // gets the same fix, and the same assertion.
    let long_line = "x".repeat(4000);
    let mut app = open(&(long_line + "\n"));
    app.doc_mut().expect("doc").scroll.left_col = 900;
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let bar = app
        .horizontal_scrollbar()
        .expect("the horizontal bar must be drawn");
    let x = bar.track_start + bar.thumb_start;
    let y = app.regions.editor.bottom() - 1;
    let before = app.doc().expect("doc").scroll.left_col;
    down(&mut app, x, y);
    assert_eq!(
        app.doc().expect("doc").scroll.left_col,
        before,
        "grabbing the horizontal thumb moved the view from {before}"
    );
}

#[test]
fn dragging_the_horizontal_thumb_moves_by_travel_and_still_reaches_the_end() {
    let long_line = "x".repeat(4000);
    let mut app = open(&(long_line + "\n"));
    app.doc_mut().expect("doc").scroll.left_col = 900;
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let bar = app.horizontal_scrollbar().expect("bar");
    let x0 = bar.track_start + bar.thumb_start;
    // The track's last usable cell. The corner shared with the vertical bar hit-tests to the
    // vertical bar, so a horizontal drag stops one short of `track_start + track_len`.
    let right = bar.track_start + bar.track_len - 2;
    let y = app.regions.editor.bottom() - 1;
    let before = app.doc().expect("doc").scroll.left_col;
    down(&mut app, x0, y);
    drag(&mut app, x0 + 2, y);
    assert!(
        app.doc().expect("doc").scroll.left_col > before,
        "dragging the horizontal thumb right must scroll right"
    );
    // And the far end is still reachable from a thumb grab. The horizontal track's last cell is
    // the corner shared with the vertical bar, which hit-tests to the *vertical* bar, so the
    // gesture ends one cell short of `track_len` -- that is the real limit a user meets, and the
    // sticky-end rule has to work with it rather than against it.
    for x in x0..=right {
        drag(&mut app, x, y);
    }
    // Held at the end of the trough, as a terminal keeps reporting.
    for _ in 0..3 {
        drag(&mut app, right, y);
    }
    let max = app
        .doc()
        .expect("doc")
        .buffer
        .max_line_width(app.geometry.tab_width.max(1));
    let visible = app.geometry.viewport_cols();
    let reached = app.doc().expect("doc").scroll.left_col;
    // `scope.max_offset` for the horizontal bar is `content - 1 - visible`, one short of the
    // `content - visible` that `scroll_to_col` itself allows. That discrepancy is pre-existing --
    // reproduced on the baseline commit before this stage's changes -- and is recorded in
    // docs/STATUS.md rather than fixed here, because that ceiling is S17's and both bars are drawn
    // against it. What this test owns is that a thumb dragged to the trough's end lands on the
    // bar's own ceiling rather than stalling somewhere mid-line.
    assert_eq!(
        reached + visible,
        max - 1,
        "a horizontal drag to the trough's end must reach the bar's ceiling: \
         reached={reached} visible={visible} max={max}"
    );
}
