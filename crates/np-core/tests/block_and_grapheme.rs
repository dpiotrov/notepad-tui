//! Block-selection shaping, block paste, and grapheme-cluster ranges (stage S7).
//!
//! These are the pieces spec FR-E3 and FR-E12 rest on: a rectangular copy has to keep its
//! shape, a rectangular paste has to land in the same column, and overtype has to replace
//! one *visible* character rather than one scalar value.

use std::time::Instant;

use unicode_width::UnicodeWidthStr;

use np_core::buffer::BlockRect;
use np_core::{buffer, Buffer, Document, EncodingId, Eol, LineCol, Selection, SelectionKind};

fn buf(text: &str) -> Buffer {
    Buffer::from_str_lossy(text)
}

fn doc(text: &str) -> Document {
    Document::from_decoded(
        std::path::PathBuf::from("/tmp/block-and-grapheme.txt"),
        text,
        EncodingId::Utf8,
        np_core::detect_dominant_eol(text),
        false,
    )
}

fn t() -> Instant {
    Instant::now()
}

// ------------------------------------------------------------- block_text

#[test]
fn block_from_mid_line_copies_a_rectangle() {
    let b = buf("abcdefgh\nABCDEFGH\nabcdefgh\nabcdefgh");
    let rect = BlockRect::new(0, 3, 2, 4, 4);
    assert_eq!(buffer::block_text(&b, rect), "cdef\nCDEF\ncdef");
}

#[test]
fn block_pads_short_lines_with_spaces() {
    let b = buf("abcdefgh\nab\nabcdefgh");
    let got = buffer::block_text(&b, BlockRect::new(0, 3, 2, 4, 4));
    assert_eq!(got, "cdef\n    \ncdef");
    // Equal row widths are the whole point of the padding.
    for row in got.split('\n') {
        assert_eq!(row.chars().count(), 4);
    }
}

#[test]
fn block_beyond_the_last_line_is_clamped() {
    let b = buf("abcdef\nabcdef");
    assert_eq!(
        buffer::block_text(&b, BlockRect::new(0, 99, 2, 4, 4)),
        "cdef\ncdef"
    );
}

#[test]
fn block_past_the_end_of_a_line_yields_padding() {
    let b = buf("ab\nabcdef");
    assert_eq!(
        buffer::block_text(&b, BlockRect::new(0, 2, 4, 2, 4)),
        "  \nef"
    );
}

#[test]
fn zero_width_or_zero_rows_is_empty() {
    let b = buf("abc\ndef");
    assert_eq!(buffer::block_text(&b, BlockRect::new(0, 2, 0, 0, 4)), "");
    assert_eq!(buffer::block_text(&b, BlockRect::new(0, 0, 0, 3, 4)), "");
}

#[test]
fn block_first_line_past_the_end_is_empty() {
    let b = buf("abc");
    assert_eq!(buffer::block_text(&b, BlockRect::new(7, 3, 0, 3, 4)), "");
}

#[test]
fn block_measures_cells_so_cjk_columns_line_up() {
    // "语" is two cells. A 3-cell-wide block from column 1 takes one space plus "语".
    let b = buf("a\u{8bed}xyz\nabcdef");
    let got = buffer::block_text(&b, BlockRect::new(0, 2, 1, 3, 4));
    let rows: Vec<&str> = got.split('\n').collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.width(), 3, "row {row:?} must be exactly 3 cells");
    }
}

#[test]
fn block_cut_on_a_wide_char_boundary_pads_the_half_cell() {
    // Rectangle covers only the right half of a double-width glyph. A half cell is never
    // written, so the row comes back empty and is fully padded.
    let b = buf("\u{8bed}\u{8bed}");
    let got = buffer::block_text(&b, BlockRect::new(0, 1, 1, 2, 4));
    assert_eq!(got, " ");
}

// ------------------------------------------------------------ paste_block

