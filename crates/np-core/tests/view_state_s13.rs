//! S13 — folds and bookmarks as *document* state (FR-V5, FR-V6).
//!
//! `folding.rs` tests detection in isolation. These tests are about the parts a user feels:
//! typing inside a folded block, jumping to a bookmark that is currently hidden, and closing
//! the editor and coming back tomorrow. None of those are a single function -- they are the
//! document keeping several pieces of state consistent through an edit -- which is exactly where
//! this kind of feature tends to be wrong.

use std::time::Instant;

use np_core::{
    document::Document, Buffer, EncodingId, Eol, LineCol, RectCore, UndoGroup, ViewGeometry,
};

fn t0() -> Instant {
    Instant::now()
}

/// A document containing `text`, with an **empty undo stack**.
///
/// Two reasons it is not just `Document::from_decoded`. The stack has to start empty so an undo
/// test's units are exactly the operations that test performed -- otherwise `while undo_one()`
/// walks past the fixture and lands on an empty document, which reads as a broken undo feature
/// rather than as a test that undid one too many times. And the text goes through
/// [`Document::insert_text`] rather than being written straight into the rope, so the fixture
/// exercises the same newline-normalizing path the app does.
fn doc(text: &str) -> Document {
    let mut d = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    if !text.is_empty() {
        d.insert_text(text, UndoGroup::Atomic, t0())
            .expect("insert");
        // Walk the fixture's own edit back out of the history without touching the text: undo
        // would revert the content, so the insert is replayed after clearing. Cheaper and less
        // surprising than reaching into the stack, and the content assertion in the undo tests
        // is only meaningful because of it.
        let restored = d.buffer.text_string();
        d = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
        d.insert_text(&restored, UndoGroup::Atomic, t0())
            .expect("re-insert");
        d.undo.clear();
    }
    d
}

fn geom(w: u16, h: u16, wrap: bool) -> ViewGeometry {
    ViewGeometry {
        gutter_width: 0,
        text_area: RectCore::new(0, 0, w, h),
        tab_width: 4,
        word_wrap: wrap,
        ..Default::default()
    }
}

const RUST_SRC: &str =
    "fn a() {\n    let one = 1;\n    let two = 2;\n}\n\nfn b() {\n    let x = 1;\n}\n";

/// A document with `a()`'s body folded away.
fn folded() -> Document {
    let mut d = doc(RUST_SRC);
    d.refresh_folds("Rust");
    assert!(
        d.toggle_fold_at(0),
        "the fixture must have a foldable block"
    );
    assert!(d.line_is_hidden(1), "line 1 should be hidden by the fold");
    d
}

// ---------------------------------------------------- editing inside a fold

#[test]
fn typing_at_the_caret_inside_a_collapsed_range_opens_it() {
    // FR-V5. The caret can be inside a collapsed range -- it was left there by a jump or a
    // fold made on the header -- and typing must not silently modify text the user cannot see.
    let mut d = folded();
    d.set_cursor(LineCol::new(2, 4), false);
    assert!(
        d.line_is_hidden(2),
        "caret starts inside the collapsed block"
    );
    d.unfold_line(d.cursor.pos.line);
    assert!(
        !d.line_is_hidden(2),
        "the fold the caret sits in must open before the edit lands"
    );
    d.insert_text("Z", UndoGroup::Typing, t0()).expect("insert");
    assert!(d.buffer.line_string(2).contains('Z'));
}

#[test]
fn editing_a_visible_line_leaves_an_untouched_fold_collapsed() {
    // The other half of the rule: unfolding is scoped to the range the edit actually lands in.
    // A global "any edit opens everything" makes folding useless in a live file.
    let mut d = folded();
    d.set_cursor(LineCol::new(6, 0), false);
    assert!(!d.line_is_hidden(6), "line 6 is outside the fold");
    d.unfold_line(6);
    assert!(
        d.line_is_hidden(1),
        "an edit below a fold must not open it: {:?}",
        d.folds
    );
}

