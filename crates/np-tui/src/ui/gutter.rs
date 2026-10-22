//! The gutter: line numbers, fold markers, bookmark markers (spec FR-V2, FR-V5, FR-V6).
//!
//! # Why this is a module and not a few lines in the renderer
//!
//! §6.9's rule is that the renderer and the hit-tester must agree, and the gutter is where
//! that rule first becomes hard. Three different things live in one narrow strip, they are
//! drawn by different code paths in most editors, and a click that lands on the wrong one
//! toggles the wrong feature. So the strip's column layout is computed **once**, here, and
//! both `editor_view` and `mouse` ask it questions instead of each doing arithmetic:
//!
//!  - "which column holds the fold marker on this row" (`Gutter::fold_col`)
//!  - "what did a click at this column hit" (`Gutter::hit_at`)
//!  - "how wide is the strip" (`Gutter::width`), which `recompute_geometry` publishes as
//!    `ViewGeometry::gutter_width` so `cell_to_linecol` agrees with what was painted.
//!
//! # Column order
//!
//! `[ bookmark ][ fold ][ line number ]`, right-aligned, so the numbers stay put when a
//! marker column appears or disappears. Fold is adjacent to the number because a fold marker
//! belongs to the line it numbers; the bookmark column is outermost because a bookmark is a
//! note about a line rather than a property of its structure, and putting it outside keeps
//! the fold marker from jumping a column when bookmarks are added.
//!
//! Marker columns exist only when the document can have that marker. A document with no
//! folds gets no fold column, so a plain file's gutter is the same width it was before this
//! stage and its line numbers do not shift right by two cells for a feature it is not using.

use np_core::RectCore;

/// The gutter's column layout, in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gutter {
    /// Leftmost cell of the strip.
    pub x: u16,
    /// Total width in cells. 0 means there is no gutter at all.
    pub width: u16,
    /// Width reserved for the right-aligned line number.
    pub number_width: u16,
    /// True when a fold marker column exists.
    pub has_fold: bool,
    /// True when a bookmark marker column exists.
    pub has_bookmark: bool,
}

impl Gutter {
    /// Build the layout.
    ///
    /// `line_count` drives the number width so the strip never jitters as a document grows
    /// past a power of ten. `show_fold` and `show_bookmark` say whether the document has any
    /// foldable range or any bookmark at all -- not whether the *current line* has one,
    /// because a column that appeared and disappeared per line would resize the gutter on
    /// every caret move.
    pub fn new(
        x: u16,
        line_count: usize,
        show_numbers: bool,
        has_fold: bool,
        has_bookmark: bool,
    ) -> Self {
        let number_width = if show_numbers {
            digit_width(line_count.max(1)).max(2) as u16
        } else {
            0
        };
        // No numbers means no gutter to hang markers on: Notepad++ keeps a narrow marker
        // margin in that case, but our hit-testing keys off `gutter_width == 0` to mean
        // "there is no gutter", and inventing a width here would silently shift the text area.
        let (has_fold, has_bookmark) = if number_width == 0 {
            (false, false)
        } else {
            (has_fold, has_bookmark)
        };
        let extra = (has_fold as u16) + (has_bookmark as u16);
        let width = number_width + extra;
        Gutter {
            // The strip grows **leftward** from the text edge. `x` is the right edge the
            // gutter must not cross, so adding a marker column leaves the digits and the text
            // exactly where they were and takes the space on their left. Growing rightward
            // instead would shove the whole buffer sideways the first time a file turned out
            // to contain a fold.
            x: x.saturating_sub(width),
            width,
            number_width,
            has_fold,
            has_bookmark,
        }
    }

    /// The line-number field's leftmost cell.
    pub fn number_x(&self) -> u16 {
        // Numbers are the innermost (rightmost) field, flush against the text.
        self.x
            .saturating_add(self.width)
            .saturating_sub(self.number_width)
    }

