//! A single-line text field with caret, selection and grapheme-safe editing (FR-U4).
//!
//! # Why this is its own type rather than a `String` on `App`
//!
//! Five dialogs need an editable field (Open, Save As, Go To, Find, Preferences' number
//! boxes). Before S14 each kept a bare `String` and hand-rolled backspace, which meant
//! backspace deleted one *scalar* rather than one grapheme cluster and the field could hold
//! `e` + U+0301 with the accent left behind. Editing rules belong to the widget that owns
//! the text, and there must be exactly one copy of them.
//!
//! # Offsets are grapheme indices
//!
//! `caret` and `selection_anchor` count **grapheme clusters**, not bytes and not `char`s.
//! That is the unit the screen draws in for the purposes of a caret, it is what
//! `UnicodeSegmentation` walks, and it means `caret_to_display` only has to worry about
//! East Asian width, never about cluster boundaries.
//!
//! # Clipping is the field's business, not the caller's
//!
//! A field narrower than its text scrolls a window so the caret stays visible, and it keeps
//! a margin of one cluster at each edge so the caret is never flush against the bracket.
//! [`TextInput::window`] answers "which clusters do I draw", and the renderer must trust it
//! rather than truncating on its own -- two clipping rules is how a caret ends up drawn
//! outside its box.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A single-line editable field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Caret position as a grapheme index, `0..=cluster_count`.
    caret: usize,
    /// Selection anchor as a grapheme index, or `None` when nothing is selected.
    anchor: Option<usize>,
    /// Leftmost visible cluster index. Only ever moves to keep the caret in view.
    left: usize,
}

/// What a key did to a field, so the caller can decide whether a key was consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    /// The field changed.
    Changed,
    /// The field did not change (Backspace on an empty field, Left at the start).
    Untouched,
    /// The field has a selection and the key replaced it -- the caller may want to keep it
    /// for a "yank the selection" flow. Not used by dialogs today.
    ReplacedSelection,
}