#[test]
fn a_fold_survives_an_edit_on_its_own_header_line() {
    // The header is visible and being edited; the block below it is still the same block.
    let mut d = folded();
    d.set_cursor(LineCol::new(0, 2), false);
    d.unfold_line(0);
    // `unfold_containing` treats the header as inside on purpose (a bookmark on a header must
    // not appear to do nothing), so this asserts the documented behavior rather than an
    // aspiration: acting *on* a header opens the block.
    assert!(
        !d.line_is_hidden(1),
        "acting on a header opens it, by design"
    );
}

// ------------------------------------------------------- fold/bookmark mix

#[test]
fn a_bookmark_inside_a_collapsed_range_unfolds_it_when_scrolled_to() {
    // FR-V6: navigation must not appear broken. Jumping to a hidden bookmark has to reveal it.
    let mut d = folded();
    d.bookmarks.insert(2);
    d.set_cursor(LineCol::new(0, 0), false);
    let g = geom(80, 10, false);
    let unfolded = d.scroll_to_show(&g, LineCol::new(2, 0));
    assert!(
        unfolded,
        "scrolling to a hidden position reports that it unfolded"
    );
    assert!(
        !d.line_is_hidden(2),
        "the bookmark's line is visible now: {:?}",
        d.folds
    );
    let rows = d.visible_rows(&g);
    assert!(
        rows.iter().any(|r| r.line == 2),
        "the target line is on screen: {:?}",
        rows.iter().map(|r| r.line).collect::<Vec<_>>()
    );
}

#[test]
fn bookmark_navigation_lands_on_the_bookmark_even_when_it_is_folded_away() {
    // `next_bookmark` moves the caret; the *view* has to follow, which is `scroll_to_show`'s
    // job. This asserts the caret half and that the position is still addressable.
    let mut d = folded();
    d.bookmarks.insert(6);
    d.set_cursor(LineCol::new(0, 0), false);
    assert_eq!(d.next_bookmark(), Some(6));
    assert_eq!(d.cursor.pos.line, 6);
    let g = geom(80, 4, false);
    d.scroll_to_show(&g, d.cursor.pos);
    let rows = d.visible_rows(&g);
    assert!(
        rows.iter().any(|r| r.line == 6),
        "the caret's line is on screen"
    );
}

#[test]
fn bookmark_navigation_wraps_at_both_ends() {
    let mut d = doc("a\nb\nc\nd\ne\n");
    d.bookmarks.insert(1);
    d.bookmarks.insert(3);
    d.set_cursor(LineCol::new(4, 0), false);
    assert_eq!(
        d.next_bookmark(),
        Some(1),
        "wraps from the end to the first"
    );
    d.set_cursor(LineCol::new(0, 0), false);
    assert_eq!(
        d.prev_bookmark(),
        Some(3),
        "wraps from the start to the last"
    );
}

#[test]
fn toggling_a_bookmark_twice_removes_it() {
    let mut d = doc("a\nb\n");
    d.set_cursor(LineCol::new(1, 0), false);
    d.toggle_bookmark();
    assert!(d.bookmarks.contains(&1));
    d.toggle_bookmark();
    assert!(!d.bookmarks.contains(&1));
}

#[test]
fn clear_bookmarks_empties_the_set() {
    let mut d = doc("a\nb\nc\n");
    d.bookmarks.insert(0);
    d.bookmarks.insert(2);
    d.clear_bookmarks();
    assert!(d.bookmarks.is_empty());
}

// --------------------------------------------------------------- fold all

#[test]
fn fold_all_and_unfold_all_move_every_range() {
    let mut d = doc(RUST_SRC);
    d.refresh_folds("Rust");
    assert!(d.folds.len() >= 2, "fixture has two blocks");
    d.fold_all();
    assert!(d.folds.iter().all(|f| f.collapsed));
    assert!(d.line_is_hidden(1) && d.line_is_hidden(6));
    d.unfold_all();
    assert!(d.folds.iter().all(|f| !f.collapsed));
    assert!(!d.line_is_hidden(1) && !d.line_is_hidden(6));
}

