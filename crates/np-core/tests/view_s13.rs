//! S13 — the visible-row layout: wrap, folds, and the mapping between them (FR-V1, FR-V5).
//!
//! This is the highest-value test target in the stage. Everything the renderer paints, every
//! click resolves, and every arrow key moves through these functions, so a bug here is a bug
//! the user experiences as a broken editor rather than as a wrong pixel.

use np_core::{
    document_rows, layout_visible_rows, line_is_visible, next_visible_line, row_end_for,
    total_row_count, Buffer, FoldRange, LineCol, RectCore, ViewGeometry, VisualRow,
};

fn geom(w: u16, h: u16, wrap: bool) -> ViewGeometry {
    ViewGeometry {
        gutter_width: 0,
        text_area: RectCore::new(0, 0, w, h),
        tab_width: 4,
        word_wrap: wrap,
        // No scrollbars; see the note on `BarOccupancy`.
        ..Default::default()
    }
}

/// The text one row displays, for readable assertions.
fn row_text(buf: &Buffer, row: &VisualRow) -> String {
    buf.line_string(row.line)
        .chars()
        .skip(row.start_col)
        .take(row.char_len())
        .collect()
}

fn rows_dump(buf: &Buffer, rows: &[VisualRow]) -> Vec<String> {
    rows.iter().map(|r| row_text(buf, r)).collect()
}

/// Every row's display width, to check nothing overflows the area.
fn row_widths(buf: &Buffer, rows: &[VisualRow], tab: usize) -> Vec<usize> {
    rows.iter()
        .map(|r| buf.display_width_range(r.line, r.start_col, r.end_col, tab))
        .collect()
}

// ------------------------------------------------------------------- wrapping

#[test]
fn wrap_off_emits_one_row_per_logical_line() {
    let buf =
        Buffer::from_str_lossy("a very long line that would certainly wrap in a narrow area\nb\n");
    let rows = layout_visible_rows(&buf, &geom(20, 24, false), 0, &[]);
    assert_eq!(rows.len(), 3, "no wrap means one row each, however long");
    assert_eq!(rows[0].char_len(), 59, "the whole 59-char line is one row");
    assert_eq!(rows[0].end_col, buf.line_len_chars(0));
}

