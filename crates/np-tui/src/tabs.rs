//! Tab lifecycle: naming, closing, closed-tab history, and confirm-on-close (FR-F1, FR-F5, FR-U2).
//!
//! # Why the rules live here
//!
//! Every question this module answers has a data-loss or a wrong-index consequence, and the
//! answers are easy to get subtly wrong at the call site: which tab becomes active when one
//! closes, whether a modified document's unsaved text survives a close, what the next untitled
//! number is after a close. Keeping them in one place means `App` asks rather than decides.
//!
//! # The zero-document rule
//!
//! `App::documents` is never empty. Closing the last tab creates a fresh untitled document
//! instead. Every index assumption in the codebase (`self.active`, the tab bar, hit-testing,
//! `doc()`) relies on that invariant, and the alternative is a special "no document" state in
//! every one of those places. Notepad++ behaves the same way: closing the last buffer opens a
//! new empty one rather than quitting.
//!
//! # Modified markers
//!
//! Never stored on the tab. `Document::modified` is the single source of truth, because a marker
//! copied onto a tab would go stale the moment the user typed, and a stale "unsaved" dot is
//! worse than none.

use std::path::PathBuf;

use np_core::{Document, EncodingId, Eol};

/// A document that was closed, kept so `Reopen Closed Tab` can restore it (FR-U2).
///
/// # Why unsaved text is kept
///
/// Reopening a closed modified tab and finding the edits gone would be plain data loss, and
/// closing a tab is not an uncommon way to lose it. A closed clean tab needs only its path,
/// because the file on disk is the content. A closed modified tab carries the text so the
/// reopen restores what the user was looking at, still flagged modified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedTab {
    /// `None` for a document that never had a file. Reopening gives a fresh untitled buffer
    /// carrying the retained text.
    pub path: Option<PathBuf>,
    /// `Some` only when the document had unsaved changes at close time.
    pub unsaved_text: Option<String>,
    pub encoding: EncodingId,
    pub eol: Eol,
    /// Read-only at close time, restored so a locked file does not become editable on reopen.
    pub readonly: bool,
}

impl ClosedTab {
    /// Capture a closing document, keeping its text only when that text is not on disk.
    pub fn capture(doc: &Document) -> ClosedTab {
        ClosedTab {
            path: doc.path.clone(),
            unsaved_text: doc.modified.then(|| doc.buffer.text_string()),
            encoding: doc.encoding.clone(),
            eol: doc.eol,
            readonly: doc.readonly,
        }
    }
}

/// The three answers a save-changes prompt can take (FR-F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmButton {
    /// Save, then do the deferred operation.
    Save,
    /// Drop the changes and do the deferred operation.
    DontSave,
    /// Abandon the prompt and change nothing.
    Cancel,
    /// A yes/no prompt's yes (FR-N5 binary confirm, whose default is No).
    Yes,
    /// A yes/no prompt's no.
    No,
}

impl ConfirmButton {
    /// The button's label, as the dialog draws it.
    pub fn label(self) -> &'static str {
        match self {
            ConfirmButton::Save => "Save",
            ConfirmButton::DontSave => "Don't save",
            ConfirmButton::Cancel => "Cancel",
            ConfirmButton::Yes => "Yes",
            ConfirmButton::No => "No",
        }
    }
}

/// The operation a confirmation is standing in front of.
///
/// `CloseOthers` and `CloseAll` carry no index because the set they act on is derived from
/// `App::active` at the moment the prompt was raised, and that index must not shift while the
/// prompt is open. Storing a snapshot set would be the honest alternative; deriving it is
/// simpler and the prompt blocks everything that could shift it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    /// Close this one tab.
    CloseTab(usize),
    /// Close every tab except this one.
    CloseOthers(usize),
    /// Close every tab.
    CloseAll,
    /// Reload the active document from disk, discarding changes.
    Reload,
    /// Quit the application.
    Exit,
    /// Discard and forget: used by paths that have nothing else to do.
    Discard,
}

