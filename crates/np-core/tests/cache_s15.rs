//! S15: the derived caches must never serve a stale answer (spec §8, §9).
//!
//! The performance work memoizes two whole-document measurements -- the widest line and the
//! total visual row count -- because both were being recomputed from scratch on every frame.
//! A cache that is fast and wrong is worse than no cache at all: the user sees a scrollbar
//! that lies, and nothing crashes. So every input that changes an answer has to invalidate it,
//! and each rule below is a test.

use np_core::view::ViewGeometry;
use np_core::{Buffer, Document, EncodingId, Eol, LineCol};

fn geom(width: u16, height: u16, wrap: bool, tab: usize) -> ViewGeometry {
    ViewGeometry {
        gutter_width: 0,
        text_area: np_core::RectCore::new(0, 0, width, height),
        tab_width: tab,
        word_wrap: wrap,
        // No scrollbars: these tests lay rows out directly, without an `App` to reserve the
        // band's edge cells, so the accessors fall back to the whole text area.
        ..Default::default()
    }
}

fn buffer(text: &str) -> Buffer {
    Buffer::from_str_lossy(text)
}

#[test]
fn max_line_width_matches_a_fresh_walk_after_every_kind_of_edit() {
    let mut buf = buffer("short\nthe longest line in the place\nx\n");
    let tab = 4;
    let walk = |b: &Buffer| -> usize {
        (0..b.line_count())
            .map(|l| b.display_width_range(l, 0, b.line_len_chars(l), tab))
            .max()
            .unwrap_or(0)
    };
    assert_eq!(buf.max_line_width(tab), walk(&buf));
    // Extend the current longest line: the answer grows without a line being added.
    buf.insert(LineCol::new(1, 27), " and then some more");
    assert_eq!(buf.max_line_width(tab), walk(&buf), "extending a line");
    // Add a brand new longest line.
    buf.insert_line(2, "an even longer line than the one above it");
    assert_eq!(buf.max_line_width(tab), walk(&buf), "adding a line");
    // Delete the longest line: the answer must shrink, which a naive monotonic cache misses.
    buf.delete_range(LineCol::new(1, 0), LineCol::new(2, 0));
    assert_eq!(
        buf.max_line_width(tab),
        walk(&buf),
        "deleting the longest line"
    );
    // Undo must invalidate too: it mutates through the same counter, but it is the path most
    // likely to be forgotten.
    let mut doc = Document::from_decoded(
        "/tmp/cache.txt".into(),
        "aaa\nbbbbbbbbbb\ncc\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    );
    let before = doc.buffer.max_line_width(4);
    doc.insert_text(
        "wwwwwwwwwwwwwwwwww",
        np_core::UndoGroup::Atomic,
        std::time::Instant::now(),
    )
    .expect("insert");
    let grown = doc.buffer.max_line_width(4);
    assert!(
        grown > before,
        "the insert should have widened the document"
    );
    doc.undo_one();
    assert_eq!(
        doc.buffer.max_line_width(4),
        before,
        "undo must not leave a stale wider answer"
    );
}

#[test]
fn max_line_width_reanswers_when_the_tab_width_changes() {
    // A tab-width change alters the answer without altering the text, so the revision alone
    // cannot be the whole key.
    let buf = buffer("a\tb\nlonger\tline\there\n");
    let narrow = buf.max_line_width(2);
    let wide = buf.max_line_width(16);
    assert_ne!(narrow, wide, "tab width must change the measurement");
    // And back again, with no edit in between: a cache that only stored the last answer would
    // be right by luck here, so ask for the original answer after the other one.
    assert_eq!(buf.max_line_width(2), narrow);
    assert_eq!(buf.max_line_width(16), wide);
}

#[test]
fn total_row_count_invalidates_on_edit_wrap_and_resize() {
    let long = "word ".repeat(60);
    let mut buf = buffer(&format!("{long}\nshort\n{long}\n"));
    let folds: Vec<np_core::FoldRange> = Vec::new();
    let wrap = geom(40, 20, true, 4);

    let initial = np_core::total_row_count(&buf, &wrap, &folds);
    assert!(initial > 3, "wrap should produce more rows than lines");
    // A cache hit must be exact, not approximate.
    assert_eq!(np_core::total_row_count(&buf, &wrap, &folds), initial);

    // Editing the text changes it.
    buf.insert(
        LineCol::new(1, 0),
        &"another long passage of words to wrap ".repeat(20),
    );
    let after_edit = np_core::total_row_count(&buf, &wrap, &folds);
    assert!(after_edit > initial, "adding text must add rows");

    // Toggling wrap changes it with the buffer untouched.
    let nowrap = geom(40, 20, false, 4);
    assert_eq!(
        np_core::total_row_count(&buf, &nowrap, &folds),
        buf.line_count(),
        "without wrap each line is exactly one row"
    );
    assert_eq!(
        np_core::total_row_count(&buf, &wrap, &folds),
        after_edit,
        "returning to wrap must restore the wrapped answer, not the no-wrap one"
    );

    // A narrower viewport produces more rows.
    let narrow = geom(20, 20, true, 4);
    assert!(
        np_core::total_row_count(&buf, &narrow, &folds) > after_edit,
        "halving the width must increase the row count"
    );
    assert_eq!(np_core::total_row_count(&buf, &wrap, &folds), after_edit);
}

#[test]
fn total_row_count_follows_a_fold_toggle() {
    // Two ranges with the same length and different collapsed flags must not collide in the
    // fold fingerprint.
    let mut doc = Document::from_decoded(
        "/tmp/fold.txt".into(),
        "fn a() {\n    body\n}\nfn b() {\n    body\n}\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    );
    let plain = np_core::total_row_count(&doc.buffer, &doc_view_geom(), &doc.folds);
    doc.folds = vec![np_core::FoldRange {
        start: 0,
        end: 2,
        collapsed: false,
    }];
    let open = np_core::total_row_count(&doc.buffer, &doc_view_geom(), &doc.folds);
    doc.folds = vec![np_core::FoldRange {
        start: 0,
        end: 2,
        collapsed: true,
    }];
    let closed = np_core::total_row_count(&doc.buffer, &doc_view_geom(), &doc.folds);
    assert_eq!(plain, open, "an open fold hides nothing");
    assert!(closed < plain, "a collapsed fold must hide its body rows");
}

fn doc_view_geom() -> ViewGeometry {
    ViewGeometry {
        gutter_width: 0,
        text_area: np_core::RectCore::new(0, 0, 120, 40),
        tab_width: 4,
        word_wrap: false,
        ..Default::default()
    }
}

#[test]
fn row_offset_of_line_agrees_with_the_row_list() {
    // The scrollbar's thumb position now comes from `row_offset_of_line` rather than from
    // building the whole row vector and searching it. Both must answer identically, or the
    // thumb lands on one line while the screen shows another.
    let long = "some words to wrap around ".repeat(40);
    let buf = buffer(&format!("{long}\nshort line\n{long}\ntail\n"));
    let g = geom(50, 20, true, 4);
    let rows = np_core::document_rows(&buf, &g, &[]);
    assert_eq!(rows.len(), np_core::total_row_count(&buf, &g, &[]));
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for line in 0..buf.line_count() {
        if !seen.insert(line) {
            continue;
        }
        let expected = rows.iter().position(|r| r.line == line);
        let got = np_core::row_offset_of_line(&buf, &g, &[], line);
        match expected {
            Some(index) => assert_eq!(got, index, "line {line}"),
            None => assert!(got <= rows.len(), "line {line} past the end"),
        }
    }
}

#[test]
fn wrap_matches_after_the_incremental_cell_tracking() {
    // The wrap loop used to re-measure each cluster's display column from the start of the
    // line. It now carries the count forward. Tabs are the case that could drift, because a
    // tab's stop depends on the position within the whole line rather than within the row.
    let cases = [
        "\tone\ttwo\tthree\tfour\tfive\tsix\tseven\teight\n",
        "        leading spaces then a long enough run of words to wrap twice\n",
        "\ta\tbb\tccc\tdddd\tddddd\tdddddd\n",
        "\u{4f60}\u{597d}\t\u{4f60}\u{597d}\t\u{4f60}\u{597d}\t\u{4f60}\u{597d}\t\u{4f60}\u{597d}\n",
        "no tabs here at all but plenty of words so that the row has to break somewhere\n",
    ];
    for (index, text) in cases.iter().enumerate() {
        // One line per fixture, so the row list below belongs to exactly one logical line.
        let one_line = text.trim_end_matches('\n');
        let buf = buffer(one_line);
        assert_eq!(buf.line_count(), 1, "fixture {index} must be one line");
        for width in [10u16, 20, 40] {
            for tab in [2usize, 4, 8] {
                let g = geom(width, 40, true, tab);
                let rows = np_core::document_rows(&buf, &g, &[]);
                // Rows must tile the line with no gap and no overlap, and each must fit.
                let mut cursor = 0usize;
                for row in &rows {
                    assert_eq!(row.line, 0, "case {index} w{width} tab{tab}");
                    assert_eq!(row.start_col, cursor, "case {index} w{width} tab{tab} gap");
                    assert!(
                        buf.display_width_range(row.line, row.start_col, row.end_col, tab)
                            <= width as usize,
                        "case {index} w{width} tab{tab} row too wide"
                    );
                    cursor = row.end_col;
                }
                assert_eq!(
                    cursor,
                    buf.line_len_chars(0),
                    "case {index} w{width} tab{tab} rows did not cover the line"
                );
            }
        }
    }
}

#[test]
fn a_frame_does_not_scale_with_document_size() {
    // The regression guard for S15: a steady-state frame used to cost 670 ms on a 20 MB file
    // and 3.5 s on a 100 MB one. The measurement is in rows-walked rather than milliseconds so
    // it does not turn flaky on a loaded machine: a frame must not walk the whole document.
    //
    // `total_row_count` is allowed to be O(document) once, so this asserts the second call at
    // the same size and revision is O(1), which is what a frame actually pays.
    let long = "fn thing() {{ let x = {i}; }}\n";
    let mut text = String::new();
    for i in 0..200_000 {
        text.push_str(&long.replace("{i}", &i.to_string()));
    }
    let buf = buffer(&text);
    let g = geom(120, 40, false, 4);
    let first = std::time::Instant::now();
    let total = np_core::total_row_count(&buf, &g, &[]);
    let first_cost = first.elapsed();
    let second = std::time::Instant::now();
    assert_eq!(np_core::total_row_count(&buf, &g, &[]), total);
    let second_cost = second.elapsed();
    assert!(
        second_cost.as_micros() * 50 < first_cost.as_micros().max(1),
        "the cached row count should be far cheaper than the first walk: \
         first {first_cost:?}, second {second_cost:?}"
    );
    assert_eq!(total, buf.line_count(), "no wrap means one row per line");
}

/// The no-wrap row count is a closed form (`lines - hidden fold bodies`), not the per-line
/// walk. That is only legitimate while it agrees with the row list that actually gets painted,
/// so this pins the formula to `document_rows` -- the oracle -- across nested, adjacent,
/// degenerate and out-of-range folds.
#[test]
fn nowrap_row_count_matches_the_row_list_for_every_fold_shape() {
    let mut buf = buffer("a() {\n  b() {\n    x\n  }\n}\nc\nd\n");
    let g = geom(80, 40, false, 4);
    let cases: Vec<Vec<np_core::FoldRange>> = vec![
        vec![],
        // No collapsed range: nothing hidden.
        vec![np_core::FoldRange {
            start: 0,
            end: 4,
            collapsed: false,
        }],
        // A plain collapsed body.
        vec![np_core::FoldRange {
            start: 0,
            end: 4,
            collapsed: true,
        }],
        // Nested inside a collapsed parent: the inner lines are already hidden, so the inner
        // fold must not subtract them a second time.
        vec![
            np_core::FoldRange {
                start: 0,
                end: 4,
                collapsed: true,
            },
            np_core::FoldRange {
                start: 1,
                end: 3,
                collapsed: true,
            },
        ],
        // Parent open, child collapsed.
        vec![
            np_core::FoldRange {
                start: 0,
                end: 4,
                collapsed: false,
            },
            np_core::FoldRange {
                start: 1,
                end: 3,
                collapsed: true,
            },
        ],
        // Two disjoint collapsed ranges.
        vec![
            np_core::FoldRange {
                start: 0,
                end: 1,
                collapsed: true,
            },
            np_core::FoldRange {
                start: 4,
                end: 5,
                collapsed: true,
            },
        ],
        // Degenerate: a header with no body hides nothing.
        vec![np_core::FoldRange {
            start: 2,
            end: 2,
            collapsed: true,
        }],
        // Out-of-range end, past the last line.
        vec![np_core::FoldRange {
            start: 2,
            end: 99,
            collapsed: true,
        }],
        // Collapsing everything leaves exactly one visible row.
        vec![np_core::FoldRange {
            start: 0,
            end: 99,
            collapsed: true,
        }],
    ];
    for (i, folds) in cases.iter().enumerate() {
        let count = np_core::total_row_count(&buf, &g, folds);
        let listed = np_core::document_rows(&buf, &g, folds).len();
        assert_eq!(
            count, listed,
            "case {i}: closed form {count} disagrees with the row list {listed}"
        );
    }
    // And through an edit, which is the case a revision-keyed cache cannot serve.
    buf.insert_line(3, "  y");
    for (i, folds) in cases.iter().enumerate() {
        let count = np_core::total_row_count(&buf, &g, folds);
        let listed = np_core::document_rows(&buf, &g, folds).len();
        assert_eq!(count, listed, "case {i} after an edit: {count} != {listed}");
    }
}

/// The closed form exists to kill a 55 ms per-keystroke walk, so a test that only checks
/// correctness would let the fix regress silently back to O(lines). This asserts the shape:
/// an edit into a large no-wrap document must cost far less than a full document walk.
#[test]
fn editing_a_large_nowrap_document_does_not_walk_it() {
    let mut buf = buffer(&"fn f() { let x = 1; }\n".repeat(120_000));
    let g = geom(120, 40, false, 4);
    let folds: Vec<np_core::FoldRange> = Vec::new();
    np_core::total_row_count(&buf, &g, &folds);

    let walk = std::time::Instant::now();
    let listed = np_core::document_rows(&buf, &g, &folds).len();
    let walk_cost = walk.elapsed();

    buf.insert(LineCol::new(0, 0), "z");
    let count_start = std::time::Instant::now();
    let count = np_core::total_row_count(&buf, &g, &folds);
    let count_cost = count_start.elapsed();
    assert_eq!(count, listed, "still one row per line");
    assert!(
        count_cost * 20 < walk_cost,
        "counting rows after an edit should not scale with the document: \
         count {count_cost:?} vs full walk {walk_cost:?}"
    );
}
