//! Screen geometry and the visible-row layout.
//!
//! This is the single authority on "what appears on which terminal row", which is what
//! makes mouse hit-testing and word-wrap navigation testable without a renderer. Render
//! code consumes [`layout_visible_rows`]; it must not compute rows itself.
//!
//! # The two transforms, and their order
//!
//! A logical line becomes zero, one or many terminal rows through two independent
//! transforms, applied in this order and in no other:
//!
//!  1. **Folding** removes lines. A collapsed [`FoldRange`] hides every line after its
//!     header, so those lines never reach the wrapper.
//!  2. **Wrapping** splits whatever survives into rows that fit the text area.
//!
//! Folding first is the only order that works. Wrap first would split a line into rows and
//! then hide *rows*, which is wrong twice over: the fold's unit is a logical line, and a
//! collapsed range whose header wraps would leave orphan continuation rows on screen. The
//! observable consequence is that fold arithmetic stays in line space and wrap arithmetic
//! stays in row space, and neither has to know the other exists.
//!
//! # Cost
//!
//! This runs on every keystroke and every resize, so it walks *rows*, not the document:
//! it starts at the fold-aware successor of `scroll_top_line` and stops once the viewport
//! is full. A million-line file costs the same as a ten-line one, which is the spec §5.4
//! requirement. Nothing is cached here — [`crate::Buffer::revision`] lets a caller key its
//! own cache if it wants one, and a row list this cheap to rebuild is not worth the
//! staleness class it would introduce.

use crate::buffer::{Buffer, RowCountCache};
use crate::types::LineCol;

/// A rectangle in terminal cells.
///
/// Core-local rather than `ratatui::layout::Rect` because `np-core` must not depend on
/// the UI crate. `np-tui` converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectCore {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl RectCore {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        RectCore { x, y, w, h }
    }

    pub const fn width(&self) -> u16 {
        self.w
    }

    pub const fn height(&self) -> u16 {
        self.h
    }

    /// Exclusive right edge.
    pub fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }

    /// Exclusive bottom edge.
    pub fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }

    /// True when `(px, py)` lies inside this rectangle. Coordinates outside are simply
    /// false, never an error.
    pub fn contains(&self, px: u16, py: u16) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// One row of this rectangle, `index` rows down from its top edge.
    ///
    /// A list that hit-tests per row needs the row's own rectangle, and computing it here
    /// keeps the arithmetic next to `contains`, which is what a click is finally resolved
    /// against. An index past the bottom yields a zero-height rectangle so it can never hit.
    pub fn row_slice(&self, index: u16) -> RectCore {
        if index >= self.h {
            return RectCore::new(self.x, self.y, self.w, 0);
        }
        RectCore::new(self.x, self.y + index, self.w, 1)
    }

    /// Shrink from all sides, clamped so a degenerate inset yields an empty rectangle
    /// at the origin corner rather than wrapping around.
    pub fn inset(&self, left: u16, right: u16, top: u16, bottom: u16) -> RectCore {
        let x = self.x.saturating_add(left);
        let y = self.y.saturating_add(top);
        let w = self.w.saturating_sub(left.saturating_add(right));
        let h = self.h.saturating_sub(top.saturating_add(bottom));
        RectCore { x, y, w, h }
    }
}

/// One terminal row's worth of a logical line.
///
/// Without wrap there is exactly one `VisualRow` per visible logical line, with
/// `start_col == 0` and `end_col == line_len_chars`. With wrap a long line yields
/// several consecutive rows that partition its characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    /// Char column where this screen row starts.
    pub start_col: usize,
    /// Char column where it ends, exclusive.
    pub end_col: usize,
}

impl VisualRow {
    pub fn whole(line: usize, len_chars: usize) -> Self {
        VisualRow {
            line,
            start_col: 0,
            end_col: len_chars,
        }
    }

    pub fn char_len(&self) -> usize {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Whether the character at `col` on this row is displayed here.
    ///
    /// A position equal to `end_col` belongs to this row only when the row is the last
    /// one of its logical line, so the caret can sit at end-of-line.
    pub fn holds(&self, col: usize, is_last_row_of_line: bool) -> bool {
        if col >= self.start_col && col < self.end_col {
            return true;
        }
        is_last_row_of_line && col == self.end_col
    }
}

/// How many cells of the editor band the scrollbars occupy, and therefore how much of it
/// is left for text.
///
/// # Why this is a type rather than two subtracted numbers
///
/// A scrollbar is drawn *over* the editor band: the vertical bar takes the band's last
/// column and the horizontal bar its last row. Nothing else in the layout knows that, so
/// every consumer that counts text rows or columns has to subtract the same one cell, and
/// there are five of them -- the row layout that decides what is visible, the wrap width,
/// the viewport-following code, the caret placement, and the mouse hit-test.
///
/// When each of those subtracted for itself, they disagreed. The row layout counted one
/// more row than the screen could show, so the caret reached a screen row that belonged to
/// the horizontal bar and the terminal put the cursor on top of it; the same happened on
/// the right-hand column. Making the answer a single value computed once, in the place the
/// geometry is computed, is what turns "five places must agree" into "one place decides".
///
/// # Existence is decided here, once
///
/// A bar only occupies cells when it is actually drawn, which means: not while wrapping
/// (there is no horizontal scroll at all, FR-V1), and not when the content fits the
/// viewport. Those two tests used to live in the scrollbar-geometry functions, which the
/// renderer and the hit-tester called but the layout did not -- so the layout had to guess,
/// and guessed wrong. Callers pass the two facts it cannot know (whether the content is
/// taller than the band, and whether any line is wider than the band) and get one answer
/// that every consumer then shares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BarOccupancy {
    /// 1 when the horizontal scrollbar is drawn, taking the editor band's bottom row.
    pub rows: u16,
    /// 1 when the vertical scrollbar is drawn, taking the band's rightmost column.
    pub cols: u16,
    /// Viewport height in rows: the band's height minus [`Self::rows`], at least 1.
    pub viewport_rows: usize,
    /// Viewport width in columns: the text area's width minus [`Self::cols`], at least 1.
    pub viewport_cols: usize,
}

impl BarOccupancy {
    /// Work out the occupancy for one laid-out editor band.
    ///
    /// - `band_rows`: height of the editor region, chrome excluded.
    /// - `band_cols`: width of the text area, i.e. the band minus the gutter.
    /// - `content_rows`: total visual rows the document occupies under the current folds.
    /// - `content_cols`: widest line in display cells, or 0 under wrap.
    /// - `wrap`: word wrap on, which removes the horizontal bar unconditionally.
    ///
    /// A bar that would leave no room for text is not counted. On a 3-row terminal with a
    /// long line, showing the bar and no text is worse than showing text and no bar, and
    /// the floor of one row and one column keeps every downstream division away from zero.
    pub fn compute(
        band_rows: u16,
        band_cols: u16,
        content_rows: usize,
        content_cols: usize,
        wrap: bool,
    ) -> Self {
        // A bar needs a row (or column) of its own *and* leaves at least one for text.
        let rows = if band_rows >= 2 && !wrap && content_cols > band_cols as usize {
            1
        } else {
            0
        };
        let cols = if band_cols >= 2 && content_rows > band_rows.saturating_sub(rows) as usize {
            1
        } else {
            0
        };
        BarOccupancy {
            rows,
            cols,
            viewport_rows: band_rows.saturating_sub(rows).max(1) as usize,
            viewport_cols: band_cols.saturating_sub(cols).max(1) as usize,
        }
    }
}