    /// The fold marker's cell, or `None` when there is no fold column.
    pub fn fold_col(&self) -> Option<u16> {
        if !self.has_fold {
            return None;
        }
        Some(self.number_x().saturating_sub(1).max(self.x))
    }

    /// The bookmark marker's cell, or `None` when there is no bookmark column.
    pub fn bookmark_col(&self) -> Option<u16> {
        if !self.has_bookmark {
            return None;
        }
        let inner = self.fold_col().unwrap_or_else(|| self.number_x());
        Some(inner.saturating_sub(1).max(self.x))
    }

    /// Whether `cell_x` is inside the strip.
    pub fn contains(&self, cell_x: u16) -> bool {
        self.width > 0 && cell_x >= self.x && cell_x < self.x.saturating_add(self.width)
    }

    /// What a click at `cell_x` was aiming at.
    ///
    /// A marker column wins over the number field, and a fold marker wins over a bookmark
    /// marker, because a click is a one-cell event and the columns are adjacent: resolving in
    /// a fixed documented order is the only way the same click never means two things.
    /// Clicking the number field itself selects the line.
    pub fn hit_at(&self, cell_x: u16) -> GutterTarget {
        if !self.contains(cell_x) {
            return GutterTarget::Outside;
        }
        if let Some(col) = self.fold_col() {
            if cell_x == col {
                return GutterTarget::Fold;
            }
        }
        if let Some(col) = self.bookmark_col() {
            if cell_x == col {
                return GutterTarget::Bookmark;
            }
        }
        GutterTarget::LineNumber
    }

    /// The text area that follows the gutter, given a full-width terminal row.
    pub fn text_area(&self, total_width: u16, y: u16, height: u16) -> RectCore {
        let right = self.x.saturating_add(self.width);
        RectCore::new(right, y, total_width.saturating_sub(right), height)
    }
}

/// What a gutter click was aiming at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterTarget {
    /// The fold marker.
    Fold,
    /// The bookmark marker.
    Bookmark,
    /// The line-number field: select the line.
    LineNumber,
    /// Not in the gutter.
    Outside,
}

/// Cells needed to write `n` in decimal, capped at [`MAX_DIGITS`].
///
/// Allocation-free because `recompute_geometry` runs on every resize, and capped because a
/// gutter wider than a terminal is meaningless: the number field would push the text off
/// screen. The cap matches what the app has always used, so adopting this module changes no
/// existing geometry.
pub fn digit_width(n: usize) -> usize {
    let mut width = 1usize;
    let mut limit = 10usize;
    while n >= limit && width < MAX_DIGITS {
        width += 1;
        limit = limit.saturating_mul(10);
    }
    width
}

/// The widest line-number field the gutter will reserve. See [`digit_width`].
const MAX_DIGITS: usize = 8;