/// A confirmation the app owes the user, and what happens on each answer.
///
/// The dialog *widget* is S14. What lives here is the state and the resolution, so the
/// close/exit paths are complete and testable now: a test calls `resolve_confirm` with each
/// button and asserts the outcome, and S14 only has to draw the buttons and route the click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmRequest {
    /// The question, with the document name already interpolated.
    pub prompt: String,
    /// Buttons in display order. Always `[Save] [Don't save] [Cancel]` for a save prompt.
    pub buttons: Vec<ConfirmButton>,
    /// What to do once the user has answered.
    pub op: PendingOp,
    /// The document the prompt is about, for the save path and for highlighting.
    pub target: Option<usize>,
}

impl ConfirmRequest {
    /// The standard "save changes to X?" prompt in front of `op`.
    pub fn save_changes(name: &str, op: PendingOp, target: Option<usize>) -> ConfirmRequest {
        ConfirmRequest {
            prompt: format!("Save changes to {name}?"),
            buttons: vec![
                ConfirmButton::Save,
                ConfirmButton::DontSave,
                ConfirmButton::Cancel,
            ],
            op,
            target,
        }
    }
}

/// The next untitled document label (FR-F1).
///
/// `counter` is the number of untitled documents created so far and is **never decremented**,
/// so closing a tab does not let a later `New File` reuse its name. Reusing a name would make
/// two tabs read `Untitled-2` and leave the user unable to tell which one they were typing in.
pub fn untitled_label(counter: usize) -> String {
    format!("Untitled-{}", counter + 1)
}

/// A fresh untitled document with the config defaults (FR-F1).
pub fn new_document(encoding: EncodingId, eol: Eol, counter: usize) -> (Document, usize) {
    let next = counter + 1;
    let mut doc = Document::new_untitled(encoding, eol);
    // The label goes in `untitled_name`, never in `path`: a fake path would make Save write a
    // file literally named `Untitled-N` and Reload try to read one (found in S5).
    doc.untitled_name = Some(untitled_label(counter));
    (doc, next)
}

/// Which tab becomes active after index `closed` is removed, and which survives the removal.
///
/// The rule is **prefer the left neighbour**, falling back to the new right one only when the
/// first tab was closed. That keeps the eye on the same document the user was reading when they
/// closed a tab in the middle of a row, which is the common case, and it is what most tab UIs
/// do. Closing the last tab leaves `0`, which is the fresh untitled document.
///
/// Returns `(new_active, still_exists)`; `still_exists` is false when the tab list will be empty
/// and the caller must create a document.
pub fn neighbor_after_close(closed: usize, count: usize) -> (usize, bool) {
    if count <= 1 {
        return (0, false);
    }
    // A stale index (the tab vanished between the click and now) is clamped rather than
    // trusted: `closed - 1` would underflow on 0, and an out-of-range `closed` would pick a
    // neighbour that does not exist.
    let closed = closed.min(count - 1);
    if closed > 0 {
        return (closed - 1, true);
    }
    // Closed the first tab: there is nothing to the left, so follow what slides into 0.
    (0, true)
}

/// Shift a recorded tab index to name the same tab after `removed` has been closed.
///
/// A pending operation records indices against the list as it stood when the prompt was raised,
/// and a batch closes tabs before it finishes, so those indices go stale mid-operation. `keep`
/// in `CloseOthers` is the case that bites: recorded as index 1 of `[f0, f1, f2]`, it means `f1`,
/// and once `f0` is gone index 1 names `f2`. The user's kept tab silently becomes a close target.
///
/// Returns `None` when the recorded index *is* the removed tab, meaning the thing the operation
/// was pointing at no longer exists and the caller must decide what that means.
pub fn shift_after_removal(index: usize, removed: usize) -> Option<usize> {
    if index == removed {
        return None;
    }
    Some(if index > removed { index - 1 } else { index })
}