/// Everything needed to map terminal cells to document positions and back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewGeometry {
    /// Width of the gutter (line numbers, fold and bookmark markers). 0 when line
    /// numbers are off and no gutter markers are shown.
    pub gutter_width: usize,
    /// The text area, in terminal cells, already excluding the gutter.
    pub text_area: RectCore,
    pub tab_width: usize,
    pub word_wrap: bool,
    /// Cells the scrollbars take out of the editor band, and the viewport that is left.
    ///
    /// Computed by `App::recompute_geometry` on the np-tui side, which is the only place that
    /// knows the document, the folds and the band size together. Consumers must read
    /// [`Self::viewport_rows`] and [`Self::viewport_cols`] rather than `text_area.h` and
    /// [`Self::text_width`] -- see [`BarOccupancy`].
    pub bars: BarOccupancy,
}

impl Default for ViewGeometry {
    fn default() -> Self {
        ViewGeometry {
            gutter_width: 0,
            text_area: RectCore::new(0, 0, 80, 24),
            tab_width: 4,
            word_wrap: false,
            bars: BarOccupancy::default(),
        }
    }
}

impl ViewGeometry {
    /// Cells available for text on one row. Never zero unless the area itself is empty.
    pub fn text_width(&self) -> usize {
        self.text_area.w as usize
    }

    /// The viewport height in rows, with the horizontal scrollbar's row already removed.
    ///
    /// Read this instead of `text_area.h` anywhere a row of text is counted for the screen.
    /// See [`ViewGeometry::bars`].
    ///
    /// A geometry whose `bars` were never filled in -- the all-zero default, which is what a
    /// test or a caller outside the app builds -- falls back to the whole band. Deriving that
    /// here rather than requiring every construction site to remember keeps a geometry that
    /// has no bars from claiming a viewport of zero rows, which would lay out no text at all.
    pub fn viewport_rows(&self) -> usize {
        if self.bars == BarOccupancy::default() {
            return self.text_area.h as usize;
        }
        self.bars.viewport_rows
    }

    /// Cells of text per row, with the vertical scrollbar's column already removed.
    ///
    /// The [`Self::text_width`] contract with the bars taken out. Wrap math, horizontal
    /// scrolling and caret visibility all ask this question, and any one of them asking the
    /// raw width instead lets text be laid out under the bar.
    pub fn viewport_cols(&self) -> usize {
        if self.bars == BarOccupancy::default() {
            return self.text_area.w as usize;
        }
        self.bars.viewport_cols
    }

    /// Screen row index (0-based **within the text area**) to the row at that index.
    ///
    /// `rows` is expected to already be the visible window, so indexing is direct.
    pub fn row_at(&self, rows: &[VisualRow], screen_row: u16) -> Option<VisualRow> {
        rows.get(screen_row as usize).copied()
    }

    /// Inverse of [`ViewGeometry::row_at`]: the screen row displaying `lc`.
    ///
    /// The search is confined to the rows of `lc`'s **line**. That is not an optimization: a
    /// wrapped row's last cell and the next row's first cell are adjacent, so a scan across the
    /// whole list would let a later line's row claim an earlier line's end-of-line position and
    /// put the caret a line too far down. Restricting to the line makes the answer well defined
    /// with no tie-breaking rules at all:
    ///
    ///  - a column at or past the line's last row's end is that last row, which is where
    ///    end-of-line lives;
    ///  - otherwise the row whose slice holds the column, and a row's `end_col` is only its own
    ///    row's when it is the line's last one -- a continuation row ends *inside* the line, so
    ///    its `end_col` position belongs to the row after it.
    pub fn screen_row_of(&self, rows: &[VisualRow], lc: LineCol) -> Option<u16> {
        let cap = |i: usize| Some(i.min(u16::MAX as usize) as u16);
        let line_rows: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.line == lc.line)
            .map(|(i, _)| i)
            .collect();
        let last_index = *line_rows.last()?;
        if lc.col >= rows[last_index].end_col {
            return cap(last_index);
        }
        match line_rows
            .iter()
            .copied()
            .find(|&i| rows[i].holds(lc.col, i == last_index))
        {
            Some(index) => cap(index),
            None => cap(line_rows[0]),
        }
    }

    /// Map a terminal cell to a document position.
    ///
    /// `col` is a terminal column; cells inside the gutter resolve to column 0 of the
    /// row's line, and a click past the end of a row clamps to that row's end (spec §6.9
    /// row 1).
    pub fn cell_to_linecol(&self, buf: &Buffer, rows: &[VisualRow], col: u16, row: u16) -> LineCol {
        let Some(vr) = self.row_at(rows, row) else {
            return LineCol::default();
        };
        if (col as usize) < self.gutter_width {
            return LineCol::new(vr.line, 0);
        }
        let text_col = (col as usize) - self.gutter_width + self.horizontal_scroll_col();
        self.position_in_row(buf, vr, text_col)
    }

    /// Resolve a display column **inside one visual row** to a document position.
    ///
    /// `display_col` counts cells from the row's own left edge, which is the only reading
    /// that makes sense once a logical line owns several rows. Two rules live here and both
    /// are things a naive implementation gets wrong:
    ///
    ///  - the row's leading slice is added before converting, because
    ///    [`Buffer::col_from_display`] counts from the start of the *line*; converting a
    ///    second-row offset against the whole line points the click back at the beginning;
    ///  - the result is clamped to `row.end_col`, because a wrapped row owns only its own
    ///    slice. Without this a click in the empty space right of a short wrapped row jumps
    ///    the caret to the end of the logical line, several characters further right than
    ///    where the user clicked.
    pub fn position_in_row(&self, buf: &Buffer, row: VisualRow, display_col: usize) -> LineCol {
        let tab = self.tab_width.max(1);
        let row_lead = buf.display_col(row.line, row.start_col, tab);
        let mut col = buf.col_from_display(row.line, display_col + row_lead, tab);
        if row.end_col > row.start_col && col > row.end_col {
            col = row.end_col;
        }
        buf.clamp(LineCol::new(row.line, col))
    }

    /// The first visible character column of the window. Forced to 0 under wrap, since
    /// horizontal scrolling is disabled then (spec FR-V1).
    fn horizontal_scroll_col(&self) -> usize {
        0
    }

    /// Where the caret goes for `delta` **screen rows** of vertical motion (FR-V1, FR-E2).
    ///
    /// With wrap off this agrees with `Document::move_vertical` -- one press per logical line.
    /// With wrap on the caret walks the row list, one press per visual row, which is what
    /// makes a wrapped paragraph readable top to bottom instead of being jumped over.
    ///
    /// # The row is chosen positionally, the column is chosen by goal
    ///
    /// The target row is `here + delta`, full stop. A column rule must never get a vote on
    /// *which* row the caret lands on, and a goal-clamp does exactly that: a caret at the end
    /// of a short line whose goal is beyond the next row's width clamps to that row's
    /// `end_col`, which `screen_row_of` then reads back as a *later* row, and the press
    /// silently skips one. So the row is decided first, and the goal is used only to pick a
    /// column inside the row that was already chosen. The invariant this buys is the one the
    /// user actually feels: one press, one row, no row visited twice, no row missed.
    ///
    /// # The goal column
    ///
    /// `goal_col` is [`crate::Cursor::goal_col`]: a display column measured from the start of
    /// the **logical line**, which is the space the no-wrap path uses too. Keeping one space
    /// for both is the point -- a caret that descends a wrapped paragraph and then has wrap
    /// switched off must land where it started, and that only holds if the goal meant the
    /// same thing on every row.
    ///
    /// The goal is a *preference*, so it is clamped rather than adopted: a caret that steps
    /// down through a short line keeps its aim and returns to the column it came from on the
    /// way back. That is `move_vertical`'s documented behavior and it is reproduced here,
    /// including the rule that a move which saturates at the top or bottom row adopts the
    /// position it actually reached, because a goal beyond an edge is not reachable.
    ///
    /// `goal_col` is `None` after a purely horizontal move, which has no preferred column;
    /// the caret's own display column is used, again matching the no-wrap path.
    pub fn vertical_of(
        &self,
        buf: &Buffer,
        rows: &[VisualRow],
        from: LineCol,
        delta: i64,
        goal_col: Option<usize>,
    ) -> Option<LineCol> {
        if rows.is_empty() {
            return None;
        }
        let tab = self.tab_width.max(1);
        let here = self
            .motion_origin(rows, buf, from)
            .or_else(|| self.nearest_row(rows, from))?;
        let goal = goal_col.unwrap_or_else(|| buf.display_col(from.line, from.col, tab));
        let target = here as i64 + delta;

        // Past either end of the row list. Those are the document's first and last rows, so
        // this is where the no-wrap path's "saturate at line 0 / last line" lives, and the
        // saturation rule means the caret adopts the edge rather than keeping a goal it can
        // never reach again.
        if target < 0 {
            let row = rows[0];
            return Some(LineCol::new(row.line, row.start_col));
        }
        if target >= rows.len() as i64 {
            let row = rows[rows.len() - 1];
            return Some(LineCol::new(row.line, buf.line_len_chars(row.line)));
        }
        let target = target as usize;
        if target == here {
            return Some(from);
        }
        let row = rows[target];
        let line_len = buf.line_len_chars(row.line);

        // Land inside the target row's own slice. A goal that points before the row began
        // belongs at its start; one at or past the row's last cell belongs at its end.
        //
        // `end_col` is addressable only where `screen_row_of` agrees it is: on a line's
        // final row, where it *is* end-of-line. On a continuation row the position at
        // `end_col` is rendered by the following row, so claiming it would move the caret to
        // a row the user did not press toward. There the caret takes the last cell this row
        // really owns, and keeps its goal for the next press.
        let row_start_display = buf.display_col(row.line, row.start_col, tab);
        let row_end_display = buf.display_col(row.line, row.end_col, tab);
        let is_line_last = line_rows_last(rows, target);
        let col = if goal < row_start_display {
            row.start_col
        } else if goal >= row_end_display {
            if is_line_last || row.end_col == row.start_col {
                row.end_col
            } else {
                prev_cluster(buf, row.line, row.end_col).max(row.start_col)
            }
        } else {
            buf.col_from_display(row.line, goal, tab)
        };
        Some(LineCol::new(row.line, col.min(line_len)))
    }

    /// Which row a vertical move starts *from*.
    ///
    /// Not the same question as "which row draws this position", and the difference is one
    /// specific position: a caret sitting at a continuation row's `end_col`. That cell is
    /// rendered by the *following* row -- a continuation row's slice is half-open, so
    /// `end_col` is its first excluded offset -- and `screen_row_of` correctly answers "the
    /// later row". As a starting point that answer double-counts: pressing down would move
    /// from the later row and appear to skip the one the caret is touching.
    ///
    /// So the caret is anchored on the last row of its line that both starts at or before the
    /// caret's display column and still contains it. When the caret is at a row's end and that
    /// row is not the line's last, the earlier row is the one being left; when the caret is at
    /// a line's end there is nowhere further to go and `screen_row_of`'s answer is used.
    fn motion_origin(&self, rows: &[VisualRow], buf: &Buffer, from: LineCol) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        let tab = self.tab_width.max(1);
        let from_display = buf.display_col(from.line, from.col, tab);
        let mut origin = None;
        let mut last_of_line = None;
        for (index, row) in rows.iter().enumerate() {
            if row.line != from.line {
                continue;
            }
            last_of_line = Some(index);
            let starts_before = buf.display_col(row.line, row.start_col, tab) <= from_display;
            let contains = row.start_col <= from.col && from.col < row.end_col;
            if starts_before && (contains || row.start_col == from.col) {
                origin = Some(index);
            }
        }
        if let Some(index) = origin {
            // A caret at the very end of a line is on that line's last row, not on the
            // earlier row whose slice stops short of it.
            if from.col >= rows[last_of_line?].end_col {
                return last_of_line;
            }
            return Some(index);
        }
        self.screen_row_of(rows, from).map(|v| v as usize)
    }

    /// The index of the row closest to `lc`, used when `lc` is not on screen.
    fn nearest_row(&self, rows: &[VisualRow], lc: LineCol) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        if lc.line < rows[0].line {
            return Some(0);
        }
        if lc.line > rows[rows.len() - 1].line {
            return Some(rows.len() - 1);
        }
        // Somewhere in the middle: the last row of the greatest line <= `lc.line`, which is
        // the line the caret would land on if the fold hiding it were opened.
        rows.iter().rposition(|r| r.line <= lc.line).or(Some(0))
    }
}