#[test]
fn block_paste_into_a_shorter_line_pads_with_spaces() {
    let mut b = buf("ab\nabcdef\nabcdefgh");
    let after = buffer::paste_block(&mut b, LineCol::new(0, 1), "XY\nZW", 4);
    // Row 0 is shorter than the band's right edge, so the band is padded out to it.
    assert_eq!(b.line_string(0), "aXY");
    assert_eq!(b.line_string(1), "aZWdef");
    assert_eq!(after, LineCol::new(1, 3));
}

#[test]
fn block_paste_overwrites_only_the_column_band() {
    let mut b = buf("abcdefgh\nabcdefgh");
    buffer::paste_block(&mut b, LineCol::new(0, 2), "XY\nZW", 4);
    // The tail after the band survives: a block paste overwrites a band, not a line.
    assert_eq!(b.line_string(0), "abXYefgh");
    assert_eq!(b.line_string(1), "abZWefgh");
}

#[test]
fn block_paste_at_eof_creates_lines() {
    let mut b = buf("one\ntwo");
    let after = buffer::paste_block(&mut b, LineCol::new(1, 0), "AB\nCD\nEF", 4);
    // Row 0 overwrites only the 2-cell band of "two", so the trailing "o" survives: a
    // block paste replaces a band, not a whole line. Rows 1 and 2 have no target line
    // and are appended below it.
    assert_eq!(b.line_string(0), "one");
    assert_eq!(b.line_string(1), "ABo");
    assert_eq!(b.line_string(2), "CD");
    assert_eq!(b.line_string(3), "EF");
    assert_eq!(after, LineCol::new(3, 2));
}

#[test]
fn block_paste_pads_when_the_target_line_is_shorter_than_the_band() {
    let mut b = buf("a\nabcdefg");
    buffer::paste_block(&mut b, LineCol::new(0, 2), "XY\nZW", 4);
    assert_eq!(b.line_string(0), "aXY");
    assert_eq!(b.line_string(1), "aZWdefg");
}

#[test]
fn block_paste_empty_text_changes_nothing() {
    let mut b = buf("abc\ndef");
    let before = b.text_string();
    let at = buffer::paste_block(&mut b, LineCol::new(0, 1), "", 4);
    assert_eq!(b.text_string(), before);
    assert_eq!(at, LineCol::new(0, 1));
}

#[test]
fn copy_then_paste_a_block_round_trips_the_rectangle() {
    // The property that matters: copy a rectangle out of one place and paste it into
    // another, and it occupies the same columns it did on screen.
    let src = buf("01234567\n01234567\n01234567");
    let rect = BlockRect::new(0, 3, 2, 3, 4);
    let text = buffer::block_text(&src, rect);
    let mut dst = buf("aaaaaaaa\nbb\ncccccccc");
    buffer::paste_block(&mut dst, LineCol::new(0, 4), &text, 4);
    assert_eq!(dst.line_string(0), "aaaa234a");
    // "bb" is 2 cells; the band starts at cell 4, so 2 pad spaces then the 3-cell row.
    assert_eq!(dst.line_string(1), "bb234");
    assert_eq!(dst.line_string(2), "cccc234c");
}

// ------------------------------------------------------- Document wiring

#[test]
fn document_block_selection_copy_pads() {
    let mut d = doc("abcdefgh\nab\nabcdefgh");
    d.begin_selection(LineCol::new(0, 2), LineCol::new(2, 6), SelectionKind::Block);
    assert_eq!(d.selected_text().unwrap_or_default(), "cdef\n    \ncdef");
}

#[test]
fn document_block_paste_is_one_undo_unit() {
    let mut d = doc("abcdefgh\nabcdefgh");
    d.cursor.pos = LineCol::new(0, 2);
    d.paste_block("XY\nZW", t()).unwrap_or_default();
    assert_eq!(d.buffer.line_string(0), "abXYefgh");
    assert_eq!(d.buffer.line_string(1), "abZWefgh");
    // One step back restores both lines: that is what "one undo unit" means.
    assert!(d.undo_one());
    assert_eq!(d.buffer.text_string(), "abcdefgh\nabcdefgh");
    assert!(!d.undo_one());
}