/// Trim closed-tab history to `cap` entries, dropping the oldest (FR-U2).
///
/// `cap` of 0 disables the history, which is a legitimate config choice; the caller still
/// closes the tab, it just cannot be reopened.
pub fn trim_history(history: &mut Vec<ClosedTab>, cap: usize) {
    if history.len() > cap {
        let excess = history.len() - cap;
        history.drain(0..excess);
    }
}

// ------------------------------------------------------------------ tab bar layout
//
// The renderer and the hit-tester must agree on where every tab sits, down to the cell.
// They do so by calling the same two functions here rather than by each measuring labels
// on their own: `mouse::tab_label` was the S6 version of this idea, but it could not
// express elision or scrolling, and a tab that is drawn elided while being hit-tested at
// full width puts the close button in the wrong place.

/// The widest a single tab is ever drawn, in cells.
///
/// Notepad++ caps tab width for the same reason: one file with a 200-character name would
/// otherwise push every other document off the bar. 24 leaves room for a name plus the
/// `●`/`×`/lock markers and still shows three or four tabs in an 80-column terminal.
pub const TAB_MAX_WIDTH: usize = 24;

/// The narrowest a tab is ever drawn.
///
/// A tab of zero cells would occupy space in the window arithmetic but be impossible to
/// click, so a document would become unreachable. One cell is the least that a click can
/// land on.
pub const MIN_TAB_WIDTH: usize = 1;

/// A tab's rendered span: where it starts, how wide it is, and where its close button is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSpan {
    /// Index into `App::documents`.
    pub index: usize,
    /// First cell of the tab, including its leading padding.
    pub x: u16,
    /// Total cells consumed, including one cell of padding on each side.
    pub width: u16,
    /// The `×` cell, present only on the active or hovered tab, where there is room for it.
    pub close: Option<u16>,
}

impl TabSpan {
    /// True when `x` falls inside this span.
    pub fn contains(&self, x: u16) -> bool {
        x >= self.x && x < self.x.saturating_add(self.width)
    }
}

/// Where the tab bar starts drawing, after any scroll offset.
///
/// Split out from [`tab_spans`] because the hit-tester needs the same window the renderer
/// used, and because the tests need to ask "which tab is at cell 61" without redrawing.
pub fn visible_window(widths: &[usize], width: usize, active: usize) -> usize {
    if widths.is_empty() || width == 0 {
        return 0;
    }
    let active = active.min(widths.len() - 1);

    // If everything from 0 through the active tab fits, no scrolling at all. This is the
    // common case and it is checked first precisely so the bar does not move.
    let mut through_active = 0usize;
    for w in &widths[..=active] {
        through_active += w;
    }
    if through_active <= width {
        return 0;
    }

    // The active tab has been pushed off the right edge. Anchor on it and pull earlier tabs
    // in while they fit, so a click on the active tab is surrounded by its neighbours rather
    // than by blank space. A tab wider than the whole bar still gets a window of its own.
    let mut start = active;
    let mut used = widths[active].min(width);
    while start > 0 && used.saturating_add(widths[start - 1]) <= width {
        start -= 1;
        used += widths[start];
    }
    start
}

/// The rendered label for a document, padded and marked (FR-U2).
///
/// `show_close` adds the `×`. It is only ever true for the active or hovered tab, because a
/// bar full of close buttons is visual noise and makes it easy to click the wrong one.
///
/// Markers: `●` (U+25CF) for unsaved, `×` (U+00D7) to close, `▮` (U+25AE) for read-only.
/// The `●` replaces the S3-era `*`; a dot reads as "dirty" at a glance where an asterisk
/// reads as part of the filename, and Notepad++ uses the same convention.
pub fn tab_label(doc: &np_core::Document, show_close: bool) -> String {
    let mut label = String::from(" ");
    if doc.readonly {
        label.push('\u{25ae}');
    }
    if doc.modified {
        label.push('\u{25cf}');
    }
    label.push_str(&doc.display_name());
    if show_close {
        label.push(' ');
        label.push('\u{00d7}');
    }
    label.push(' ');
    label
}