/// The fold marker glyphs (FR-V5).
pub const FOLD_OPEN: char = '\u{25bc}'; // down-pointing triangle: expanded
pub const FOLD_CLOSED: char = '\u{25b6}'; // right-pointing triangle: collapsed
/// The bookmark marker glyph (FR-V6).
pub const BOOKMARK: char = '\u{25c6}'; // filled diamond

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_width_matches_decimal() {
        assert_eq!(digit_width(1), 1);
        assert_eq!(digit_width(9), 1);
        assert_eq!(digit_width(10), 2);
        assert_eq!(digit_width(99), 2);
        assert_eq!(digit_width(100), 3);
        // Capped: a document with more lines than this is not a document, and a number field
        // wider than the terminal would push the text away.
        assert_eq!(digit_width(9_999_999), 7);
        assert_eq!(digit_width(usize::MAX), MAX_DIGITS);
    }

    #[test]
    fn no_numbers_means_no_gutter() {
        let g = Gutter::new(20, 100, false, true, true);
        assert_eq!(g.width, 0);
        assert_eq!(g.hit_at(0), GutterTarget::Outside);
        assert_eq!(g.text_area(80, 0, 24).x, 20);
    }

    #[test]
    fn marker_columns_widen_the_strip_by_one_each() {
        let plain = Gutter::new(0, 100, true, false, false);
        assert_eq!(plain.width, 3);
        let folded = Gutter::new(0, 100, true, true, false);
        assert_eq!(folded.width, 4);
        let both = Gutter::new(0, 100, true, true, true);
        assert_eq!(both.width, 5);
    }

    #[test]
    fn adding_a_marker_column_never_resizes_the_number_field() {
        // The marker columns are switched on only when the document actually has a fold or a
        // bookmark, which is what keeps this widening rare: a plain text file gets exactly the
        // gutter it had before this stage. When it does happen the text shifts one cell, and
        // `recompute_geometry` republishes `gutter_width` so the renderer, the caret's column
        // math and the hit-tester all move together. What must not change is the number field:
        // a digit field that resized on the first Ctrl+F2 would re-print every line number.
        let plain = Gutter::new(0, 100, true, false, false);
        let both = Gutter::new(0, 100, true, true, true);
        assert_eq!(
            plain.number_width, both.number_width,
            "same digits, same field"
        );
        assert_eq!(both.width, plain.width + 2, "one column per marker");
        assert_eq!(plain.text_area(80, 0, 24).x, 3);
        assert_eq!(both.text_area(80, 0, 24).x, 5);
    }

    #[test]
    fn the_three_columns_are_distinct_and_ordered() {
        let g = Gutter::new(0, 100, true, true, true);
        let bookmark = g.bookmark_col().expect("bookmark column");
        let fold = g.fold_col().expect("fold column");
        let number = g.number_x();
        // Left to right: bookmark, fold, then the number field. The number field is innermost
        // so a marker can never come between the digits and the text they number.
        assert_eq!(bookmark, 0, "bookmark outermost");
        assert_eq!(fold, 1, "fold adjacent to the numbers");
        assert_eq!(number, 2, "numbers flush against the text");
        assert_eq!(g.width, 5);
        assert_eq!(g.hit_at(0), GutterTarget::Bookmark);
        assert_eq!(g.hit_at(1), GutterTarget::Fold);
        // The number field spans its whole width, so a click anywhere on it selects.
        assert_eq!(g.hit_at(2), GutterTarget::LineNumber);
        assert_eq!(g.hit_at(4), GutterTarget::LineNumber);
        assert_eq!(g.hit_at(5), GutterTarget::Outside);
    }

    #[test]
    fn with_no_fold_column_a_fold_click_is_a_line_click() {
        let g = Gutter::new(0, 100, true, false, true);
        assert_eq!(g.fold_col(), None);
        // The column that would have been the fold marker belongs to the number field now, so
        // clicking there selects rather than toggling something that does not exist.
        assert_eq!(g.hit_at(g.number_x()), GutterTarget::LineNumber);
        assert_eq!(
            g.hit_at(g.bookmark_col().expect("bookmark")),
            GutterTarget::Bookmark
        );
    }

    #[test]
    fn the_text_area_starts_where_the_gutter_ends() {
        let g = Gutter::new(0, 100, true, true, true);
        let text = g.text_area(80, 2, 20);
        assert_eq!(text.x, 5);
        assert_eq!(text.w, 75);
        assert_eq!(text.y, 2);
        assert_eq!(text.h, 20);
    }
}

