//! Rope-backed text buffer with correct grapheme and wide-character column math.
//!
//! Two coordinate systems exist and must not be conflated:
//!  - **char coordinates** (`LineCol.col`) index Unicode scalar values, and are what the
//!    model, undo, and search use.
//!  - **display columns** count terminal cells after tab expansion, and are what the
//!    mouse and renderer use.
//!
//! Conversion goes through [`Buffer::display_col`] and [`Buffer::col_from_display`],
//! both of which are grapheme-cluster safe: a caret can never land inside a cluster, and
//! the trailing half of a double-width cell resolves to that same character.
//!
//! This module knows nothing about files, encodings, or undo.

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::types::LineCol;

/// A `Buffer` is a `Rope` plus the invariants below.
///
/// Invariants:
///  - `line_count()` is always >= 1; an empty document is one empty line.
///  - Line contents never contain a line terminator. Ropey splits on `\r`, `\n`, `\r\n`
///    and several Unicode breaks, so a lone `\r` would otherwise become a line break and
///    destroy the per-line EOL model (spec FR-N4). [`Buffer::from_str_lossy`]
///    normalizes incoming text to `\n` for exactly that reason; S5 records the original
///    endings alongside the buffer.
///  - Every method taking a `LineCol` clamps instead of panicking.
pub struct Buffer {
    rope: Rope,
    /// Bumped on every mutation. See [`Buffer::revision`].
    revision: u64,
    /// Memoized line widths, powering [`Buffer::max_line_width`].
    ///
    /// Interior-mutable so the query stays `&self`: the scrollbar geometry that needs it is
    /// computed while the renderer holds the document immutably.
    width_rows: std::cell::RefCell<WidthRows>,
    /// Memoized answer to `view::total_row_count`, keyed the same way. See there.
    row_cache: std::cell::Cell<Option<RowCountCache>>,
    /// Lowest line index touched by mutations since the last reset, or `usize::MAX` for
    /// "nothing edited". See [`Buffer::dirty_from`].
    dirty_from: std::cell::Cell<usize>,
}

/// Per-line display widths, tagged with the revision and tab width they were measured at.
#[derive(Debug, Clone, Default)]
struct WidthRows {
    revision: u64,
    tab_width: usize,
    widths: Vec<usize>,
    /// Widest line among `widths`, valid only when `revision` and `tab_width` still match.
    cells: usize,
}

/// A memoized whole-document row count, keyed on everything that changes the answer.
///
/// Lives here rather than in `view` because the cache has to travel with the buffer it
/// describes. Public so `view::total_row_count` can read and write it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowCountCache {
    pub revision: u64,
    pub text_width: u16,
    pub text_height: u16,
    pub tab_width: usize,
    pub word_wrap: bool,
    /// Fingerprint of the fold set; see `view::fold_signature`.
    pub fold_signature: u64,
    pub rows: usize,
}

impl std::fmt::Debug for Buffer {
    /// Short form on purpose: dumping a whole rope into a `{:?}` during a test failure can
    /// emit hundreds of megabytes, so the summary carries what identifies the buffer instead.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffer")
            .field("lines", &self.line_count())
            .field("chars", &self.char_count())
            .field("revision", &self.revision)
            .finish()
    }
}

impl Clone for Buffer {
    /// Hand-written so a clone copies the revision rather than resetting it.
    ///
    /// A derived impl would be correct for the data but wrong for the purpose: a buffer
    /// cloned into an undo snapshot would come back with revision 0 and look edited, which
    /// would throw away the highlight cache on every undo.
    fn clone(&self) -> Self {
        Buffer {
            rope: self.rope.clone(),
            revision: self.revision,
            width_rows: std::cell::RefCell::new(self.width_rows.borrow().clone()),
            row_cache: std::cell::Cell::new(self.row_cache.get()),
            dirty_from: std::cell::Cell::new(self.dirty_from.get()),
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            rope: Rope::new(),
            revision: 0,
            width_rows: std::cell::RefCell::new(WidthRows::default()),
            row_cache: std::cell::Cell::new(None),
            dirty_from: std::cell::Cell::new(usize::MAX),
        }
    }
}