impl TextInput {
    /// A field holding `text`, caret at the end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let caret = clusters(&text);
        TextInput {
            text,
            caret,
            anchor: None,
            left: 0,
        }
    }

    /// An empty field.
    pub fn empty() -> Self {
        TextInput::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Cluster count of the whole text.
    pub fn len(&self) -> usize {
        clusters(&self.text)
    }

    /// Caret as a grapheme index.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selected span as `(start, end)` grapheme indices, or `None`.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            None
        } else {
            Some((anchor.min(self.caret), anchor.max(self.caret)))
        }
    }

    /// The selected text, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(slice_clusters(&self.text, a, b))
    }

    /// Replace the whole contents, putting the caret at the end. Clears selection.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.caret = clusters(&self.text);
        self.anchor = None;
        self.left = 0;
    }

    /// Move the caret. `extend` keeps or moves the anchor instead of clearing it.
    pub fn set_caret(&mut self, index: usize, extend: bool) {
        let index = index.min(self.len());
        if extend {
            let anchor = self.anchor.unwrap_or(self.caret);
            self.anchor = Some(anchor);
        } else {
            self.anchor = None;
        }
        self.caret = index;
    }

    /// Select every cluster.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.len();
    }

    /// Drop the selection, leaving the caret where it is.
    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Delete the selection, or do nothing. Returns whether text changed.
    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            return false;
        };
        self.text = format!(
            "{}{}",
            slice_clusters(&self.text, 0, a),
            slice_clusters(&self.text, b, self.len())
        );
        self.caret = a;
        self.anchor = None;
        true
    }

    /// Insert `s` at the caret, replacing any selection. Grapheme-safe by construction: the
    /// text is spliced on cluster boundaries computed from the *existing* text.
    pub fn insert(&mut self, s: &str) -> FieldKey {
        let had_selection = self.delete_selection();
        if s.is_empty() {
            return if had_selection {
                FieldKey::ReplacedSelection
            } else {
                FieldKey::Untouched
            };
        }
        let head = slice_clusters(&self.text, 0, self.caret);
        let tail = slice_clusters(&self.text, self.caret, self.len());
        self.text = format!("{head}{s}{tail}");
        self.caret += clusters(s);
        if had_selection {
            FieldKey::ReplacedSelection
        } else {
            FieldKey::Changed
        }
    }

    /// Insert one character.
    pub fn insert_char(&mut self, c: char) -> FieldKey {
        self.insert(&c.to_string())
    }

    /// Backspace: delete the selection if there is one, else the cluster to the left.
    ///
    /// Deleting a whole cluster is the point of this type. `String::pop` removes one
    /// `char`, so a combining mark or an emoji ZWJ sequence would come apart.
    pub fn backspace(&mut self) -> FieldKey {
        if self.delete_selection() {
            return FieldKey::Changed;
        }
        if self.caret == 0 {
            return FieldKey::Untouched;
        }
        let head = slice_clusters(&self.text, 0, self.caret - 1);
        let tail = slice_clusters(&self.text, self.caret, self.len());
        self.text = format!("{head}{tail}");
        self.caret -= 1;
        FieldKey::Changed
    }

    /// Delete key: delete the selection, else the cluster under the caret.
    pub fn delete_forward(&mut self) -> FieldKey {
        if self.delete_selection() {
            return FieldKey::Changed;
        }
        if self.caret >= self.len() {
            return FieldKey::Untouched;
        }
        let head = slice_clusters(&self.text, 0, self.caret);
        let tail = slice_clusters(&self.text, self.caret + 1, self.len());
        self.text = format!("{head}{tail}");
        FieldKey::Changed
    }

    /// Left arrow: collapse a selection onto its left edge, else step one cluster.
    pub fn move_left(&mut self) -> FieldKey {
        if let Some((a, _)) = self.selection() {
            self.caret = a;
            self.anchor = None;
            return FieldKey::Changed;
        }
        if self.caret == 0 {
            return FieldKey::Untouched;
        }
        self.caret -= 1;
        FieldKey::Changed
    }

    /// Right arrow.
    pub fn move_right(&mut self) -> FieldKey {
        if let Some((_, b)) = self.selection() {
            self.caret = b;
            self.anchor = None;
            return FieldKey::Changed;
        }
        if self.caret >= self.len() {
            return FieldKey::Untouched;
        }
        self.caret += 1;
        FieldKey::Changed
    }

    /// Home.
    pub fn home(&mut self) {
        self.caret = 0;
        self.anchor = None;
    }

    /// End.
    pub fn end(&mut self) {
        self.caret = self.len();
        self.anchor = None;
    }

    /// The cluster index Ctrl+Right should land on from `from`.
    ///
    /// It lands on the *start of the next word*, not the end of the current one, which is
    /// what every editor the user knows does: the caret ends up where typing would continue.
    /// Walk out of the current word first (if inside one), then across the separator, then
    /// stop -- so a caret mid-word jumps to the next word's first character rather than to the
    /// space after its own.
    fn word_forward(&self, from: usize) -> usize {
        let list: Vec<&str> = self.text.graphemes(true).collect();
        let mut i = from.min(list.len());
        while i < list.len() && is_word_char(list[i]) {
            i += 1;
        }
        while i < list.len() && !is_word_char(list[i]) {
            i += 1;
        }
        i
    }

    /// The cluster index Ctrl+Left should land on from `from`: the start of the word to the
    /// left, skipping any separator first.
    fn word_boundary(&self, from: usize, forward: bool) -> usize {
        if forward {
            return self.word_forward(from);
        }
        let list: Vec<&str> = self.text.graphemes(true).collect();
        {
            let mut i = from.min(list.len());
            while i > 0 && !is_word_char(list[i - 1]) {
                i -= 1;
            }
            while i > 0 && is_word_char(list[i - 1]) {
                i -= 1;
            }
            i
        }
    }

    /// Ctrl+Left.
    pub fn word_left(&mut self) {
        let target = self.word_boundary(self.caret, false);
        self.set_caret(target, false);
    }

    /// Ctrl+Right.
    pub fn word_right(&mut self) {
        let target = self.word_boundary(self.caret, true);
        self.set_caret(target, false);
    }

    /// Delete from the caret back to the previous word start (Ctrl+Backspace).
    pub fn delete_word_back(&mut self) -> FieldKey {
        let target = self.word_boundary(self.caret, false);
        if target == self.caret {
            return FieldKey::Untouched;
        }
        let head = slice_clusters(&self.text, 0, target);
        let tail = slice_clusters(&self.text, self.caret, self.len());
        self.text = format!("{head}{tail}");
        self.caret = target;
        FieldKey::Changed
    }

    /// The cluster range to draw in a field `width` clusters wide.
    ///
    /// Pure: it clamps the stored left edge into range and answers with the window that
    /// *would* be drawn, without mutating. Keeping it `&self` means a renderer can ask
    /// during a borrow of `App` that it cannot make mutable, which is the normal case for a
    /// dialog drawn over `&App`.
    ///
    /// `width` is in clusters, not display cells; a field holding wide characters may still
    /// overflow, and the renderer clips at its box.
    pub fn window(&self, width: usize) -> (usize, usize) {
        let total = self.len();
        if width == 0 {
            return (0, 0);
        }
        if total <= width {
            return (0, total);
        }
        // Clamped so the caret is inside `[start, end]` inclusive: a caret at `total` sits
        // *after* the last cluster and is legitimately one past the window's last index, so
        // the end bound is what has to reach it. Deriving the left edge from `caret + 1 -
        // width` alone lets a stale scroll leave the caret off-screen at the end of the text.
        let left = self
            .left
            .min(self.caret)
            .min(self.caret.saturating_add(1).saturating_sub(width))
            .min(total.saturating_sub(width));
        (left, (left + width).min(total))
    }

    /// Scroll so the caret is visible in a field `width` clusters wide.
    ///
    /// The caret is kept inside with no forced margin: a caret at the very first visible
    /// cluster is still fully drawn, and inventing a margin costs a cluster of text for no
    /// benefit.
    pub fn scroll_into_view(&mut self, width: usize) {
        if width == 0 {
            self.left = 0;
            return;
        }
        let total = self.len();
        if total <= width {
            self.left = 0;
            return;
        }
        let mut left = self.left;
        if self.caret >= left + width {
            left = self.caret + 1 - width;
        }
        if self.caret < left {
            left = self.caret;
        }
        self.left = left.min(total.saturating_sub(1));
    }

    /// Caret column relative to the window's start, in display cells.
    pub fn caret_display_col(&self, width: usize) -> usize {
        let (start, _) = self.window(width);
        let before = slice_clusters(self.text(), start, self.caret);
        before.width()
    }

    /// The clusters inside the window for `width`, for rendering.
    pub fn window_text(&self, width: usize) -> String {
        let (start, end) = self.window(width);
        slice_clusters(self.text(), start, end)
    }
}

/// Cluster count. `str::chars().count()` would miscount a combined emoji.
fn clusters(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Cluster `[from..to)`, clamped. Never splits a cluster, so the result is always valid
/// UTF-8 and always renders as whole characters.
fn slice_clusters(s: &str, from: usize, to: usize) -> String {
    let (lo, hi) = (from.min(to), from.max(to));
    s.graphemes(true)
        .skip(lo)
        .take(hi.saturating_sub(lo))
        .collect()
}

/// Word characters for Ctrl+arrow. Alphanumerics and `_`, which matches how the editor's
/// own `word_left` treats identifiers.
fn is_word_char(g: &str) -> bool {
    g.chars()
        .next()
        .map(|c| c.is_alphanumeric() || c == '_')
        .unwrap_or(false)
}