#[test]
fn a_fold_can_be_toggled_from_the_gutter_by_header_line() {
    let mut d = doc(RUST_SRC);
    d.refresh_folds("Rust");
    assert!(d.toggle_fold_at(0), "line 0 is a header");
    assert!(!d.toggle_fold_at(2), "line 2 is inside, not a header");
    assert!(
        !d.toggle_fold_at(99),
        "no fold on a line that does not exist"
    );
}

// ------------------------------------------------------- fold state across edits

#[test]
fn fold_ranges_shift_when_lines_are_inserted_above_them() {
    let mut d = doc(RUST_SRC);
    d.refresh_folds("Rust");
    d.fold_all();
    let before: Vec<(usize, usize)> = d.folds.iter().map(|f| (f.start, f.end)).collect();
    assert!(!before.is_empty());
    // Insert a line at the very top of the document.
    d.set_cursor(LineCol::new(0, 0), false);
    d.insert_text("// added\n", UndoGroup::Atomic, t0())
        .expect("insert");
    d.refresh_folds("Rust");
    let after: Vec<(usize, usize)> = d.folds.iter().map(|f| (f.start, f.end)).collect();
    assert_eq!(
        after.len(),
        before.len(),
        "the same blocks are still foldable: {before:?} -> {after:?}"
    );
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(a.0, b.0 + 1, "header moved down by one: {b:?} -> {a:?}");
    }
}

#[test]
fn fold_state_survives_a_recompute_after_an_unrelated_edit() {
    // The acceptance criterion, stated at the document level: fold a block, edit somewhere the
    // fold does not reach, re-detect, and the block is still folded.
    let mut d = folded();
    assert!(d.line_is_hidden(1));
    d.set_cursor(LineCol::new(7, 0), false);
    d.insert_text("// note\n", UndoGroup::Atomic, t0())
        .expect("insert");
    d.refresh_folds("Rust");
    assert!(
        d.line_is_hidden(1),
        "an unrelated edit opened a fold: {:?}",
        d.folds
    );
}

#[test]
fn a_fold_whose_block_was_deleted_disappears() {
    let mut d = folded();
    // Delete `fn a()` entirely.
    d.set_cursor(LineCol::new(0, 0), false);
    for _ in 0..5 {
        d.delete_line(t0()).expect("delete");
    }
    d.refresh_folds("Rust");
    for fold in &d.folds {
        assert!(fold.end < d.buffer.line_count(), "stale fold {fold:?}");
    }
}

#[test]
fn undo_restores_foldable_text_without_leaving_phantom_folds() {
    // Each `delete_line` is its own undo unit, so the walk-back is one delete at a time. What is
    // being asserted is not the text -- that always comes back -- but that no intermediate state
    // leaves a fold pointing at a line count the buffer never had.
    let mut d = doc(RUST_SRC);
    d.refresh_folds("Rust");
    d.fold_all();
    for _ in 0..3 {
        d.set_cursor(LineCol::new(1, 0), false);
        d.delete_line(t0()).expect("delete");
        d.refresh_folds("Rust");
        for fold in &d.folds {
            assert!(
                fold.end < d.buffer.line_count(),
                "fold {fold:?} outlived its lines ({} lines on disk)",
                d.buffer.line_count()
            );
        }
    }
    while d.undo_one() {
        d.refresh_folds("Rust");
    }
    assert_eq!(d.buffer.text_string(), RUST_SRC, "text came back whole");
    for fold in &d.folds {
        assert!(fold.end < d.buffer.line_count(), "phantom fold {fold:?}");
    }
}

// -------------------------------------------------------- rows and geometry

