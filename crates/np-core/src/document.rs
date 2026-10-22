//! The `Document` aggregate (spec §5.5): a buffer plus all per-tab state.
//!
//! # Mutation discipline
//!
//! **All text mutation goes through a method on `Document`.** Each one records undo and
//! sets `modified` in the same place, which is the only way to guarantee stage S2's
//! acceptance rule that no mutation path bypasses history. `buffer` is public for reads;
//! mutating it directly from UI code is a bug.
//!
//! # Read-only behavior
//!
//! `readonly` is enforced in exactly one place, [`Document::check_writable`], which every
//! mutating method calls first. The convention is: mutation methods return
//! `CoreResult<LineCol>` (or `CoreResult<()>`) and yield
//! `Err(CoreError::Invalid("document is read-only"))` rather than silently no-op'ing.
//! The UI turns that error into a toast (FR-F9). Silent no-ops were rejected because the
//! user would see a keystroke vanish with no explanation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::buffer::Buffer;
use crate::cursor::{Cursor, ScrollPos, Selection, SelectionKind};
use crate::error::{CoreError, CoreResult};
use crate::types::{EncodingId, Eol, LanguageId, LineCol};
use crate::undo::{UndoEdit, UndoGroup, UndoStack};
use crate::view::FoldRange;

/// Sort direction for [`Document::sort_lines`] (spec FR-E7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// Case transform for [`Document::change_case`] (spec FR-E8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    Upper,
    Lower,
    Title,
}

/// A file plus all per-tab editing state (spec §5.5).
#[derive(Debug, Clone)]
pub struct Document {
    pub path: Option<PathBuf>,
    /// Read-only view of the text. Mutate only through `Document` methods.
    pub buffer: Buffer,
    pub encoding: EncodingId,
    pub eol: Eol,
    pub modified: bool,
    pub readonly: bool,
    pub cursor: Cursor,
    pub selection: Option<Selection>,
    pub scroll: ScrollPos,
    /// 0-based line numbers (spec §5.5).
    pub bookmarks: BTreeSet<usize>,
    pub folds: Vec<FoldRange>,
    pub language: LanguageId,
    pub undo: UndoStack,
    /// Overtype mode, toggled by `Insert` (spec FR-E12). Status bar shows INS/OVR.
    pub overtype: bool,
    /// `[editor] tab_width` (spec §7.1), pushed in by the UI via [`Document::set_tab_width`]
    /// once S9 wires config. Defaults to the spec value of 4. Held per-document because
    /// column math needs it and `np-core` must not reach for global config.
    pub tab_width: usize,
    /// Byte length and mtime captured at load/save so the UI can detect external
    /// changes (FR-F7).
    pub file_size: Option<u64>,
    pub file_mtime: Option<std::time::SystemTime>,
    /// Per-line terminators as loaded, index `i` being line `i`'s ending (FR-N4). The buffer
    /// normalizes everything to `\n`, so the original mixed endings live only here.
    ///
    /// A shorter vector than the line count means those later lines are new and take the
    /// document's dominant ending. Edits that insert or remove lines shift this vector the
    /// same way they shift bookmarks; [`Document::eols_for_save`] reconciles whatever drift
    /// remains at save time rather than trying to keep the two in lockstep through every
    /// operation.
    pub line_eols: Vec<Eol>,
    /// Whether the saved file should end with a line terminator (FR-N2).
    ///
    /// The buffer cannot answer this. `"a\nb\nc"` with the caret at the end of `c` is
    /// byte-identical in the rope whether the user is mid-way through typing a file or has
    /// just loaded one that ended without a newline, and the two want different saves: the
    /// first must not gain a newline, the second must keep not having one.
    ///
    /// Set at load, refreshed at save, forced true by [`Document::convert_eol`], and false once
    /// the user makes the final line genuinely open (content on the last line, or a backspace
    /// through the final terminator). Nothing else touches it, which is what stops an unrelated
    /// edit in the middle of a file from quietly changing how it ends.
    pub ends_with_newline: bool,
    /// Tab label for a document with no file yet, e.g. `Untitled-3` (FR-F1).
    ///
    /// This is deliberately **not** stored in `path`. `path` means "the file this document
    /// reads from and writes to", and Save, Reload, and external-change detection all key off
    /// it; putting a display name there makes Save create a file literally named `Untitled-3`
    /// and makes Reload try to read one. Kept as a separate field so those paths stay honest.
    pub untitled_name: Option<String>,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            path: None,
            buffer: Buffer::new(),
            encoding: EncodingId::Utf8,
            eol: Eol::default(),
            modified: false,
            readonly: false,
            cursor: Cursor::default(),
            selection: None,
            scroll: ScrollPos::default(),
            bookmarks: BTreeSet::new(),
            folds: Vec::new(),
            language: LanguageId::default(),
            undo: UndoStack::new(),
            overtype: false,
            tab_width: 4,
            file_size: None,
            file_mtime: None,
            line_eols: Vec::new(),
            ends_with_newline: true,
            untitled_name: None,
        }
    }
}

impl Document {
    /// A blank untitled document with the configured defaults (spec FR-F1).
    pub fn new_untitled(encoding: EncodingId, eol: Eol) -> Self {
        Document {
            path: None,
            buffer: Buffer::new(),
            encoding,
            eol,
            ..Default::default()
        }
    }

    /// Build from decoded file text (spec FR-F2; S5 supplies the decoded payload).
    ///
    /// `eol_per_line` is the per-line terminator history that FR-N4 requires. The buffer
    /// itself stores LF only (see `buffer.rs`), so the endings live here.
    pub fn from_decoded(
        path: PathBuf,
        text: &str,
        encoding: EncodingId,
        eol: Eol,
        readonly: bool,
    ) -> Self {
        let (_lines, line_eols) = crate::eol::split_lines_keep_eol(text);
        let ends_with_newline = text.is_empty() || text.ends_with('\n') || text.ends_with('\r');
        Document {
            path: Some(path),
            buffer: Buffer::from_str_lossy(text),
            encoding,
            eol,
            modified: false,
            readonly,
            line_eols,
            ends_with_newline,
            ..Default::default()
        }
    }

    /// Build a document from a file that was just read (FR-F1).
    ///
    /// The single way a document takes on file state: text, encoding, per-line endings,
    /// dominant ending, read-only flag and the size/mtime stamp all arrive together, so no
    /// caller can load a file and forget to record the stamp — which would silently disable
    /// external-change detection.
    pub fn from_loaded(loaded: &crate::file::LoadedFile) -> Self {
        let mut doc = Document {
            path: Some(loaded.path.clone()),
            buffer: Buffer::from_str_lossy(&loaded.decoded.text),
            encoding: loaded.decoded.encoding.clone(),
            eol: loaded.dominant_eol,
            modified: false,
            readonly: loaded.readonly,
            ends_with_newline: loaded.decoded.text.is_empty()
                || loaded.decoded.text.ends_with('\n')
                || loaded.decoded.text.ends_with('\r'),
            line_eols: loaded.line_eols.clone(),
            file_size: Some(loaded.size),
            file_mtime: Some(loaded.mtime),
            ..Default::default()
        };
        doc.cursor.pos = LineCol::new(0, 0);
        doc
    }

    /// The size/mtime stamp for the file this document was loaded from or last saved to.
    pub fn stamp(&self) -> Option<crate::file::FileStamp> {
        match (self.file_mtime, self.file_size) {
            (Some(mtime), Some(size)) => Some(crate::file::FileStamp { mtime, size }),
            _ => None,
        }
    }

    /// Record a fresh stamp after a successful load or save (FR-F3).
    pub fn set_stamp(&mut self, info: &crate::file::SaveInfo) {
        self.file_size = Some(info.bytes_written);
        self.file_mtime = Some(info.mtime);
    }

    /// True when the file on disk no longer matches what this document holds (FR-F7).
    ///
    /// False for an untitled document, which has no file to disagree with. A deleted file
    /// counts as changed: the caller then offers Save As rather than resurrecting it (spec §8).
    pub fn changed_on_disk(&self) -> bool {
        match (&self.path, self.stamp()) {
            (Some(path), Some(stamp)) => crate::file::changed_on_disk(&stamp, path),
            _ => false,
        }
    }

    /// True when the file this document came from has been deleted.
    pub fn file_deleted(&self) -> bool {
        match &self.path {
            Some(path) => !crate::file::exists(path),
            None => false,
        }
    }

    /// The per-line endings to write.
    ///
    /// The vector's length **is** the terminator count: `file::save_atomic` writes an ending for
    /// every line it covers and for no others. So this function's whole job is to decide how many
    /// lines end with a newline, and it does so from two facts the buffer cannot supply on its
    /// own:
    ///
    ///  - [`Document::ends_with_newline`] — whether the file is meant to end with a newline. Ropey
    ///    cannot say: a document whose last line is empty is the same shape whether that empty
    ///    line came from the file's own trailing terminator or from the user pressing Enter, and
    ///    a document whose last line has content cannot tell "loaded without a trailing newline"
    ///    from "still typing".
    ///  - the current line count — which is what grows when the user appends, and therefore what
    ///    gives a brand-new line an ending instead of letting it glue onto its neighbour (FR-N4).
    ///
    /// Terminated files therefore get one ending per content line, with the empty line a trailing
    /// terminator implies left alone; open files stop one line short. Deleting a line drops its
    /// ending with it, which is what makes backspacing a final newline stick. Loaded lines the
    /// user never touched keep the ending they came in with, which is the mixed-ending half of
    /// FR-N4.
    pub fn eols_for_save(&self) -> Vec<Eol> {
        let lines = self.buffer.line_count();
        if lines == 0 {
            return Vec::new();
        }
        // Ropey's extra trailing line, if any: an empty final line that a terminator accounts
        // for is not content and must not collect an ending of its own.
        let implied_trailing = lines > 1 && self.buffer.line_string(lines - 1).is_empty();
        let content_lines = if implied_trailing { lines - 1 } else { lines };

        let wanted = if self.ends_with_newline {
            content_lines
        } else {
            // One line stays open. When the open line is the implied trailing one it has no
            // ending anyway, so the count is already right; when it is a content line, stop
            // before it.
            content_lines.saturating_sub(if implied_trailing { 0 } else { 1 })
        };
        let mut out = self.line_eols.clone();
        if out.len() > wanted {
            out.truncate(wanted);
        }
        if out.len() < wanted {
            // Lines created since the load take the document's current ending.
            out.resize(wanted, self.eol);
        }
        out
    }

    // --------------------------------------------------------- predicates

    /// The single read-only gate. See the module docs for the error convention.
    fn check_writable(&self) -> CoreResult<()> {
        if self.readonly {
            Err(CoreError::Invalid("document is read-only".into()))
        } else {
            Ok(())
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.char_count() == 0
    }

    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// The display name shown on the tab: file name, or "Untitled" (FR-F1, FR-U2).
    pub fn display_name(&self) -> String {
        match (&self.path, &self.untitled_name) {
            (Some(p), _) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            (None, Some(name)) => name.clone(),
            (None, None) => "Untitled".into(),
        }
    }

    /// True when this document has no file behind it and therefore needs a Save As path.
    pub fn is_untitled(&self) -> bool {
        self.path.is_none()
    }

    // ---------------------------------------------------------------- file

    /// Write this document to `path` atomically (FR-F3).
    ///
    /// On success the modified flag clears and the size/mtime stamp refreshes. On failure
    /// **nothing about the document changes**: it stays modified, the text is untouched, and
    /// the error is returned so the UI can show a sticky toast. There is deliberately no path
    /// through which a failed save loses work.
    pub fn save_to(
        &mut self,
        path: &Path,
        mode: crate::eol::EolMode,
        opts: &crate::file::SaveOptions,
    ) -> CoreResult<crate::file::SaveInfo> {
        if self.readonly {
            return Err(CoreError::Invalid("document is read-only".into()));
        }
        let eols = self.eols_for_save();
        let info = crate::file::save_atomic(
            path,
            &self.buffer,
            &eols,
            &self.encoding,
            self.eol,
            mode,
            opts,
        )?;
        // Only now is the document considered saved. The bytes on disk are the new baseline, so
        // the per-line endings become the ones just written: without this, a document that
        // gained lines since its load saves once and then, on the next save, reverts to the
        // ending count it loaded with and silently drops the terminator off the appended line.
        self.path = Some(path.to_path_buf());
        self.line_eols = eols;
        self.ends_with_newline = self.buffer.line_count() <= 1 || self.ends_with_newline;
        self.modified = false;
        self.readonly = !crate::file::is_writable(path);
        self.set_stamp(&info);
        Ok(info)
    }

    /// Save to the document's current path. An untitled document is an error, not a silent
    /// no-op: the caller routes it to Save As (FR-F3).
    pub fn save(
        &mut self,
        mode: crate::eol::EolMode,
        opts: &crate::file::SaveOptions,
    ) -> CoreResult<crate::file::SaveInfo> {
        match self.path.clone() {
            Some(path) => self.save_to(&path, mode, opts),
            None => Err(CoreError::Invalid(
                "document has no path; use Save As".into(),
            )),
        }
    }

    /// Re-read the file from disk (FR-F6).
    ///
    /// Encoding and per-line endings are re-detected. The cursor keeps its **line** where
    /// possible and moves to the start of it, because a reloaded file's column arithmetic has
    /// no meaning against the new bytes.
    ///
    /// **Undo history is discarded.** The old history refers to text that no longer exists,
    /// and an undo that half-applies to reloaded content is worse than no undo: it would
    /// silently produce a document that is neither the old nor the new. The user asked for the
    /// disk version, so that becomes the whole state. Documented in DEVIATIONS.
    ///
    /// A failed read leaves the document exactly as it was.
    pub fn reload(&mut self, force: Option<EncodingId>) -> CoreResult<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| CoreError::Invalid("document has no file to reload".into()))?;
        let loaded = crate::file::load(&path, force)?;
        let keep_line = self.cursor.pos.line;