/// Normalize every flavor of line terminator to `\n`, remembering nothing. S5 keeps the
/// original per-line endings; the buffer itself stays single-line-per-rope-line.
fn normalize_newlines(text: &str) -> String {
    // Fast path: the overwhelmingly common case is pure LF with no other breaks.
    if !text.contains(['\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}', '\u{2029}']) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF is one break, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            other => out.push(other),
        }
    }
    out
}

impl Buffer {
    pub fn new() -> Self {
        Buffer {
            rope: Rope::new(),
            revision: 0,
            width_rows: std::cell::RefCell::new(WidthRows::default()),
            row_cache: std::cell::Cell::new(None),
            dirty_from: std::cell::Cell::new(usize::MAX),
        }
    }

    /// Build a buffer from text, normalizing line terminators to `\n`.
    pub fn from_str_lossy(text: &str) -> Self {
        Buffer {
            rope: Rope::from_str(&normalize_newlines(text)),
            revision: 0,
            width_rows: std::cell::RefCell::new(WidthRows::default()),
            row_cache: std::cell::Cell::new(None),
            dirty_from: std::cell::Cell::new(usize::MAX),
        }
    }

    /// The underlying rope, for read-only consumers that need chunk access.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    // ---------------------------------------------------------------- sizes

    /// Always at least 1.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    /// Character length of `line`, excluding its terminator. Out-of-range yields 0.
    pub fn line_len_chars(&self, line: usize) -> usize {
        match self.rope.get_line(line) {
            Some(slice) => slice.len_chars() - terminator_chars(slice),
            None => 0,
        }
    }

    /// A counter that changes on every mutation.
    ///
    /// Exists so derived caches can notice staleness without being told. The alternative is
    /// every mutating call site remembering to invalidate, and a document has a dozen of them
    /// -- insert, delete, paste, sort, comment, case, indent, undo, redo, reload. Missing one
    /// yields a stale highlight that looks like a theme bug and cannot be reproduced from the
    /// keystrokes. Comparing a counter before drawing makes correctness the default.
    ///
    /// Wraps rather than panicking. A wrap would cost one needless re-highlight once per
    /// 2^64 edits, which is not a number a text editor reaches.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The single place a mutation records itself.
    ///
    /// Marks accumulate until a consumer resets the range, so a burst of edits between two
    /// frames reports the earliest line any of them touched.
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Record that a mutation touched `line`, keeping the lowest such line.
    ///
    /// Every mutating `Buffer` method calls this, which is why an invalidation cannot be
    /// forgotten the way a per-call-site notification would be.
    pub fn mark_dirty_line(&self, line: usize) {
        if line < self.dirty_from.get() {
            self.dirty_from.set(line);
        }
    }

    /// The lowest line any mutation has touched since the last reset.
    ///
    /// `None` means nothing has been edited, so a derived cache is still exactly right.
    /// `Some(line)` is conservative: from `line` up may be stale, below it is provably
    /// untouched. The unset value is "no information" rather than line 0 so a consumer cannot
    /// mistake the default for "everything changed" and throw away the benefit.
    pub fn dirty_from(&self) -> Option<usize> {
        let line = self.dirty_from.get();
        if line == usize::MAX {
            None
        } else {
            Some(line)
        }
    }

    /// Forget the dirty range. Called by `App` once per frame, after every consumer has had
    /// its chance to use it.
    ///
    /// Resetting is deliberately not done by the consumers themselves: the width cache, the
    /// row-count cache and the highlight store all need the same answer for one edit, and the
    /// first of them to reset would leave the others re-walking the whole document. One reset
    /// per frame, by the frame's owner.
    ///
    /// Only [`Buffer::mark_dirty_line`] re-arms it. A mutation path that never calls that
    /// leaves the range unset and each consumer recomputes in full rather than serving a stale
    /// answer.
    pub fn clear_dirty_range(&self) {
        self.dirty_from.set(usize::MAX);
    }

    /// The memoized whole-document row count, if one is stored.
    pub(crate) fn row_count_cache(&self) -> Option<RowCountCache> {
        self.row_cache.get()
    }

    /// Store the memoized whole-document row count.
    pub(crate) fn set_row_count_cache(&self, value: RowCountCache) {
        self.row_cache.set(Some(value));
    }