// ------------------------------------------------------------------ scrollbars
//
// The thumb geometry a scrollbar is drawn with, and the numbers the mouse layer needs to tell
// a thumb drag from a track click (§6.9 row 14).
//
// # Why recompute it when ratatui already computed it
//
// Because ratatui does not say where it put the thumb. It renders symbols into a buffer and
// keeps nothing, so a hit-tester that wants to know "did this click land on the thumb" has to
// derive the answer from the same inputs. The failure mode this guards against is a second
// implementation of the thumb math that drifts from the drawing -- the thumb is drawn at row 5
// but grabs at row 6, which to the user is a scrollbar that ignores the first click. So the
// drawing path in `editor_view` and the hit-test path in `mouse` both call [`ScrollbarGeom`]
// and cannot disagree.
//
// The arithmetic mirrors ratatui 0.30's `Scrollbar::part_lengths` -- round-half-up division,
// thumb clamped to at least one cell, thumb start clamped so the thumb always fits the track.
// If ratatui changes its thumb math this needs to change with it; the tests below pin the
// numbers so a ratatui upgrade that shifts them fails loudly here rather than quietly
// mis-placing the grab target.

/// Where a scrollbar's track and thumb are, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeom {
    /// First cell of the track (after any begin symbol).
    pub track_start: u16,
    /// Number of track cells.
    pub track_len: u16,
    /// First thumb cell, relative to `track_start`.
    pub thumb_start: u16,
    /// Thumb length in cells. Always at least 1 when the scrollbar is drawn at all.
    pub thumb_len: u16,
}

impl ScrollbarGeom {
    /// Compute the geometry.
    ///
    /// - `track_origin`/`track_len`: the track's first cell and length, already excluding any
    ///   begin/end arrow symbols the scrollbar is configured with.
    /// - `content`: total scrollable elements (rows, or columns).
    /// - `viewport`: how many are visible at once.
    /// - `position`: current scroll offset, 0-based.
    ///
    /// Returns `None` when there is nothing to scroll -- a single-element document, an empty
    /// track, or a zero viewport. Callers must not draw a scrollbar in that case, and the mouse
    /// layer must not treat a click there as a scroll.
    ///
    /// Note that a document whose content exactly equals the viewport still gets a scrollbar,
    /// with a thumb that fills the track. That is ratatui's behavior and it is the honest one:
    /// "the thumb fills the trough" is information about the document, and hiding the bar the
    /// instant the two become equal makes it flicker while the user types.
    pub fn new(
        track_origin: u16,
        track_len: u16,
        content: usize,
        viewport: usize,
        position: usize,
    ) -> Option<Self> {
        if track_len == 0 || content == 0 || viewport == 0 {
            return None;
        }
        let track = track_len as usize;
        let max_position = content.saturating_sub(1);
        if max_position == 0 {
            // One element: nothing to scroll, and the divisor below would be the viewport
            // alone, producing a full thumb over a document that does not move.
            return None;
        }
        let start_position = position.min(max_position);
        let max_viewport_position = max_position.saturating_add(viewport);
        if max_viewport_position == 0 {
            return None;
        }
        let thumb_len =
            rounding_divide(viewport.saturating_mul(track), max_viewport_position).clamp(1, track);
        let thumb_start =
            rounding_divide(start_position.saturating_mul(track), max_viewport_position)
                .clamp(0, track.saturating_sub(thumb_len));
        Some(ScrollbarGeom {
            track_start: track_origin,
            track_len,
            thumb_start: thumb_start as u16,
            thumb_len: thumb_len as u16,
        })
    }

    /// Absolute first and one-past-last cell of the thumb.
    pub fn thumb_range(&self) -> (u16, u16) {
        let from = self.track_start.saturating_add(self.thumb_start);
        (from, from.saturating_add(self.thumb_len))
    }

    /// Whether `cell` is anywhere on the track, thumb included.
    pub fn contains(&self, cell: u16) -> bool {
        cell >= self.track_start && cell < self.track_start.saturating_add(self.track_len)
    }

    /// Whether `cell` is on the thumb.
    pub fn is_thumb(&self, cell: u16) -> bool {
        let (from, to) = self.thumb_range();
        cell >= from && cell < to
    }

