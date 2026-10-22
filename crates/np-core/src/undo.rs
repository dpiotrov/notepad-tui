//! Grouped undo/redo stack (spec FR-E4).
//!
//! # Design
//!
//! An [`UndoEdit`] stores absolute character offsets plus the text on each side of the
//! change, which is enough to invert it exactly:
//!
//! ```text
//!   ... removed_text ...
//!            ^
//!        offset            (start of the affected region, before the edit)
//! ```
//!
//! Applying the inverse means: delete `[offset, offset + inserted.len())`, then insert
//! `removed` at `offset`. Doing delete-then-insert in that order keeps every other stored
//! offset in the same *group* meaningful, because a group is applied as one unit.
//!
//! # Why one capture entry point instead of `before_edit`/`after_edit`
//!
//! The original contract had the caller bracket an edit with two calls. That leaves an
//! obvious way to corrupt history: mutate the buffer and forget one of the calls. Stage
//! S2's acceptance criterion says no mutation path may bypass undo recording, so
//! [`UndoStack::record`] is the single supported entry: it takes the *before* and *after*
//! regions together, and `Document` is the only code that calls it. See
//! `docs/DEVIATIONS.md`.

use std::time::{Duration, Instant};

use crate::types::LineCol;

/// Groups rapid edits (spec FR-E4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoGroup {
    /// Merge into the previous group when that group was also `Typing`, the idle gap is
    /// under [`UNDO_GROUP_IDLE`], and no word boundary was crossed.
    Typing,
    /// Always its own unit: paste, replace-all, line ops, block ops, case ops, comment
    /// toggle.
    Atomic,
}

/// Idle time beyond which typing stops coalescing (spec FR-E4).
pub const UNDO_GROUP_IDLE: Duration = Duration::from_millis(800);

/// Maximum retained undo steps; oldest are evicted (spec FR-E4).
pub const UNDO_MAX_STEPS: usize = 10_000;

/// One reversible change.
///
/// Offsets are absolute character offsets in the buffer *as it looked before this edit*.
///
/// # Field convention (single, direction-independent)
///
/// Applying an edit means: **delete `removed` at `offset`, then insert `inserted`**.
/// A forward edit records what the user did (so `removed` is the text that was there and
/// `inserted` is the new text); re-applying it therefore deletes the old text again and
/// puts the new text back. [`UndoEdit::inverted`] swaps the pair, which makes the very
/// same apply rule undo the change. Keeping one apply rule for both directions is what
/// stops undo from accidentally re-adding text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoEdit {
    /// Start of the affected region.
    pub offset: usize,
    /// Text that was there before the edit (to restore).
    pub removed: String,
    /// Text the edit put there (to remove on undo).
    pub inserted: String,
    /// Caret position to restore on undo / redo, so undo does not dump the caret at the
    /// start of the document.
    pub cursor_before: LineCol,
    pub cursor_after: LineCol,
}

impl UndoEdit {
    /// The opposite change, for turning an undo step into a redo step.
    fn inverted(&self) -> UndoEdit {
        UndoEdit {
            offset: self.offset,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
            cursor_before: self.cursor_after,
            cursor_after: self.cursor_before,
        }
    }

    /// How many characters an applier must delete at `offset` before restoring.
    ///
    /// The rule is direction-independent: **delete `removed`, then insert `inserted`**.
    /// A forward edit stores the text that was taken away in `removed`, so re-applying it
    /// (redo) deletes that same text first. An undo step has had its fields swapped by
    /// [`UndoEdit::inverted`], so `removed` then holds what the original edit *added* --
    /// exactly what must come out. One apply rule serves both directions, which is what
    /// prevents undo from re-adding text instead of removing it.
    pub fn delete_len(&self) -> usize {
        self.removed.chars().count()
    }
}

impl UndoEdit {
    /// Build an edit from the pieces a caller has on hand.
    pub fn new(
        offset: usize,
        removed: &str,
        inserted: &str,
        cursor_before: LineCol,
        cursor_after: LineCol,
    ) -> UndoEdit {
        UndoEdit {
            offset,
            removed: removed.to_string(),
            inserted: inserted.to_string(),
            cursor_before,
            cursor_after,
        }
    }
}

/// A group is one undoable unit: one or more edits that coalesce.
#[derive(Debug, Clone)]
struct UndoGroupEntry {
    edits: Vec<UndoEdit>,
    /// When the last edit joined this group.
    last_at: Instant,
    /// True when this group may still absorb `Typing` edits.
    coalescible: bool,
}