    /// Total character count of the document.
    pub fn char_count(&self) -> usize {
        self.rope.len_chars()
    }

    /// Borrowed view of `line` when it lives in a single chunk, else `None`.
    /// Callers that must always have text use [`Buffer::line_string`].
    pub fn line(&self, line: usize) -> Option<&str> {
        self.rope
            .get_line(line)
            .and_then(|s| s.as_str())
            .map(strip_terminator)
    }

    /// Owned copy of `line` without its terminator. Out-of-range yields `String::new()`.
    pub fn line_string(&self, line: usize) -> String {
        match self.rope.get_line(line) {
            Some(slice) => {
                let end = slice.len_chars() - terminator_chars(slice);
                if end == slice.len_chars() {
                    slice.to_string()
                } else {
                    slice.slice(0..end).to_string()
                }
            }
            None => String::new(),
        }
    }

    // ------------------------------------------------------------ offsets

    /// Absolute char offset of `lc`, or `None` if it is out of range.
    ///
    /// `col` beyond the end of the line is accepted (it addresses the line break), which
    /// is what the caret needs while typing at EOL.
    pub fn char_offset(&self, lc: LineCol) -> Option<usize> {
        if lc.line >= self.line_count() {
            return None;
        }
        let len = self.line_len_chars(lc.line);
        if lc.col > len {
            return None;
        }
        Some(self.rope.line_to_char(lc.line) + lc.col)
    }

    /// Absolute char offset, clamped so it always succeeds.
    fn char_offset_clamped(&self, lc: LineCol) -> usize {
        let line = lc.line.min(self.line_count() - 1);
        let len = self.line_len_chars(line);
        let start = self.rope.line_to_char(line);
        start + lc.col.min(len)
    }

    /// Inverse of [`Buffer::char_offset`]. Offsets past the end clamp to end of document.
    pub fn line_col(&self, offset: usize) -> LineCol {
        let capped = offset.min(self.rope.len_chars());
        let line = self.rope.char_to_line(capped);
        LineCol::new(line, capped - self.rope.line_to_char(line))
    }

    /// Clamp an arbitrary position into the document. Total: never panics.
    pub fn clamp(&self, lc: LineCol) -> LineCol {
        let line = lc.line.min(self.line_count() - 1);
        LineCol::new(line, lc.col.min(self.line_len_chars(line)))
    }

    // ------------------------------------------------------------- widths

    /// Display width of `line` with tabs expanded to `tab_width` stops.
    ///
    /// A tab at display column `c` advances to `((c / tab_width) + 1) * tab_width`, so
    /// `"\ta"` is 5 cells wide at `tab_width = 4`, not 4.
    pub fn display_width(&self, line: usize, tab_width: usize) -> usize {
        self.display_width_range(line, 0, self.line_len_chars(line), tab_width)
    }

    /// Widest line in the document, in display cells.
    ///
    /// O(document) on the first call and O(1) afterwards, until the buffer is edited or
    /// `tab_width` changes. The horizontal scrollbar needs this on every frame, and the
    /// uncached walk measured 2.5 s on a 100 MB file -- which, multiplied by one per frame,
    /// is the difference between a usable editor and a hang. S15 spec §9 found this.
    ///
    /// Keyed on [`Buffer::revision`] rather than invalidated by call sites, for the reason
    /// given there: a dozen mutating paths and only one counter to remember. A tab-width
    /// change alters the answer without altering the text, so it is part of the key.
    pub fn max_line_width(&self, tab_width: usize) -> usize {
        let tab_width = tab_width.max(1);
        let lines = self.line_count();
        // Exact hit: nothing has changed since the last measurement.
        if let Some(hit) = self
            .width_rows
            .borrow()
            .matches(self.revision, tab_width, lines)
        {
            return hit;
        }
        // Partial reuse. The buffer reports the lowest line any edit touched, so every line
        // below it still has the width it was measured at and only the tail is re-walked.
        //
        // Reuse requires the previous pass to be at the current revision *before* this edit --
        // that is, `rows.revision` is an ancestor of `self.revision` with nothing measured in
        // between -- and requires the reported line to be a valid index. Anything else falls
        // back to a full walk, which is the correct-if-slow default: a mutation path that
        // never called `mark_dirty_line` yields `dirty_from() == None`, and `None` here means
        // "no information", so the whole document is re-measured rather than trusted.
        let dirty = self.dirty_from();
        let mut rows = self.width_rows.borrow_mut();
        // The rows on hand were measured at some revision *before* this one, which is what
        // makes the prefix below the edited line reusable. Captured first, because the
        // assignment a few lines down overwrites it.
        let measured_at = rows.revision;
        let from = match dirty {
            Some(line)
                if rows.tab_width == tab_width
                    && measured_at < self.revision
                    && !rows.widths.is_empty() =>
            {
                line.min(rows.widths.len())
            }
            _ => 0,
        };
        rows.revision = self.revision;
        rows.tab_width = tab_width;
        // A shorter document drops the stale tail; a longer one gains zeroed entries that the
        // loop below always overwrites.
        rows.widths.resize(lines, 0);
        let mut widest = 0usize;
        for line in 0..from {
            if rows.widths[line] > widest {
                widest = rows.widths[line];
            }
        }
        for line in from..lines {
            let w = self.display_width_range(line, 0, self.line_len_chars(line), tab_width);
            rows.widths[line] = w;
            if w > widest {
                widest = w;
            }
        }
        rows.cells = widest;
        widest
    }

