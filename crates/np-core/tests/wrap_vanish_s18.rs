//! S18: text after a long wrapped line vanished from the screen.
//!
//! Reported as: "in word wrap mode long string disappear when it reach bottom of the text area
//! and all the text after it also disappear."
//!
//! # The mechanism
//!
//! `push_rows_from` lays out one line at a time and, when a line does not fit the rows remaining
//! in the viewport, throws the partial layout away -- correct, because painting half a wrapped
//! line strands the other half (no scroll position can begin a window partway through one line's
//! row list). The defect was that it then `break`ed out of the whole page.
//!
//! So a line taller than the *rows left at the bottom of the page* ended the page, and every line
//! after it was hidden at every scroll position. Because the page also could not reach past it,
//! there was no gesture that recovered the rest of the file: the screen showed the lines above
//! the long one and blank below, which to a user is missing content.
//!
//! # The two kinds of over-tall line, which need different answers
//!
//!  - One that would fit inside a whole viewport, given room. It paints correctly from its own
//!    line onward, so dropping it and ending the page is right: the page stays a prefix of the
//!    document's row list, and the next scroll position shows it whole.
//!  - One taller than the **entire viewport**. No position can ever show it whole, so refusing to
//!    paint it protects nothing -- its tail is unreachable either way -- while ending the page
//!    hid every line beneath it. It is clipped to what fits and the page continues.
//!
//! The tests below assert reachability rather than exact row counts, because the correct row count
//! depends on the viewport and the wrap width. What must never happen is a line that no scroll
//! position can show.

use np_core::{layout_visible_rows, total_row_count, Buffer, FoldRange, ViewGeometry};

fn geom(width: u16, height: u16) -> ViewGeometry {
    ViewGeometry {
        gutter_width: 0,
        text_area: np_core::RectCore::new(0, 0, width, height),
        tab_width: 4,
        word_wrap: true,
        bars: np_core::BarOccupancy::default(),
    }
}

/// Short lines, one long paragraph, short lines again -- the shape the report describes.
fn file_with_long_line(long_len: usize, after: usize) -> Buffer {
    let mut text = String::new();
    for i in 0..20 {
        text.push_str(&format!("short line {i}\n"));
    }
    text.push_str(&format!("{} TAIL-OF-LONG-LINE\n", "L".repeat(long_len)));
    for i in 0..after {
        text.push_str(&format!("after long line {i}\n"));
    }
    Buffer::from_str_lossy(&text)
}

/// Every line the document has, as seen from every scroll position.
fn reachable_lines(buf: &Buffer, g: &ViewGeometry) -> Vec<usize> {
    let folds: &[FoldRange] = &[];
    let mut seen = vec![false; buf.line_count()];
    for top in 0..buf.line_count() {
        for row in layout_visible_rows(buf, g, top, folds) {
            if row.line < seen.len() {
                seen[row.line] = true;
            }
        }
    }
    (0..buf.line_count()).filter(|i| !seen[*i]).collect()
}

/// The documented trade, asserted rather than assumed.
///
/// A page may legitimately be shorter than the viewport: when the next line needs more rows than
/// remain, painting part of it would strand the remainder, so the line is dropped whole and the
/// page ends. `view_s13::the_viewport_height_caps_the_row_list_under_wrap` pins that, and skipping
/// ahead to a later line instead would show line 3 while line 2 sat unseen above it.
///
/// What must hold is that the shortfall is **bounded by the one line that caused it**. A page
/// short by more than that line's row count means something is emptying the screen rather than
/// declining to halve a line -- which is exactly the reported defect, where a long paragraph cost
/// the whole rest of the file.
#[test]
fn a_long_line_reaching_the_bottom_does_not_take_the_rest_of_the_file_with_it() {
    // The reported case exactly: a line tall enough that it cannot fit the rows left at the
    // bottom of the screen. At width 100 and a 27-row viewport that means more than about 2600
    // characters. Before the fix the page stopped at the line *above* it with seven rows blank
    // beneath, and the long line and everything after it were absent from that screen.
    let g = geom(100, 27);
    for long_len in [3000usize, 5000] {
        let buf = file_with_long_line(long_len, 10);
        for top in 0..buf.line_count() {
            let rows = layout_visible_rows(&buf, &g, top, &[]);
            let Some(last) = rows.last() else { continue };
            if last.line + 1 >= buf.line_count() {
                continue;
            }
            let last_line = last.line;
            let short = g.viewport_rows().saturating_sub(rows.len());
            assert_eq!(
                short, 0,
                "page from top {top} (long_len {long_len}) painted {} of {} rows and stopped at                  line {last_line}, leaving {short} blank rows above text that continues to line {}",
                rows.len(),
                g.viewport_rows(),
                buf.line_count() - 1,
            );
        }
    }
}