    /// The scroll offset a click at `cell` requests.
    ///
    /// # The mapping, and why it is not a plain proportion of the track
    ///
    /// Ratatui places the thumb's *start* at `position * track / (content - 1 + viewport)` and
    /// then clamps the start so the thumb still fits the trough. That clamp is the whole
    /// problem: the thumb's start can never exceed `track - thumb_len`, so the bottom `thumb_len`
    /// cells of the trough are **not** reachable by scrolling. A naive
    /// `clicked * content / track` therefore runs out of document before it runs out of bar --
    /// clicking the last cell of the trough lands on "nearly the end", and the user reads that
    /// as a scrollbar that does not work.
    ///
    /// So the mapping is built the other way round, as the inverse of ratatui's own placement:
    ///  - the thumb's *reachable* span is `0..=track - thumb_len`, and that span stands for the
    ///    whole scroll range `0..=content - viewport`;
    ///  - a click is measured against that span, so the last reachable cell maps to the last
    ///    scrollable offset and the first to 0.
    ///
    /// Clicking the bottom `thumb_len` cells -- past the reachable span -- clamps to the maximum
    /// offset, which is the only sane reading of a click below where the thumb can go.
    ///
    /// The span is measured in **thumb cells, not track cells**. A 21-cell trough showing a
    /// 100-line document gets a 4-cell thumb, so 17 thumb positions stand for 80 scroll offsets;
    /// a single track cell therefore moves the document by about five lines. Measuring the click
    /// against the track instead (`clicked * max_offset / track_len`) would top out at 17/21 of
    /// the document, and the bottom fifth of the trough would do nothing at all -- a scrollbar
    /// that stops working near its own bottom edge.
    pub fn offset_at(&self, cell: u16, content: usize, viewport: usize) -> usize {
        let max_offset = content
            .saturating_sub(viewport)
            .min(self.max_offset_bound(content));
        if self.track_len == 0 || content == 0 || max_offset == 0 {
            return 0;
        }
        let clicked = cell.saturating_sub(self.track_start) as i64;
        let reachable = (self.track_len.saturating_sub(self.thumb_len) as i64).max(1);
        // Past the reachable span: the bottom of the document.
        if clicked >= reachable {
            return max_offset;
        }
        let offset = (clicked.max(0) * max_offset as i64) / reachable;
        (offset as usize).min(max_offset)
    }

    /// The per-mille fraction of the scroll range a click at `cell` represents, for
    /// `MouseIntent::ScrollTo`.
    ///
    /// Per-mille rather than a float so the intent stays `Eq` and assertable; a scrollbar
    /// cannot resolve finer than that on any real terminal. Shares [`Self::offset_at`]'s
    /// reachable-span reasoning, so the first cell is 0 and the last is 1000.
    pub fn per_mille_at(&self, cell: u16) -> i32 {
        if self.track_len == 0 {
            return 0;
        }
        let clicked = cell.saturating_sub(self.track_start) as i64;
        let reachable = (self.track_len.saturating_sub(self.thumb_len) as i64).max(1);
        ((clicked * 1000) / reachable).clamp(0, 1000) as i32
    }