    /// Display cells occupied by `line[from..to]`.
    ///
    /// Public because the view layer needs exactly this: a wrapped screen row owns a slice of
    /// its logical line, and mapping a click inside that row means knowing how many cells the
    /// row's leading slice consumes. `display_width` cannot express it, and neither can a
    /// caller without re-implementing tab stops and grapheme widths.
    pub fn display_width_range(
        &self,
        line: usize,
        from: usize,
        to: usize,
        tab_width: usize,
    ) -> usize {
        let tab_width = tab_width.max(1);
        let text = self.line_string(line);
        let mut cells = 0;
        for (i, grapheme) in graphemes_of(&text).enumerate() {
            if i < from {
                continue;
            }
            if i >= to {
                break;
            }
            if grapheme == "\t" {
                cells += tab_stop_advance(cells, tab_width);
            } else {
                cells += grapheme_width(grapheme);
            }
        }
        cells
    }

    /// Display column of char index `col` on `line`.
    ///
    /// A wide character occupies two cells; its **start** is what this returns, so the
    /// caret sits on the left half.
    pub fn display_col(&self, line: usize, col: usize, tab_width: usize) -> usize {
        let col = col.min(self.line_len_chars(line));
        self.display_width_range(line, 0, col, tab_width)
    }

    /// Nearest valid char index to display column `display_col` on `line`.
    ///
    /// Never lands inside a grapheme cluster. For a double-width character, both its
    /// own cell and the trailing half-cell resolve to that character, so clicking the
    /// right half of a CJK glyph puts the caret before that glyph rather than after it.
    pub fn col_from_display(&self, line: usize, display_col: usize, tab_width: usize) -> usize {
        if display_col == 0 {
            return 0;
        }
        let text = self.line_string(line);
        let tab_width = tab_width.max(1);
        let mut cells = 0;
        let mut chars = 0;
        for grapheme in graphemes_of(&text) {
            let w = if grapheme == "\t" {
                tab_stop_advance(cells, tab_width)
            } else {
                grapheme_width(grapheme)
            };
            // The click lands on this grapheme if it starts at or before the target
            // column and the target falls inside the cells it occupies.
            if cells + w > display_col {
                return if w == 0 {
                    // Zero-width cluster: keep walking, attach to the next real cell.
                    chars += grapheme.chars().count();
                    continue;
                } else {
                    chars
                };
            }
            cells += w;
            chars += grapheme.chars().count();
        }
        // Past the last cell: clamp to end of line.
        self.line_len_chars(line)
    }

    // ------------------------------------------------------------- edits

    /// Insert `text` at `lc`, normalizing line terminators to `\n`.
    pub fn insert(&mut self, lc: LineCol, text: &str) {
        let lc = self.clamp(lc);
        self.mark_dirty_line(lc.line);
        let offset = self.char_offset_clamped(lc);
        self.rope.insert(offset, &normalize_newlines(text));
        self.bump();
    }