#[test]
fn a_short_page_is_caused_by_the_line_that_followed_it() {
    let g = geom(100, 27);
    let all_of = |buf: &Buffer, line: usize| {
        np_core::document_rows(buf, &g, &[])
            .iter()
            .filter(|r| r.line == line)
            .count()
    };
    for long_len in [400usize, 800, 1200, 1600, 2000] {
        let buf = file_with_long_line(long_len, 10);
        for top in 0..buf.line_count() {
            let rows = layout_visible_rows(&buf, &g, top, &[]);
            let Some(last) = rows.last() else { continue };
            if last.line + 1 >= buf.line_count() {
                continue; // the real end of the document
            }
            let short = g.viewport_rows().saturating_sub(rows.len());
            let next_rows = all_of(&buf, last.line + 1);
            assert!(
                short <= next_rows.max(1),
                "page from top {top} (long_len {long_len}) is {short} rows short of a viewport                  but the next line is only {next_rows} rows: the screen was left with blank space                  larger than the line that caused it",
            );
        }
    }
}

#[test]
fn every_line_is_shown_whole_from_its_own_scroll_position() {
    // Whatever a page does at an arbitrary offset, asking for a line as the top of the page must
    // show that line whole -- unless it is taller than the entire viewport and cannot be.
    let g = geom(100, 27);
    for long_len in [400usize, 800, 1200, 2000] {
        let buf = file_with_long_line(long_len, 10);
        let all = np_core::document_rows(&buf, &g, &[]);
        // The document's last line is excluded: asking for a page at or past the end resolves to
        // the start of the last page rather than to that line, so it appears as part of a page
        // instead of as the page top. That is `clamp_scroll_top`'s documented case 2, and the
        // reachability test above already covers the last line's visibility.
        for line in 0..buf.line_count().saturating_sub(1) {
            let whole = all.iter().filter(|r| r.line == line).count();
            let rows = layout_visible_rows(&buf, &g, line, &[]);
            let shown = rows.iter().filter(|r| r.line == line).count();
            if whole > g.viewport_rows() {
                assert!(
                    shown > 0,
                    "line {line} is over-long and must still show some rows"
                );
            } else {
                assert_eq!(
                    shown, whole,
                    "line {line} (long_len {long_len}) needs {whole} rows and a viewport holds                      {}, so it must be whole when it is the page top; showed {shown}",
                    g.viewport_rows(),
                );
            }
        }
    }
}

#[test]
fn lines_after_a_long_wrapped_line_are_reachable() {
    // A line of 1200 characters wraps to roughly thirteen rows at width 100. Before the fix, the
    // ten lines after it were unreachable from every scroll position.
    let g = geom(100, 27);
    let buf = file_with_long_line(1200, 10);
    let long_line = 20;
    assert!(
        total_row_count(&buf, &g, &[]) > g.viewport_rows(),
        "premise: the document must be taller than the viewport",
    );
    // The long line itself must wrap to more than one row, or it is not the case under test.
    let rows_at_own_top = layout_visible_rows(&buf, &g, long_line, &[]);
    assert!(
        rows_at_own_top
            .iter()
            .filter(|r| r.line == long_line)
            .count()
            > 1,
        "premise: the long line should wrap to several rows",
    );
    let missing = reachable_lines(&buf, &g);
    assert!(
        missing.is_empty(),
        "lines unreachable at every scroll position: {missing:?}",
    );
}

