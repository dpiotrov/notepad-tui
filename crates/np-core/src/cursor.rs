//! Cursor, selection, and scroll state.
//!
//! Positions are always [`LineCol`] (character offsets). A `Selection` stores an anchor
//! and a focus rather than a sorted pair, so extending a selection leftwards keeps
//! behaving like Notepad++ (the caret stays at the focus).

use crate::types::LineCol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub pos: LineCol,
    /// Preferred display column for vertical movement (spec FR-E2). `None` means
    /// "stick to the nearest valid column", which is what a plain arrow press sets.
    pub goal_col: Option<usize>,
}

impl Cursor {
    pub fn at(pos: LineCol) -> Self {
        Cursor {
            pos,
            goal_col: None,
        }
    }
}

/// Range selection or rectangular block (spec FR-E3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionKind {
    #[default]
    Range,
    Block,
}

/// Selection between `anchor` (where it started) and `focus` (where the caret is).
///
/// Either endpoint may come first in document order; use [`Selection::bounds`] when you
/// need a normalized pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: LineCol,
    pub focus: LineCol,
    pub kind: SelectionKind,
}

impl Selection {
    pub fn range(anchor: LineCol, focus: LineCol) -> Self {
        Selection {
            anchor,
            focus,
            kind: SelectionKind::Range,
        }
    }

    pub fn block(anchor: LineCol, focus: LineCol) -> Self {
        Selection {
            anchor,
            focus,
            kind: SelectionKind::Block,
        }
    }

    /// Normalized `(start, end)` in document order, regardless of drag direction.
    pub fn bounds(&self) -> (LineCol, LineCol) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// True when the selection covers no characters.
    ///
    /// For `Block` this means "no area": the empty case is a single point, so a
    /// zero-*height* block that spans columns still covers text and is not empty.
    /// Copy/cut with a truly empty selection falls back to the whole line (FR-C1).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// True when the selection occupies zero visual area.
    ///
    /// A `Block` confined to one line has no area even when it spans columns: there is
    /// nothing to copy as a rectangle, and block paste should behave like a plain
    /// insert. Distinct from [`Selection::is_empty`], which drives the line-copy
    /// fallback.
    pub fn is_degenerate_block(&self) -> bool {
        self.kind == SelectionKind::Block && self.anchor.line == self.focus.line
    }

    /// Number of logical lines the selection touches. Used by the "In selection"
    /// auto-check, which fires at >= 2 lines (spec FR-S1).
    pub fn line_span(&self) -> usize {
        let (s, e) = self.bounds();
        e.line - s.line + 1
    }

    /// True if `pos` lies within the selection's bounding box. For `Block` this is the
    /// rectangle, not the swept range.
    pub fn contains(&self, pos: LineCol) -> bool {
        let (s, e) = self.bounds();
        match self.kind {
            SelectionKind::Range => pos >= s && pos < e,
            SelectionKind::Block => {
                if pos.line < s.line || pos.line >= e.line {
                    return false;
                }
                let lo = s.col.min(e.col);
                let hi = s.col.max(e.col);
                pos.col >= lo && pos.col < hi
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrollPos {
    /// First visible logical line.
    pub top_line: usize,
    /// First visible character column; forced to 0 while word wrap is on (FR-V1).
    pub left_col: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lc(line: usize, col: usize) -> LineCol {
        LineCol::new(line, col)
    }

    #[test]
    fn bounds_normalizes_both_directions() {
        let forward = Selection::range(lc(1, 2), lc(3, 5));
        let backward = Selection::range(lc(3, 5), lc(1, 2));
        assert_eq!(forward.bounds(), (lc(1, 2), lc(3, 5)));
        assert_eq!(backward.bounds(), (lc(1, 2), lc(3, 5)));
    }

    #[test]
    fn same_line_selection_normalizes_by_column() {
        let s = Selection::range(lc(2, 7), lc(2, 3));
        assert_eq!(s.bounds(), (lc(2, 3), lc(2, 7)));
    }

    #[test]
    fn empty_only_when_endpoints_identical() {
        assert!(Selection::range(lc(1, 1), lc(1, 1)).is_empty());
        assert!(!Selection::range(lc(1, 1), lc(1, 2)).is_empty());
        // Different line, same column: still covers a line break.
        assert!(!Selection::range(lc(1, 0), lc(2, 0)).is_empty());
        // A one-line block dragged across columns still covers text, so it is not
        // "empty" for the FR-C1 line-copy fallback -- but it has no rectangle area.
        let one_line_block = Selection::block(lc(1, 0), lc(1, 4));
        assert!(!one_line_block.is_empty());
        assert!(one_line_block.is_degenerate_block());
        // A multi-line block with columns is a real rectangle.
        let real_block = Selection::block(lc(1, 0), lc(3, 4));
        assert!(!real_block.is_empty());
        assert!(!real_block.is_degenerate_block());
        // A block dragged to a single point is both.
        let point = Selection::block(lc(2, 3), lc(2, 3));
        assert!(point.is_empty());
        assert!(point.is_degenerate_block());
    }

    #[test]
    fn line_span_counts_touched_lines() {
        assert_eq!(Selection::range(lc(0, 3), lc(0, 9)).line_span(), 1);
        assert_eq!(Selection::range(lc(0, 3), lc(1, 0)).line_span(), 2);
        assert_eq!(Selection::range(lc(5, 0), lc(1, 0)).line_span(), 5);
    }

    #[test]
    fn range_contains_is_half_open() {
        let s = Selection::range(lc(1, 2), lc(2, 2));
        assert!(!s.contains(lc(1, 1)));
        assert!(s.contains(lc(1, 2)));
        assert!(s.contains(lc(1, 9)));
        assert!(s.contains(lc(2, 0)));
        assert!(!s.contains(lc(2, 2)), "end must be exclusive");
    }

    #[test]
    fn block_contains_uses_the_rectangle() {
        // Dragged right-to-left, top-to-bottom.
        let s = Selection::block(lc(1, 6), lc(3, 2));
        assert!(s.contains(lc(2, 3)));
        assert!(!s.contains(lc(2, 7)), "outside the column span");
        assert!(!s.contains(lc(0, 3)), "above the box");
        assert!(!s.contains(lc(3, 3)), "end line exclusive");
    }

    #[test]
    fn cursor_default_has_no_goal() {
        let c = Cursor::default();
        assert_eq!(c.pos, LineCol::default());
        assert_eq!(c.goal_col, None);
    }
}