        self.buffer = Buffer::from_str_lossy(&loaded.decoded.text);
        self.encoding = loaded.decoded.encoding;
        self.eol = loaded.dominant_eol;
        self.ends_with_newline = loaded.decoded.text.is_empty()
            || loaded.decoded.text.ends_with('\n')
            || loaded.decoded.text.ends_with('\r');
        self.line_eols = loaded.line_eols;
        self.file_size = Some(loaded.size);
        self.file_mtime = Some(loaded.mtime);
        self.readonly = loaded.readonly;
        self.modified = false;
        self.selection = None;
        self.undo.clear();
        // Keep the line, clamp into the reloaded document, and drop the column: the old
        // column refers to text that may not exist any more.
        let line = keep_line.min(self.buffer.line_count().saturating_sub(1));
        self.cursor.pos = LineCol::new(line, 0);
        self.cursor.goal_col = None;
        Ok(())
    }

    /// Convert the document's text to a new encoding, keeping the bytes in memory (FR-N3
    /// "Convert to…"). The file is only re-encoded when the user saves.
    pub fn convert_encoding(&mut self, enc: EncodingId) {
        self.encoding = enc;
        // A conversion is a real change to how the file will be written, so the document is
        // modified even if no keystroke happened.
        self.modified = true;
    }

    /// Convert every line to one ending (FR-N3 EOL menu). Per-line endings are replaced, so
    /// a later `eol_mode = file` save writes the new uniform ending.
    pub fn convert_eol(&mut self, target: Eol) {
        let lines = self.buffer.line_count();
        self.eol = target;
        // Every existing line gets the new ending; a final unterminated line keeps that
        // property by leaving the last slot empty.
        self.line_eols = vec![target; lines.saturating_sub(1)];
        self.ends_with_newline = true;
        self.modified = true;
    }

    // ------------------------------------------------------------- cursor

    /// Move the caret, optionally extending the selection from its current anchor
    /// (spec FR-E2, FR-E3).
    ///
    /// `goal_col` is set to the caret's display column so subsequent vertical moves try to
    /// return to it. Extending keeps the existing anchor; not extending clears selection.
    pub fn set_cursor(&mut self, lc: LineCol, extend: bool) {
        let pos = self.buffer.clamp(lc);
        let display = self.buffer.display_col(pos.line, pos.col, self.tab_width());

        if extend {
            let anchor = self.selection.map(|s| s.anchor).unwrap_or(self.cursor.pos);
            let kind = self
                .selection
                .map(|s| s.kind)
                .unwrap_or(SelectionKind::Range);
            self.selection = Some(Selection {
                anchor,
                focus: pos,
                kind,
            });
        } else {
            self.selection = None;
        }

        self.cursor.pos = pos;
        self.cursor.goal_col = Some(display);
    }

    /// Begin or replace a selection with an explicit anchor (mouse press, S6).
    pub fn begin_selection(&mut self, anchor: LineCol, focus: LineCol, kind: SelectionKind) {
        let a = self.buffer.clamp(anchor);
        let f = self.buffer.clamp(focus);
        if a == f {
            self.selection = None;
        } else {
            self.selection = Some(Selection {
                anchor: a,
                focus: f,
                kind,
            });
        }
        self.cursor.pos = f;
        self.cursor.goal_col = Some(self.buffer.display_col(f.line, f.col, self.tab_width()));
    }

    pub fn select_all(&mut self) {
        let end_line = self.buffer.line_count() - 1;
        let end = LineCol::new(end_line, self.buffer.line_len_chars(end_line));
        self.cursor.pos = end;
        self.cursor.goal_col = Some(self.buffer.display_col(end.line, end.col, self.tab_width()));
        if self.buffer.char_count() == 0 {
            self.selection = None;
        } else {
            self.selection = Some(Selection::range(LineCol::new(0, 0), end));
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The selected text, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        if sel.is_empty() {
            return None;
        }
        match sel.kind {
            SelectionKind::Range => {
                let (s, e) = sel.bounds();
                let start = self.buffer.char_offset(s)?;
                let end = self.buffer.char_offset(e)?;
                Some(self.slice_chars(start, end))
            }
            SelectionKind::Block => Some(self.block_text(sel)),
        }
    }

    /// Rectangle text for a block selection, padding short lines with spaces (FR-E3).
    ///
    /// Shaping lives in [`crate::buffer::block_text`] so the rule has one home and can be
    /// tested against a bare `Buffer`.
    pub fn block_text(&self, sel: Selection) -> String {
        crate::buffer::block_text(&self.buffer, self.block_rect(sel))
    }

    /// The rectangle a block selection covers, in display cells (spec FR-E3).
    ///
    /// The row span is normalized by line; the column span comes from anchor/focus directly,
    /// because `bounds()` normalizes `(line, col)` as a pair, so on a downward drag it hands
    /// back the start line's column twice and the caret's column is lost.
    ///
    /// Cell conversion uses two reference lines, and which is which matters. The **left
    /// edge** is measured on the top line, so the band starts at the screen column the user
    /// first saw and every row below shares it. The **width** is measured on the anchor
    /// line, because that is where the drag began and its tabs determined how many cells the
    /// span occupied. When the anchor is the top line, as it is for a normal downward drag,
    /// both are the same line and the rectangle is exactly what was dragged.
    pub fn block_rect(&self, sel: Selection) -> crate::buffer::BlockRect {
        let (first_line, last_line) = if sel.anchor.line <= sel.focus.line {
            (sel.anchor.line, sel.focus.line)
        } else {
            (sel.focus.line, sel.anchor.line)
        };
        let last_line = last_line.min(self.buffer.line_count() - 1);
        let tab = self.tab_width();
        let lo_char = sel.anchor.col.min(sel.focus.col);
        let hi_char = sel.anchor.col.max(sel.focus.col);
        let left = self.buffer.display_col(first_line, lo_char, tab);
        let right = self.buffer.display_col(sel.anchor.line, hi_char, tab);
        crate::buffer::BlockRect::new(
            first_line,
            last_line.saturating_sub(first_line) + 1,
            left,
            right.saturating_sub(left),
            tab,
        )
    }

    /// Text in the half-open char range `[start, end)`, clamped to the buffer.
    ///
    /// Windowing the rope directly keeps this proportional to the *slice*, not the whole
    /// document. Block-selection paths call it once per selected row, so a walk from
    /// offset 0 would turn a tall rectangle into quadratic work.
    fn slice_chars(&self, start: usize, end: usize) -> String {
        let total = self.buffer.char_count();
        let start = start.min(total);
        let end = end.clamp(start, total);
        if end <= start {
            return String::new();
        }
        let mut out = String::with_capacity(end - start);
        let slice = self.buffer.rope().slice(start..end);
        for chunk in slice.chunks() {
            out.push_str(chunk);
        }
        out
    }

    /// Effective tab width, always at least 1.
    pub fn tab_width(&self) -> usize {
        self.tab_width.max(1)
    }

    /// Set by the UI from `[editor] tab_width` (S9).
    pub fn set_tab_width(&mut self, width: usize) {
        self.tab_width = width.max(1);
    }

    // ------------------------------------------------------------ movement

    pub fn move_left(&mut self) {
        self.cursor.goal_col = None;
        let pos = self.cursor.pos;
        if pos.col > 0 {
            let prev =
                crate::buffer::prev_grapheme_col(&self.buffer, pos.line, pos.col).unwrap_or(0);
            self.set_cursor(LineCol::new(pos.line, prev), false);
        } else if pos.line > 0 {
            let prev = pos.line - 1;
            let len = self.buffer.line_len_chars(prev);
            self.set_cursor(LineCol::new(prev, len), false);
        } else {
            self.set_cursor(pos, false);
        }
    }

    pub fn move_right(&mut self) {
        self.cursor.goal_col = None;
        let pos = self.cursor.pos;
        let len = self.buffer.line_len_chars(pos.line);
        if pos.col < len {
            let next = crate::buffer::next_grapheme_col(&self.buffer, pos.line, pos.col)
                .unwrap_or(pos.col + 1);
            self.set_cursor(LineCol::new(pos.line, next.min(len)), false);
        } else if pos.line + 1 < self.buffer.line_count() {
            self.set_cursor(LineCol::new(pos.line + 1, 0), false);
        } else {
            self.set_cursor(pos, false);
        }
    }

    /// Vertical movement honoring `goal_col` (spec FR-E2).
    ///
    /// When the target line is long enough to hold the goal column, the caret returns to
    /// it; otherwise the caret clamps to that line's end and the goal is **kept**, so
    /// moving back out restores the original column.
    pub fn move_vertical(&mut self, delta: i64, extend: bool) {
        let pos = self.cursor.pos;
        let target_line = if delta < 0 {
            pos.line.saturating_sub((-delta) as usize)
        } else {
            (pos.line + delta as usize).min(self.buffer.line_count() - 1)
        };

        let goal = self
            .cursor
            .goal_col
            .unwrap_or_else(|| self.buffer.display_col(pos.line, pos.col, self.tab_width()));

        let line_len = self.buffer.line_len_chars(target_line);
        let col = self
            .buffer
            .col_from_display(target_line, goal, self.tab_width());
        let target = LineCol::new(target_line, col.min(line_len));
        // When the move saturates at a document edge, the goal column no longer
        // describes a reachable position on this line; adopt the position actually
        // reached so a later PgUp/PgDn does not try to restore a column the caret was
        // never really on.
        let saturated = (delta < 0 && target_line == 0)
            || (delta > 0 && target_line == self.buffer.line_count() - 1);
        let goal_after = if saturated {
            self.buffer
                .display_col(target.line, target.col, self.tab_width())
        } else {
            goal
        };

        self.cursor.pos = target;
        self.cursor.goal_col = Some(goal_after);
        if extend {
            let anchor = self.selection.map(|s| s.anchor).unwrap_or(self.cursor.pos);
            self.selection = Some(Selection::range(anchor, target));
        } else {
            self.selection = None;
        }
    }

    pub fn word_left(&mut self) {
        self.cursor.goal_col = None;
        let next = self.buffer.word_start_before(self.cursor.pos);
        self.set_cursor(next, false);
    }

    pub fn word_right(&mut self) {
        self.cursor.goal_col = None;
        let next = self.buffer.word_end_after(self.cursor.pos);
        self.set_cursor(next, false);
    }

    pub fn line_start(&mut self) {
        self.cursor.goal_col = Some(0);
        self.set_cursor(LineCol::new(self.cursor.pos.line, 0), false);
    }

    pub fn line_end(&mut self) {
        let len = self.buffer.line_len_chars(self.cursor.pos.line);
        let display = self
            .buffer
            .display_width(self.cursor.pos.line, self.tab_width());
        self.cursor.goal_col = Some(display);
        self.set_cursor(LineCol::new(self.cursor.pos.line, len), false);
        self.cursor.goal_col = Some(display);
    }

    pub fn doc_start(&mut self) {
        self.cursor.goal_col = Some(0);
        self.set_cursor(LineCol::new(0, 0), false);
    }

    pub fn doc_end(&mut self) {
        let last = self.buffer.line_count() - 1;
        let len = self.buffer.line_len_chars(last);
        self.set_cursor(LineCol::new(last, len), false);
    }

    // ------------------------------------------------------------- editing

    /// Insert `text` at `lc` (spec FR-E3). Terminators are normalized to the document EOL
    /// so the buffer stays LF-only (see `buffer.rs`).
    pub fn insert_at(
        &mut self,
        lc: LineCol,
        text: &str,
        group: UndoGroup,
        now: Instant,
    ) -> CoreResult<LineCol> {
        self.check_writable()?;
        let text = self.normalize_eols(text);
        if text.is_empty() {
            return Ok(self.cursor.pos);
        }
        let at = self.buffer.clamp(lc);
        let offset = self.buffer.char_offset(at).unwrap_or(0);

        self.undo.record(
            group,
            &UndoEdit::new(offset, "", &text, self.cursor.pos, at),
            now,
        );

        let ends_nl = self.ends_with_newline_after_insert(&text, at);
        self.buffer.insert(at, &text);
        self.ends_with_newline = ends_nl;
        self.modified = true;
        Ok(self.advance_for_insert(at, &text))
    }

    /// Insert at the caret, replacing any selection first (spec FR-E3, FR-E13).
    ///
    /// This is the normal typing/paste entry point. `group` should be `Typing` for a
    /// single keystroke and `Atomic` for a paste so the paste is one undo unit.
    pub fn insert_text(
        &mut self,
        text: &str,
        group: UndoGroup,
        now: Instant,
    ) -> CoreResult<LineCol> {
        self.check_writable()?;
        let text = self.normalize_eols(text);
        if text.is_empty() {
            return Ok(self.cursor.pos);
        }
        // Replace the selection and insert, as ONE undo unit (spec FR-E4: typing over a
        // selection, and a paste, are single units). `take_selection_text` cannot be used
        // here because it records its own step, which would split this into two.
        let (removed, replace_start) = self.delete_selection_silent();
        let at = replace_start;
        let offset = self.buffer.char_offset(at).unwrap_or(0);

        self.undo
            .record(group, &UndoEdit::new(offset, &removed, &text, at, at), now);
        let ends_nl = self.ends_with_newline_after_insert(&text, at);
        self.buffer.insert(at, &text);
        self.ends_with_newline = ends_nl;
        self.modified = true;
        // Typing or pasting a newline at column 0 creates real lines, so content at and
        // below the caret shifts. The S2 spec left this undefined; see DEVIATIONS.
        let added = text.matches('\n').count();
        if added > 0 && at.col == 0 {
            Self::shift_bookmarks_insert(&mut self.bookmarks, at.line, added);
        }
        Ok(self.advance_for_insert(at, &text))
    }

    /// Delete the selection; returns the new caret position.
    ///
    /// With an empty selection this is a no-op returning `None`; backspace/delete
    /// behavior with no selection is handled by [`Document::delete_char_before`] and
    /// [`Document::delete_char_after`], matching the editor's key handling.
    pub fn delete_selection(&mut self, now: Instant) -> CoreResult<Option<LineCol>> {
        self.check_writable()?;
        let Some(sel) = self.selection else {
            return Ok(None);
        };
        if sel.is_empty() {
            return Ok(None);
        }
        // `take_selection_text` records the undo step itself; the deleted text is what a
        // caller would want for a clipboard, which S7 consumes.
        let removed = self.take_selection_text(now);
        self.modified = true;
        debug_assert!(
            !removed.is_empty(),
            "a non-empty selection deletes something"
        );
        Ok(Some(self.cursor.pos))
    }

    /// Delete a range, endpoints in either order (spec FR-E3).
    pub fn delete_range(
        &mut self,
        from: LineCol,
        to: LineCol,
        group: UndoGroup,
        now: Instant,
    ) -> CoreResult<LineCol> {
        self.check_writable()?;
        let a = self.buffer.clamp(from);
        let b = self.buffer.clamp(to);
        let sel = Selection::range(a, b);
        self.remove_selection_text(sel, group, now)?;
        self.modified = true;
        Ok(self.cursor.pos)
    }

    /// Backspace: delete the character before the caret, or the selection if there is one.
    pub fn delete_char_before(&mut self, now: Instant) -> CoreResult<Option<LineCol>> {
        self.check_writable()?;
        if let Some(sel) = self.selection {
            if !sel.is_empty() {
                return self
                    .delete_selection(now)
                    .map(|o| o.or(Some(self.cursor.pos)));
            }
        }
        let pos = self.cursor.pos;
        let target = if pos.col > 0 {
            // A whole grapheme cluster, not one scalar value: `e` + U+0301 is one visible
            // character and one backspace must remove both.
            let prev =
                crate::buffer::prev_grapheme_col(&self.buffer, pos.line, pos.col).unwrap_or(0);
            LineCol::new(pos.line, prev)
        } else if pos.line > 0 {
            let pl = pos.line - 1;
            LineCol::new(pl, self.buffer.line_len_chars(pl))
        } else {
            return Ok(None);
        };
        let sel = Selection::range(target, pos);
        self.remove_selection_text(sel, UndoGroup::Typing, now)?;
        self.modified = true;
        Ok(Some(self.cursor.pos))
    }

    /// Delete forward (Delete key).
    pub fn delete_char_after(&mut self, now: Instant) -> CoreResult<Option<LineCol>> {
        self.check_writable()?;
        if let Some(sel) = self.selection {
            if !sel.is_empty() {
                return self
                    .delete_selection(now)
                    .map(|o| o.or(Some(self.cursor.pos)));
            }
        }
        let pos = self.cursor.pos;
        let len = self.buffer.line_len_chars(pos.line);
        let target = if pos.col < len {
            // Cluster-forward, for the same reason backspace is cluster-backward.
            let next = crate::buffer::next_grapheme_col(&self.buffer, pos.line, pos.col)
                .unwrap_or(pos.col + 1);
            LineCol::new(pos.line, next.min(len))
        } else if pos.line + 1 < self.buffer.line_count() {
            LineCol::new(pos.line + 1, 0)
        } else {
            return Ok(None);
        };
        let sel = Selection::range(pos, target);
        self.remove_selection_text(sel, UndoGroup::Typing, now)?;
        self.modified = true;
        Ok(Some(self.cursor.pos))
    }

    /// Apply an inverse edit produced by `UndoStack::undo`/`redo`.
    ///
    /// Deleting first then inserting keeps offsets meaningful when several edits from one
    /// group are applied in sequence.
    pub fn apply_edit(&mut self, edit: &UndoEdit) {
        let start = edit.offset.min(self.buffer.char_count());
        let end = (edit.offset + edit.delete_len()).min(self.buffer.char_count());
        if start < end {
            let from = self.buffer.line_col(start);
            let to = self.buffer.line_col(end);
            // Bookmarks go with the text they marked: removing lines here takes their
            // bookmarks with it and pulls the rest up.
            if to.line > from.line {
                Self::shift_bookmarks_remove(&mut self.bookmarks, from.line, to.line - 1);
            }
            self.buffer.delete_range(from, to);
        }
        if !edit.inserted.is_empty() {
            let at = self.buffer.line_col(start);
            let added = edit.inserted.matches('\n').count();
            self.buffer.insert(at, &edit.inserted);
            if added > 0 && at.col == 0 {
                Self::shift_bookmarks_insert(&mut self.bookmarks, at.line, added);
            }
        }
        self.clamp_bookmarks();
        // Undo is not itself undoable, and the document is still considered modified
        // unless the user has walked all the way back to the saved state (tracked by S9).
        self.modified = true;
        self.selection = None;
        self.cursor.pos = self.buffer.clamp(edit.cursor_before);
        self.cursor.goal_col = None;
    }

    /// Undo one unit. Returns false when there is nothing to undo.
    pub fn undo_one(&mut self) -> bool {
        let Some(edits) = self.undo.undo() else {
            return false;
        };
        for edit in &edits {
            self.apply_edit(edit);
        }
        true
    }

    /// Redo one unit. Returns false when there is nothing to redo.
    pub fn redo_one(&mut self) -> bool {
        let Some(edits) = self.undo.redo() else {
            return false;
        };
        for edit in &edits {
            self.apply_edit(edit);
        }
        true
    }

    // ---------------------------------------------------------- line ops

    /// Duplicate the caret's line, or every line the selection touches (spec FR-E7).
    pub fn duplicate_line(&mut self, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let (first, last) = self.selection_line_span();
        let mut block = String::new();
        for line in first..=last {
            block.push_str(&self.buffer.line_string(line));
            block.push('\n');
        }
        // Insert at the START of the line after the block, so the duplicate lands on its
        // own line. Inserting at the last line's end would splice it in before that
        // line's newline and glue the two together ("b" + "b\n" -> "bb\n\n").
        let at = if last + 1 < self.buffer.line_count() {
            LineCol::new(last + 1, 0)
        } else {
            // Duplicating the final line: append, adding a newline first when the
            // document has no trailing one.
            let len = self.buffer.line_len_chars(last);
            if !self.buffer.text_string().ends_with('\n') {
                block.insert(0, '\n');
            }
            LineCol::new(last, len)
        };
        let offset = self.buffer.char_offset(at).unwrap_or(0);
        self.undo.record(
            UndoGroup::Atomic,
            &UndoEdit::new(offset, "", &block, self.cursor.pos, at),
            now,
        );
        self.buffer.insert(at, &block);
        self.modified = true;
        // The copy lands below the original, so bookmarks on the source block stay put
        // and everything at or after the insertion line moves down.
        Self::shift_bookmarks_insert(&mut self.bookmarks, last + 1, last - first + 1);
        self.cursor.pos = LineCol::new(first + (last - first + 1), self.cursor.pos.col);
        self.cursor.pos = self.buffer.clamp(self.cursor.pos);
        self.selection = None;
        Ok(())
    }

    /// Delete the caret's line, or every line the selection touches (spec FR-E7).
    pub fn delete_line(&mut self, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let (first, last) = self.selection_line_span();
        // Deleting the only line empties the document rather than panicking.
        if self.buffer.line_count() == 1 {
            let removed = self.buffer.line_string(0);
            let offset = 0;
            self.undo.record(
                UndoGroup::Atomic,
                &UndoEdit::new(offset, &removed, "", self.cursor.pos, LineCol::new(0, 0)),
                now,
            );
            self.buffer.delete_range(
                LineCol::new(0, 0),
                LineCol::new(0, self.buffer.line_len_chars(0)),
            );
            self.modified = true;
            self.bookmarks.clear();
            self.cursor.pos = LineCol::new(0, 0);
            self.selection = None;
            return Ok(());
        }
        let start = LineCol::new(first, 0);
        let end = if last + 1 < self.buffer.line_count() {
            LineCol::new(last + 1, 0)
        } else {
            LineCol::new(last, self.buffer.line_len_chars(last))
        };
        let removed = {
            let a = self.buffer.char_offset(start).unwrap_or(0);
            let b = self.buffer.char_offset(end).unwrap_or(0);
            self.slice_chars(a, b)
        };
        let cursor_after = LineCol::new(first.min(self.buffer.line_count() - 2), 0);
        self.undo.record(
            UndoGroup::Atomic,
            &UndoEdit::new(
                self.buffer.char_offset(start).unwrap_or(0),
                &removed,
                "",
                self.cursor.pos,
                cursor_after,
            ),
            now,
        );
        self.buffer.delete_range(start, end);
        self.modified = true;
        Self::shift_bookmarks_remove(&mut self.bookmarks, first, last);
        self.clamp_bookmarks();
        self.cursor.pos = self.buffer.clamp(cursor_after);
        self.selection = None;
        Ok(())
    }

    /// Swap the caret's line (or the selected block) with the one above (spec FR-E7).
    pub fn move_line_up(&mut self, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        self.swap_line_block(-1, now)
    }

    pub fn move_line_down(&mut self, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        self.swap_line_block(1, now)
    }

    fn swap_line_block(&mut self, delta: i64, now: Instant) -> CoreResult<()> {
        let (first, last) = self.selection_line_span();
        let count = self.buffer.line_count();
        if count < 2 {
            return Ok(());
        }
        let moved_first = if delta < 0 {
            if first == 0 {
                return Ok(());
            }
            first - 1
        } else {
            if last + 1 >= count {
                return Ok(());
            }
            first + 1
        };

        let mut lines: Vec<String> = (0..count).map(|l| self.buffer.line_string(l)).collect();
        let span = last - first + 1;
        let block: Vec<String> = lines[first..=last].to_vec();
        lines.drain(first..=last);
        let insert_at = moved_first.min(lines.len());
        for (i, line) in block.into_iter().enumerate() {
            lines.insert(insert_at + i, line);
        }

        let new_text = rebuild_text(&lines);
        self.replace_whole_document(new_text, now);
        Self::shift_bookmarks_swap(
            &mut self.bookmarks,
            first,
            last,
            insert_at as i64 - first as i64,
        );
        self.cursor.pos = LineCol::new(insert_at.min(count - 1), self.cursor.pos.col);
        self.cursor.pos = self.buffer.clamp(self.cursor.pos);
        self.selection = if span > 1 {
            Some(Selection::range(
                LineCol::new(insert_at, 0),
                LineCol::new(insert_at + span - 1, 0),
            ))
        } else {
            None
        };
        Ok(())
    }

    /// Sort the selected lines, or the caret's line when nothing spans lines (FR-E7).
    pub fn sort_lines(&mut self, order: SortOrder, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let (first, last) = self.selection_line_span();
        // A document ending in a newline parses as having a final empty line. That
        // phantom line is not content: sorting it in would float it to the top and move
        // the trailing newline to the front. Clamp the range to real content lines.
        let last = last.min(self.last_content_line());
        if first > last {
            return Ok(());
        }
        let mut lines: Vec<String> = (first..=last).map(|l| self.buffer.line_string(l)).collect();
        lines.sort_by(|a, b| {
            let cmp = a.cmp(b);
            match order {
                SortOrder::Ascending => cmp,
                SortOrder::Descending => cmp.reverse(),
            }
        });
        let count = self.buffer.line_count();
        let mut all: Vec<String> = (0..count).map(|l| self.buffer.line_string(l)).collect();
        for (i, line) in lines.into_iter().enumerate() {
            all[first + i] = line;
        }
        let new_text = rebuild_text(&all);
        self.replace_whole_document(new_text, now);
        self.cursor.pos = self.buffer.clamp(self.cursor.pos);
        Ok(())
    }

    /// Change case over the selection, or over the caret's word when nothing is selected
    /// (spec FR-E8).
    pub fn change_case(&mut self, mode: CaseMode, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let target = match self.selection {
            Some(sel) if !sel.is_empty() => sel,
            _ => {
                let (s, e) = self.buffer.word_range_at(self.cursor.pos);
                if s == e {
                    return Ok(());
                }
                Selection::range(s, e)
            }
        };

        match target.kind {
            SelectionKind::Range => {
                let (s, e) = target.bounds();
                let a = self.buffer.char_offset(s).unwrap_or(0);
                let b = self.buffer.char_offset(e).unwrap_or(0);
                let old = self.slice_chars(a, b);
                let new = apply_case(&old, mode);
                if old == new {
                    return Ok(());
                }
                self.undo.record(
                    UndoGroup::Atomic,
                    &UndoEdit::new(a, &old, &new, self.cursor.pos, e),
                    now,
                );
                self.buffer.delete_range(s, e);
                self.buffer.insert(s, &new);
            }
            SelectionKind::Block => {
                // Apply case per row of the rectangle, leaving padding untouched.
                let (s, e) = target.bounds();
                for line in (s.line..=e.line).rev() {
                    let lo = s.col.min(e.col);
                    let hi = s.col.max(e.col);
                    let line_len = self.buffer.line_len_chars(line);
                    if lo >= line_len {
                        continue;
                    }
                    let from = LineCol::new(line, lo);
                    let to = LineCol::new(line, hi.min(line_len));
                    let a = self.buffer.char_offset(from).unwrap_or(0);
                    let b = self.buffer.char_offset(to).unwrap_or(0);
                    let old = self.slice_chars(a, b);
                    let new = apply_case(&old, mode);
                    if old == new {
                        continue;
                    }
                    self.undo.record(
                        UndoGroup::Atomic,
                        &UndoEdit::new(a, &old, &new, self.cursor.pos, to),
                        now,
                    );
                    self.buffer.delete_range(from, to);
                    self.buffer.insert(from, &new);
                }
            }
        }
        self.modified = true;
        self.clamp_bookmarks();
        Ok(())
    }

    /// Toggle a line comment on every line the selection touches (spec FR-E9).
    ///
    /// If **every** non-blank touched line already starts with the token (after leading
    /// whitespace) the token is stripped; otherwise it is added to the lines that lack it.
    /// A mixed selection therefore ends up fully commented, which is the Notepad++
    /// behavior and avoids a surprising partial uncomment.
    pub fn toggle_comment(&mut self, token: &str, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let (first, last) = self.selection_line_span();
        let mut lines: Vec<String> = (0..self.buffer.line_count())
            .map(|l| self.buffer.line_string(l))
            .collect();

        let meaningful: Vec<&String> = (first..=last)
            .map(|i| &lines[i])
            .filter(|l| !l.trim().is_empty())
            .collect();
        let all_commented =
            !meaningful.is_empty() && meaningful.iter().all(|l| l.trim_start().starts_with(token));

        // Iterating mutably over the touched slice rather than indexing keeps clippy's
        // `needless_range_loop` happy and makes it impossible to address a line outside
        // the span the selection actually covers.
        for line in &mut lines[first..=last] {
            if line.trim().is_empty() {
                continue;
            }
            if all_commented {
                if let Some(new_line) = strip_comment_token(line, token) {
                    *line = new_line;
                }
            } else if !line.trim_start().starts_with(token) {
                let indent_len = line.len() - line.trim_start().len();
                *line = format!("{}{}{}", &line[..indent_len], token, &line[indent_len..]);
            }
        }

        let new_text = rebuild_text(&lines);
        self.replace_whole_document(new_text, now);
        self.cursor.pos = self.buffer.clamp(self.cursor.pos);
        Ok(())
    }

    /// Replace the entire buffer contents as a single undo unit.
    ///
    /// Used by the line-oriented operations above, which are simpler and safer to express
    /// as a whole-document rewrite than as a sequence of splices. It costs more memory than
    /// a targeted edit but keeps undo exact, and the operations involved (sort, case,
    /// comment, move line) are inherently whole-line.
    fn replace_whole_document(&mut self, new_text: String, now: Instant) {
        let old = self.buffer.text_string();
        if old == new_text {
            return;
        }
        self.undo.record(
            UndoGroup::Atomic,
            &UndoEdit::new(0, &old, &new_text, self.cursor.pos, self.cursor.pos),
            now,
        );
        self.buffer = Buffer::from_str_lossy(&new_text);
        self.clamp_bookmarks();
        self.modified = true;
    }

    /// Paste a rectangle at the caret, as one undo unit (spec FR-E3).
    ///
    /// The rectangle is shaped by [`crate::buffer::paste_block`]: short document lines are
    /// padded so the pasted column stays aligned, a document line longer than the rectangle
    /// keeps its tail, and rows that run past the last line create new lines.
    ///
    /// Recorded as a whole-document rewrite rather than N per-line edits. A block paste
    /// touches a different offset on every row, and `UndoEdit` carries one offset, so an
    /// N-edit recording would have to get the ordering exactly right to survive a partial
    /// apply. The rewrite is one atomic step, undo restores the exact previous text, and the
    /// cost is bounded by the document, which for a block paste is already the touched area.
    pub fn paste_block(&mut self, text: &str, now: Instant) -> CoreResult<LineCol> {
        self.check_writable()?;
        if text.is_empty() {
            return Ok(self.cursor.pos);
        }
        let at = self.buffer.clamp(self.cursor.pos);
        let before = self.buffer.text_string();
        let mut probe = self.buffer.clone();
        let tab = self.tab_width();
        let after_pos = crate::buffer::paste_block(&mut probe, at, text, tab);
        let after = probe.text_string();
        if before == after {
            return Ok(self.cursor.pos);
        }
        self.undo.record(
            UndoGroup::Atomic,
            &UndoEdit::new(0, &before, &after, self.cursor.pos, after_pos),
            now,
        );
        self.buffer = probe;
        self.ends_with_newline = after.ends_with('\n');
        self.clamp_bookmarks();
        self.modified = true;
        self.cursor.pos = self.buffer.clamp(after_pos);
        self.cursor.goal_col = Some(self.buffer.display_col(
            self.cursor.pos.line,
            self.cursor.pos.col,
            self.tab_width(),
        ));
        self.selection = None;
        Ok(self.cursor.pos)
    }

    /// Indent every line the selection touches by one indent unit (spec FR-E6).
    ///
    /// Whole lines only: a selection starting mid-line still indents that whole line,
    /// because indenting from the caret would produce ragged text the user cannot predict.
    /// Recorded as one undo unit so a multi-line indent reverses in one step, which is what
    /// makes the caret count preserved across undo (the caret returns with the text).
    pub fn indent_lines(&mut self, unit: &str, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        if unit.is_empty() {
            return Ok(());
        }
        let (first, last) = self.selection_line_span();
        let mut lines: Vec<String> = (0..self.buffer.line_count())
            .map(|i| self.buffer.line_string(i))
            .collect();
        // A document ending in a newline has a trailing empty line that is the terminator's
        // artifact, not content. Indenting it would write spaces onto a line the user never
        // wrote and cannot see, so the span is clamped to the last line with content.
        let last = last
            .min(self.last_content_line())
            .min(lines.len().saturating_sub(1));
        for target in lines.iter_mut().take(last + 1).skip(first) {
            target.insert_str(0, unit);
        }
        let after = rebuild_text(&lines);
        self.replace_whole_document(after, now);
        // Every touched line grew by `unit`, so a caret inside the block moves right by
        // exactly that much; a caret outside it is untouched.
        if self.cursor.pos.line >= first && self.cursor.pos.line <= last {
            self.cursor.pos.col += unit.chars().count();
        }
        Ok(())
    }

    /// Remove one indent unit from every line the selection touches (spec FR-E6).
    ///
    /// Removes up to one unit: a line indented by 2 spaces under a 4-wide unit loses those
    /// 2 and no more, and a tab is removed as a whole tab.
    pub fn outdent_lines(&mut self, unit_width: usize, now: Instant) -> CoreResult<()> {
        self.check_writable()?;
        let unit_width = unit_width.max(1);
        let (first, last) = self.selection_line_span();
        let mut lines: Vec<String> = (0..self.buffer.line_count())
            .map(|i| self.buffer.line_string(i))
            .collect();
        let last = last
            .min(self.last_content_line())
            .min(lines.len().saturating_sub(1));
        let mut removed_from_caret_line = 0usize;
        let caret_line = self.cursor.pos.line;
        for (line, target) in lines.iter_mut().enumerate().take(last + 1).skip(first) {
            let removed = outdent_one(target, unit_width);
            if removed > 0 {
                *target = target[removed..].to_string();
                if line == caret_line {
                    removed_from_caret_line = removed;
                }
            }
        }
        let after = rebuild_text(&lines);
        self.replace_whole_document(after, now);
        if removed_from_caret_line > 0 {
            self.cursor.pos.col = self.cursor.pos.col.saturating_sub(removed_from_caret_line);
        }
        Ok(())
    }

    // ----------------------------------------------------------- bookmarks

    /// Toggle a bookmark on the caret's line (spec FR-V6).
    pub fn toggle_bookmark(&mut self) {
        let line = self.cursor.pos.line;
        if !self.bookmarks.remove(&line) {
            self.bookmarks.insert(line);
        }
    }

    /// Next bookmark at or after the caret, wrapping (spec FR-V6).
    pub fn next_bookmark(&mut self) -> Option<usize> {
        if self.bookmarks.is_empty() {
            return None;
        }
        let line = self.cursor.pos.line;
        let found = self
            .bookmarks
            .iter()
            .find(|b| **b > line)
            .or_else(|| self.bookmarks.iter().next())
            .copied();
        if let Some(l) = found {
            self.go_to_line(l);
        }
        found
    }

    /// Previous bookmark at or before the caret, wrapping (spec FR-V6).
    pub fn prev_bookmark(&mut self) -> Option<usize> {
        if self.bookmarks.is_empty() {
            return None;
        }
        let line = self.cursor.pos.line;
        let found = self
            .bookmarks
            .iter()
            .rev()
            .find(|b| **b < line)
            .or_else(|| self.bookmarks.iter().next_back())
            .copied();
        if let Some(l) = found {
            self.go_to_line(l);
        }
        found
    }

    pub fn clear_bookmarks(&mut self) {
        self.bookmarks.clear();
    }
    /// Jump the caret to `line`, clamped, preserving the goal column where possible.
    pub fn go_to_line(&mut self, line: usize) {
        let clamped = line.min(self.buffer.line_count() - 1);
        let goal = self.cursor.goal_col.unwrap_or(0);
        let col = self
            .buffer
            .col_from_display(clamped, goal, self.tab_width())
            .min(self.buffer.line_len_chars(clamped));
        self.cursor.pos = LineCol::new(clamped, col);
        self.selection = None;
    }

    // --------------------------------------------------------------- search

    /// Replace one matched span with `replacement`, as a single undo unit.
    ///
    /// Used by the UI's Replace command. The span comes from a match, which the caller has
    /// already resolved; this method does not search, so the search engine stays a pure
    /// function of a `Buffer` and the document stays the only place undo is recorded.
    pub fn replace_range(
        &mut self,
        from: LineCol,
        to: LineCol,
        replacement: &str,
        now: Instant,
    ) -> CoreResult<LineCol> {
        self.check_writable()?;
        let a = self.buffer.clamp(from);
        let b = self.buffer.clamp(to);
        if a == b && replacement.is_empty() {
            return Ok(self.cursor.pos);
        }
        let offset = self.buffer.char_offset(a).unwrap_or(0);
        let removed = if a == b {
            String::new()
        } else {
            let end = self.buffer.char_offset(b).unwrap_or(offset);
            self.slice_chars(offset, end)
        };
        let inserted = self.normalize_eols(replacement);
        self.undo.record(
            UndoGroup::Atomic,
            &UndoEdit::new(offset, &removed, &inserted, a, a),
            now,
        );
        if a != b {
            self.buffer.delete_range(a, b);
        }
        if !inserted.is_empty() {
            self.buffer.insert(a, &inserted);
        }
        self.modified = true;
        // A replacement that spans a line break removes lines, and bookmarks marked on them
        // have to go with the text they sat on -- the same rule `apply_edit` follows.
        let removed_lines = b.line.saturating_sub(a.line);
        if removed_lines > 0 {
            Self::shift_bookmarks_remove(&mut self.bookmarks, a.line + 1, b.line);
        }
        let added = inserted.matches('\n').count();
        if added > 0 && a.col == 0 {
            Self::shift_bookmarks_insert(&mut self.bookmarks, a.line, added);
        }
        // Only a replacement that touches the buffer's very end can change whether the file
        // ends with a terminator (FR-N2). Reusing `ends_with_newline_after_insert` here was
        // wrong: it asks about the insert position, and a mid-file replacement reports the
        // *inserted* text's ending, so replacing `foo` with `X` at the end of line 1 of a
        // 2-line document answered "no" and dropped line 2's terminator on save.
        if self.ends_with_newline {
            let last = self.buffer.line_count().saturating_sub(1);
            let buffer_now_ends_open = b.line == last
                && b.col >= self.buffer.line_len_chars(b.line)
                && inserted.is_empty()
                && !inserted.ends_with('\n');
            if buffer_now_ends_open {
                self.ends_with_newline = false;
            }
        } else if inserted.ends_with('\n') {
            let last = self.buffer.line_count().saturating_sub(1);
            if b.line >= last {
                self.ends_with_newline = true;
            }
        }
        self.selection = None;
        self.cursor.pos = self.advance_for_insert(a, &inserted);
        self.clamp_bookmarks();
        Ok(self.cursor.pos)
    }

    /// Replace many spans at once and return how many were replaced.
    ///
    /// Spans are applied **right to left**, which is what keeps every earlier span's
    /// coordinates valid: an edit only moves text at or after itself. Sorting is by start
    /// descending, so overlapping spans (the literal engine reports them) collapse into the
    /// expected result rather than corrupting each other.
    ///
    /// One undo unit per replacement, matching what a single Replace does, so `Ctrl+Z` after
    /// Replace All walks back one substitution at a time. That is the Notepad++ behavior and
    /// the one a user can reason about.
    pub fn replace_ranges(
        &mut self,
        spans: &[(LineCol, LineCol, String)],
        now: Instant,
    ) -> CoreResult<usize> {
        self.check_writable()?;
        if spans.is_empty() {
            return Ok(0);
        }
        let mut ordered: Vec<(LineCol, LineCol, String)> = spans.to_vec();
        ordered.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
        let mut done = 0usize;
        for (from, to, replacement) in ordered {
            if self.replace_range(from, to, &replacement, now).is_ok() {
                done += 1;
            }
        }
        Ok(done)
    }

    // -------------------------------------------------------------- folds
    //
    // `Document` stores fold ranges but never *detects* them: detection needs a language
    // name and the whole text, and a document that recomputed folds on every keystroke would
    // be doing O(lines) work in a path that must be O(1). The UI owns `refresh_folds` and
    // calls it when a file opens, when its language changes, and after an edit that could
    // have changed the structure. What lives here is the state machinery: shifting ranges as
    // lines come and go, and the rule that a fold never hides the caret's own line.

    /// Whether `line` is hidden by a collapsed range.
    pub fn line_is_hidden(&self, line: usize) -> bool {
        !crate::view::line_is_visible(line, &self.folds)
    }

    /// Unfold any collapsed range containing `line`. Editing inside a folded range unfolds
    /// it (spec FR-V5).
    pub fn unfold_line(&mut self, line: usize) {
        crate::folding::unfold_containing(&mut self.folds, line);
    }

    /// Recompute fold ranges from the current text, keeping the user's open/closed choices.
    ///
    /// `language` is the resolved syntax name, `""` for plain text. Ranges that no longer
    /// describe a real block disappear, and one that still describes the same span keeps its
    /// `collapsed` flag, so typing inside an open block never pops a fold shut under the
    /// user.
    pub fn refresh_folds(&mut self, language: &str) {
        self.refresh_folds_with(language, false);
    }

    /// [`Self::refresh_folds`], with the session-restore rule switched on.
    ///
    /// `preserve_collapsed` keeps a collapsed range that detection no longer proposes, provided
    /// its lines still exist. That is what a restored session needs -- its fold list is a record
    /// of what the user closed, and the heuristics get no vote on it -- and it is off by default
    /// because during editing a stale closed fold *is* a bug: it hides lines whose block was
    /// deleted around them, and the user cannot see why the text jumped.
    ///
    /// The two rules cannot be one function, because they answer different questions about who
    /// is right: detection is the authority while typing, the recorded session is the authority
    /// on restore.
    pub fn refresh_folds_with(&mut self, language: &str, preserve_collapsed: bool) {
        let text = self.buffer.text_string();
        let computed = crate::folding::compute_folds(&text, language);
        self.folds = if preserve_collapsed {
            crate::folding::merge_folds_preserving(&computed, &self.folds, self.buffer.line_count())
        } else {
            crate::folding::merge_folds(&computed, &self.folds)
        };
        self.clamp_folds();
    }

    /// Collapse or expand every detected fold (FR-V5 Fold All / Unfold All).
    pub fn fold_all(&mut self) {
        crate::folding::set_all_collapsed(&mut self.folds, true);
    }

    pub fn unfold_all(&mut self) {
        crate::folding::set_all_collapsed(&mut self.folds, false);
    }

    /// Toggle the fold whose header is `line` (gutter click, spec §6.9 row 12).
    ///
    /// Returns false when no fold starts there, so the caller can tell a marker click from a
    /// click on a line that merely looks foldable.
    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        crate::folding::toggle_fold_at(&mut self.folds, line)
    }

    /// The widest fold starting at `line`, for the gutter's `\u{25bc}`/`\u{25b6}` choice.
    pub fn fold_at(&self, line: usize) -> Option<&FoldRange> {
        self.folds
            .iter()
            .filter(|f| f.start == line)
            .max_by_key(|f| f.end)
    }

    /// Drop folds the document no longer has, after a whole-document replace.
    fn clamp_folds(&mut self) {
        let last = self.buffer.line_count().saturating_sub(1);
        self.folds
            .retain(|f| f.start <= last && f.end <= last && f.end > f.start);
    }

    /// The rows the viewport would show, using `geom`. Convenience over calling
    /// [`crate::view::layout_visible_rows`] with this document's own state.
    pub fn visible_rows(&self, geom: &crate::view::ViewGeometry) -> Vec<crate::view::VisualRow> {
        crate::view::layout_visible_rows(&self.buffer, geom, self.scroll.top_line, &self.folds)
    }

    /// Scroll so `target` is on screen, keeping the current top when it already is.
    ///
    /// This is the fold- and wrap-aware version of "center the caret". `geom` decides row
    /// heights, so the caller passes the geometry it is about to draw with -- the two must
    /// agree or the caret lands off screen.
    ///
    /// A target hidden by a fold unfolds first rather than being scrolled to as if it were
    /// visible: jumping to a bookmark inside a collapsed block must show the bookmark, not
    /// sit still.
    pub fn scroll_to_show(&mut self, geom: &crate::view::ViewGeometry, target: LineCol) -> bool {
        let mut unfolded = false;
        if self.line_is_hidden(target.line) {
            self.unfold_line(target.line);
            unfolded = true;
        }
        let rows = self.all_rows(geom);
        let Some(index) = geom.screen_row_of(&rows, target).map(|v| v as usize) else {
            // The position is not addressable in this layout at all (a column past the end of
            // a folded line, say). Scrolling to its line is the best available answer.
            let line_rows = self.all_rows(geom);
            let height = geom.viewport_rows().max(1);
            let at = line_rows
                .iter()
                .position(|r| r.line >= target.line)
                .unwrap_or(line_rows.len().saturating_sub(1));
            self.scroll.top_line = self.line_at_row(geom, at.saturating_sub(height / 2));
            return unfolded;
        };
        let height = geom.viewport_rows().max(1);
        let top = self.row_offset_of_top(geom);
        if index < top || index >= top + height {
            let want = index.saturating_sub(height.saturating_sub(1) / 2);
            self.scroll.top_line = self.line_at_row(geom, want);
        }
        unfolded
    }

    /// The row index of the row displaying `scroll.top_line`.
    ///
    /// A line index is not a row index once anything is folded: a collapsed range contributes one
    /// row for its header and nothing for its body, so every line below it sits that many rows
    /// earlier. Deriving the row from the line plus the hidden count above it is O(folds) instead
    /// of O(document), which matters because this runs inside `scroll_to_show` on every jump.
    fn row_offset_of_top(&self, geom: &crate::view::ViewGeometry) -> usize {
        let top = self.scroll.top_line;
        let hidden = crate::view::hidden_rows_before(top, &self.folds);
        // Without wrap the two agree exactly, and no row walk is needed at all.
        if !geom.word_wrap {
            return top.saturating_sub(hidden);
        }
        let rows = self.all_rows(geom);
        rows.iter()
            .position(|r| r.line >= top)
            .unwrap_or(rows.len())
    }

    /// The logical line occupying row `index` of [`Self::all_rows`].
    fn line_at_row(&self, geom: &crate::view::ViewGeometry, index: usize) -> usize {
        let rows = self.all_rows(geom);
        rows.get(index).or(rows.last()).map(|r| r.line).unwrap_or(0)
    }

    /// The fold- and wrap-aware row list for the whole document, top to bottom.
    ///
    /// O(rows in the document), so it belongs in a jump or a scrollbar calculation, not in a
    /// frame. The per-frame path is [`crate::view::layout_visible_rows`], which starts from
    /// a logical line and stops when the viewport is full.
    ///
    /// Delegates each line's splitting to [`crate::view::document_rows`], the same splitter the
    /// viewport and [`crate::view::total_row_count`] use. It used to re-derive rows here, and
    /// the two disagreed by one row per paragraph that wrapped at a space -- which showed up as
    /// `scroll_to_show` scrolling to a row the screen does not have.
    fn all_rows(&self, geom: &crate::view::ViewGeometry) -> Vec<crate::view::VisualRow> {
        crate::view::document_rows(&self.buffer, geom, &self.folds)
    }

    // ------------------------------------------------------------- helpers

    /// Normalize CRLF/CR to the document's EOL, then to LF for storage (FR-E13, FR-N4).
    fn normalize_eols(&self, text: &str) -> String {
        // The buffer stores LF only; the document EOL is applied at save time by S5.
        text.replace("\r\n", "\n").replace('\r', "\n")
    }

    /// Index of the last line holding content.
    ///
    /// For a document whose text ends with a newline, `line_count()` counts a final empty
    /// line that is a terminator artifact rather than content. Line-oriented operations
    /// (sort, comment, case) should not treat it as a line to transform.
    fn last_content_line(&self) -> usize {
        let count = self.buffer.line_count();
        if count > 1 && self.buffer.text_string().ends_with('\n') {
            count - 2
        } else {
            count - 1
        }
    }

    /// Lines the selection touches, for line-oriented operations (FR-E7..E9).
    fn selection_line_span(&self) -> (usize, usize) {
        match self.selection {
            Some(sel) if !sel.is_empty() => {
                let (s, e) = sel.bounds();
                // A selection reaching column 0 of the end line does not include that
                // line's content, but Notepad++ still treats it as touched for line ops.
                (s.line, e.line)
            }
            _ => (self.cursor.pos.line, self.cursor.pos.line),
        }
    }

    /// Delete the current selection and return the deleted text, recording undo.
    fn take_selection_text(&mut self, now: Instant) -> String {
        let Some(sel) = self.selection else {
            return String::new();
        };
        if sel.is_empty() {
            return String::new();
        }
        let text = self.selected_text().unwrap_or_default();
        match sel.kind {
            SelectionKind::Range => {
                let (s, e) = sel.bounds();
                let a = self.buffer.char_offset(s).unwrap_or(0);
                let b = self.buffer.char_offset(e).unwrap_or(0);
                self.undo.record(
                    UndoGroup::Atomic,
                    &UndoEdit::new(a, &text, "", self.cursor.pos, s),
                    now,
                );
                let _ = (a, b);
                self.buffer.delete_range(s, e);
            }
            SelectionKind::Block => {
                // Delete each row of the rectangle bottom-up so earlier offsets survive.
                let (s, e) = sel.bounds();
                let lo = s.col.min(e.col);
                let hi = s.col.max(e.col);
                for line in (s.line..=e.line).rev() {
                    let line_len = self.buffer.line_len_chars(line);
                    if lo >= line_len {
                        continue;
                    }
                    let from = LineCol::new(line, lo);
                    let to = LineCol::new(line, hi.min(line_len));
                    let a = self.buffer.char_offset(from).unwrap_or(0);
                    let seg = {
                        let b = self.buffer.char_offset(to).unwrap_or(a);
                        self.slice_chars(a, b)
                    };
                    self.undo.record(
                        UndoGroup::Atomic,
                        &UndoEdit::new(a, &seg, "", self.cursor.pos, from),
                        now,
                    );
                    self.buffer.delete_range(from, to);
                }
            }
        }
        self.cursor.pos = self.buffer.clamp(sel.bounds().0);
        self.selection = None;
        self.clamp_bookmarks();
        text
    }

    /// Delete the current selection **without** recording undo, returning the deleted
    /// text and the position where deletion started.
    ///
    /// Callers that combine the deletion with a subsequent insertion must use this and
    /// then record one [`UndoGroup`] covering both halves, so the operation stays a single
    /// undo unit as FR-E4 requires.
    fn delete_selection_silent(&mut self) -> (String, LineCol) {
        let Some(sel) = self.selection else {
            return (String::new(), self.cursor.pos);
        };
        if sel.is_empty() {
            return (String::new(), self.cursor.pos);
        }
        let text = self.selected_text().unwrap_or_default();
        match sel.kind {
            SelectionKind::Range => {
                let (s, e) = sel.bounds();
                self.buffer.delete_range(s, e);
                self.cursor.pos = self.buffer.clamp(s);
                self.selection = None;
                (text, s)
            }
            SelectionKind::Block => {
                let (first_line, last_line) = if sel.anchor.line <= sel.focus.line {
                    (sel.anchor.line, sel.focus.line)
                } else {
                    (sel.focus.line, sel.anchor.line)
                };
                let lo = sel.anchor.col.min(sel.focus.col);
                let hi = sel.anchor.col.max(sel.focus.col);
                for line in (first_line..=last_line).rev() {
                    let line_len = self.buffer.line_len_chars(line);
                    if lo >= line_len {
                        continue;
                    }
                    let from = LineCol::new(line, lo);
                    let to = LineCol::new(line, hi.min(line_len));
                    self.buffer.delete_range(from, to);
                }
                let start = LineCol::new(first_line, lo);
                self.cursor.pos = self.buffer.clamp(start);
                self.selection = None;
                (text, start)
            }
        }
    }

    /// Remove `sel`'s text as a recorded undo unit. Used by range/backspace/delete paths.
    fn remove_selection_text(
        &mut self,
        sel: Selection,
        group: UndoGroup,
        now: Instant,
    ) -> CoreResult<String> {
        if sel.is_empty() {
            return Ok(String::new());
        }
        let (s, e) = sel.bounds();
        let a = self.buffer.char_offset(s).unwrap_or(0);
        let b = self.buffer.char_offset(e).unwrap_or(0);
        let removed = self.slice_chars(a, b);
        if removed.is_empty() {
            return Ok(removed);
        }
        self.undo.record(
            group,
            &UndoEdit::new(a, &removed, "", self.cursor.pos, s),
            now,
        );
        let reached_final_line = e.line + 1 >= self.buffer.line_count();
        self.buffer.delete_range(s, e);
        self.cursor.pos = self.buffer.clamp(s);
        self.selection = None;
        self.modified = true;
        self.clamp_bookmarks();
        if reached_final_line {
            // Whatever terminator sat on the last line is gone with it.
            self.ends_with_newline = false;
        }
        Ok(removed)
    }

    /// Recompute [`Document::ends_with_newline`] for an insertion about to happen.
    ///
    /// Call this **before** mutating the buffer: `at` is the end of the buffer only until the
    /// insert lands, after which the new caret position always looks like the end.
    ///
    /// A newline typed at the very end is the user asking for a trailing newline. Anything else
    /// typed onto a final empty line takes that back, because that line is now content and the
    /// file ends on it. Edits above the last line leave the answer alone, which is what keeps an
    /// unrelated change in the middle of a file from altering how the file ends.
    fn ends_with_newline_after_insert(&self, inserted: &str, at: LineCol) -> bool {
        let last = self.buffer.line_count().saturating_sub(1);
        let at_buffer_end = at.line >= last && at.col >= self.buffer.line_len_chars(at.line);
        if !at_buffer_end {
            return self.ends_with_newline;
        }
        inserted.ends_with('\n')
    }

    // ---------------------------------------------------------- bookmarks
    //
    // Bookmarks are line numbers, so any edit that adds or removes lines has to move
    // them or they end up pointing at unrelated text. The rule is deliberately simple
    // and is documented in `docs/DEVIATIONS.md`:
    //
    //  - Inserting lines at `at` moves every bookmark on or after `at` down by the
    //    number of added lines. A bookmark on the insertion line itself moves, because
    //    the inserted text pushes existing content down rather than replacing it.
    //  - Removing lines `[first, last]` deletes bookmarks inside the range and pulls
    //    everything below up by the number of removed lines.
    //  - A swap (`move_line_up`/`move_line_down`) relocates the moved block's bookmarks
    //    to the block's new position and shifts the lines it jumped over.
    //
    // Undo and redo restore them for free: every line op that can change line structure
    // also records enough information for [`Document::apply_edit`] to call back into
    // these helpers.

    /// Shift bookmarks down to account for `added` new lines inserted at `at_line`.
    fn shift_bookmarks_insert(bookmarks: &mut BTreeSet<usize>, at_line: usize, added: usize) {
        if added == 0 {
            return;
        }
        let moved = bookmarks
            .iter()
            .map(|l| if *l >= at_line { l + added } else { *l })
            .collect();
        *bookmarks = moved;
    }

    /// Drop bookmarks inside `[first, last]` and pull the rest up by the span size.
    fn shift_bookmarks_remove(bookmarks: &mut BTreeSet<usize>, first: usize, last: usize) {
        let removed_count = last - first + 1;
        let moved = bookmarks
            .iter()
            .filter_map(|l| {
                if *l >= first && *l <= last {
                    None
                } else if *l > last {
                    Some(l - removed_count)
                } else {
                    Some(*l)
                }
            })
            .collect();
        *bookmarks = moved;
    }

    /// Relocate bookmarks when the line block `[first, last]` moved by `delta` lines.
    ///
    /// `delta` is signed (`new_first - first`). Bookmarks inside the block ride along;
    /// bookmarks in the gap the block jumped through shift the opposite way by the
    /// block's size, because that gap closes as the block vacates it.
    fn shift_bookmarks_swap(
        bookmarks: &mut BTreeSet<usize>,
        first: usize,
        last: usize,
        delta: i64,
    ) {
        if delta == 0 {
            return;
        }
        let span = (last - first + 1) as i64;
        let moved = bookmarks
            .iter()
            .map(|&l| {
                let l = l as i64;
                let f = first as i64;
                let e = last as i64;
                // The destination window is `[f+delta, e+delta]`. The lines the block
                // jumps over are those between the source and destination windows, and
                // they all move by the same amount in the same direction, so a single
                // branch covers both up and down.
                let (lo, hi) = if delta > 0 {
                    (e + 1, e + delta)
                } else {
                    (f + delta, f - 1)
                };
                let shifted = if l >= f && l <= e {
                    l + delta
                } else if l >= lo && l <= hi {
                    l - delta.signum() * span
                } else {
                    l
                };
                shifted as usize
            })
            .collect();
        *bookmarks = moved;
    }

    /// Drop any bookmark that no longer addresses a real line.
    ///
    /// The shift helpers already keep bookmarks aligned with the text they mark, but a
    /// line-count change that happens outside them (a whole-document rewrite, an undo step
    /// whose region ends exactly at the last line) can leave a bookmark pointing past the
    /// end. The renderer would then index a line that does not exist, so every mutation
    /// funnels through here as a final safety net.
    fn clamp_bookmarks(&mut self) {
        let last = self.buffer.line_count().saturating_sub(1);
        let kept: BTreeSet<usize> = self
            .bookmarks
            .iter()
            .copied()
            .filter(|l| *l <= last)
            .collect();
        self.bookmarks = kept;
    }

    /// Move the caret forward past freshly inserted text.
    fn advance_for_insert(&mut self, at: LineCol, text: &str) -> LineCol {
        let newlines = text.matches('\n').count();
        let new_pos = if newlines == 0 {
            LineCol::new(at.line, at.col + text.chars().count())
        } else {
            let last_chunk_len = text
                .rsplit('\n')
                .next()
                .map(|s| s.chars().count())
                .unwrap_or(0);
            LineCol::new(at.line + newlines, last_chunk_len)
        };
        let clamped = self.buffer.clamp(new_pos);
        self.cursor.pos = clamped;
        self.cursor.goal_col = Some(self.buffer.display_col(clamped.line, clamped.col, 4));
        self.selection = None;
        clamped
    }
}