    /// Delete the half-open range between two positions, in either order.
    ///
    /// Order-independent because a selection dragged right-to-left hands us `focus`
    /// before `anchor` (see [`Selection::bounds`](np_core::Selection::bounds)).
    pub fn delete_range(&mut self, from: LineCol, to: LineCol) {
        let from = self.clamp(from);
        let to = self.clamp(to);
        self.mark_dirty_line(from.line.min(to.line));
        let a = self.char_offset_clamped(from);
        let b = self.char_offset_clamped(to);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if lo < hi {
            self.rope.remove(lo..hi);
            self.bump();
        }
    }

    /// Insert `text` as a new line above `line`, terminated by `\n`.
    pub fn insert_line(&mut self, line: usize, text: &str) {
        let line = line.min(self.line_count() - 1);
        self.mark_dirty_line(line);
        let offset = self.rope.line_to_char(line);
        let mut payload = normalize_newlines(text);
        payload.push('\n');
        self.rope.insert(offset, &payload);
        self.bump();
    }

    /// Remove `line` and its terminator. The last line has none, so removing the final
    /// line deletes the preceding terminator instead, keeping the document well-formed.
    pub fn remove_line(&mut self, line: usize) {
        self.mark_dirty_line(line.min(self.line_count().saturating_sub(1)));
        if self.line_count() <= 1 {
            self.rope.remove(0..self.rope.len_chars());
            self.bump();
            return;
        }
        let line = line.min(self.line_count() - 1);
        let start = self.rope.line_to_char(line);
        let end = if line + 1 < self.line_count() {
            self.rope.line_to_char(line + 1)
        } else {
            start.saturating_sub(1)
        };
        if start < end {
            self.rope.remove(start..end);
            self.bump();
        } else if line == 0 {
            self.rope.remove(0..end);
            self.bump();
        }
    }

    // ------------------------------------------------------ word motions