/// Build the list of rows to display, top to bottom.
///
/// Folds are applied first, then wrap; see the module docs for why the order matters.
/// `rows` come out in document order and never exceed [`ViewGeometry::viewport_rows`] of
/// them -- the band's height with the horizontal scrollbar's row already taken out.
///
/// `scroll_top_line` is a **logical** line, which is what makes the scroll position
/// survive a fold change: collapsing a range above the viewport does not move the line the
/// user is looking at, it moves how many rows sit above it.
pub fn layout_visible_rows(
    buf: &Buffer,
    geom: &ViewGeometry,
    scroll_top_line: usize,
    folds: &[FoldRange],
) -> Vec<VisualRow> {
    let height = geom.viewport_rows();
    if height == 0 {
        return Vec::new();
    }
    let line_count = buf.line_count();
    if line_count == 0 {
        return Vec::new();
    }
    // A scroll position past the end shows the last page rather than a blank screen, which
    // is the behavior every existing test and every real editor has.
    //
    // The clamp is two-pass, and the second pass is the part a single pass cannot do: with
    // wrap on, the *row* count depends on where the window starts, because a line that
    // begins a row mid-way still consumes a whole row of its own. So the first pass finds
    // the position, and the second lays it out.
    let start = clamp_scroll_top(buf, geom, folds, scroll_top_line, height);

    let mut rows = Vec::with_capacity(height.min(line_count));
    push_rows_from(buf, geom, folds, start, height, &mut rows);
    rows
}

/// The logical line a `scroll_top_line` request actually resolves to.
///
/// Three cases, in the order they are decided, because getting the order wrong turns two of
/// them into the wrong answer:
///
///  1. **A line hidden by a fold** resolves to the first visible line at or after it, or, when
///     the hidden run reaches the end of the document, to the deepest collapsed header
///     containing it. A header is the on-screen representative of everything it hides.
///     Decided first: a hidden line has no row count of its own, so feeding one to the page
///     arithmetic below reads "nothing visible after this" as "pin to the bottom" and jumps
///     the user somewhere they did not ask for.
///  2. **A request at or past the document's last line** -- the `PgDn`-into-the-void case --
///     resolves to the start of the last page: the deepest line from which the window still
///     reaches the document's final row.
///  3. **Any other line is honored exactly**, clamped only so the window cannot start past the
///     end. This is the ordinary scroll case and it is deliberately *not* pulled backwards.
///
/// # Why an ordinary top is never pulled back
///
/// The tempting rule -- "walk the start line back until the window fills the viewport" -- is
/// wrong, and wrong in the way that is hardest to see in a screenshot: it drops rows off the
/// *top*. A three-paragraph document that wraps to eight rows in a viewport of eight starts at
/// line 0 and shows all of it; pulled back, it starts at line 1 and shows six rows with the
/// first paragraph gone, while still not filling the screen. A document shorter than a page
/// has no last page to pin to; the honest answer is "show it from the top". Only case 2, an
/// explicit request for a position at or past the end, gets the backward walk, and that walk
/// is bounded by the viewport so it cannot reach back further than one screen.
fn clamp_scroll_top(
    buf: &Buffer,
    geom: &ViewGeometry,
    folds: &[FoldRange],
    scroll_top_line: usize,
    height: usize,
) -> usize {
    let line_count = buf.line_count();
    let last = line_count.saturating_sub(1);
    let clamped = scroll_top_line.min(last);

    // Case 1: a hidden line is about visibility, not about pages.
    if !line_is_visible(clamped, folds) {
        return first_visible_at_or_after(clamped, folds, last);
    }

    // Case 3 for any position inside the document: honored exactly. Without wrap the only
    // further work is the "not past the last page" clamp the no-wrap callers already rely on.
    if scroll_top_line < last {
        return clamped;
    }

    // Case 2: a request at or past the document's last line.
    if !geom.word_wrap {
        return line_count.saturating_sub(height).min(last);
    }
    // The last page is the window whose bottom row is the document's last row, so it starts
    // `height` rows short of the end. Row offsets come from the shared counter, which is the
    // same arithmetic the scrollbar and `Document::scroll_to_show` use; an earlier version
    // walked start lines and laid each one out instead, and that walk is what pulled an
    // ordinary top line backwards and dropped rows off the top of the screen.
    let total = total_row_count(buf, geom, folds);
    let wanted = total.saturating_sub(height);
    match line_at_row_offset(buf, geom, folds, wanted) {
        Some(line) if line_is_visible(line, folds) => line,
        // The target offset falls inside a collapsed range: land on the header that stands in
        // for it, which is the closest thing to "the bottom" that is actually on screen.
        Some(line) => first_visible_at_or_after(line, folds, last),
        None => first_visible_line(folds),
    }
}