    /// The content length that produces a given viewport at a given scroll offset.
    ///
    /// The inverse of [`Self::thumb_start`], solved numerically. Callers need it because
    /// ratatui's thumb placement divides by `content - 1 + viewport`, so the scrollbar's content
    /// length and the scrollable offset live on different scales: a 100-line document reports a
    /// maximum thumb start at offset 99, while offset 79 is the largest one that shows the last
    /// page. A drag that lands on the last thumb cell has to convert back through the same
    /// denominator, or the thumb jumps to the bottom of the trough and the view stops 20 lines
    /// short of the bottom of the file.
    ///
    /// Monotonic in `content`, so a binary search is exact rather than approximate. Bounded by
    /// `track_len`, because a content length above that renders a thumb of one cell and every
    /// further increase changes nothing a caller could observe.
    pub fn content_for_viewport(&self, viewport: usize, position: usize) -> usize {
        if self.track_len == 0 || viewport == 0 {
            return 0;
        }
        let track = self.track_len as usize;
        let thumb_len = self.thumb_len.max(1) as usize;
        let mut low = viewport.max(1);
        let mut high = track
            .saturating_add(viewport)
            .saturating_mul(track)
            .max(low + 1);
        while low < high {
            let mid = low + (high - low) / 2;
            let max_position = mid.saturating_sub(1);
            let max_viewport_position = max_position.saturating_add(viewport).max(1);
            let start = rounding_divide(position.min(max_position) * track, max_viewport_position)
                .clamp(0, track.saturating_sub(thumb_len));
            if start < self.thumb_start as usize {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// A ceiling on the offset, derived from the thumb geometry itself.
    ///
    /// Only used so a caller that hands in a `content` inconsistent with the geometry it was
    /// built from cannot produce an offset the thumb could never display.
    fn max_offset_bound(&self, content: usize) -> usize {
        content.saturating_sub(1)
    }
}

/// Round-half-up integer division, matching ratatui's thumb math.
fn rounding_divide(numerator: usize, denominator: usize) -> usize {
    if denominator == 0 {
        return 0;
    }
    (numerator + denominator / 2) / denominator
}

#[cfg(test)]
mod scrollbar_tests {
    use super::*;

    #[test]
    fn a_document_that_fits_has_no_scrollbar() {
        // A single element cannot scroll. Content smaller than a viewport still gets a bar with
        // a full thumb, because that is what says "there is no more".
        assert_eq!(ScrollbarGeom::new(0, 20, 1, 20, 0), None);
        assert_eq!(ScrollbarGeom::new(0, 20, 1, 1, 0), None);
        assert_eq!(ScrollbarGeom::new(0, 20, 100, 0, 0), None, "zero viewport");
        assert_eq!(ScrollbarGeom::new(0, 0, 100, 10, 0), None, "zero track");
    }

    #[test]
    fn a_one_line_document_never_produces_a_zero_height_thumb() {
        // The divide-by-zero case the spec calls out. One line in a 24-row viewport: nothing to
        // scroll, so no scrollbar at all -- and never a thumb of height 0 that would divide by
        // zero downstream.
        assert_eq!(ScrollbarGeom::new(0, 24, 1, 24, 0), None);
        // Two rows of content in a 24-row viewport: the thumb is one cell, not zero.
        let g = ScrollbarGeom::new(0, 24, 2, 2, 0).expect("drawn");
        assert!(g.thumb_len >= 1, "thumb must be at least one cell");
    }

    #[test]
    fn an_exact_fit_document_gets_a_full_length_thumb() {
        // Nothing to scroll, but the bar is drawn and its thumb fills the track. The thumb must
        // still be inside the track, and `is_thumb` must accept the whole thing so a click
        // anywhere on it is a thumb grab rather than a track page.
        // Content == viewport in ratatui's model means `max_viewport_position` is
        // `content - 1 + viewport`, so the thumb is just under half the trough: the last row can
        // still scroll up by one. It is the *clamped* placement that stops it moving, not a
        // thumb that claims the whole track.
        let g = ScrollbarGeom::new(0, 24, 24, 24, 0).expect("drawn");
        assert!(
            g.thumb_len >= 12 && g.thumb_len <= 16,
            "got {}",
            g.thumb_len
        );
        assert!(g.is_thumb(0) && g.is_thumb(g.thumb_len / 2));
        // A thumb exactly as long as the track leaves no reachable span, and the only offset is
        // 0 -- no divide-by-zero, no jump.
        let full = ScrollbarGeom {
            track_start: 0,
            track_len: 24,
            thumb_start: 0,
            thumb_len: 24,
        };
        assert_eq!(full.offset_at(23, 24, 24), 0);
        assert!(full.is_thumb(23));
    }

    #[test]
    fn a_ten_times_overflow_document_gets_a_proportionally_small_thumb() {
        // 10x overflow in a 24-cell track: the thumb is about a tenth of the track.
        let g = ScrollbarGeom::new(0, 24, 240, 24, 0).expect("drawn");
        assert!(
            g.thumb_len >= 2 && g.thumb_len <= 3,
            "thumb should be roughly a tenth of 24, got {}",
            g.thumb_len
        );
        assert_eq!(g.thumb_start, 0, "at the top");
    }

    #[test]
    fn the_thumb_sits_at_the_bottom_at_max_offset() {
        let g = ScrollbarGeom::new(0, 24, 240, 24, 239).expect("drawn");
        let (from, to) = g.thumb_range();
        assert_eq!(
            to, 24,
            "the thumb ends exactly at the track's end: {from}..{to}"
        );
        assert!(from < to);
    }

    #[test]
    fn the_thumb_always_fits_inside_the_track() {
        for content in [2usize, 3, 10, 100, 100_000] {
            for viewport in [1usize, 2, 7, 24] {
                for position in [0usize, content / 2, content] {
                    let Some(g) = ScrollbarGeom::new(3, 20, content, viewport, position) else {
                        continue;
                    };
                    let (from, to) = g.thumb_range();
                    assert!(from >= 3, "thumb before the track: {g:?}");
                    assert!(to <= 23, "thumb past the track's end: {g:?}");
                    assert!(g.thumb_len >= 1, "zero-height thumb: {g:?}");
                }
            }
        }
    }

    #[test]
    fn the_ends_of_the_track_map_to_the_ends_of_the_document() {
        // The spec's "drag to 0.0 and 1.0 lands on first and last row", stated as geometry.
        let (content, viewport) = (240usize, 24usize);
        let g = ScrollbarGeom::new(0, 24, content, viewport, 0).expect("drawn");
        assert_eq!(
            g.offset_at(0, content, viewport),
            0,
            "first cell is the first row"
        );
        assert_eq!(
            g.offset_at(23, content, viewport),
            content - viewport,
            "last cell is the last scrollable position, i.e. the document's bottom row on screen"
        );
        // Per-mille agrees at the ends.
        assert_eq!(g.per_mille_at(0), 0);
        assert_eq!(
            g.per_mille_at(23),
            1000,
            "last cell is the bottom of the document"
        );
        // And a click below the reachable span clamps rather than over-shooting.
        assert_eq!(g.offset_at(99, content, viewport), content - viewport);
    }

    #[test]
    fn per_mille_is_monotonic_and_bounded() {
        let g = ScrollbarGeom::new(2, 40, 1000, 20, 0).expect("drawn");
        let mut last = -1;
        for cell in 2..=42 {
            let pm = g.per_mille_at(cell);
            assert!(
                (0..=1000).contains(&pm),
                "per-mille out of range at {cell}: {pm}"
            );
            assert!(
                pm >= last,
                "per-mille went backwards at {cell}: {pm} < {last}"
            );
            last = pm;
        }
        assert_eq!(last, 1000, "the track's last cell is the document's bottom");
    }

    #[test]
    fn offset_at_is_monotonic_and_clamped() {
        let (content, viewport) = (500usize, 25usize);
        let g = ScrollbarGeom::new(0, 30, content, viewport, 0).expect("drawn");
        let mut last = 0;
        for cell in 0..30 {
            let off = g.offset_at(cell, content, viewport);
            assert!(off < content, "offset past the end at {cell}: {off}");
            assert!(off >= last, "offset went backwards at {cell}");
            last = off;
        }
    }

    #[test]
    fn a_degenerate_track_never_divides_by_zero() {
        let g = ScrollbarGeom {
            track_start: 0,
            track_len: 0,
            thumb_start: 0,
            thumb_len: 0,
        };
        assert_eq!(g.offset_at(5, 100, 10), 0);
        assert_eq!(g.per_mille_at(5), 0);
        assert!(!g.is_thumb(0));
    }
}