/// Elide a label to at most `max` cells, keeping the file extension visible.
///
/// The extension is what tells `parser.rs` from `parser.ts` in a bar of similar names, so
/// truncating the tail -- the obvious thing to do -- discards the one part worth reading.
/// The middle goes instead.
///
/// The label splits into three parts and only the middle is ever elided:
///
/// * **head**: the pad cell plus the `▮`/`●` markers. Load-bearing, kept verbatim.
///   Replacing a `●` with `…` would hide unsaved work, which is the opposite of what the
///   marker exists for.
/// * **name**: the filename, including a dotfile's leading dot.
/// * **tail**: the extension, then the ` ×` close button and pad cell.
///
/// When head and tail alone do not fit, the extension is sacrificed before a marker.
pub fn elide(label: &str, max: usize) -> String {
    let chars: Vec<char> = label.chars().collect();
    if chars.len() <= max {
        return label.to_string();
    }
    if max == 0 {
        return String::new();
    }

    let has_close = label.ends_with(" \u{00d7} ");
    let tail_len = if has_close { 4 } else { 1 };
    let name_end = chars.len().saturating_sub(tail_len);

    // Head: the leading pad cell, then any marker glyphs.
    let mut head_end = 1usize.min(name_end);
    while head_end < name_end && !is_name_start(chars[head_end]) {
        head_end += 1;
    }
    let head: Vec<char> = chars[..head_end].to_vec();

    // Tail: the extension, then the close button and pad cell.
    let name = &chars[head_end..name_end];
    let ext_len = extension_len(name);
    let mut tail: Vec<char> = Vec::new();
    if ext_len > 0 {
        tail.extend_from_slice(&name[name.len() - ext_len..]);
    }
    if has_close {
        tail.extend_from_slice(&chars[chars.len() - 4..]);
    }

    // Head and tail alone overflow: keep the markers, drop the extension and the name.
    if head.len() + tail.len() >= max {
        let mut out: Vec<char> = head;
        let room = max.saturating_sub(out.len());
        out.extend(chars[head_end..].iter().take(room));
        return out.into_iter().collect();
    }

    // Only the part of the name *before* the extension is elidable; the extension itself is
    // already in `tail` and must not be emitted twice.
    let stem_len = name.len() - ext_len;
    let stem = &name[..stem_len];

    let room_for_stem = max.saturating_sub(head.len() + tail.len() + 1); // +1 for the ellipsis
    let keep = stem.len().min(room_for_stem);
    // Bias toward the front: the start of the word is what distinguishes two files whose
    // names share a suffix.
    let front = keep.div_ceil(2);
    let back = keep - front;

    let mut out: Vec<char> = head;
    out.extend_from_slice(&stem[..front]);
    if stem.len() > keep {
        out.push('\u{2026}');
    }
    if back > 0 {
        out.extend_from_slice(&stem[stem.len() - back..]);
    }
    out.extend(tail);
    out.into_iter().collect()
}

/// True for the first character of the filename itself, as opposed to a marker glyph.
fn is_name_start(c: char) -> bool {
    !matches!(c, '\u{25ae}' | '\u{25cf}')
}

/// How many trailing cells of a name make up its extension: `.md`, `.rs`, `.tar.gz`.
///
/// A dotfile such as `.gitignore` has no extension in this sense -- the leading dot is not
/// a separator -- so this returns 0 and the whole name stays elidable.
fn extension_len(name: &[char]) -> usize {
    let Some(dot) = name.iter().rposition(|c| *c == '.') else {
        return 0;
    };
    if dot == 0 {
        return 0;
    }
    // An extension is alphanumerics plus a few characters real filenames use. A name ending
    // in ". " or "." gets nothing, because there is nothing worth preserving there.
    let ext = &name[dot + 1..];
    if ext.is_empty()
        || ext.len() > 8
        || !ext
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_'))
    {
        return 0;
    }
    ext.len() + 1
}