#[test]
fn a_300_character_line_wraps_at_word_boundaries_in_a_40_column_area() {
    // 300 characters of 9-letter words and spaces, so every boundary is unambiguous.
    let line = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu ".repeat(6);
    let line = line.trim_end().to_string();
    assert!(
        line.chars().count() > 300,
        "fixture must exceed 300 chars, is {}",
        line.chars().count()
    );
    let buf = Buffer::from_str_lossy(&line);
    let width = 40;
    let rows = layout_visible_rows(&buf, &geom(width, 100, true), 0, &[]);

    // No row exceeds the area.
    for w in row_widths(&buf, &rows, 4) {
        assert!(w <= width as usize, "row is {w} cells, over {width}");
    }
    // No word was split: every row boundary falls on a space in the source.
    for row in &rows {
        if row.end_col >= line.chars().count() {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(
            chars[row.end_col - 1],
            ' ',
            "row ending at col {} splits a word: {:?}",
            row.end_col,
            row_text(&buf, row)
        );
    }
    // The rows partition the line exactly, in order, with no gap and no overlap.
    assert_eq!(rows[0].start_col, 0);
    for pair in rows.windows(2) {
        assert_eq!(pair[0].end_col, pair[1].start_col, "gap between rows");
    }
    assert_eq!(rows.last().expect("rows").end_col, line.chars().count());
}

#[test]
fn a_word_longer_than_the_area_gets_a_hard_break() {
    let buf = Buffer::from_str_lossy("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa tail");
    let width = 10;
    let rows = layout_visible_rows(&buf, &geom(width, 40, true), 0, &[]);
    // The 50-character run of `a` occupies exactly five full rows, then " tail" gets its own.
    assert_eq!(rows.len(), 6);
    for row in &rows[..5] {
        assert_eq!(
            buf.display_width_range(row.line, row.start_col, row.end_col, 4),
            10,
            "a hard-broken row fills the width"
        );
    }
    assert_eq!(row_text(&buf, rows.last().expect("row")), " tail");
}

#[test]
fn wrapping_a_double_width_cjk_line_never_strands_a_half_cell() {
    // 20 CJK characters, 2 cells each: 40 cells in a 7-cell area.
    let buf = Buffer::from_str_lossy("日本語のテキストがありますとても長いです");
    assert_eq!(buf.display_width(0, 4), 40);
    let width = 7;
    let rows = layout_visible_rows(&buf, &geom(width, 40, true), 0, &[]);
    for row in &rows {
        let w = buf.display_width_range(row.line, row.start_col, row.end_col, 4);
        assert!(w <= width as usize, "row of {w} cells overflows {width}");
    }
    // Each row holds whole characters only: 3 chars = 6 cells in a 7-cell area, never a
    // 4th character clipped to its left half.
    for row in &rows {
        let held = row.end_col - row.start_col;
        assert_eq!(
            buf.display_width_range(row.line, row.start_col, row.end_col, 4),
            held * 2,
            "row does not hold whole characters"
        );
    }
    assert_eq!(
        rows.iter().map(|r| r.end_col - r.start_col).sum::<usize>(),
        20
    );
}

#[test]
fn a_combining_mark_stays_with_its_base_character() {
    // "e" + U+0301 is one grapheme cluster of one cell; a break between them would render a
    // floating accent on the next row.
    let buf = Buffer::from_str_lossy("abca\u{0301}defghijklmnop");
    // "a","b","c","a"+accent = 4 cells; a 4-cell area is the tight fit that would split a
    // cluster if the wrapper counted chars instead of clusters.
    // The mark itself is zero-width, but `grapheme_width` floors a cluster at one cell so a
    // row can never stall, which makes the cluster 2 cells: a, b, c, and a+mark.
    assert_eq!(buf.display_width(0, 4), 17, "17 cells for 16 graphemes");
    let rows = layout_visible_rows(&buf, &geom(4, 40, true), 0, &[]);
    let texts = rows_dump(&buf, &rows);
    assert_eq!(
        texts[0], "abca\u{0301}",
        "cluster must not be split: {texts:?}"
    );
    assert_eq!(texts[1], "defg", "next row picks up at the next cluster");
    // The break after the accented 'a' is at a cluster boundary, so no row starts with a mark.
    for row in &rows {
        let first = buf.line_string(row.line).chars().nth(row.start_col);
        assert!(
            !first.is_some_and(|c| ('\u{0300}'..='\u{036F}').contains(&c)),
            "row starts with a combining mark: {:?}",
            rows_dump(&buf, &rows)
        );
    }
}

#[test]
fn tabs_are_expanded_for_wrap_width_and_never_split() {
    let buf = Buffer::from_str_lossy("\tone\ttwo\tthree\tfour\tfive\tsix");
    let width = 12;
    let rows = layout_visible_rows(&buf, &geom(width, 40, true), 0, &[]);
    for (row, w) in rows.iter().zip(row_widths(&buf, &rows, 4)) {
        assert!(
            w <= width as usize
                || buf.line_string(row.line).chars().nth(row.start_col) == Some('\t'),
            "tab-expanded row of {w} overflows {width}: {:?}",
            row_text(&buf, row)
        );
    }
    // A row that ends on a tab ends on the *whole* tab, so its cell count lands on a tab
    // stop rather than partway through the tab's cells.
    for row in &rows {
        let text = row_text(&buf, row);
        if text.ends_with('\t') {
            let w = buf.display_width_range(row.line, row.start_col, row.end_col, 4);
            assert_eq!(w % 4, 0, "row {text:?} ends mid-tab at {w} cells");
        }
    }
    let texts = rows_dump(&buf, &rows);
    assert_eq!(
        texts.concat(),
        "\tone\ttwo\tthree\tfour\tfive\tsix",
        "wrapping must not lose or reorder a character: {texts:?}"
    );
}

#[test]
fn an_empty_line_still_occupies_one_row() {
    let buf = Buffer::from_str_lossy("\n\n");
    let rows = layout_visible_rows(&buf, &geom(10, 40, true), 0, &[]);
    assert_eq!(
        rows.len(),
        3,
        "two terminators make three lines, each a row"
    );
    assert!(rows.iter().all(|r| r.char_len() == 0));
}

#[test]
fn a_line_that_exactly_fills_the_width_does_not_gain_an_empty_row() {
    let buf = Buffer::from_str_lossy("abcdefghij");
    let rows = layout_visible_rows(&buf, &geom(10, 40, true), 0, &[]);
    assert_eq!(rows.len(), 1, "an exact fit is one row, not two");
    assert_eq!(rows[0].end_col, 10);
}

#[test]
fn a_narrow_area_never_loops_and_never_drops_text() {
    let buf = Buffer::from_str_lossy("the quick brown fox jumps over the lazy dog");
    for width in 1..12u16 {
        let rows = layout_visible_rows(&buf, &geom(width, 200, true), 0, &[]);
        let joined: String = rows.iter().map(|r| row_text(&buf, r)).collect();
        assert_eq!(
            joined, "the quick brown fox jumps over the lazy dog",
            "width {width} lost or reordered text"
        );
    }
}

#[test]
fn the_viewport_height_caps_the_row_list_under_wrap() {
    // Fixture rows at width 10 are 2 + 2 + 2 + 1 + 1, so a viewport of four holds exactly two
    // whole lines and a viewport of five holds two lines plus one row of the third.
    let buf = Buffer::from_str_lossy("one two three\nfive six seven\nnine ten eleven\nlast\n");
    let fits = layout_visible_rows(&buf, &geom(10, 4, true), 0, &[]);
    assert_eq!(fits.len(), 4, "exactly two whole lines");
    assert_eq!(
        fits.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![0, 0, 1, 1],
        "a line is never started and left half-painted"
    );
    // The fifth row would be line 2's first of two, so line 2 is dropped whole and the window
    // shows four rows rather than five. That is the deliberate trade: see the "Why a viewport
    // never paints half a wrapped line" note in `view`.
    let short = layout_visible_rows(&buf, &geom(10, 5, true), 0, &[]);
    assert_eq!(
        short.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![0, 0, 1, 1],
        "one spare row must not buy half a line"
    );
    // A viewport of nothing paints nothing.
    assert!(layout_visible_rows(&buf, &geom(10, 0, true), 0, &[]).is_empty());
}

// --------------------------------------------------------------------- folds

fn collapsed(start: usize, end: usize) -> FoldRange {
    FoldRange {
        start,
        end,
        collapsed: true,
    }
}

#[test]
fn a_collapsed_fold_hides_every_line_but_its_header() {
    let buf = Buffer::from_str_lossy("head\nbody1\nbody2\ntail\n");
    let folds = [collapsed(0, 2)];
    let rows = layout_visible_rows(&buf, &geom(80, 10, false), 0, &folds);
    assert_eq!(
        rows.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![0, 3, 4],
        "lines 1 and 2 gone, header and everything after kept"
    );
}

#[test]
fn nested_collapsed_folds_skip_the_outer_range() {
    let buf = Buffer::from_str_lossy("a\nb\nc\nd\ne\nf\ng\n");
    let folds = [collapsed(0, 5), collapsed(1, 3)];
    let rows = layout_visible_rows(&buf, &geom(80, 10, false), 0, &folds);
    assert_eq!(
        rows.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![0, 6, 7]
    );
    assert_eq!(next_visible_line(0, &folds), 6, "outer fold wins");
}

#[test]
fn an_expanded_fold_changes_nothing() {
    let buf = Buffer::from_str_lossy("a\nb\nc\n");
    let folds = [FoldRange {
        start: 0,
        end: 1,
        collapsed: false,
    }];
    let rows = layout_visible_rows(&buf, &geom(80, 10, false), 0, &folds);
    assert_eq!(rows.len(), 4);
}

#[test]
fn folds_apply_before_wrap_so_a_folded_line_contributes_no_rows() {
    // Line 1 is long enough to wrap, and it is folded away: its rows must not appear at all.
    let long = "word ".repeat(40);
    let text = format!("head\n{long}tail line\n");
    let buf = Buffer::from_str_lossy(&text);
    let open = layout_visible_rows(&buf, &geom(20, 40, true), 0, &[]);
    let closed = layout_visible_rows(&buf, &geom(20, 40, true), 0, &[collapsed(0, 1)]);
    assert!(
        closed.len() < open.len(),
        "folding a wrapped line must remove its rows: {} vs {}",
        closed.len(),
        open.len()
    );
    assert_eq!(closed[0].line, 0);
    assert_eq!(
        closed[1].line, 2,
        "the line after the fold follows the header directly"
    );
    assert!(!closed.iter().any(|r| r.line == 1));
}

#[test]
fn a_collapsed_range_followed_by_a_long_wrapped_line_produces_the_expected_rows() {
    let buf = Buffer::from_str_lossy("def f() {\n  hidden();\n}\nlet explanation = \"alpha bravo charlie delta echo foxtrot golf\";\n");
    let folds = [collapsed(0, 1)];
    let rows = layout_visible_rows(&buf, &geom(20, 20, true), 0, &folds);
    assert_eq!(rows[0].line, 0, "fold header first");
    assert!(!rows.iter().any(|r| r.line == 1), "hidden line absent");
    assert!(
        rows.iter().filter(|r| r.line == 3).count() > 1,
        "the long line after the fold still wraps"
    );
    // Rows carry their own slice and no terminator, so the concatenation is the visible text
    // with the line breaks removed -- the proof that nothing was dropped or duplicated.
    assert_eq!(
        rows_dump(&buf, &rows).concat(),
        "def f() {}let explanation = \"alpha bravo charlie delta echo foxtrot golf\";",
        "rows are exactly the visible text, in order"
    );
}

#[test]
fn a_fold_header_that_wraps_keeps_all_its_rows() {
    let long = "alpha bravo charlie delta echo foxtrot golf hotel india";
    let buf = Buffer::from_str_lossy(&format!("{long}\n  hidden\n  hidden2\ntail\n"));
    let folds = [collapsed(0, 2)];
    let rows = layout_visible_rows(&buf, &geom(20, 20, true), 0, &folds);
    let header_rows = rows.iter().filter(|r| r.line == 0).count();
    assert!(
        header_rows > 1,
        "a wrapping header must not be cut to one row"
    );
    assert_eq!(
        rows.iter().filter(|r| r.line == 3).count(),
        1,
        "the tail is one row"
    );
    assert!(!rows.iter().any(|r| r.line == 1 || r.line == 2));
}

#[test]
fn scrolling_to_a_hidden_line_lands_on_the_next_visible_one() {
    let buf = Buffer::from_str_lossy("head\nbody1\nbody2\ntail\n");
    let folds = [collapsed(0, 2)];
    let rows = layout_visible_rows(&buf, &geom(80, 4, false), 1, &folds);
    assert_eq!(
        rows[0].line, 3,
        "line 1 is hidden, so the window starts at line 3"
    );
    // Scrolling to a hidden line at the very end of the document resolves to the fold header
    // rather than jumping to the last line, which is a different place entirely.
    let buf2 = Buffer::from_str_lossy("head\nbody1\nbody2");
    let rows2 = layout_visible_rows(&buf2, &geom(80, 4, false), 2, &[collapsed(0, 2)]);
    assert_eq!(
        rows2[0].line, 0,
        "no visible line follows, so the header represents the fold"
    );
}

#[test]
fn line_is_visible_and_next_visible_line_agree() {
    let folds = [collapsed(2, 4), collapsed(7, 9)];
    for line in 0..12 {
        let visible = line_is_visible(line, &folds);
        assert_eq!(
            visible,
            !folds
                .iter()
                .any(|f| f.collapsed && line > f.start && line <= f.end),
            "line {line}"
        );
        let next = next_visible_line(line, &folds);
        assert!(next > line, "must always advance");
        if next < 12 {
            assert!(
                line_is_visible(next, &folds),
                "line {line} skipped to hidden {next}"
            );
        }
    }
}

// ------------------------------------------------------- round-trip inverses

#[test]
fn screen_row_of_and_row_at_are_exact_inverses_without_wrap() {
    let buf = Buffer::from_str_lossy("aaa\nbbb\nccc\nddd\n");
    let g = geom(80, 24, false);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(g.row_at(&rows, index as u16), Some(*row));
        for col in 0..=buf.line_len_chars(row.line) {
            let lc = LineCol::new(row.line, col);
            assert_eq!(
                g.screen_row_of(&rows, lc),
                Some(index as u16),
                "col {col} of row {index}"
            );
        }
    }
}