/// The logical line whose first row sits at row offset `offset` in the whole-document layout.
///
/// O(lines), using the same per-line height answer as [`total_row_count`], so a row offset and
/// a row count can never disagree about where a line begins.
pub fn line_at_row_offset(
    buf: &Buffer,
    geom: &ViewGeometry,
    folds: &[FoldRange],
    offset: usize,
) -> Option<usize> {
    let line_count = buf.line_count();
    if line_count == 0 {
        return None;
    }
    let mut seen = 0usize;
    let mut line = 0;
    while line < line_count {
        let height = rows_for_line(buf, geom, line);
        if seen + height > offset {
            return Some(line);
        }
        seen += height;
        line = next_visible_line(line, folds);
    }
    last_visible_line(buf, folds).into()
}

/// The last line a fold leaves visible.
fn last_visible_line(buf: &Buffer, folds: &[FoldRange]) -> usize {
    let last = buf.line_count().saturating_sub(1);
    let mut probe = 0usize;
    let mut seen = 0usize;
    let mut guard = 0;
    while probe <= last && guard <= last + folds.len() + 2 {
        guard += 1;
        seen = probe;
        let next = next_visible_line(probe, folds);
        if next <= probe {
            break;
        }
        probe = next;
    }
    seen
}

/// The first line the layout shows: line 0, since a fold cannot hide it (its header is line 0
/// or later and a header is always visible).
fn first_visible_line(_folds: &[FoldRange]) -> usize {
    0
}

/// The first line at or after `line` that a fold is not hiding, falling back to the deepest
/// collapsed header containing `line`.
fn first_visible_at_or_after(line: usize, folds: &[FoldRange], last: usize) -> usize {
    let mut probe = line;
    let mut guard = 0;
    while probe < last && guard <= folds.len() + 2 {
        if line_is_visible(probe, folds) {
            return probe;
        }
        let next = next_visible_line(probe, folds);
        if next <= probe {
            break;
        }
        probe = next;
        guard += 1;
    }
    if line_is_visible(probe, folds) {
        return probe;
    }
    folds
        .iter()
        .filter(|f| f.collapsed && line >= f.start && line <= f.end)
        .map(|f| f.start)
        .max()
        .unwrap_or(0)
}

/// Rows one logical line occupies under this geometry, with no viewport cap.
///
/// The single place "how tall is this line" is answered. Everything that counts rows -- the
/// viewport measurements above, [`total_row_count`], the scrollbar thumb, `Document`'s jump
/// arithmetic -- goes through here, so a thumb and the screen it scrolls cannot disagree about
/// a line's height. That agreement is not a nicety: when the two drift, the thumb says the
/// document is eight rows and the screen paints six, and the last two rows of a short file
/// become unreachable.
///
/// It delegates to the real splitter rather than dividing the line's display width by the
/// area's. Those answer different questions. A row that broke on a word boundary is *shorter*
/// than the area, so `ceil(width)` over-counts an indented line whose first row gave up cells
/// to a word that would not fit; and a line ending in a consumed trailing space does not gain
/// a row, because end-of-line is a position on the last row rather than a cell of its own (see
/// the "No synthetic caret row" rule on [`wrap_line`]). Only the splitter knows either answer,
/// and there must be exactly one of it.
///
/// **Uncapped on purpose.** An earlier version clamped a line at the viewport height, which
/// quietly made the scrollbar lie about documents containing a line taller than the screen:
/// a 1 000-row line in a 24-row viewport reported 24 rows total, so the thumb filled the
/// trough and the caret could be scrolled to rows the thumb insisted did not exist. A line
/// taller than the viewport is real scrollable content. The cost of walking such a line is
/// bounded by [`WRAP_SCAN_CAP`] rather than by the viewport, and the viewport cap belongs to
/// the callers that actually have a viewport -- [`measure_from`] and
/// [`layout_visible_rows`].
/// How many screen rows `line` occupies: 1 with wrap off, otherwise the wrapped row count.
///
/// Public because "how tall is this line on screen" is asked by the auto-scroll logic as well
/// as by the painter, and two answers to it is how a caret ends up off screen.
pub fn rows_for_line(buf: &Buffer, geom: &ViewGeometry, line: usize) -> usize {
    let width = geom.viewport_cols();
    let len = buf.line_len_chars(line);
    if !geom.word_wrap || width == 0 {
        return 1;
    }
    if len == 0 {
        return 1;
    }
    count_wrapped_rows(buf, line, len, width, geom.tab_width.max(1))
}

/// How many rows the splitter produces for one line, with no viewport limit.
///
/// [`wrap_line`] with an unbounded `limit`, minus the allocation: the loop is the same two
/// lines, driven by [`wrap_point`]'s progress guarantee. Bounded by [`WRAP_SCAN_CAP`] rows so
/// a single pathological multi-megabyte line costs a fixed amount instead of its own length.
fn count_wrapped_rows(buf: &Buffer, line: usize, len: usize, width: usize, tab: usize) -> usize {
    let mut start = 0usize;
    let mut count = 0usize;
    while start < len && count < WRAP_SCAN_CAP {
        let end = wrap_point(buf, line, start, len, width, tab);
        if end <= start {
            break;
        }
        count += 1;
        start = end;
    }
    count.max(1)
}

/// Row ceiling for a single logical line, so one absurd line cannot cost its own length.
///
/// Far past any terminal session's scrollback for one line, and low enough that the worst
/// realistic case -- a minified file -- stays a fixed per-frame cost.
const WRAP_SCAN_CAP: usize = 1 << 20;

/// Whether `rows[index]` is the final row of its logical line.
///
/// Rows are emitted in document order and a logical line's rows are contiguous, so the last
/// row of a line is simply the one followed by a different line -- or by nothing.
fn line_rows_last(rows: &[VisualRow], index: usize) -> bool {
    match rows.get(index + 1) {
        Some(next) => next.line != rows[index].line,
        None => true,
    }
}