/// How many leading characters one outdent step removes from `line`.
///
/// A leading tab is one step whatever its rendered width. Otherwise drop up to
/// `unit_width` leading spaces, stopping at the first non-space, so a line indented by
/// fewer spaces than the unit never loses content.
fn outdent_one(line: &str, unit_width: usize) -> usize {
    let mut chars = line.chars();
    match chars.next() {
        Some('\t') => 1,
        Some(' ') => {
            let mut taken = 1;
            while taken < unit_width {
                match chars.next() {
                    Some(' ') => taken += 1,
                    _ => break,
                }
            }
            taken
        }
        _ => 0,
    }
}

/// Rebuild document text from a full list of logical lines.
///
/// `Buffer::line_string` over every `line_count()` line already includes the trailing
/// empty element for a document that ends in a newline (`"a\n"` parses as two lines,
/// `"a"` and `""`), so a plain `join` reproduces the original byte-for-byte. Do **not**
/// append an extra newline based on a separate flag: that double-counts and produces a
/// spurious blank line.
fn rebuild_text(lines: &[String]) -> String {
    lines.join("\n")
}

/// Apply a case transform. Title case uppercases the first alphabetic character of each
/// whitespace-delimited word and lowercases the rest, per FR-E8's "Title case".
fn apply_case(text: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Upper => text.to_uppercase(),
        CaseMode::Lower => text.to_lowercase(),
        CaseMode::Title => {
            let mut out = String::with_capacity(text.len());
            let mut in_word = false;
            for ch in text.chars() {
                if ch.is_whitespace() {
                    in_word = false;
                    out.push(ch);
                } else if !in_word && ch.is_alphabetic() {
                    in_word = true;
                    out.extend(ch.to_uppercase());
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
            out
        }
    }
}

/// Remove a leading comment token, keeping the indent. Returns `None` when the line is
/// not commented.
fn strip_comment_token(line: &str, token: &str) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let rest = &line[indent_len..];
    let stripped = rest.strip_prefix(token)?;
    // Also drop one space that was inserted between the token and the code, so
    // "// foo" round-trips back to "foo" rather than leaving "//foo" -> " foo".
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    Some(format!("{}{}", &line[..indent_len], stripped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn doc(text: &str) -> Document {
        Document::from_decoded(
            PathBuf::from("/tmp/x.txt"),
            text,
            EncodingId::Utf8,
            Eol::Lf,
            false,
        )
    }

    #[test]
    fn display_name_uses_the_file_name() {
        assert_eq!(doc("x").display_name(), "x.txt");
        assert_eq!(
            Document::new_untitled(EncodingId::Utf8, Eol::Lf).display_name(),
            "Untitled"
        );
    }

    #[test]
    fn insert_marks_modified_and_records_undo() {
        let mut d = doc("hello");
        assert!(!d.modified);
        d.doc_end();
        d.insert_text("!", UndoGroup::Typing, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "hello!");
        assert!(d.modified);
        assert_eq!(d.undo.len(), 1);
    }

    #[test]
    fn readonly_refuses_every_mutation() {
        let mut d = Document::from_decoded(
            PathBuf::from("/tmp/ro.txt"),
            "content",
            EncodingId::Utf8,
            Eol::Lf,
            true,
        );
        let err = d.insert_text("x", UndoGroup::Atomic, t0()).unwrap_err();
        assert!(err.to_string().contains("read-only"), "{err}");
        assert!(d.delete_selection(t0()).is_err());
        assert!(d.delete_char_before(t0()).is_err());
        assert!(d.delete_char_after(t0()).is_err());
        assert!(d.delete_line(t0()).is_err());
        assert!(d.duplicate_line(t0()).is_err());
        assert!(d.move_line_up(t0()).is_err());
        assert!(d.move_line_down(t0()).is_err());
        assert!(d.sort_lines(SortOrder::Ascending, t0()).is_err());
        assert!(d.change_case(CaseMode::Upper, t0()).is_err());
        assert!(d.toggle_comment("//", t0()).is_err());
        assert_eq!(d.buffer.text_string(), "content", "text untouched");
        assert!(!d.modified);
        assert!(d.undo.is_empty(), "no history for refused edits");
    }

    #[test]
    fn insert_text_replaces_selection_as_one_unit() {
        let mut d = doc("hello world");
        d.begin_selection(LineCol::new(0, 0), LineCol::new(0, 5), SelectionKind::Range);
        d.insert_text("bye", UndoGroup::Atomic, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "bye world");
        assert_eq!(d.undo.len(), 1);
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "hello world");
    }

    #[test]
    fn crlf_input_into_a_lf_document_stores_lf_only() {
        let mut d = doc("a\nb");
        d.set_cursor(LineCol::new(0, 1), false);
        d.insert_text("X\r\nY\rZ", UndoGroup::Atomic, t0()).unwrap();
        let text = d.buffer.text_string();
        assert!(!text.contains('\r'), "buffer must stay LF-only: {text:?}");
        assert_eq!(text, "aX\nY\nZ\nb");
        assert_eq!(d.eol, Eol::Lf);
    }

    #[test]
    fn undo_restores_text_and_cursor() {
        let mut d = doc("start");
        d.doc_end();
        d.insert_text("!!", UndoGroup::Atomic, t0()).unwrap();
        assert_eq!(d.cursor.pos, LineCol::new(0, 7));
        assert!(d.undo_one());
        assert_eq!(d.buffer.text_string(), "start");
        assert_eq!(d.cursor.pos, LineCol::new(0, 5), "caret returns too");
    }

    #[test]
    fn redo_reapplies() {
        let mut d = doc("start");
        d.doc_end();
        d.insert_text("!!", UndoGroup::Atomic, t0()).unwrap();
        d.undo_one();
        assert!(d.redo_one());
        assert_eq!(d.buffer.text_string(), "start!!");
    }

    #[test]
    fn undo_on_empty_history_is_false() {
        let mut d = doc("x");
        assert!(!d.undo_one());
        assert!(!d.redo_one());
    }

    #[test]
    fn multiline_insert_positions_the_caret_correctly() {
        let mut d = doc("ab");
        d.set_cursor(LineCol::new(0, 1), false);
        d.insert_text("X\nYZ", UndoGroup::Atomic, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "aX\nYZb");
        assert_eq!(d.cursor.pos, LineCol::new(1, 2));
    }

    #[test]
    fn backspace_at_doc_start_does_nothing() {
        let mut d = doc("abc");
        d.set_cursor(LineCol::new(0, 0), false);
        assert_eq!(d.delete_char_before(t0()).unwrap(), None);
        assert_eq!(d.buffer.text_string(), "abc");
    }

    #[test]
    fn backspace_joins_lines() {
        let mut d = doc("ab\ncd");
        d.set_cursor(LineCol::new(1, 0), false);
        d.delete_char_before(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "abcd");
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "ab\ncd");
    }

    #[test]
    fn delete_forward_at_end_does_nothing() {
        let mut d = doc("abc");
        d.set_cursor(LineCol::new(0, 3), false);
        assert_eq!(d.delete_char_after(t0()).unwrap(), None);
    }

    // ------------------------------------------------------------ movement

    #[test]
    fn vertical_movement_preserves_goal_column() {
        let mut d = doc("a\nvery long line here\nb");
        d.set_cursor(LineCol::new(0, 1), false);
        d.move_vertical(1, false);
        assert_eq!(d.cursor.pos, LineCol::new(1, 1));
        d.move_vertical(1, false);
        // Third line is only 1 char: caret clamps to its end but the goal survives.
        assert_eq!(d.cursor.pos, LineCol::new(2, 1));
        d.move_vertical(-1, false);
        assert_eq!(d.cursor.pos, LineCol::new(1, 1), "goal column restored");
    }

    #[test]
    fn vertical_movement_clamps_at_document_edges() {
        let mut d = doc("one\ntwo");
        d.set_cursor(LineCol::new(1, 2), false);
        // Jumping far up lands on line 0; the goal column (2) is reachable there, so the
        // caret keeps it rather than snapping to column 0.
        d.move_vertical(-99, false);
        assert_eq!(d.cursor.pos.line, 0);
        assert_eq!(d.cursor.pos.col, 2, "goal column is honored when reachable");
        // Jumping far down lands on the last line, whose goal also fits.
        d.move_vertical(99, false);
        assert_eq!(d.cursor.pos.line, 1);

        // A goal column beyond a short target line clamps to that line's end.
        let mut wide = doc("x\nonly-one-char-line");
        wide.set_cursor(LineCol::new(0, 1), false);
        wide.move_vertical(1, false);
        assert_eq!(wide.cursor.pos, LineCol::new(1, 1));
        wide.move_vertical(-1, false);
        assert_eq!(
            wide.cursor.pos,
            LineCol::new(0, 1),
            "goal survived the clamp"
        );
    }

    #[test]
    fn horizontal_moves_wrap_across_lines() {
        let mut d = doc("ab\ncd");
        d.set_cursor(LineCol::new(0, 2), false);
        d.move_right();
        assert_eq!(d.cursor.pos, LineCol::new(1, 0));
        d.move_left();
        assert_eq!(d.cursor.pos, LineCol::new(0, 2));
    }

    #[test]
    fn doc_start_end_and_line_edges() {
        let mut d = doc("one\ntwo\nthree");
        d.set_cursor(LineCol::new(1, 1), false);
        d.doc_end();
        assert_eq!(d.cursor.pos, LineCol::new(2, 5));
        d.doc_start();
        assert_eq!(d.cursor.pos, LineCol::new(0, 0));
        d.set_cursor(LineCol::new(1, 2), false);
        d.line_end();
        assert_eq!(d.cursor.pos, LineCol::new(1, 3));
        d.line_start();
        assert_eq!(d.cursor.pos, LineCol::new(1, 0));
    }

    #[test]
    fn extend_selection_keeps_the_anchor() {
        let mut d = doc("hello world");
        d.set_cursor(LineCol::new(0, 0), false);
        d.set_cursor(LineCol::new(0, 5), true);
        let sel = d.selection.expect("selecting");
        assert_eq!(sel.anchor, LineCol::new(0, 0));
        assert_eq!(sel.focus, LineCol::new(0, 5));
        assert_eq!(d.selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn select_all_covers_the_document() {
        let mut d = doc("one\ntwo");
        d.select_all();
        assert_eq!(d.selected_text().as_deref(), Some("one\ntwo"));
        let empty = doc("");
        let mut empty = empty;
        empty.select_all();
        assert_eq!(empty.selection, None, "nothing to select");
    }

    // ------------------------------------------------------------ line ops

    #[test]
    fn duplicate_line_inserts_below() {
        let mut d = doc("a\nb\nc");
        d.set_cursor(LineCol::new(1, 0), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb\nb\nc");
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "a\nb\nc");
    }

    #[test]
    fn duplicate_line_on_a_single_line_doc() {
        let mut d = doc("only");
        d.set_cursor(LineCol::new(0, 2), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "only\nonly\n");
    }

    #[test]
    fn duplicating_the_last_line_of_an_unterminated_doc_adds_a_terminator() {
        // The copy is built as `content + "\n"`, and an unterminated document prepends a
        // separator so the copy lands on its own line. This asymmetry is what the S2 undo
        // fuzz model mirrors, so it is pinned here.
        let mut d = doc("abc");
        d.set_cursor(LineCol::new(0, 0), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "abc\nabc\n");
    }

    #[test]
    fn duplicating_the_last_line_of_a_terminated_doc_adds_no_extra_newline() {
        let mut d = doc("a\n");
        d.set_cursor(LineCol::new(0, 0), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\na\n");
        assert_eq!(d.buffer.line_count(), 3);
    }

    #[test]
    fn duplicating_the_line_of_an_empty_document() {
        // Measured: `"\n\n"`. The separator and the copy's own terminator are both added,
        // so `line_count()` reports 3. This is a rope line-accounting artifact rather than
        // line content, which is why the fuzz model assigns it directly.
        let mut d = doc("");
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "\n\n");
        assert_eq!(d.buffer.line_count(), 3);
    }

    #[test]
    fn duplicating_a_blank_line_between_content_lines() {
        let mut d = doc("a\n\nb");
        d.set_cursor(LineCol::new(1, 0), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\n\n\nb");
        d.undo_one();
        assert_eq!(
            d.buffer.text_string(),
            "a\n\nb",
            "undo restores the blank line"
        );
    }

    #[test]
    fn duplicate_selected_lines() {
        let mut d = doc("a\nb\nc");
        d.begin_selection(LineCol::new(0, 0), LineCol::new(1, 1), SelectionKind::Range);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb\na\nb\nc");
    }

    #[test]
    fn delete_line_last_line_and_single_line() {
        let mut d = doc("a\nb");
        d.set_cursor(LineCol::new(1, 0), false);
        d.delete_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\n");
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "a\nb");

        let mut single = doc("solo");
        single.set_cursor(LineCol::new(0, 2), false);
        single.delete_line(t0()).unwrap();
        assert_eq!(single.buffer.text_string(), "");
        assert_eq!(single.buffer.line_count(), 1);
        single.undo_one();
        assert_eq!(single.buffer.text_string(), "solo");
    }

    #[test]
    fn delete_line_on_an_empty_doc_does_not_panic() {
        let mut d = doc("");
        d.delete_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "");
    }

    #[test]
    fn move_line_up_and_down() {
        let mut d = doc("a\nb\nc");
        d.set_cursor(LineCol::new(2, 0), false);
        d.move_line_up(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nc\nb");
        d.move_line_down(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb\nc");
    }

    #[test]
    fn line_ops_preserve_trailing_newline_exactly() {
        // Each of these rewrites the whole document, so a trailing-newline mistake shows
        // up as a spurious extra blank line.
        for (name, op) in [
            ("sort", as_op(SortKind::Sort)),
            ("comment", as_op(SortKind::Comment)),
            ("case", as_op(SortKind::Case)),
        ] {
            let mut d = doc("b\na\nc\n");
            d.select_all();
            op(&mut d);
            assert_eq!(
                d.buffer.line_count(),
                4,
                "{name} changed the line count of a trailing-newline doc"
            );
            assert!(
                d.buffer.text_string().ends_with('\n'),
                "{name} dropped the trailing newline"
            );
            assert!(
                !d.buffer.text_string().ends_with("\n\n"),
                "{name} added a spurious blank line: {:?}",
                d.buffer.text_string()
            );
        }
    }

    enum SortKind {
        Sort,
        Comment,
        Case,
    }

    fn as_op(kind: SortKind) -> Box<dyn Fn(&mut Document)> {
        Box::new(move |d: &mut Document| {
            let t = Instant::now();
            match kind {
                SortKind::Sort => {
                    d.sort_lines(SortOrder::Ascending, t).unwrap();
                }
                SortKind::Comment => {
                    d.toggle_comment("//", t).unwrap();
                }
                SortKind::Case => {
                    d.change_case(CaseMode::Upper, t).unwrap();
                }
            }
        })
    }

    #[test]
    fn line_ops_on_a_doc_without_trailing_newline_add_none() {
        let mut d = doc("b\na\nc");
        d.select_all();
        d.sort_lines(SortOrder::Ascending, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb\nc");
        let mut c = doc("b\na\nc");
        c.select_all();
        c.toggle_comment("#", t0()).unwrap();
        assert_eq!(c.buffer.text_string(), "#b\n#a\n#c");
    }

    #[test]
    fn move_line_at_the_edges_is_a_no_op() {
        let mut d = doc("a\nb");
        d.set_cursor(LineCol::new(0, 0), false);
        d.move_line_up(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb");
        d.set_cursor(LineCol::new(1, 0), false);
        d.move_line_down(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb");
    }

    #[test]
    fn move_line_preserves_a_trailing_newline() {
        let mut d = doc("a\nb\nc\n");
        d.set_cursor(LineCol::new(0, 0), false);
        d.move_line_down(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "b\na\nc\n");
    }

    #[test]
    fn sort_lines_ascending_and_descending() {
        let mut d = doc("cherry\napple\nbanana");
        d.select_all();
        d.sort_lines(SortOrder::Ascending, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "apple\nbanana\ncherry");
        d.sort_lines(SortOrder::Descending, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "cherry\nbanana\napple");
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "apple\nbanana\ncherry");
    }

    #[test]
    fn sort_lines_mixed_case_and_numbers() {
        let mut d = doc("b\nA\nc\nB\na");
        d.select_all();
        d.sort_lines(SortOrder::Ascending, t0()).unwrap();
        // Byte order: uppercase sorts before lowercase, which is what a plain
        // lexicographic sort gives and is what Notepad++ "sort lines" does.
        assert_eq!(d.buffer.text_string(), "A\nB\na\nb\nc");

        let mut nums = doc("10\n9\n100\n1");
        nums.select_all();
        nums.sort_lines(SortOrder::Ascending, t0()).unwrap();
        assert_eq!(nums.buffer.text_string(), "1\n10\n100\n9");
    }

    #[test]
    fn sort_lines_only_touches_the_selected_range() {
        let mut d = doc("z\n---\ny\nx");
        d.begin_selection(LineCol::new(2, 0), LineCol::new(3, 1), SelectionKind::Range);
        d.sort_lines(SortOrder::Ascending, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "z\n---\nx\ny");
    }

    #[test]
    fn sorting_an_already_sorted_range_records_no_history() {
        let mut d = doc("a\nb\nc");
        d.select_all();
        let before = d.undo.len();
        d.sort_lines(SortOrder::Ascending, t0()).unwrap();
        assert_eq!(d.undo.len(), before, "no-op must not add an undo step");
    }

    // ---------------------------------------------------------- case ops

    #[test]
    fn case_ops_on_a_selection() {
        let mut d = doc("hello world");
        d.select_all();
        d.change_case(CaseMode::Upper, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "HELLO WORLD");
        d.change_case(CaseMode::Lower, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "hello world");
        d.change_case(CaseMode::Title, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "Hello World");
    }

    #[test]
    fn case_op_without_a_selection_targets_the_word() {
        let mut d = doc("one two three");
        d.set_cursor(LineCol::new(0, 5), false);
        d.change_case(CaseMode::Upper, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "one TWO three");
    }

    #[test]
    fn case_op_is_undoable_as_one_unit() {
        let mut d = doc("abc def");
        d.select_all();
        d.change_case(CaseMode::Upper, t0()).unwrap();
        assert_eq!(d.undo.len(), 1);
        d.undo_one();
        assert_eq!(d.buffer.text_string(), "abc def");
    }

    #[test]
    fn unicode_case_round_trip() {
        let mut d = doc("straße");
        d.select_all();
        d.change_case(CaseMode::Upper, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "STRASSE");
    }

    // -------------------------------------------------------- comment ops

    #[test]
    fn toggle_comment_adds_then_strips() {
        let mut d = doc("aaa\nbbb");
        d.select_all();
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "//aaa\n//bbb");
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "aaa\nbbb");
    }

    #[test]
    fn toggle_comment_keeps_the_indent() {
        let mut d = doc("    indented");
        d.select_all();
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "    //indented");
    }

    #[test]
    fn toggle_comment_on_a_mixed_selection_adds_everywhere() {
        let mut d = doc("//one\ntwo\n//three");
        d.select_all();
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(
            d.buffer.text_string(),
            "//one\n//two\n//three",
            "mixed selections go to the 'add' side"
        );
        // Now every line is commented, so the next toggle strips.
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "one\ntwo\nthree");
    }

    #[test]
    fn toggle_comment_skips_blank_lines() {
        let mut d = doc("a\n\nb");
        d.select_all();
        d.toggle_comment("#", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "#a\n\n#b");
    }

    #[test]
    fn toggle_comment_on_one_line_only_touches_that_line() {
        let mut d = doc("a\nb\nc");
        d.set_cursor(LineCol::new(1, 0), false);
        d.toggle_comment("//", t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\n//b\nc");
    }

    // ---------------------------------------------------------- block sel

    #[test]
    fn block_selection_pads_short_lines() {
        let mut d = doc("abcdef\nab\nabcde");
        d.begin_selection(LineCol::new(0, 1), LineCol::new(2, 4), SelectionKind::Block);
        // Cols 1..4 of each line, padding short lines to the block width of 3:
        // "abcdef"->"bcd", "ab"->"b  ", "abcde"->"bcd".
        assert_eq!(d.selected_text().as_deref(), Some("bcd\nb  \nbcd"));
    }

    #[test]
    fn block_case_applies_per_row() {
        let mut d = doc("abcdef\nabcd\nabc");
        d.begin_selection(LineCol::new(0, 0), LineCol::new(2, 3), SelectionKind::Block);
        d.change_case(CaseMode::Upper, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "ABCdef\nABCd\nABC");
    }

    #[test]
    fn block_delete_removes_each_row() {
        let mut d = doc("abcdef\nabcd\nabc");
        d.begin_selection(LineCol::new(0, 0), LineCol::new(2, 2), SelectionKind::Block);
        d.delete_selection(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "cdef\ncd\nc");
    }

    #[test]
    fn degenerate_block_is_not_empty_for_copy_fallback() {
        let mut d = doc("hello");
        d.begin_selection(LineCol::new(0, 0), LineCol::new(0, 3), SelectionKind::Block);
        let sel = d.selection.unwrap();
        assert!(sel.is_degenerate_block());
        assert!(!sel.is_empty());
    }

    // ---------------------------------------------------------- bookmarks

    #[test]
    fn bookmark_toggle_and_navigation() {
        let mut d = doc("a\nb\nc\nd");
        d.set_cursor(LineCol::new(1, 0), false);
        d.toggle_bookmark();
        d.set_cursor(LineCol::new(3, 0), false);
        d.toggle_bookmark();
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&1, &3]);

        d.set_cursor(LineCol::new(0, 0), false);
        assert_eq!(d.next_bookmark(), Some(1));
        assert_eq!(d.next_bookmark(), Some(3));
        assert_eq!(d.next_bookmark(), Some(1), "wraps");
        assert_eq!(d.prev_bookmark(), Some(3), "wraps backwards");
    }

    #[test]
    fn bookmark_toggle_removes() {
        let mut d = doc("a\nb");
        d.set_cursor(LineCol::new(0, 0), false);
        d.toggle_bookmark();
        d.toggle_bookmark();
        assert!(d.bookmarks.is_empty());
    }

    #[test]
    fn bookmarks_on_an_empty_set_are_safe() {
        let mut d = doc("a");
        assert_eq!(d.next_bookmark(), None);
        assert_eq!(d.prev_bookmark(), None);
        d.clear_bookmarks();
    }

    #[test]
    fn bookmarks_shift_when_a_line_is_inserted_above() {
        // Decided behavior: bookmarks track the text they mark, so inserting a line above
        // pushes them down. A bookmark on line 2 still marks the same content afterwards.
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(2);
        d.set_cursor(LineCol::new(0, 0), false);
        d.insert_text("new\n", UndoGroup::Atomic, t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "new\na\nb\nc");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&3]);
        assert_eq!(d.buffer.line_string(3), "c");
    }

    #[test]
    fn bookmark_below_a_deletion_is_pulled_up() {
        let mut d = doc("a\nb\nc\nd");
        d.bookmarks.insert(3);
        d.set_cursor(LineCol::new(1, 0), false);
        d.delete_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nc\nd");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&2]);
        assert_eq!(d.buffer.line_string(2), "d");
    }

    #[test]
    fn bookmark_on_a_deleted_line_is_dropped() {
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(1);
        d.set_cursor(LineCol::new(1, 0), false);
        d.delete_line(t0()).unwrap();
        assert!(d.bookmarks.is_empty());
    }

    #[test]
    fn bookmarks_ride_along_with_a_moved_line() {
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(0);
        d.set_cursor(LineCol::new(0, 0), false);
        d.move_line_down(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "b\na\nc");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&1]);
        assert_eq!(d.buffer.line_string(1), "a");
    }

    #[test]
    fn a_line_jump_shifts_the_bookmark_it_leapt_over() {
        // Moving line 0 down two places passes line 1, whose bookmark must come up.
        let mut d = doc("a\nb\nc\nd");
        d.bookmarks.insert(1);
        d.set_cursor(LineCol::new(0, 0), false);
        d.move_line_down(t0()).unwrap();
        d.move_line_down(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "b\nc\na\nd");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&0]);
        assert_eq!(d.buffer.line_string(0), "b");
    }

    #[test]
    fn bookmarks_move_up_with_a_line_moved_up() {
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(2);
        d.set_cursor(LineCol::new(2, 0), false);
        d.move_line_up(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nc\nb");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&1]);
        assert_eq!(d.buffer.line_string(1), "c");
    }

    #[test]
    fn duplicating_a_line_leaves_the_source_bookmark_put() {
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(2);
        d.set_cursor(LineCol::new(1, 0), false);
        d.duplicate_line(t0()).unwrap();
        assert_eq!(d.buffer.text_string(), "a\nb\nb\nc");
        // The copy lands above the bookmark, so the bookmark moves down with its text.
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&3]);
        assert_eq!(d.buffer.line_string(3), "c");
    }

    #[test]
    fn undo_restores_bookmark_positions() {
        let mut d = doc("a\nb\nc");
        d.bookmarks.insert(2);
        d.set_cursor(LineCol::new(0, 0), false);
        d.insert_text("new\n", UndoGroup::Atomic, t0()).unwrap();
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&3]);
        assert!(d.undo_one());
        assert_eq!(d.buffer.text_string(), "a\nb\nc");
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&2]);
        assert!(d.redo_one());
        assert_eq!(d.bookmarks.iter().collect::<Vec<_>>(), vec![&3]);
    }

    // ------------------------------------------------------------- folds

    #[test]
    fn unfold_line_opens_a_collapsed_range() {
        let mut d = doc("a\nb\nc");
        d.folds.push(FoldRange {
            start: 0,
            end: 2,
            collapsed: true,
        });
        assert!(d.line_is_hidden(1));
        assert!(!d.line_is_hidden(0), "the fold header is never hidden");
        d.unfold_line(1);
        assert!(!d.line_is_hidden(1));
    }
}