#[test]
fn screen_row_of_and_row_at_are_exact_inverses_with_wrap() {
    let buf = Buffer::from_str_lossy(
        "alpha bravo charlie\ndelta\necho foxtrot golf hotel india juliet\n",
    );
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    assert!(rows.len() > 5, "fixture must wrap");
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(g.row_at(&rows, index as u16), Some(*row));
        // Every column the row owns maps back to it. The row's own end_col belongs to it
        // only on the line's last row, which is what `holds` encodes.
        for col in row.start_col..row.end_col {
            let lc = LineCol::new(row.line, col);
            assert_eq!(
                g.screen_row_of(&rows, lc),
                Some(index as u16),
                "line {} col {col} should map to row {index}",
                row.line
            );
        }
    }
}

#[test]
fn screen_row_of_and_row_at_are_exact_inverses_with_folds_and_wrap() {
    let buf = Buffer::from_str_lossy(
        "def a():\n  hidden line that is long enough to wrap in ten columns\n  hidden\ndef b():\n  alpha bravo charlie delta echo\n",
    );
    let folds = [collapsed(0, 2)];
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &folds);
    assert!(!rows.iter().any(|r| r.line == 1 || r.line == 2));
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(g.row_at(&rows, index as u16), Some(*row));
        for col in row.start_col..row.end_col {
            assert_eq!(
                g.screen_row_of(&rows, LineCol::new(row.line, col)),
                Some(index as u16),
                "line {} col {col}",
                row.line
            );
        }
    }
}