/// The next logical line that reaches the screen at or after `line`.
///
/// `line` itself is always returned when it is a fold header or outside every range: a
/// collapsed fold hides its *contents*, never its header, and the caller is asking for
/// `line` specifically.
pub fn next_visible_line(line: usize, folds: &[FoldRange]) -> usize {
    let mut next = line + 1;
    // A line can sit inside several collapsed ranges at once (nested folds), so the scan
    // has to keep going until nothing else swallows the candidate. One pass per nesting
    // level, and nesting is bounded by the document's structure, not by input length.
    let mut guard = 0;
    while guard <= folds.len() {
        guard += 1;
        match folds
            .iter()
            .find(|f| f.collapsed && next > f.start && next <= f.end)
        {
            Some(f) => next = f.end + 1,
            None => break,
        }
    }
    next
}

/// The last visible line at or before `line`, the mirror of [`next_visible_line`].
///
/// Auto-scroll needs to walk *upward* from the caret as well as down: when the caret sits
/// above the top of the viewport, the new top must be the caret's own line, and that line is
/// hidden whenever a fold swallows it. Returning `line` itself when it is visible keeps the
/// caller from having to know about folds at all.
pub fn prev_visible_line(line: usize, folds: &[FoldRange]) -> usize {
    let mut probe = line;
    let mut guard = 0;
    while guard <= folds.len() {
        guard += 1;
        match folds
            .iter()
            .find(|f| f.collapsed && probe > f.start && probe <= f.end)
        {
            Some(f) => probe = f.start.saturating_sub(1),
            None => break,
        }
    }
    probe
}

/// Whether `line` is displayed at all: not inside a collapsed range.
pub fn line_is_visible(line: usize, folds: &[FoldRange]) -> bool {
    !folds
        .iter()
        .any(|f| f.collapsed && line > f.start && line <= f.end)
}

/// Total rows the whole document would occupy, folds and wrap applied.
///
/// O(document) to compute, and memoized for exactly that reason: the vertical scrollbar needs
/// it every frame, and an uncached walk measured 240 ms per frame on a 100 MB file, which is
/// 15x the spec §9 keystroke budget on its own. The cache key is every input the walk reads:
/// the buffer revision, the geometry (text width, tab width, wrap), and the fold set. A fold
/// toggle changes the answer without touching the buffer, and a resize changes it without
/// touching either, so both are part of the key rather than assumed away.
pub fn total_row_count(buf: &Buffer, geom: &ViewGeometry, folds: &[FoldRange]) -> usize {
    if let Some(hit) = buf.row_count_cache() {
        if hit.revision == buf.revision()
            && hit.text_width == geom.viewport_cols() as u16
            && hit.text_height == geom.viewport_rows() as u16
            && hit.tab_width == geom.tab_width
            && hit.word_wrap == geom.word_wrap
            && hit.fold_signature == fold_signature(folds)
        {
            return hit.rows;
        }
    }
    let rows = total_row_count_uncached(buf, geom, folds);
    buf.set_row_count_cache(RowCountCache {
        revision: buf.revision(),
        text_width: geom.viewport_cols() as u16,
        text_height: geom.viewport_rows() as u16,
        tab_width: geom.tab_width,
        word_wrap: geom.word_wrap,
        fold_signature: fold_signature(folds),
        rows,
    });
    rows
}

/// A fingerprint of the fold set, so a cache hit cannot serve a count from a different fold
/// state. Length alone is not enough: toggling one range keeps the length and changes the
/// count, so the collapsed flags and bounds are folded in as well.
fn fold_signature(folds: &[FoldRange]) -> u64 {
    // FNV-1a. Collision risk here would mean a scrollbar thumb that is momentarily one fold
    // off, and any later edit or toggle repairs it; the alternative is a per-frame O(folds)
    // clone of the whole set.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: u64| {
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(folds.len() as u64);
    for fold in folds {
        mix(fold.start as u64);
        mix(fold.end as u64);
        mix(fold.collapsed as u64);
    }
    hash
}

/// The walk itself, with no caching.
fn total_row_count_uncached(buf: &Buffer, geom: &ViewGeometry, folds: &[FoldRange]) -> usize {
    let height = geom.viewport_rows().max(1);
    let line_count = buf.line_count();
    // No wrap: every line that is displayed occupies exactly one row, whatever its content,
    // because `rows_for_line` returns 1 as soon as wrap is off. So the walk collapses to
    // "lines that are displayed" -- the total minus the bodies of collapsed ranges, each of
    // which is replaced by the single row its header already occupies. That is O(folds)
    // instead of O(lines), and it is exact, not an approximation.
    //
    // This matters because the cache in `total_row_count` cannot help while typing: every
    // keystroke moves the revision, and an uncached walk of a 100k-line file measured 55 ms --
    // most of the §9 frame budget spent counting rows.
    if !geom.word_wrap {
        let hidden = hidden_rows_before(line_count, folds);
        return line_count.saturating_sub(hidden).max(1);
    }
    let mut total = 0usize;
    let mut line = 0;
    while line < line_count {
        // `rows_for_line` is the single answer to "how tall is this line", shared with the
        // viewport measurements that decide scrolling. A thumb and the screen it scrolls must
        // be counting the same rows.
        total += rows_for_line(buf, geom, line);
        if total > ROW_COUNT_CAP {
            // A 100 MB file does not need an exact row count to draw a scrollbar, and the
            // caller clamps to the viewport anyway. Capping keeps this O(lines) instead of
            // O(lines) * width work for pathological single-line files.
            return ROW_COUNT_CAP.max(height);
        }
        line = next_visible_line(line, folds);
    }
    // An empty document is still one row of nothing, which is what the layout paints and what
    // a thumb needs to see to know there is nothing to scroll.
    total.max(1)
}

/// Rows hidden by collapsed ranges whose header is before `line`.
///
/// Overlapping and nested collapsed ranges hide a line once, not once per range: the count is
/// the size of the *union* of the hidden spans. See the body for why a plain sum is wrong.
///
/// [`line_at_row_offset`] answers "which line occupies row N" by counting rows top-down, and a
/// collapsed range contributes one row for its header and nothing for its body. So a row offset
/// and a line index differ by exactly the hidden-line count above that line -- which is what
/// `Document::scroll_to_show` needs, because it decides visibility in row space while
/// `scroll.top_line` is stored as a line.
pub fn hidden_rows_before(line: usize, folds: &[FoldRange]) -> usize {
    // A union, not a sum. Fold detection emits nested ranges -- an outer block and a block
    // inside it are both offered -- and the row walk hides a line once no matter how many
    // collapsed ranges claim it. Summing the ranges subtracts an inner body a second time
    // after its outer parent already hid it, which under-counts rows and drags the scrollbar
    // thumb and `Document::row_offset_of_top` to the wrong place.
    //
    // A collapsed range hides its body `start + 1 ..= end` and never its header, so the hidden
    // interval is `[start + 1, end + 1)` in exclusive form. Clipping `to` at `line` drops a
    // range at or past `line` on its own: it hides nothing *above* `line`, and the header rule
    // is what makes `start + 1` the left edge rather than `start`.
    let mut spans: Vec<(usize, usize)> = folds
        .iter()
        .filter(|f| f.collapsed)
        .map(|f| (f.start + 1, (f.end + 1).min(line)))
        .filter(|(from, to)| from < to)
        .collect();
    spans.sort_unstable();
    // Merge adjacent-or-overlapping intervals, then add up the merged lengths. Touching
    // intervals (`from == gto`) merge: `[1,3)` and `[3,5)` describe one run of hidden lines.
    let mut hidden = 0usize;
    let mut group: Option<(usize, usize)> = None;
    for (from, to) in spans {
        match group {
            Some((group_from, group_to)) if from <= group_to => {
                group = Some((group_from, group_to.max(to)));
            }
            Some((group_from, group_to)) => {
                hidden += group_to - group_from;
                group = Some((from, to));
            }
            None => group = Some((from, to)),
        }
    }
    if let Some((group_from, group_to)) = group {
        hidden += group_to - group_from;
    }
    hidden
}