#[test]
fn a_page_below_a_long_line_still_carries_the_lines_after_it() {
    // The narrower claim: with the long line just above the viewport's bottom edge, the lines
    // after it must appear on that page rather than the page stopping dead.
    let g = geom(100, 27);
    let buf = file_with_long_line(1200, 10);
    // Start one line past the long one: those rows must be present.
    let rows = layout_visible_rows(&buf, &g, 21, &[]);
    assert!(
        !rows.is_empty(),
        "a page starting after the long line must paint"
    );
    assert_eq!(
        rows.first().map(|r| r.line),
        Some(21),
        "the page must start where it was asked to",
    );
    let last = rows.last().map(|r| r.line).unwrap_or(0);
    assert!(
        last > 24,
        "the page must run well past the long line, reached only line {last}",
    );
}

#[test]
fn a_line_taller_than_the_whole_viewport_is_clipped_not_hidden() {
    // 3000 characters at width 100 is far more than a 27-row viewport can hold. Such a line can
    // never be shown whole, so it is clipped to what fits -- and the lines after it must still be
    // reachable, which is what the old `break` destroyed.
    let g = geom(100, 27);
    let buf = file_with_long_line(3000, 10);
    let rows = layout_visible_rows(&buf, &g, 20, &[]);
    let shown = rows.iter().filter(|r| r.line == 20).count();
    assert!(
        shown > 1,
        "a line taller than the viewport must still show its first rows, showed {shown}",
    );
    assert!(
        shown <= g.viewport_rows(),
        "a clipped line must not exceed the viewport: {shown} of {}",
        g.viewport_rows(),
    );
    let missing = reachable_lines(&buf, &g);
    assert!(missing.is_empty(), "unreachable lines: {missing:?}");
}

#[test]
fn a_line_one_row_too_tall_ends_the_page_without_hiding_later_lines() {
    // The other kind: it fits in a whole viewport, so the page ends (the page stays a prefix of
    // the row list) -- but a later scroll position must still show both the line and everything
    // after it.
    let g = geom(100, 27);
    let buf = file_with_long_line(500, 10);
    let missing = reachable_lines(&buf, &g);
    assert!(
        missing.is_empty(),
        "no line may be unreachable: {missing:?}",
    );
    // And the line is shown whole somewhere.
    let rows = layout_visible_rows(&buf, &g, 20, &[]);
    let whole = np_core::document_rows(&buf, &g, &[])
        .iter()
        .filter(|r| r.line == 20)
        .count();
    let shown = rows.iter().filter(|r| r.line == 20).count();
    assert_eq!(
        shown, whole,
        "a line that fits a whole viewport must be shown whole from its own line onward"
    );
}

#[test]
fn no_page_ever_paints_half_a_line_that_could_be_shown_whole() {
    // The invariant the S13 tests defend, restated for documents containing an over-long line:
    // any number of rows a line contributes must be either zero or all of them, unless the line
    // is taller than the entire viewport and can never be whole.
    let g = geom(100, 27);
    let buf = file_with_long_line(1200, 10);
    let all = np_core::document_rows(&buf, &g, &[]);
    let whole_of = |line: usize| all.iter().filter(|r| r.line == line).count();
    for top in 0..buf.line_count() {
        let rows = layout_visible_rows(&buf, &g, top, &[]);
        for line in 0..buf.line_count() {
            let shown = rows.iter().filter(|r| r.line == line).count();
            let whole = whole_of(line);
            if shown == 0 || shown == whole {
                continue;
            }
            assert!(
                whole > g.viewport_rows(),
                "line {line} painted {shown} of {whole} rows from top {top}, but it would fit a \
                 whole viewport and must never be halved",
            );
        }
    }
}