// ------------------------------------------------------------ click mapping

#[test]
fn a_click_on_the_second_row_of_a_wrapped_line_hits_the_right_character() {
    let buf = Buffer::from_str_lossy("abcdefghijklmnopqrstuvwxyz");
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    assert_eq!(rows.len(), 3);
    // Second row starts at 'k'; clicking its third cell selects before 'n'.
    let second = rows[1];
    assert_eq!(second.start_col, 10);
    let lc = g.position_in_row(&buf, second, 3);
    assert_eq!(
        lc,
        LineCol::new(0, 13),
        "click at row-3 cells lands on char 13"
    );
}

#[test]
fn a_click_right_of_a_short_wrapped_row_clamps_to_that_rows_end() {
    let buf = Buffer::from_str_lossy("aaa bbb ccc ddd eee fff ggg hhh");
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let last = *rows.last().expect("rows");
    // Ten cells past the row's content must not reach into a row that is not this one.
    let lc = g.position_in_row(&buf, last, 40);
    assert!(
        lc.col <= last.end_col,
        "{lc:?} escaped row [{}..{})",
        last.start_col,
        last.end_col
    );
}

// ----------------------------------------------------------- vertical motion

#[test]
fn down_arrow_visits_every_visual_row_in_order() {
    let buf = Buffer::from_str_lossy("alpha bravo charlie\ndelta\necho foxtrot golf\n");
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let mut seen = vec![LineCol::new(0, 0)];
    for _ in 1..rows.len() {
        let next = g
            .vertical_of(
                &buf,
                &rows,
                *seen.last().expect("at least one row"),
                1,
                None,
            )
            .expect("motion within the window");
        seen.push(next);
    }
    // One step per row: the sequence of (line, row) pairs must be exactly the row list.
    let as_rows: Vec<(usize, usize)> = seen
        .iter()
        .map(|lc| {
            let index = g.screen_row_of(&rows, *lc).expect("on screen");
            (index as usize, lc.line)
        })
        .collect();
    let indices: Vec<usize> = as_rows.iter().map(|(i, _)| *i).collect();
    // Strictly increasing, and it reaches the last row. A row can be *passed over* rather
    // than visited when the previous row ends on a consumed trailing space: the goal column
    // carries the caret to the next row that can hold it. Skipping is correct; going
    // backwards, revisiting, or never reaching the bottom is not.
    for pair in indices.windows(2) {
        assert!(
            pair[1] > pair[0],
            "down-arrow must advance one row per press, got {indices:?}"
        );
    }
    assert_eq!(
        *indices.last().expect("visits"),
        rows.len() - 1,
        "down-arrow must reach the last row, got {indices:?}"
    );
    // Every logical line the window shows is visited, which is the user-visible guarantee.
    let visited_lines: Vec<usize> = seen.iter().map(|lc| lc.line).collect();
    for row in &rows {
        assert!(
            visited_lines.contains(&row.line),
            "line {} never visited: {visited_lines:?}",
            row.line
        );
    }
    for (index, (_, line)) in as_rows.iter().enumerate() {
        assert_eq!(*line, rows[index].line, "row {index} has the wrong line");
    }
    // And the columns must be inside the rows they claim to be on.
    for lc in &seen {
        let row = g.row_at(&rows, g.screen_row_of(&rows, *lc).expect("on screen"));
        let row = row.expect("row");
        assert!(
            lc.col >= row.start_col && lc.col <= row.end_col,
            "{lc:?} outside row [{}..{})",
            row.start_col,
            row.end_col
        );
    }
}