/// The row offset a line's **first** row sits at, in the whole-document layout.
///
/// The inverse of [`line_at_row_offset`] restricted to line starts, which is the question a
/// scroller asks: `scroll.top_line` names a line, and the viewport always begins at that line's
/// first row, so a caller holding a row offset needs to know where the line containing it begins.
/// Without this, a request for a row in the middle of a long wrapped line silently means "start
/// at the top of that line instead", and the caller can end up thinking it moved when it did not.
pub fn row_offset_of_line(
    buf: &Buffer,
    geom: &ViewGeometry,
    folds: &[FoldRange],
    line: usize,
) -> usize {
    let line_count = buf.line_count();
    if line_count == 0 || line == 0 {
        return 0;
    }
    let mut seen = 0usize;
    let mut probe = 0;
    while probe < line_count && probe < line {
        let height = rows_for_line(buf, geom, probe);
        // A hidden line contributes the single row its collapsed header stands for, which
        // `rows_for_line` already answers because it walks folded lines the same way.
        seen += height;
        probe = next_visible_line(probe, folds);
    }
    seen
}

/// Every row the whole document would occupy, folds and wrap applied, in document order.
///
/// The row-space twin of [`total_row_count`], and built by the same splitter, so
/// `document_rows(...).len()` and `total_row_count(...)` are always the same number. Callers
/// that need to *index* rows -- jump to a row, drag a scrollbar thumb to a row, find the row
/// offset of the caret -- use this; callers that only need a count use `total_row_count`, which
/// skips the allocation.
///
/// O(rows in the document). Never call it per frame: [`layout_visible_rows`] exists precisely
/// so a frame costs a viewport.
pub fn document_rows(buf: &Buffer, geom: &ViewGeometry, folds: &[FoldRange]) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    push_rows_from(buf, geom, folds, 0, usize::MAX, &mut rows);
    rows
}

/// Append rows from `start` onward, stopping at `limit`.
///
/// Shared by [`document_rows`] and by anything that wants a bounded prefix. `layout_visible_rows`
/// keeps its own loop because it needs the viewport's budget arithmetic in row units, but the
/// per-line split is the same [`wrap_line`] call, which is the part that must not be duplicated.
fn push_rows_from(
    buf: &Buffer,
    geom: &ViewGeometry,
    folds: &[FoldRange],
    start: usize,
    limit: usize,
    out: &mut Vec<VisualRow>,
) {
    let width = geom.viewport_cols();
    let tab = geom.tab_width.max(1);
    let line_count = buf.line_count();
    let first = out.len();
    let mut line = start;
    while line < line_count && out.len() < limit {
        let budget = limit - out.len();
        let len = buf.line_len_chars(line);
        if geom.word_wrap && width > 0 {
            // Lay the line out into a scratch buffer so a line that does not fit whole can be
            // thrown away instead of half-painted. The allocation is one `Vec` of a handful of
            // `VisualRow`s per visible line, which at terminal widths is noise next to the
            // per-cluster width math the splitter already does.
            let needed = rows_for_line(buf, geom, line);
            if needed > budget {
                // This line needs more rows than the viewport has left, so it cannot be painted
                // whole here. Painting it partly would strand the remainder: no scroll position
                // can begin a window partway through one line's row list, so a half-painted line's
                // tail is unreachable, unclickable, and skipped by a caret walking the rows.
                //
                // Two kinds of line arrive here and they must be treated differently. Treating
                // them alike is what made everything after a long paragraph disappear.
                if needed > limit {
                    // **Taller than the entire viewport.** It can never be shown whole from any
                    // position, so refusing to paint it would not protect the tail -- the tail is
                    // unreachable either way -- while ending the page here hid every line below
                    // it at every scroll position. The file read as "text, then blank", with no
                    // way past it. Clip it to a whole viewport's worth of rows and continue with
                    // the lines after it. This is the same concession the never-empty rule below
                    // already makes, applied wherever such a line lands rather than only when it
                    // happens to be the first line on the page.
                    let mut clipped: Vec<VisualRow> = Vec::new();
                    wrap_line(buf, line, len, width, tab, limit, &mut clipped);
                    clipped.truncate(budget);
                    out.append(&mut clipped);
                    line = next_visible_line(line, folds);
                    continue;
                }
                // **Would fit in a whole viewport**, given room. Drop it whole -- never half --
                // and end the page. Ending rather than skipping to the next line is what keeps a
                // page a prefix of the document's row list, which the caret's row arithmetic,
                // `row_at` and `screen_row_of` all assume, and what
                // `view_s13::the_viewport_height_caps_the_row_list_under_wrap` pins.
                //
                // The cost is a page shorter than the viewport when the offending line is one row
                // too tall for the bottom. That is the intended trade, and it is bounded: the next
                // scroll position shows the line whole, so nothing is ever unreachable. The
                // scrollbar ceiling (`App::deepest_visible_top_line`) is derived from the painter,
                // so the last page it aims at is a page that actually reaches the end.
                break;
            }
            let mut scratch: Vec<VisualRow> = Vec::new();
            if !wrap_line(buf, line, len, width, tab, budget, &mut scratch) {
                // Unreachable: the line was measured against this same budget above. Kept as a
                // guard rather than an `expect`, and treated as the end of the page.
                out.append(&mut scratch);
                break;
            }
            out.append(&mut scratch);
        } else {
            out.push(VisualRow::whole(line, len));
        }
        line = next_visible_line(line, folds);
    }
    // Never leave a window empty. A start line taller than the whole viewport fits no rule
    // above, and a blank screen cannot be clicked, typed into, or scrolled from -- the user
    // cannot tell it apart from a crash. Clipping that one line to the rows that do fit is the
    // lesser evil, and it is the only place the "never half a line" rule is allowed to bend.
    if out.len() == first && start < line_count {
        let len = buf.line_len_chars(start);
        // A budget of one, not `limit`: `limit` is zero for a zero-height area, and
        // `layout_visible_rows` already returns early there. This branch exists precisely
        // because the budget ran out, so it asks for exactly the row it is willing to clip to.
        wrap_line(buf, start, len, width, tab, 1, out);
    }
}

/// Why a viewport never paints half a wrapped line
///
/// A viewport boundary that lands in the middle of a wrapped line must not paint the part that
/// happened to fit. The missing part is not merely off screen: scrolling moves the window by
/// *logical* lines, so no scroll position can start a window partway through one line's row list
/// either. Paint rows 1-2 of a three-row paragraph and the third row's characters become
/// unreachable -- invisible, unclickable, and skipped by a caret walking down the rows. Dropping
/// the partial line shows one row fewer and keeps every character of the document addressable.
///
/// The rule lives in [`push_rows_from`], applied per line against that line's own budget, using
/// the completeness answer from [`wrap_line`]. Two edges are handled there rather than being left
/// to intuition:
///  - a line that ends **exactly** at the budget is complete and stays, so a full page loses
///    nothing. Testing "did the loop run out" instead of "did the line finish" is what makes a
///    layout eat its own last line;
///  - a window that would end up **empty** gets the first row of its start line anyway.
///
/// [`total_row_count`] deliberately does *not* apply this rule. The scrollbar counts what the
/// document *is*, so a thumb stays honest about a document whose lines are taller than the
/// screen; the layout decides what the screen *shows*. Different questions, and only the second
/// one trims.
///
/// Ceiling for [`total_row_count`], well past any scrollable document on a terminal.
const ROW_COUNT_CAP: usize = 1 << 40;

/// The char column at which the wrapped row starting at `start` ends.
///
/// Exposed so a caller that needs the *whole* document's row list -- a jump-to-line, a
/// scrollbar thumb -- uses exactly the same splitter as [`layout_visible_rows`] rather than
/// approximating it. Two wrap implementations drifting apart is a caret that lands on the
/// wrong row, which is exactly the class of bug this module exists to prevent.
pub fn row_end_for(
    buf: &Buffer,
    line: usize,
    start: usize,
    len: usize,
    width: usize,
    tab_width: usize,
) -> usize {
    wrap_point(buf, line, start, len, width, tab_width.max(1))
}