/// Undo/redo history.
#[derive(Debug, Clone, Default)]
pub struct UndoStack {
    undo_groups: Vec<UndoGroupEntry>,
    redo_groups: Vec<UndoGroupEntry>,
}

impl UndoStack {
    pub fn new() -> Self {
        UndoStack::default()
    }

    /// Record one edit.
    ///
    /// `offset` is the start of the affected region in the pre-edit buffer; `removed` and
    /// `inserted` are the text on each side. `now` is injected rather than read from a
    /// clock so grouping behavior is deterministic under test.
    ///
    /// Coalescing rules (spec FR-E4):
    ///  - `Typing` joins the previous group when that group is still `coalescible`, the
    ///    gap since its last edit is `< UNDO_GROUP_IDLE`, and joining keeps the group from
    ///    crossing a word boundary (see [`word_boundary_crossed`]).
    ///  - `Atomic` always starts a new group.
    ///
    /// Recording anything clears the redo branch.
    pub fn record(&mut self, group: UndoGroup, edit: &UndoEdit, now: Instant) {
        if edit.removed.is_empty() && edit.inserted.is_empty() {
            // A no-op edit must not create an undo step, or undo would appear to do nothing.
            return;
        }
        let edit = edit.clone();

        self.redo_groups.clear();

        // Decide and act in one pass over `last_mut()`. Computing a boolean from
        // `last()` and then calling `last_mut()` again would need an `expect` whose safety
        // the compiler cannot see, and project rules ban `expect` outside tests. Borrowing
        // the tail once means the `None` case is simply the "start a new group" branch.
        let mut joined = false;
        if matches!(group, UndoGroup::Typing) {
            if let Some(prev) = self.undo_groups.last_mut() {
                let within_idle = now.duration_since(prev.last_at) < UNDO_GROUP_IDLE;
                if prev.coalescible && within_idle && !word_boundary_crossed(prev, &edit) {
                    prev.edits.push(edit.clone());
                    prev.last_at = now;
                    // Stay open only while the group is homogeneous: all insertions (a
                    // typed word) or all deletions (a backspace run). Mixing the two is a
                    // change of intent and closes the group.
                    let all_insertions = prev.edits.iter().all(|e| e.removed.is_empty());
                    let all_deletions = prev.edits.iter().all(|e| e.inserted.is_empty());
                    prev.coalescible = all_insertions || all_deletions;
                    joined = true;
                }
            }
        }

        if !joined {
            self.undo_groups.push(UndoGroupEntry {
                edits: vec![edit],
                last_at: now,
                coalescible: matches!(group, UndoGroup::Typing),
            });
            self.evict_to_cap();
        }
    }