#[test]
fn goal_column_survives_a_wrapped_descent_and_return() {
    // Line 0 is wide and wraps; line 2 is wide too. Line 1 is short, so the goal has to be
    // *kept* rather than adopted, and coming back up must restore the column.
    let buf = Buffer::from_str_lossy(
        "alpha bravo charlie delta echo foxtrot\nx\none two three four five six seven\n",
    );
    let g = geom(10, 40, false);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let start = LineCol::new(0, 12);
    let down = g
        .vertical_of(&buf, &rows, start, 1, Some(12))
        .expect("motion");
    assert_eq!(down.line, 1);
    assert_eq!(down.col, 1, "clamped to the short line, goal not adopted");
    let back = g
        .vertical_of(&buf, &rows, down, -1, Some(12))
        .expect("motion");
    assert_eq!(back, start, "the goal column brings the caret home");
}

#[test]
fn a_goal_column_can_land_on_a_wrapped_lines_middle_row() {
    let buf = Buffer::from_str_lossy("alpha bravo charlie delta echo foxtrot golf\nshort\n");
    let g = geom(10, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    // Column 25 on line 0 lives on its third screen row.
    let deep = g.vertical_of(&buf, &rows, LineCol::new(1, 0), -2, Some(25));
    let deep = deep.expect("motion");
    assert_eq!(deep.line, 0);
    let row = g.screen_row_of(&rows, deep).expect("on screen");
    assert!(
        row >= 2,
        "column 25 belongs to a later row of the wrapped line, got row {row}"
    );
}

#[test]
fn up_at_the_first_row_and_down_at_the_last_are_clamped_not_lost() {
    let buf = Buffer::from_str_lossy("one\ntwo\nthree\n");
    let g = geom(80, 40, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let top = g
        .vertical_of(&buf, &rows, LineCol::new(0, 0), -5, Some(0))
        .expect("motion");
    assert_eq!(top, LineCol::new(0, 0));
    let bottom = g
        .vertical_of(&buf, &rows, LineCol::new(0, 0), 50, Some(0))
        .expect("motion");
    assert_eq!(bottom.line, 3, "the document's last line");
}

#[test]
fn vertical_of_agrees_with_a_one_row_per_line_move_when_wrap_is_off() {
    let buf = Buffer::from_str_lossy("aaa\nbb\ncccc\n");
    let g = geom(80, 40, false);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let mut line = 0usize;
    for _ in 0..3 {
        let next = g
            .vertical_of(&buf, &rows, LineCol::new(line, 1), 1, Some(1))
            .expect("motion");
        assert_eq!(next.line, line + 1, "one press, one line");
        line = next.line;
    }
}

// --------------------------------------------------------------- scroll clamp

#[test]
fn a_short_wrapped_document_agrees_with_the_row_counter() {
    // Three wrapped lines plus ropey's trailing empty line.
    let buf = Buffer::from_str_lossy("alpha bravo charlie\ndelta echo foxtrot\ngolf\n");
    assert_eq!(
        buf.line_count(),
        4,
        "three lines plus ropey's trailing line"
    );
    // With room to spare, the counter, the whole-document row list and the painted window are
    // one number. That equality is what the scrollbar thumb and `scroll_to_show` rest on, and
    // it is the invariant that broke when a paragraph ending on a consumed trailing space was
    // counted as owning an extra blank row the screen never painted.
    let tall = geom(10, 40, true);
    let total = total_row_count(&buf, &tall, &[]);
    let every = layout_visible_rows(&buf, &tall, 0, &[]);
    assert_eq!(
        total, 8,
        "3 rows + 3 rows + 1 row + the trailing empty line"
    );
    assert_eq!(
        every.len(),
        total,
        "the counter and the layouter must agree"
    );
    assert_eq!(every.len(), document_rows(&buf, &tall, &[]).len());
    assert!(
        !every.iter().any(|r| r.char_len() == 0 && r.line < 3),
        "a wrapped paragraph must not be followed by a blank row: {:?}",
        rows_dump(&buf, &every)
    );
    // No scroll position, however absurd, blanks the editor.
    for top in [0usize, 1, 2, 3, 8, 99, 10_000] {
        let rows = layout_visible_rows(&buf, &tall, top, &[]);
        assert!(!rows.is_empty(), "scroll top {top} blanked the editor");
    }
    // Scrolled to the end, the window shows the last rows it can: eight rows total minus a
    // viewport of five leaves row offset 3, which is line 1's first row.
    let short = geom(10, 5, true);
    let rows = layout_visible_rows(&buf, &short, 99, &[]);
    assert_eq!(
        rows.iter().map(|r| r.line).collect::<Vec<_>>(),
        vec![1, 1, 1, 2, 3],
        "the last page starts where a viewport of five reaches the document's last row"
    );
    // A viewport of exactly the document's height shows the whole thing.
    let exact = geom(10, 8, true);
    assert_eq!(layout_visible_rows(&buf, &exact, 0, &[]).len(), 8);
    assert_eq!(layout_visible_rows(&buf, &exact, 99, &[]).len(), 8);
}

#[test]
fn a_viewport_that_cannot_hold_a_whole_wrapped_line_paints_no_partial_line() {
    // Line 1 wraps to three rows. In a window with room for only two of them, painting those
    // two would show "delta echo foxtrot" with its tail missing and no way to reach the tail:
    // the layout has no row-list-offset scroll, so those characters would simply be gone.
    let buf = Buffer::from_str_lossy("alpha bravo charlie\ndelta echo foxtrot\ngolf\n");
    // Sanity: at width 10 each of the two long lines wraps to exactly three rows.
    let g = geom(10, 40, true);
    for line in 0..2 {
        let whole = document_rows(&buf, &g, &[])
            .iter()
            .filter(|r| r.line == line)
            .count();
        assert_eq!(whole, 3, "line {line} should wrap to three rows");
    }
    // A viewport of five holds line 0's three rows and two of line 1's three.
    let g = geom(10, 5, true);
    let rows = layout_visible_rows(&buf, &g, 0, &[]);
    let all = document_rows(&buf, &g, &[]);
    let lines: Vec<usize> = rows.iter().map(|r| r.line).collect();
    for line in 0..buf.line_count() {
        let owned = rows.iter().filter(|r| r.line == line).count();
        let whole = all.iter().filter(|r| r.line == line).count();
        assert!(
            owned == 0 || owned == whole,
            "line {line} painted {owned} of {whole} rows: {lines:?}"
        );
    }
    // What is left is exactly the prefix of the document that fits whole.
    assert_eq!(
        lines,
        vec![0, 0, 0],
        "line 1 dropped rather than halved: {lines:?}"
    );
}

#[test]
fn a_line_taller_than_the_viewport_still_paints_its_first_row() {
    // The degenerate case: one line, one row of screen. Clipping is bad; blanking is worse.
    let buf = Buffer::from_str_lossy("alpha bravo charlie delta echo foxtrot golf");
    let rows = layout_visible_rows(&buf, &geom(10, 1, true), 0, &[]);
    assert_eq!(rows.len(), 1, "something must be on screen");
    assert_eq!(rows[0].line, 0);
}

// ----------------------------------------------------------- total row count

#[test]
fn total_row_count_matches_laid_out_rows_for_every_scroll_position() {
    let buf =
        Buffer::from_str_lossy("alpha bravo charlie\ndelta\necho foxtrot golf hotel\n\nindia\n");
    let folds = [collapsed(2, 2)];
    let g = geom(10, 40, true);
    let total = total_row_count(&buf, &g, &folds);
    let mut expected = 0;
    let mut line = 0;
    while line < buf.line_count() {
        expected += 1;
        line = next_visible_line(line, &folds);
    }
    // Line 2 is folded away under a header, and line 0 wraps into 3 rows, line 4 into 4.
    assert!(
        total > expected - 1,
        "total {total} vs visible lines {expected}"
    );
    // The whole-document row count equals what a tall enough viewport lays out.
    let tall = geom(10, 200, true);
    let all = layout_visible_rows(&buf, &tall, 0, &folds);
    assert_eq!(total, all.len(), "the counter and the layouter must agree");
}

#[test]
fn total_row_count_for_a_one_line_document_is_one() {
    let buf = Buffer::from_str_lossy("hello");
    assert_eq!(total_row_count(&buf, &geom(80, 24, false), &[]), 1);
    assert_eq!(total_row_count(&buf, &geom(80, 24, true), &[]), 1);
    assert_eq!(total_row_count(&Buffer::new(), &geom(80, 24, true), &[]), 1);
}

#[test]
fn total_row_count_for_a_10_times_overflow_document_scales_linearly() {
    let line = "x ".repeat(500);
    let buf = Buffer::from_str_lossy(&line);
    assert_eq!(buf.line_len_chars(0), 1000);
    // The count is what the *geometry* says, never what the viewport can show. A 4-row window
    // over a ~13-row document still has ~13 rows of content, and that is what the scrollbar
    // thumb has to be sized against. Capping a line at the viewport height -- which an earlier
    // version did -- makes a single long line report "one screen", the thumb fills the trough,
    // and the caret can be scrolled to rows the thumb insists do not exist.
    let g = geom(80, 4, true);
    assert_eq!(
        total_row_count(&buf, &g, &[]),
        13,
        "viewport size is irrelevant"
    );
    let g6 = geom(80, 6, true);
    assert_eq!(
        total_row_count(&buf, &g6, &[]),
        13,
        "viewport size is irrelevant"
    );
    // The row count depends on the text *width*, not on the viewport.
    let wide = geom(160, 4, true);
    assert_eq!(
        total_row_count(&buf, &wide, &[]),
        7,
        "half as many rows at double width"
    );
    let tall = geom(80, 40, true);
    let total = total_row_count(&buf, &tall, &[]);
    assert!(total > 10 && total < 20, "unexpected row count {total}");
    // And the counter agrees with the row list at every viewport, which is the property the
    // scrollbar thumb and `Document::scroll_to_show` both depend on.
    for h in [1u16, 4, 24, 400] {
        let geom = geom(80, h, true);
        assert_eq!(
            total_row_count(&buf, &geom, &[]),
            document_rows(&buf, &geom, &[]).len(),
            "count and row list disagree at height {h}"
        );
    }
}

// -------------------------------------------------------------- row_end_for

#[test]
fn row_end_for_is_the_same_splitter_the_layout_uses() {
    let buf = Buffer::from_str_lossy("alpha bravo charlie delta echo");
    assert_eq!(buf.line_len_chars(0), 30, "fixture length");
    let width = 12;
    let rows = layout_visible_rows(&buf, &geom(width, 40, true), 0, &[]);
    let mut col = 0;
    for (index, row) in rows.iter().enumerate() {
        let end = row_end_for(&buf, 0, col, 30, width as usize, 4);
        assert_eq!(end, row.end_col, "row {index} disagrees");
        if end >= 30 {
            assert_eq!(
                index + 1,
                rows.len(),
                "the layout stopped where the splitter did"
            );
            break;
        }
        col = end;
    }
}

#[test]
fn row_end_for_always_makes_progress() {
    let buf = Buffer::from_str_lossy("aaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    for width in 1..6u16 {
        let mut col = 0;
        let mut guard = 0;
        while col < 29 && guard < 100 {
            let end = row_end_for(&buf, 0, col, 29, width as usize, 4);
            assert!(end > col, "width {width} stalled at {col}");
            col = end;
            guard += 1;
        }
        assert!(guard < 100, "width {width} did not terminate");
    }
}