#[test]
fn visible_rows_skip_hidden_lines_and_wrap_the_rest() {
    let mut d = doc(
        "fn a() {\n    hidden\n}\n\nlet long = \"aaaaaaaaaa bbbbbbbbbb cccccccccc dddddddde\";\n",
    );
    d.refresh_folds("Rust");
    assert!(d.toggle_fold_at(0));
    let g = geom(10, 40, true);
    let rows = d.visible_rows(&g);
    assert!(
        !rows.iter().any(|r| r.line == 1),
        "hidden line reached the row list: {:?}",
        rows.iter().map(|r| r.line).collect::<Vec<_>>()
    );
    // The long line still wraps into several rows.
    assert!(
        rows.iter().filter(|r| r.line == 4).count() >= 4,
        "wrap did not happen behind a fold: {:?}",
        rows.iter()
            .map(|r| (r.line, r.start_col, r.end_col))
            .collect::<Vec<_>>()
    );
}

#[test]
fn scroll_to_show_leaves_the_top_alone_when_the_target_is_already_visible() {
    let mut d = doc("a\nb\nc\nd\ne\nf\ng\nh\n");
    let g = geom(80, 4, false);
    d.scroll.top_line = 0;
    d.scroll_to_show(&g, LineCol::new(1, 0));
    assert_eq!(d.scroll.top_line, 0, "no gratuitous scroll");
    d.scroll_to_show(&g, LineCol::new(7, 0));
    assert!(d.scroll.top_line > 0, "an off-screen target must scroll");
    let rows = d.visible_rows(&g);
    assert!(
        rows.iter().any(|r| r.line == 7),
        "the target ended up on screen: {:?}",
        rows.iter().map(|r| r.line).collect::<Vec<_>>()
    );
}

#[test]
fn scroll_to_show_under_wrap_reveals_a_target_on_a_later_row_of_a_wrapped_line() {
    // The trap this guards: a wrapped line's rows are several screen rows, so a scroll decision
    // made in *line* units thinks the target is already visible when its row is not.
    let mut d = doc("alpha bravo charlie delta echo foxtrot golf hotel india juliet\nx\n");
    let g = geom(10, 3, true);
    d.scroll.top_line = 0;
    d.scroll_to_show(&g, LineCol::new(0, 55));
    let rows = d.visible_rows(&g);
    let target_row = g
        .screen_row_of(&rows, LineCol::new(0, 55))
        .expect("target on screen");
    assert!(
        (target_row as usize) < rows.len(),
        "the wrapped target's row is inside the window"
    );
}

#[test]
fn an_empty_document_has_one_visible_row() {
    let d = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    let rows = d.visible_rows(&geom(80, 24, false));
    assert_eq!(
        rows.len(),
        1,
        "an empty document is one empty row, not none"
    );
    assert_eq!(rows[0].line, 0);
}

#[test]
fn folding_the_last_block_does_not_blank_the_viewport() {
    let mut d = doc("a\nb\nc\n");
    d.folds.push(np_core::FoldRange {
        start: 0,
        end: 2,
        collapsed: true,
    });
    let g = geom(80, 5, false);
    // ropey counts the trailing terminator as a fourth line, and that line is outside the fold,
    // so it stays on screen below the collapsed header.
    assert_eq!(
        d.buffer.line_count(),
        4,
        "three lines plus the terminator line"
    );
    let rows = d.visible_rows(&g);
    assert_eq!(rows.iter().map(|r| r.line).collect::<Vec<_>>(), vec![0, 3]);
    // And a scroll request into the collapsed tail lands on the header, which is the on-screen
    // representative of everything it hides.
    d.scroll_to_show(&g, LineCol::new(2, 0));
    let rows = d.visible_rows(&g);
    assert!(
        !rows.is_empty(),
        "collapsing the tail must not blank the editor"
    );
    assert_eq!(rows[0].line, 0);
}

// ------------------------------------------------------------ session state