#[test]
fn document_block_paste_is_blocked_when_readonly() {
    let mut d = doc("abc");
    d.readonly = true;
    d.cursor.pos = LineCol::new(0, 0);
    assert!(d.paste_block("XY", t()).is_err());
}

#[test]
fn block_rect_normalizes_rows_but_not_columns() {
    let mut d = doc("abcdefgh\nabcdefgh\nabcdefgh");
    // Dragged upward from (2,6) to (0,2).
    d.begin_selection(LineCol::new(2, 6), LineCol::new(0, 2), SelectionKind::Block);
    let rect = d.block_rect(Selection::block(LineCol::new(2, 6), LineCol::new(0, 2)));
    assert_eq!((rect.first_line, rect.rows), (0, 3));
    assert_eq!((rect.col, rect.width), (2, 4));
    assert_eq!(d.selected_text().unwrap_or_default(), "cdef\ncdef\ncdef");
}

#[test]
fn indent_lines_touches_every_line_the_selection_spans() {
    let mut d = doc("one\ntwo\nthree");
    d.begin_selection(LineCol::new(0, 2), LineCol::new(1, 1), SelectionKind::Range);
    d.indent_lines("    ", t()).unwrap_or_default();
    assert_eq!(d.buffer.text_string(), "    one\n    two\nthree");
}

#[test]
fn indent_and_outdent_round_trip_as_one_undo_step() {
    let mut d = doc("one\ntwo\nthree");
    d.select_all();
    d.indent_lines("    ", t()).unwrap_or_default();
    assert_eq!(d.buffer.line_string(2), "    three");
    d.outdent_lines(4, t()).unwrap_or_default();
    assert_eq!(d.buffer.text_string(), "one\ntwo\nthree");
}

#[test]
fn outdent_removes_at_most_one_unit_and_never_content() {
    let mut d = doc("  two spaces\n\ttabbed\nplain");
    d.select_all();
    d.outdent_lines(4, t()).unwrap_or_default();
    assert_eq!(d.buffer.line_string(0), "two spaces");
    assert_eq!(d.buffer.line_string(1), "tabbed");
    assert_eq!(d.buffer.line_string(2), "plain");
}

// ----------------------------------------------------------- graphemes

#[test]
fn grapheme_range_spans_a_combining_sequence() {
    // "e" + U+0301 combining acute is one cluster of two scalars.
    let b = buf("e\u{0301}fg");
    let (s, e) = buffer::grapheme_range(&b, 0, 0).unwrap_or((0, 0));
    assert_eq!((s, e), (0, 2));
    // The next cluster starts where this one ended.
    assert_eq!(buffer::grapheme_range(&b, 0, 2), Some((2, 3)));
}

#[test]
fn grapheme_range_treats_a_cjk_char_as_one_cluster() {
    let b = buf("语hello");
    assert_eq!(buffer::grapheme_range(&b, 0, 0), Some((0, 1)));
}

#[test]
fn grapheme_range_includes_a_flag_emoji() {
    // Regional indicator pair: one cluster, two scalars.
    let b = buf("\u{1F1E6}\u{1F1E7}x");
    assert_eq!(buffer::grapheme_range(&b, 0, 0), Some((0, 2)));
}

#[test]
fn grapheme_range_at_or_past_end_is_none() {
    let b = buf("abc");
    assert_eq!(buffer::grapheme_range(&b, 0, 3), None);
    assert_eq!(buffer::grapheme_range(&b, 0, 99), None);
    // Out-of-range line, not a panic.
    assert_eq!(buffer::grapheme_range(&b, 40, 0), None);
}

#[test]
fn eol_field_is_used_by_the_fixture() {
    // Guards the `doc()` helper: a fixture that dropped EOL info would quietly make
    // every save-shape test below lie.
    let d = doc("a\r\nb\r\n");
    assert_eq!(d.eol, Eol::Crlf);
}