    /// Drop the oldest groups until the cap is met (spec FR-E4).
    fn evict_to_cap(&mut self) {
        if self.undo_groups.len() > UNDO_MAX_STEPS {
            let excess = self.undo_groups.len() - UNDO_MAX_STEPS;
            self.undo_groups.drain(0..excess);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_groups.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_groups.is_empty()
    }

    /// Number of undo units, not of individual edits.
    pub fn len(&self) -> usize {
        self.undo_groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo_groups.is_empty()
    }

    /// Pop one undo unit, returning its edits in **reverse** application order so the
    /// caller can apply them newest-first and keep earlier offsets valid.
    pub fn undo(&mut self) -> Option<Vec<UndoEdit>> {
        let mut group = self.undo_groups.pop()?;
        // Move to the redo branch as one unit, preserving order for redo.
        group.coalescible = false;
        let edits = group.edits.clone();
        self.redo_groups.push(group);
        Some(edits.into_iter().rev().map(|e| e.inverted()).collect())
    }

    /// Pop one redo unit, returning its edits in application order (oldest first, so a
    /// coalesced burst is replayed left to right).
    pub fn redo(&mut self) -> Option<Vec<UndoEdit>> {
        let mut group = self.redo_groups.pop()?;
        group.coalescible = false;
        let edits = group.edits.clone();
        self.undo_groups.push(group);
        Some(edits)
    }

    /// Discard all history. Used by reload (FR-F6), which documents that undo is lost.
    pub fn clear(&mut self) {
        self.undo_groups.clear();
        self.redo_groups.clear();
    }

    /// Number of redo units.
    pub fn redo_len(&self) -> usize {
        self.redo_groups.len()
    }
}

/// Decide whether appending `next` to `prev` would cross a word boundary (spec FR-E4).
///
/// A typing burst is only allowed to coalesce while it stays inside one word, or while it
/// stays outside one. Inserting a space, or typing right after a word boundary that the
/// previous edit created, must split the group so `Ctrl+Z` undoes a word at a time rather
/// than half a sentence.
fn word_boundary_crossed(prev: &UndoGroupEntry, next: &UndoEdit) -> bool {
    // Only single-character insertions participate in word-boundary reasoning. Anything
    // else (backspace across a boundary, multi-char insert) closes the group.
    if next.removed.len() > 1 || next.inserted.chars().count() > 1 {
        return true;
    }
    let Some(ch) = next.inserted.chars().next() else {
        // A deletion. Coalesce only with a previous deletion: backspacing through a word
        // is one intent, whereas a backspace immediately after typing is a distinct one.
        return match prev.edits.last() {
            Some(last) => last.removed.is_empty(), // previous was an insertion -> split
            None => false,
        };
    };
    let prev_was_word = prev
        .edits
        .last()
        .and_then(|e| e.inserted.chars().last())
        .map(is_word_char)
        .unwrap_or(false);

    // Space/punctuation typed after word characters (or vice versa) ends the burst.
    is_word_char(ch) != prev_was_word
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    /// Advance `from` by `ms` milliseconds.
    fn after(from: Instant, ms: u64) -> Instant {
        from + Duration::from_millis(ms)
    }

    fn type_char(stack: &mut UndoStack, offset: usize, ch: &str, now: Instant) {
        stack.record(
            UndoGroup::Typing,
            &UndoEdit::new(
                offset,
                "",
                ch,
                LineCol::new(0, offset),
                LineCol::new(0, offset + 1),
            ),
            now,
        );
    }

    #[test]
    fn rapid_typing_is_one_undo_unit() {
        // Stage spec: 5 rapid `Typing` inserts with 100 ms gaps collapse into **one** unit.
        // All five characters are word characters, so no word boundary splits the burst.
        let t0 = base();
        let mut stack = UndoStack::new();
        for (i, ch) in ['a', 'b', 'c', 'd', 'e'].iter().enumerate() {
            type_char(&mut stack, i, &ch.to_string(), after(t0, (i as u64) * 100));
        }
        assert_eq!(stack.len(), 1, "burst must coalesce to a single unit");
        // One undo must remove the whole burst, not one character.
        let edits = stack.undo().expect("one group");
        assert_eq!(edits.len(), 5, "the unit carries all five keystrokes");
    }

    #[test]
    fn slow_typing_is_one_unit_per_keystroke() {
        // Stage spec: the same five inserts 900 ms apart yield **five** units.
        let t0 = base();
        let mut stack = UndoStack::new();
        for i in 0..5 {
            // 900 ms apart exceeds UNDO_GROUP_IDLE.
            type_char(&mut stack, i, "a", after(t0, i as u64 * 900));
        }
        assert_eq!(stack.len(), 5);
    }

    #[test]
    fn idle_gap_exactly_at_the_threshold_does_not_coalesce() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        type_char(&mut stack, 1, "a", after(t0, 800));
        assert_eq!(
            stack.len(),
            2,
            "the window is strict: gap < 800 ms coalesces, 800 ms does not"
        );
    }