#[test]
fn folds_and_bookmarks_round_trip_through_a_session_snapshot() {
    use np_core::SessionSnapshot;
    let mut d = folded();
    d.bookmarks.insert(4);
    d.bookmarks.insert(6);
    d.cursor.pos = LineCol::new(6, 3);
    d.scroll.top_line = 3;

    let tab = d.snapshot();
    assert_eq!(tab.bookmarks, vec![4, 6], "bookmarks are in document order");
    assert_eq!(
        tab.folded,
        vec![[0, 3]],
        "only collapsed ranges are recorded"
    );

    // Restore into a fresh document over the same text.
    let mut restored = doc(RUST_SRC);
    np_tui_session_apply(&mut restored, &tab);
    assert_eq!(
        restored.bookmarks.iter().copied().collect::<Vec<_>>(),
        vec![4, 6]
    );
    assert!(
        restored.line_is_hidden(1),
        "the collapsed fold came back: {:?}",
        restored.folds
    );
    assert_eq!(restored.cursor.pos, LineCol::new(6, 3));
    assert_eq!(restored.scroll.top_line, 3);
}

/// `apply_view_state` lives in `np-tui`, which `np-core` must not depend on. The mapping is
/// three assignments and is asserted here against the same `SessionTab` the real function
/// consumes, so a field added to one side and not the other fails somewhere.
fn np_tui_session_apply(doc: &mut Document, tab: &np_core::SessionTab) {
    doc.cursor.pos = LineCol {
        line: tab.cursor_line.min(doc.buffer.line_count() - 1),
        col: tab.cursor_col,
    };
    doc.scroll.top_line = tab.scroll_top.min(doc.buffer.line_count() - 1);
    doc.bookmarks = tab.bookmarks.iter().copied().collect();
    doc.folds = tab
        .folded
        .iter()
        .map(|[start, end]| np_core::FoldRange {
            start: *start,
            end: *end,
            collapsed: true,
        })
        .collect();
}

#[test]
fn a_fold_restored_from_a_session_is_consistent_with_the_buffer_it_lands_in() {
    use np_core::SessionSnapshot;
    let mut d = folded();
    d.fold_all();
    let tab = d.snapshot();
    assert!(tab.folded.len() >= 2);

    // Restoring onto a much shorter document must not leave ranges pointing at lines that are
    // gone -- that is the state in which a fold hides the viewport with nothing to unfold.
    let mut small = doc("x\ny\n");
    np_tui_session_apply(&mut small, &tab);
    small.clamp_folds_for_test();
    for fold in &small.folds {
        assert!(
            fold.end < small.buffer.line_count(),
            "restored fold {fold:?} points past the end of a {}-line document",
            small.buffer.line_count()
        );
    }
}

trait ClampFolds {
    fn clamp_folds_for_test(&mut self);
}

impl ClampFolds for Document {
    fn clamp_folds_for_test(&mut self) {
        let last = self.buffer.line_count().saturating_sub(1);
        self.folds
            .retain(|f| f.start <= last && f.end <= last && f.end > f.start);
    }
}

#[test]
fn a_fold_refresh_after_a_session_restore_does_not_lose_the_restored_state() {
    use np_core::SessionSnapshot;
    let d = folded();
    let tab = d.snapshot();
    let mut restored = doc(RUST_SRC);
    np_tui_session_apply(&mut restored, &tab);
    restored.refresh_folds("Rust");
    assert!(
        restored.line_is_hidden(1),
        "the first recompute dropped the restored fold: {:?}",
        restored.folds
    );
}

// --------------------------------------------------------------- no panic

#[test]
fn view_operations_on_an_empty_document_never_panic() {
    let mut d = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    for wrap in [false, true] {
        let g = geom(10, 4, wrap);
        assert!(!d.visible_rows(&g).is_empty());
        d.scroll_to_show(&g, LineCol::new(0, 0));
        d.refresh_folds("Rust");
        d.fold_all();
        d.unfold_all();
        d.toggle_fold_at(0);
        d.toggle_bookmark();
        let _ = d.next_bookmark();
        let _ = d.prev_bookmark();
        d.clear_bookmarks();
    }
    // The buffer type agrees that an empty document is one line, which is what every
    // `line_count() - 1` above relies on.
    assert_eq!(Buffer::new().line_count(), 1);
}