/// Split one logical line into at most `limit` rows, appending to `out`.
///
/// `limit` counts rows **for this line**, not rows in the caller's output buffer: it is the
/// budget left for the line being wrapped, and `push_rows_from` subtracts what it has already
/// emitted before calling. Passing a whole-viewport limit here would let one long line overrun
/// the screen.
///
/// Returns whether the line was laid out **completely** -- `true` when the last row emitted
/// reaches the end of the line. A caller painting a viewport needs that answer to decide whether
/// to keep the rows at all; see the "Why a viewport never paints half a wrapped line" note on
/// [`push_rows_from`], which is the only thing standing between a narrow window and a paragraph
/// whose middle is unreachable.
///
/// The loop is driven entirely by [`wrap_point`], which guarantees forward progress, so this
/// cannot spin.
///
/// # No synthetic caret row
///
/// A wrapped line produces exactly as many rows as it takes to hold its *characters* -- never
/// an extra blank row for end-of-line. The temptation is to add one when the final row broke
/// on a consumed trailing space, so the caret has "a row to sit on"; doing that costs every
/// wrapped paragraph a blank row, and a document of paragraphs grows a visible gap after each
/// one. The caret does not need the extra row: end-of-line is a position, not a cell, and
/// [`ViewGeometry::screen_row_of`] maps `col == line end` onto the line's last row, which is
/// exactly where a reader expects a caret at the end of a paragraph to be drawn. Keeping this
/// rule is also what lets [`rows_for_line`] and the painted screen describe the same rows,
/// which the scrollbar math depends on.
fn wrap_line(
    buf: &Buffer,
    line: usize,
    len: usize,
    width: usize,
    tab_width: usize,
    limit: usize,
    out: &mut Vec<VisualRow>,
) -> bool {
    if len == 0 {
        out.push(VisualRow::whole(line, 0));
        return true;
    }
    let mut start = 0usize;
    loop {
        let end = wrap_point(buf, line, start, len, width, tab_width);
        let end = if end <= start { len } else { end };
        out.push(VisualRow {
            line,
            start_col: start,
            end_col: end,
        });
        start = end;
        if start >= len {
            return true;
        }
        // Out of budget with line left to lay out. Reported, not silently truncated: a caller
        // that stops here still gets the row this line already claimed, because a line that
        // contributed no row at all would read as a line that is not on screen.
        if out.len() >= limit {
            return false;
        }
    }
}

/// The char column at which the wrapped row starting at `start` ends.
///
/// # The break rule
///
/// A row holds as many **whole words** as fit; a word that does not fit starts the next row.
/// Two edges of that rule are where wrap implementations usually go wrong, and both are
/// decided here:
///
///  - **A break may only be placed at a boundary that was itself measured onto this row.** A
///    space is a legal break *after* it, and the start of a following word is a legal break
///    *before* it. A word that began inside the row and then overflowed is therefore pushed
///    whole to the next row, and the row ends at the last boundary that fitted.
///  - **A word longer than the viewport is hard-broken** at whatever cell fits. Without the
///    fallback a row would be empty and the caller's loop would never advance, which to the
///    user is an editor that hangs on a long URL. The fallback only fires when the row holds
///    nothing but that word; otherwise the word moves.
///
/// # Cluster safety
///
/// Iteration is by **grapheme cluster**, so a combining mark can never be stranded away from
/// its base character and a double-width glyph is measured as the two cells it occupies.
/// Widths and tab stops come from the same helpers the renderer uses
/// ([`Buffer::display_col`], [`crate::buffer::grapheme_width`]), which is what stops the
/// wrapper and the painter from disagreeing about where a row ends.
///
/// # Cost
///
/// `display_col` is O(line) and this walks the line, so wrapping one row is O(line length)
/// and laying out a viewport of wrapped rows is O(rows x line length). For the viewport-sized
/// lines and widths a terminal actually has, that is well inside a frame; a document made of
/// 300-character paragraphs at 80 columns lays out in microseconds. A per-line width cache
/// keyed on [`Buffer::revision`] is the escape hatch if a profile ever says otherwise.
///
/// Always returns a value strictly greater than `start` when `start < len`, for any width, any
/// tab width and any content. That is the loop-termination property every caller relies on.
fn wrap_point(
    buf: &Buffer,
    line: usize,
    start: usize,
    len: usize,
    width: usize,
    tab_width: usize,
) -> usize {
    if width == 0 || start >= len {
        return len;
    }
    let tab = tab_width.max(1);
    let row_start_display = buf.display_col(line, start, tab);
    let limit_display = row_start_display + width;
    // Fast path: the rest of the line fits, so no boundary search is needed.
    let line_display_len = buf.display_col(line, len, tab);
    if line_display_len <= limit_display {
        return len;
    }

    // Display cells carried forward instead of re-measured. Every previous call to
    // `display_col` inside this loop restarted from the beginning of the line, which made
    // wrapping O(line^2): 1.4 s per frame on a 20 MB file. The loop already walks clusters in
    // order, so the running cell count is exact -- it starts at this row's own display origin
    // and advances by each cluster's width, which is precisely how `display_col` defines it.
    let text = buf.line_string(line);
    let mut running_cells = row_start_display;
    // Char offset within the line, absolute from the first cluster.
    let mut offset = 0usize;
    // Display cells consumed by this row so far, measured absolutely from the start of the
    // line so that a tab's stop agrees with [`Buffer::display_col`] -- which is the function
    // callers use to ask "how many cells before column N", including the caller that turns a
    // click's cell back into a column. Measuring a row from its own origin instead would put
    // a tab's stop in the wrong place on every continuation row.
    let mut cells = row_start_display;
    // The last boundary that fitted: after a space run, or before the word that follows one.
    let mut last_break: Option<usize> = None;
    // Where the word currently being measured began, once it has been measured onto the row.
    let mut word_start: Option<usize> = None;
    // The furthest char a measured non-blank cluster reached, for the hard-break fallback.
    let mut hard_end = start;
    let mut measured = 0usize;

    for cluster in crate::buffer::graphemes_of(&text) {
        let cluster_len = cluster.chars().count();
        if offset >= len {
            break;
        }
        if offset < start {
            // Already on an earlier row of this line. Its width is deliberately not added to
            // `cells`, which starts at this row's own display origin; adding the leading slice
            // again would count it twice and shift the row left by its own indent.
            offset += cluster_len;
            continue;
        }
        // `running_cells` is the display column of `offset`: it starts at this row's own
        // display origin and every branch below assigns it `cluster_end_absolute`.
        // The cell this cluster ends at, in the same absolute display space as
        // [`Buffer::display_col`]. A tab resolves its stop from the start of the **line**,
        // not of the row, because that is what `display_col` does -- and `display_col` is
        // what the click mapping uses to turn a cell back into a column. A tab whose stop
        // were computed from the row's origin would land the click mapping somewhere else
        // whenever a row began partway through a line, which is the definition of wrap.
        let absolute_start = running_cells;
        let cluster_end_absolute = if cluster == "\t" {
            (absolute_start / tab + 1) * tab
        } else {
            absolute_start + crate::buffer::grapheme_width(cluster)
        };
        // `cells` tracks the same absolute space; keep the two in step so the loop's own
        // notion of "where am I" never diverges from the buffer's.
        cells = cells.max(absolute_start);
        let fits = cluster_end_absolute <= limit_display;

        if is_wrap_space(cluster) {
            if fits {
                // A blank that fits ends the row here, and the next row starts after the whole
                // run, so a continuation row never begins with a gap.
                last_break = Some(offset + cluster_len);
            }
            running_cells = cluster_end_absolute;
            offset += cluster_len;
            continue;
        }
        // A word boundary is legal *before* this cluster only if the row has content to keep:
        // an empty row cannot break before its own first word, or it would stay empty.
        let boundary_before = word_start.is_some() || last_break.is_some();
        if !fits {
            if boundary_before {
                break;
            }
            // This word is the row's only content and it does not fit: hard break below.
            break;
        }
        if word_start.is_none() {
            word_start = Some(offset);
        }
        cells = cluster_end_absolute;
        running_cells = cluster_end_absolute;
        hard_end = offset + cluster_len;
        measured += 1;
        offset = hard_end;
    }
    if offset >= len {
        // The rest of the line fitted: this is its last row.
        return len;
    }
    if let Some(at) = last_break {
        if at > start && at < len {
            return at;
        }
    }
    // No boundary the row could use: one word wider than the viewport. Take what fits, and
    // always at least one cluster so the caller makes progress.
    if measured > 0 && hard_end > start {
        return hard_end.min(len);
    }
    next_cluster_after(buf, line, start).min(len).max(start + 1)
}