/// The spans to draw for the tab bar, in screen order.
///
/// The window decision uses each tab's **capped** width -- what it will actually occupy --
/// rather than its raw label width. Using raw widths would let a 200-character filename
/// reserve 200 cells that it never fills, pushing real, readable tabs off a bar that
/// visibly has room on it.
///
/// Hit-testing calls this same function, so a click always finds the tab that was drawn at
/// that cell, including after elision and scrolling.
pub fn tab_spans(labels: &[String], bar_x: u16, bar_width: u16, active: usize) -> Vec<TabSpan> {
    if labels.is_empty() || bar_width == 0 {
        return Vec::new();
    }
    let width = bar_width as usize;
    // What each tab occupies once capped. `elide` can still return one cell over `max` in
    // the degenerate case where the head and tail alone overflow, hence the extra clamp.
    let occupied: Vec<usize> = labels
        .iter()
        // At least one cell wide: a zero-width tab would be invisible to a click.
        .map(|l| l.chars().count().clamp(MIN_TAB_WIDTH, TAB_MAX_WIDTH))
        .collect();
    let start = visible_window(&occupied, width, active);

    let mut spans = Vec::new();
    let mut x = bar_x;
    let right = bar_x.saturating_add(bar_width);
    for (i, label) in labels.iter().enumerate().skip(start) {
        if x >= right {
            break;
        }
        let shown = elide(label, TAB_MAX_WIDTH);
        // Never draw past the bar: a clipped `×` is a close button that cannot be clicked,
        // and a clipped name is unreadable. What is left over is empty tab-bar space.
        let room = right.saturating_sub(x) as usize;
        let shown = fit_cells(&shown, room);
        let draw_width = shown.chars().count();
        if draw_width == 0 {
            break;
        }
        let close = if shown.ends_with(" \u{00d7} ") && draw_width >= 2 {
            // The `×` sits second from the end, before the trailing pad cell.
            Some(x + draw_width as u16 - 2)
        } else {
            None
        };
        spans.push(TabSpan {
            index: i,
            x,
            width: draw_width as u16,
            close,
        });
        x = x.saturating_add(draw_width as u16);
    }
    spans
}

/// Hard-truncate a string to `cells` characters, for the last tab on a nearly full bar.
fn fit_cells(s: &str, cells: usize) -> String {
    if s.chars().count() <= cells {
        return s.to_string();
    }
    s.chars().take(cells).collect()
}

#[cfg(test)]
mod shift_tests {
    use super::shift_after_removal;

    #[test]
    fn an_index_after_the_removal_moves_down_one() {
        // [f0, f1, f2] with keep = 1 (f1); close f0 -> f1 slides to index 0.
        assert_eq!(shift_after_removal(2, 0), Some(1));
        assert_eq!(shift_after_removal(1, 0), Some(0));
    }

    #[test]
    fn an_index_before_the_removal_is_unchanged() {
        assert_eq!(shift_after_removal(0, 2), Some(0));
        assert_eq!(shift_after_removal(1, 2), Some(1));
    }

    #[test]
    fn the_removed_index_itself_reports_none() {
        // Naming the tab that just went away is not an index to shift; the caller has to decide.
        assert_eq!(shift_after_removal(1, 1), None);
    }

    #[test]
    fn repeated_removals_compose_to_the_right_tab() {
        // The batch case: [f0, f1, f2, f3] keeping index 3, then closing 0 and then 1.
        let after_first = shift_after_removal(3, 0).expect("kept tab survives");
        assert_eq!(after_first, 2, "f3 slid from index 3 to 2");
        let after_second = shift_after_removal(after_first, 0).expect("kept tab survives");
        assert_eq!(after_second, 1, "and to 1 after f1 goes too");
    }
}