    /// Start of the word at or before `lc` (Ctrl+Left).
    pub fn word_start_before(&self, lc: LineCol) -> LineCol {
        let line = lc.line.min(self.line_count() - 1);
        let text = self.line_string(line);
        let chars: Vec<char> = text.chars().collect();
        let mut i = lc.col.min(chars.len());
        if i == 0 {
            return LineCol::new(line, 0);
        }
        // Skip separators immediately before the caret, then the identifier itself.
        while i > 0 && !is_word_char(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word_char(chars[i - 1]) {
            i -= 1;
        }
        LineCol::new(line, i)
    }

    /// End of the word after `lc` (Ctrl+Right).
    pub fn word_end_after(&self, lc: LineCol) -> LineCol {
        let line = lc.line.min(self.line_count() - 1);
        let text = self.line_string(line);
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = lc.col.min(len);
        if i >= len {
            return LineCol::new(line, len);
        }
        while i < len && !is_word_char(chars[i]) {
            i += 1;
        }
        while i < len && is_word_char(chars[i]) {
            i += 1;
        }
        LineCol::new(line, i)
    }

    /// Half-open char range of the word under `lc`.
    ///
    /// Returns an empty range at `lc` when the position is not on a word, so callers can
    /// distinguish "no word" from "word at offset 0".
    pub fn word_range_at(&self, lc: LineCol) -> (LineCol, LineCol) {
        let line = lc.line.min(self.line_count() - 1);
        let text = self.line_string(line);
        let chars: Vec<char> = text.chars().collect();
        let col = lc.col.min(chars.len());
        // A trailing half-cell can put us one past a word; step back onto it.
        let probe = if col > 0 && col >= chars.len() {
            col - 1
        } else {
            col
        };
        if !chars.get(probe).copied().is_some_and(is_word_char) {
            let at = LineCol::new(line, col);
            return (at, at);
        }
        let start = self.word_start_before(LineCol::new(line, probe + 1)).col;
        let end = self.word_end_after(LineCol::new(line, probe)).col;
        (LineCol::new(line, start), LineCol::new(line, end))
    }

    // --------------------------------------------------------------- I/O

    /// Iterate the document as `&str` chunks without materializing it (spec FR-E1).
    /// Used by the streaming save path in S5.
    pub fn as_str_chunks(&self) -> impl Iterator<Item = &str> {
        self.rope.chunks()
    }

    /// Whole document as a `String`.
    ///
    /// Only for tests, session persistence of small untitled buffers (FR-CG3), and
    /// search backends that need a contiguous string. Never call this on a hot path.
    pub fn text_string(&self) -> String {
        self.rope.to_string()
    }
}

// ---------------------------------------------------------------- helpers

/// Identifier characters: alphanumeric plus `_`, per spec S1's definition.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Cells a tab consumes when starting at display column `col`.
fn tab_stop_advance(col: usize, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    ((col / tab_width) + 1) * tab_width - col
}

/// Display width of a grapheme cluster, in terminal cells.
///
/// Uses `UnicodeWidthStr` on the whole cluster rather than summing scalar widths, for
/// two reasons:
///  - It matches **ratatui**, whose `CellWidth` for `str` is `UnicodeWidthStr::width`.
///    Summing scalars instead would report 7 for a ZWJ family emoji while the renderer
///    writes 3, desynchronizing mouse hit-testing from what is on screen.
///  - It handles ZWJ sequences and regional-indicator flags as single glyphs, which is
///    the correct model for a caret stop.
///
/// Zero-width clusters (a lone combining mark) still occupy the cell of their base
/// character, so this floors at 1. Tabs never reach here; the caller expands them.
pub(crate) fn grapheme_width(grapheme: &str) -> usize {
    let w = grapheme.width();
    if w == 0 {
        1
    } else {
        w
    }
}

/// Grapheme clusters of a line. Ropey line slices may span chunks, so this operates on
/// an owned string; callers hold the line already.
pub(crate) fn graphemes_of(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// Character offset of a line's terminator, 0 when the line has none.
///
/// Inspects bytes directly: ropey's `Chars` iterator is not double-ended, and after
/// S1's normalization the only terminator reaching here is `\n`.
fn terminator_chars(slice: ropey::RopeSlice<'_>) -> usize {
    let bytes = slice.len_bytes();
    if bytes == 0 || slice.byte(bytes - 1) != b'\n' {
        return 0;
    }
    if bytes >= 2 && slice.byte(bytes - 2) == b'\r' {
        2
    } else {
        1
    }
}

/// Strip a trailing `\n` from a borrowed line slice.
fn strip_terminator(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

// ------------------------------------------------------------ graphemes

/// The character span of the grapheme cluster starting at or after `col` on `line`.
///
/// Overtype (spec FR-E12) has to replace a whole cluster, not one scalar value: `e` plus a
/// combining acute is one visible character made of two `char`s, and replacing only the
/// first leaves the accent attached to whatever was typed next. Returns `(start, end)` in
/// char indices, or `None` when `col` is at or past the end of the line.
pub fn grapheme_range(buf: &Buffer, line: usize, col: usize) -> Option<(usize, usize)> {
    let text = buf.line_string(line);
    let len = text.chars().count();
    if col >= len {
        return None;
    }
    let byte_start = text.char_indices().nth(col)?.0;
    let cluster = text[byte_start..].graphemes(true).next()?;
    let byte_end = byte_start + cluster.len();
    let end_col = text[..byte_end].chars().count();
    Some((col, end_col))
}

/// The char index of the last grapheme boundary strictly left of `col`, or `None` at 0.
///
/// Backspace and `Shift+Left` need this. Counting one `char` instead deletes half of an
/// accent or lands the caret between a base character and its mark, which is the failure
/// spec §8.7 tests for. The result is always a cluster boundary: a `col` sitting mid-cluster
/// -- which mouse clicks and pastes can produce -- steps back to the boundary that precedes
/// it, so `prev_grapheme_col(n) == prev_grapheme_col(cluster_start_of(n))`.
pub fn prev_grapheme_col(buf: &Buffer, line: usize, col: usize) -> Option<usize> {
    if col == 0 {
        return None;
    }
    let text = strip_terminator(&buf.line_string(line)).to_string();
    let mut prev = 0;
    let mut running = 0;
    for cluster in graphemes_of(&text) {
        if running >= col {
            break;
        }
        prev = running;
        running += cluster.chars().count();
    }
    Some(prev)
}

/// The char index of the first grapheme boundary strictly right of `col`, or `None` at the
/// line end.
///
/// The mirror of [`prev_grapheme_col`], and the inverse in the useful sense: for every
/// interior boundary `b`, `prev_grapheme_col(next_grapheme_col(b)) == b`. A `col` inside a
/// cluster advances to that cluster's end rather than stepping one scalar.
pub fn next_grapheme_col(buf: &Buffer, line: usize, col: usize) -> Option<usize> {
    let text = buf.line_string(line);
    let len = strip_terminator(&text).chars().count();
    if col >= len {
        return None;
    }
    let mut running = 0;
    for cluster in graphemes_of(strip_terminator(&text)) {
        let next = running + cluster.chars().count();
        if next > col {
            return Some(next);
        }
        running = next;
    }
    Some(len)
}

// --------------------------------------------------------- block selections

/// A rectangular region, measured in **display cells**.
///
/// Cells rather than char indices because a block selection is defined by what the user
/// dragged across the screen. With the left edge in cells, every row of the rectangle
/// starts at the same screen column even when the lines contain tabs or CJK text, which is
/// the property FR-E3 asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRect {
    pub first_line: usize,
    pub rows: usize,
    /// Left edge in display cells.
    pub col: usize,
    /// Width in display cells.
    pub width: usize,
    /// Tab stop width used for the cell math.
    pub tab_width: usize,
}

impl BlockRect {
    pub fn new(first_line: usize, rows: usize, col: usize, width: usize, tab_width: usize) -> Self {
        BlockRect {
            first_line,
            rows,
            col,
            width,
            tab_width: tab_width.max(1),
        }
    }

    /// Last line the rectangle touches, not yet clamped to the buffer.
    fn last_line(&self) -> usize {
        self.first_line.saturating_add(self.rows.saturating_sub(1))
    }
}

/// Rectangle text for `rect`, each row exactly `rect.width` display cells (spec FR-E3).
///
/// A row shorter than the rectangle is padded with spaces so the block keeps its shape when
/// pasted. A row whose cut lands on the trailing half of a double-width cluster yields one
/// space in that cell instead, because a half cell is never written; keeping the row's cell
/// count exact matters more than preserving one combining mark, and it is what makes the
/// paste land in the same column.
///
/// Fields are clamped against the buffer, so an out-of-range rectangle yields text rather
/// than panicking. Empty when `width` or `rows` is 0 or `first_line` is past the end.
pub fn block_text(buf: &Buffer, rect: BlockRect) -> String {
    if rect.rows == 0 || rect.width == 0 || rect.first_line >= buf.line_count() {
        return String::new();
    }
    let last_line = rect.last_line().min(buf.line_count() - 1);
    let mut out = String::new();
    for line in rect.first_line..=last_line {
        let (text, cells) = cell_slice(buf, line, rect.col, rect.width, rect.tab_width);
        out.push_str(&text);
        for _ in cells..rect.width {
            out.push(' ');
        }
        if line != last_line {
            out.push('\n');
        }
    }
    out
}

/// Insert a rectangle of `text` at `at` (spec FR-E3).
///
/// `at` is in char coordinates; the rectangle's column is derived from it with `tab_width`,
/// then converted back per target line, so pasting into a line with tabs still lands in the
/// same visible column. Short document lines are padded with spaces; a document line longer
/// than the band keeps everything after it. Rows past the last line create new lines.
///
/// Returns the caret position just right of the band, on its last row.
pub fn paste_block(buf: &mut Buffer, at: LineCol, text: &str, tab_width: usize) -> LineCol {
    let tab_width = tab_width.max(1);
    let at = buf.clamp(at);
    if text.is_empty() {
        return at;
    }
    let mut rows: Vec<&str> = text.split('\n').collect();
    // A trailing newline is the rectangle's terminator, not an empty row.
    if rows.len() > 1 && rows.last() == Some(&"") {
        rows.pop();
    }
    if rows.is_empty() || (rows.len() == 1 && rows[0].is_empty()) {
        return at;
    }
    let col = buf.display_col(at.line, at.col, tab_width);
    let width = rows
        .iter()
        .map(|r| row_cells(r, tab_width))
        .max()
        .unwrap_or(0);

    // Decide up front which rows have a target line. The document grows as overflow rows
    // are appended, so re-reading `line_count()` inside the loop would turn row 1 of a
    // paste that starts on the last line into an append instead of an overwrite.
    let existing = buf.line_count().saturating_sub(at.line);
    let mut last_touched = at.line;
    for (i, row) in rows.iter().enumerate() {
        if i >= existing {
            // No line here yet: append one, left-padded to the rectangle's column, since a
            // brand-new line has nothing to overwrite. `Buffer::insert_line` cannot append
            // (it clamps to `line_count() - 1`, i.e. it always inserts *above* an existing
            // line), so the newline is written explicitly at the end of the buffer.
            let last = buf.line_count() - 1;
            let end = LineCol::new(last, buf.line_len_chars(last));
            let pad = " ".repeat(col);
            buf.insert(end, &format!("\n{pad}{row}"));
            last_touched = at.line + i;
            continue;
        }
        let line = at.line + i;
        let start_char = buf.col_from_display(line, col, tab_width);
        let end_char = buf.col_from_display(line, col + width, tab_width);
        if start_char < end_char {
            buf.delete_range(LineCol::new(line, start_char), LineCol::new(line, end_char));
        }
        // Overwrite the band in place. Whatever the line held before the band stays, and a
        // line shorter than the band simply grows here.
        buf.insert(LineCol::new(line, start_char), row);
        last_touched = line;
    }
    let caret_line = last_touched.min(buf.line_count() - 1);
    let caret_col = buf.col_from_display(caret_line, col + width, tab_width);
    LineCol::new(caret_line, caret_col)
}

/// The text of `line` covering display cells `[col, col + width)`, and the cells it filled.
///
/// Walks the line's clusters once while tracking the running cell count, so it stays linear
/// in the line length. Cutting is per cluster, so the result never starts or ends inside a
/// cluster.
///
/// Cells before the first cluster that falls inside the band are counted as filled, which is
/// what keeps a rectangle's rows aligned: a line that begins past the left edge still yields
/// a full-width row rather than a narrow one, and the caller pads only what is genuinely
/// missing at the right. A cluster straddling the right edge is left out, since half a
/// double-width cell is never written.
fn cell_slice(
    buf: &Buffer,
    line: usize,
    col: usize,
    width: usize,
    tab_width: usize,
) -> (String, usize) {
    let target = col + width;
    let mut out = String::new();
    let mut cells = 0usize;
    for cluster in graphemes_of(&buf.line_string(line)) {
        let w = if cluster == "\t" {
            tab_stop_advance(cells, tab_width)
        } else {
            grapheme_width(cluster)
        };
        if cells >= target {
            break;
        }
        if cells >= col {
            if w > 0 && cells + w > target {
                break;
            }
            out.push_str(cluster);
        }
        cells += w;
    }
    // Cells the band actually contains. Cells before `col` are outside the rectangle and
    // must not count, or a short line looks full and never gets padded. A line that stops
    // before `col` contributes nothing; a line that stops inside the band contributes up to
    // where it stopped.
    let filled = cells.saturating_sub(col).min(width);
    (out, filled)
}

/// Display cells a rectangle row occupies, expanding tabs from column 0.
///
/// Row widths are measured from column 0 rather than from the rectangle's left edge, and
/// the same convention is used when a row is written back. That is a deliberate simplifying
/// choice: it is exact for every rectangle in a tab-free document, which is the normal case
/// once `insert_spaces` is on, and a rectangle dragged across tabs is a documented corner
/// (see `docs/DEVIATIONS.md`).
fn row_cells(row: &str, tab_width: usize) -> usize {
    let mut cells = 0usize;
    for g in graphemes_of(row) {
        cells += if g == "\t" {
            tab_stop_advance(cells, tab_width)
        } else {
            grapheme_width(g)
        };
    }
    cells
}

impl WidthRows {
    /// The cached answer when this memo still describes the whole document, else `None`.
    fn matches(&self, revision: u64, tab_width: usize, lines: usize) -> Option<usize> {
        if self.revision == revision && self.tab_width == tab_width && self.widths.len() == lines {
            Some(self.cells)
        } else {
            None
        }
    }
}