/// The char index of the cluster boundary just before `col`, never below 0.
///
/// Used when a row's `end_col` is not addressable for the caret (a continuation row owns a
/// half-open slice) and the caret has to take the last cell the row really holds. Cluster
/// boundaries only, so a combining mark is never separated from its base.
fn prev_cluster(buf: &Buffer, line: usize, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let text = buf.line_string(line);
    let mut prev = 0usize;
    let mut offset = 0usize;
    for cluster in crate::buffer::graphemes_of(&text) {
        offset += cluster.chars().count();
        if offset >= col {
            return prev;
        }
        prev = offset;
    }
    prev
}

/// The char index just past the cluster containing `start`, or `start + 1` if that cannot be
/// determined. Always greater than `start`, which is the progress guarantee callers need.
fn next_cluster_after(buf: &Buffer, line: usize, start: usize) -> usize {
    let text = buf.line_string(line);
    let mut offset = 0usize;
    for cluster in crate::buffer::graphemes_of(&text) {
        let cluster_len = cluster.chars().count();
        if offset >= start {
            return offset + cluster_len;
        }
        offset += cluster_len;
    }
    start + 1
}

/// Whether a grapheme cluster is whitespace that a row may break after.
///
/// Tabs count: a tab between two words is exactly as good a break point as a space, and
/// treating it otherwise would push a whole indented clause onto the next row.
fn is_wrap_space(cluster: &str) -> bool {
    cluster.chars().all(|c| c == ' ' || c == '\t')
}

/// A collapsible line range (spec FR-V5).
///
/// `start` is the header line and always renders; `end` is inclusive. Ranges nest, and the
/// row layout walks them by containment rather than assuming a flat list, so an outer fold
/// swallowing an inner one is not a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    /// First line of the range; stays visible as the fold header.
    pub start: usize,
    /// Last line, inclusive.
    pub end: usize,
    pub collapsed: bool,
}

impl FoldRange {
    pub fn contains_line(&self, line: usize) -> bool {
        line > self.start && line <= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_edges_and_contains() {
        let r = RectCore::new(2, 3, 10, 5);
        assert_eq!(r.right(), 12);
        assert_eq!(r.bottom(), 8);
        assert!(r.contains(2, 3));
        assert!(r.contains(11, 7));
        assert!(!r.contains(12, 3), "right edge exclusive");
        assert!(!r.contains(2, 8), "bottom edge exclusive");
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn rect_inset_never_wraps() {
        let r = RectCore::new(0, 0, 4, 4);
        let small = r.inset(10, 10, 10, 10);
        assert_eq!(small.w, 0);
        assert_eq!(small.h, 0);
    }

    #[test]
    fn rect_saturates_at_u16_max() {
        let r = RectCore::new(u16::MAX - 1, 0, 100, 1);
        assert_eq!(r.right(), u16::MAX, "saturating, not wrapping");
    }

    #[test]
    fn rows_whole_line_spans_the_line() {
        let r = VisualRow::whole(3, 7);
        assert_eq!(r.char_len(), 7);
        assert!(r.holds(0, true));
        assert!(r.holds(6, true));
        assert!(r.holds(7, true), "EOL is addressable on the last row");
        assert!(!r.holds(7, false));
        assert!(!r.holds(8, true));
    }

    #[test]
    fn layout_no_wrap_one_row_per_line() {
        let buf = Buffer::from_str_lossy("a\nbb\nccc\ndddd");
        let geom = ViewGeometry::default();
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2], VisualRow::whole(2, 3));
    }

    #[test]
    fn layout_respects_viewport_height() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let buf = Buffer::from_str_lossy(&text);
        let mut geom = ViewGeometry::default();
        geom.text_area.h = 10;
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0].line, 0);
    }

    #[test]
    fn layout_honours_scroll_and_clamps_past_the_end() {
        let buf = Buffer::from_str_lossy("a\nb\nc\nd\ne");
        let mut geom = ViewGeometry::default();
        geom.text_area.h = 3;
        let rows = layout_visible_rows(&buf, &geom, 2, &[]);
        assert_eq!(
            rows.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        // Scrolled past the end: clamps to the last line rather than yielding nothing.
        let rows = layout_visible_rows(&buf, &geom, 999, &[]);
        assert_eq!(rows.last().map(|r| r.line), Some(4));
    }

    #[test]
    fn row_lookup_round_trips() {
        let buf = Buffer::from_str_lossy("one\ntwo\nthree");
        let geom = ViewGeometry::default();
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        for (idx, row) in rows.iter().enumerate() {
            assert_eq!(geom.row_at(&rows, idx as u16), Some(*row));
            let lc = LineCol::new(row.line, row.start_col);
            assert_eq!(geom.screen_row_of(&rows, lc), Some(idx as u16));
        }
        assert_eq!(geom.row_at(&rows, 99), None);
    }

    #[test]
    fn cell_to_linecol_clamps_past_line_end() {
        let buf = Buffer::from_str_lossy("ab\ncdef");
        let geom = ViewGeometry::default();
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        // Click far right on row 0 ("ab") clamps to column 2.
        assert_eq!(geom.cell_to_linecol(&buf, &rows, 60, 0), LineCol::new(0, 2));
        assert_eq!(geom.cell_to_linecol(&buf, &rows, 3, 1), LineCol::new(1, 3));
    }

    #[test]
    fn cell_to_linecol_in_gutter_resolves_to_line_start() {
        let buf = Buffer::from_str_lossy("hello\nworld");
        let geom = ViewGeometry {
            gutter_width: 5,
            ..Default::default()
        };
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        assert_eq!(geom.cell_to_linecol(&buf, &rows, 2, 1), LineCol::new(1, 0));
    }

    #[test]
    fn cell_to_linecol_accounts_for_the_gutter() {
        let buf = Buffer::from_str_lossy("hello\nworld");
        let geom = ViewGeometry {
            gutter_width: 4,
            ..Default::default()
        };
        let rows = layout_visible_rows(&buf, &geom, 0, &[]);
        // Terminal column 4 is the first text cell -> character 0.
        assert_eq!(geom.cell_to_linecol(&buf, &rows, 4, 0), LineCol::new(0, 0));
        assert_eq!(geom.cell_to_linecol(&buf, &rows, 6, 0), LineCol::new(0, 2));
    }

    #[test]
    fn fold_range_membership_excludes_the_header() {
        let f = FoldRange {
            start: 10,
            end: 20,
            collapsed: true,
        };
        assert!(!f.contains_line(10), "header stays visible");
        assert!(f.contains_line(11));
        assert!(f.contains_line(20));
        assert!(!f.contains_line(21));
    }
}