    #[test]
    fn idle_gap_just_under_the_threshold_coalesces() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        type_char(&mut stack, 1, "a", after(t0, 799));
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn word_boundary_splits_a_typing_group() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        type_char(&mut stack, 1, "b", after(t0, 50));
        // Typing a space closes the word group.
        type_char(&mut stack, 2, " ", after(t0, 100));
        type_char(&mut stack, 3, "c", after(t0, 150));
        assert_eq!(
            stack.len(),
            3,
            "'ab' | ' ' | 'c' -- the space and the next word are their own units"
        );
    }

    #[test]
    fn atomic_never_coalesces() {
        let t0 = base();
        let mut stack = UndoStack::new();
        for i in 0..3 {
            stack.record(
                UndoGroup::Atomic,
                &UndoEdit::new(i, "", "x", LineCol::new(0, i), LineCol::new(0, i + 1)),
                // All three land inside the idle window, so only Atomic keeps them apart.
                after(t0, i as u64),
            );
        }
        assert_eq!(stack.len(), 3);
    }

    #[test]
    fn no_op_edits_are_not_recorded() {
        let t0 = base();
        let mut stack = UndoStack::new();
        stack.record(
            UndoGroup::Atomic,
            &UndoEdit::new(0, "", "", LineCol::default(), LineCol::default()),
            t0,
        );
        assert!(stack.is_empty());
        assert!(!stack.can_undo());
    }

    #[test]
    fn recording_clears_the_redo_branch() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        assert!(stack.undo().is_some());
        assert!(stack.can_redo());
        // A fresh edit must throw the redo branch away.
        type_char(&mut stack, 0, "b", after(t0, 5_000));
        assert!(!stack.can_redo());
    }

    #[test]
    fn undo_then_redo_returns_the_original_edit() {
        let t0 = base();
        let mut stack = UndoStack::new();
        stack.record(
            UndoGroup::Atomic,
            &UndoEdit::new(3, "old", "new", LineCol::new(0, 3), LineCol::new(0, 6)),
            t0,
        );
        let undone = stack.undo().expect("one group");
        assert_eq!(undone.len(), 1);
        assert_eq!(undone[0].removed, "new");
        assert_eq!(undone[0].inserted, "old");
        assert_eq!(undone[0].cursor_before, LineCol::new(0, 6));

        let redone = stack.redo().expect("redo available");
        assert_eq!(redone[0].removed, "old");
        assert_eq!(redone[0].inserted, "new");
        assert!(!stack.can_redo());
    }

    #[test]
    fn inverting_twice_is_the_identity() {
        let e = UndoEdit {
            offset: 7,
            removed: "abc".into(),
            inserted: "xy".into(),
            cursor_before: LineCol::new(1, 2),
            cursor_after: LineCol::new(1, 4),
        };
        assert_eq!(e.inverted().inverted(), e);
    }

    #[test]
    fn cap_evicts_the_oldest_groups() {
        let t0 = base();
        let mut stack = UndoStack::new();
        for i in 0..(UNDO_MAX_STEPS + 1) {
            stack.record(
                UndoGroup::Atomic,
                &UndoEdit::new(i, "", "z", LineCol::new(0, i), LineCol::new(0, i + 1)),
                // Spaced far apart so Atomic-vs-Typing is not confounded with timing.
                after(t0, i as u64 * 1_000),
            );
            assert!(
                stack.len() <= UNDO_MAX_STEPS,
                "stack exceeded the cap at iteration {i}"
            );
        }
        assert_eq!(stack.len(), UNDO_MAX_STEPS);
        // The oldest edit is gone: only the cap's worth remain, so exactly this many
        // undos are possible and the next one fails.
        for _ in 0..UNDO_MAX_STEPS {
            assert!(stack.undo().is_some());
        }
        assert!(!stack.can_undo());
    }

    #[test]
    fn undo_group_holds_its_edits_in_order() {
        let t0 = base();
        let mut stack = UndoStack::new();
        for (i, ch) in ['h', 'i'].iter().enumerate() {
            type_char(&mut stack, i, &ch.to_string(), after(t0, i as u64 * 10));
        }
        let edits = stack.undo().expect("one group");
        assert_eq!(edits.len(), 2);
        // undo() returns the group reversed so the caller applies newest change first,
        // and each edit is inverted, so `inserted` carries what must be re-inserted.
        // Typing was 'h'@0 then 'i'@1, hence undo yields 'i' removed first, then 'h'.
        assert_eq!(edits[0].offset, 1);
        assert_eq!(edits[0].removed, "i");
        assert_eq!(edits[1].offset, 0);
        assert_eq!(edits[1].removed, "h");
    }

    #[test]
    fn clear_removes_both_branches() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        stack.undo();
        assert!(stack.can_redo());
        stack.clear();
        assert!(!stack.can_undo());
        assert!(!stack.can_redo());
    }

    #[test]
    fn backspace_after_typing_starts_a_new_group() {
        let t0 = base();
        let mut stack = UndoStack::new();
        type_char(&mut stack, 0, "a", t0);
        stack.record(
            UndoGroup::Typing,
            &UndoEdit::new(0, "a", "", LineCol::new(0, 1), LineCol::new(0, 0)),
            after(t0, 50),
        );
        assert_eq!(stack.len(), 2, "insert and delete are distinct intents");
    }

    #[test]
    fn consecutive_backspaces_coalesce() {
        let t0 = base();
        let mut stack = UndoStack::new();
        // Backspacing "xyz" away: offsets descend, time ascends.
        for (step, i) in (0..3u64).rev().enumerate() {
            stack.record(
                UndoGroup::Typing,
                &UndoEdit::new(
                    i as usize,
                    "x",
                    "",
                    LineCol::new(0, i as usize + 1),
                    LineCol::new(0, i as usize),
                ),
                after(t0, (step as u64) * 50),
            );
        }
        assert_eq!(stack.len(), 1);
    }
}
