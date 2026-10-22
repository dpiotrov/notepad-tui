//! Find and Replace as docked service lines (FR-S1, S19).
//!
//! # Why docked rather than a floating box
//!
//! The Find dialog covered the very text being searched. In a terminal that is worse than on a
//! desktop: there is no room for a box and a useful view of a file at the same time, so opening
//! Find hid the match it had just moved the caret to. Find in Files already solved this by
//! docking its service lines against the bottom of the editor band and taking those rows out of
//! the text area, and the same form works here: the query, the options and the match count sit
//! in a strip, and the file stays visible above it.
//!
//! # The keyboard is unchanged
//!
//! Only the presentation moved. `dialog_keys::find_key` still owns every keystroke, focus is
//! still the framework's `FocusId::Field/Checkbox/Button`, and `Mode::Dialog(Find|Replace)` is
//! still the mode. That is deliberate: the dialog's keyboard model was already field-centric and
//! correct, and rewriting it to match the panel's would have been a second implementation of the
//! same search rather than a change of layout.
//!
//! # Row layout
//!
//! Three rows, the same shape as the Find in Files header so the two service strips read as one
//! kind of thing:
//!
//! ```text
//! Find : needle text                          [4 matches]
//! Repl : replacement text            (Replace mode only)
//! [x] case [ ] word [x] regex [x] wrap [ ] sel | Next Prev Replace All Count
//! ```
//!
//! The option toggles and the actions share the last row because the strip has to stay short
//! enough to leave the editor usable, and because a terminal is character-width budget: the
//! labels are the shortest ones that still say what the toggle does.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use crate::app::{App, DialogKind, Mode};
use crate::ui::framework::{draw_chrome_band, FocusId};
use crate::ui::menu_bar::{draw_str, rect};

/// Rows the strip occupies: query, optional replacement, options.
pub const FIND_ROWS: u16 = 2;
pub const REPLACE_ROWS: u16 = 3;
/// One more row while the query has an error.
///
/// A bad pattern is the one message that has to arrive in full -- "invalid regular expression"
/// compressed to a `!` tells the user something went wrong and nothing about what. The strip
/// grows by a row for as long as the error stands, which is cheaper than the old dialog's
/// permanent spare row.
pub const ERROR_ROWS: u16 = 1;

/// Whether Find/Replace is the strip that is currently docked.
pub fn is_open(app: &App) -> bool {
    matches!(
        app.mode(),
        Mode::Dialog(DialogKind::Find) | Mode::Dialog(DialogKind::Replace)
    )
}

/// How many rows the strip needs right now.
pub fn height(app: &App) -> u16 {
    let base = match app.mode() {
        Mode::Dialog(DialogKind::Replace) => REPLACE_ROWS,
        Mode::Dialog(DialogKind::Find) => FIND_ROWS,
        _ => 0,
    };
    if base > 0 && app.find.error.is_some() {
        base + ERROR_ROWS
    } else {
        base
    }
}

/// Whether this frame draws the error row.
fn showing_error(app: &App) -> bool {
    is_open(app) && app.find.error.is_some()
}

/// The row the option toggles and action words sit on.
///
/// Always the strip's last row, which is what lets the painter, the hit-test and the focus list
/// all find it without each keeping its own copy of "fields, then maybe an error, then options".
pub fn options_row(app: &App) -> u16 {
    strip_rect(app).y + strip_rect(app).h.saturating_sub(1)
}

/// The strip's rectangle: the bottom `height` rows of the editor band.
///
/// Anchored to the band for the same reason the Find in Files panel is -- `regions.editor` is
/// already reduced by the strip, so anchoring there would place the strip below the text and
/// off the bottom of the band.
pub fn strip_rect(app: &App) -> np_core::RectCore {
    let band = app.regions.band;
    let h = height(app).min(band.h);
    np_core::RectCore::new(band.x, band.y + band.h.saturating_sub(h), band.w, h)
}

/// Where the terminal's cursor belongs, or `None` when the strip has no text focus.
///
/// The terminal owns one real cursor and `draw_if_dirty` points it at the document caret. While
/// this strip has the keyboard that is wrong twice over: the caret is somewhere in the file the
/// user is not looking at, and the field they are typing into shows nothing. The report was
/// exactly that -- "when I open Search service lines, cursor dont appear in search string field".
///
/// The position is the end of the focused field's text, clamped to what the strip actually drew.
/// Clamping matters: a query longer than the row is truncated for display, and a cursor past the
/// visible end would sit off the strip or on the next widget.
///
/// Only a `Field` focus produces a cursor. With a toggle or action word focused there is no text
/// position to show, and inventing one would put a blinking block on a word that is not a field.
pub fn cursor_position(app: &App) -> Option<(u16, u16)> {
    let strip = strip_rect(app);
    if strip.w == 0 || strip.h == 0 {
        return None;
    }
    let focused = app.focused_id()?;
    let FocusId::Field(index) = focused else {
        return None;
    };
    let replace = app.find.replace_mode;
    // Row 0 is the query; row 1 is the replacement, and only exists in Replace mode.
    let row = match index {
        0 => 0,
        1 if replace => 1,
        _ => return None,
    };
    let text = if index == 0 {
        &app.find.needle
    } else {
        &app.find.replacement
    };
    // `Find : ` / `Repl : ` -- the label the painter writes, and the reason the cursor cannot
    // simply start at the strip's left edge.
    let label_cols = if index == 0 {
        FIELD_LABEL_CHARS
    } else {
        REPL_LABEL_CHARS
    };
    let want = label_cols + text.chars().count();
    let limit = strip.w.saturating_sub(1) as usize;
    let x = strip.x.saturating_add(want.min(limit) as u16);
    Some((x, strip.y + row))
}

/// Display columns consumed by the query label, `Find : `.
const FIELD_LABEL_CHARS: usize = "Find : ".len();
/// Display columns consumed by the replacement label, `Repl : `.
const REPL_LABEL_CHARS: usize = "Repl : ".len();

/// One clickable word on the options row, with the cell range it occupies.
///
/// The spans come from the same layout call the renderer makes ([`options_spans`]), which is
/// the only way a click can be trusted to hit the word the user read. Counting brackets in a
/// prefix to recover an index goes wrong the moment a label is dropped for width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionHit {
    Checkbox(usize),
    Button(usize),
}

/// The toggle labels as the strip shows them.
///
/// `CHECK_LABELS` is the dialog's list and its names are used as option keys elsewhere, so the
/// strip shortens them here rather than renaming the shared list. "Regular expression" alone
/// took a third of a narrow terminal's width and pushed every action word off the right edge.
pub fn toggle_labels() -> [&'static str; 5] {
    ["case", "word", "regex", "wrap", "in-sel"]
}

/// The action labels as the strip shows them, matching `BUTTON_LABELS` order.
pub fn action_labels() -> [&'static str; 5] {
    ["Next", "Prev", "Replace", "Replace All", "Count"]
}

/// The index `OptionHit::Button` uses for the Close word.
///
/// It is deliberately outside `BUTTON_LABELS`, whose indices `press_find_button` interprets as
/// search commands. Reusing one of those indices would make Close run a search.
pub const CLOSE_INDEX: usize = 100;

/// Whether a button hit index means "close this strip".
pub fn is_close(hit_index: usize) -> bool {
    hit_index == CLOSE_INDEX
}

/// The options row's text and the cell range of each word, relative to the strip's left edge.
///
/// Two spaces separate words, and a pipe separates the toggles from the actions; both are
/// produced here once so the painter and the hit-test cannot drift.
pub fn options_spans(app: &App) -> (String, Vec<(u16, u16, OptionHit)>) {
    let mut spans: Vec<(u16, u16, OptionHit)> = Vec::new();
    let mut col = 0u16;
    let mut text = String::new();

    let flags = [
        app.find.opts.case_sensitive,
        app.find.opts.whole_word,
        app.find.opts.regex,
        app.find.opts.wrap_around,
        app.find.in_selection,
    ];
    let toggle_labels = toggle_labels();
    for (i, label) in toggle_labels.iter().enumerate() {
        let Some(on) = flags.get(i) else {
            continue;
        };
        let word = format!("[{}] {label}", if *on { "x" } else { " " });
        let w = word.chars().count() as u16;
        spans.push((col, w, OptionHit::Checkbox(i)));
        text.push_str(&word);
        text.push_str("  ");
        col = col.saturating_add(w + 2);
    }

    let action_labels = action_labels();
    let action_words: Vec<(usize, &str)> = action_labels
        .iter()
        .enumerate()
        // Replace and Replace All only exist in Replace mode. Drawing them in Find mode invited
        // clicks that had been refused since the dialog existed.
        .filter(|(i, _)| app.find.replace_mode || !matches!(i, 2 | 3))
        .map(|(i, label)| (i, *label))
        .collect();
    if !action_words.is_empty() {
        let sep = " | ";
        text.push_str(sep);
        col = col.saturating_add(sep.len() as u16);
        for (n, (i, label)) in action_words.iter().enumerate() {
            let w = label.chars().count() as u16;
            spans.push((col, w, OptionHit::Button(*i)));
            text.push_str(label);
            let tail = if n + 1 < action_words.len() { "  " } else { "" };
            text.push_str(tail);
            col = col.saturating_add(w + tail.len() as u16);
        }
    }

    // Close, pushed to the right edge.
    //
    // Every search surface needs an obvious way out. `Esc` has always closed these, but `Esc` is
    // invisible: nothing on screen says it, and the report was that the tool areas have no Close.
    // It sits at the far right rather than after the last action so it cannot be mistaken for one
    // more search command, and so it lands in the same place on every strip.
    // `< Close` rather than `[x] Close` or a bare word: the toggles on this same row already use
    // `[ ]`/`[x]`, and a bracketed Close reads as one more checkbox that is somehow always on.
    // The angle bracket matches the `>` that closes a menu submenu, so it reads as "back out".
    let close_word = "< Close";
    let cw = close_word.chars().count() as u16;
    let strip_w = strip_rect(app).w;
    if strip_w > col + cw + 2 {
        let close_col = strip_w.saturating_sub(cw);
        // Pad with spaces so the drawn string and the span agree on where Close starts.
        while (text.chars().count() as u16) < close_col {
            text.push(' ');
        }
        spans.push((close_col, cw, OptionHit::Button(CLOSE_INDEX)));
        text.push_str(close_word);
    }
    (text, spans)
}

/// Which focus target the pointer is over, if any.
///
/// Returns the same `FocusId`s the keyboard cycles through, so a click and a Tab land on the
/// same thing.
pub fn hit_at(app: &App, x: u16, y: u16) -> Option<FocusId> {
    if !is_open(app) {
        return None;
    }
    let strip = strip_rect(app);
    if !strip.contains(x, y) {
        return None;
    }
    let offset = x.saturating_sub(strip.x);
    // Row index measured from the top of the strip, not from `strip.y` alone -- the two differ
    // as soon as the strip is not at the top of the screen, which is always.
    let row = y.saturating_sub(strip.y) as usize;
    let replace = app.find.replace_mode;
    // The rows the fields occupy, then the error row, then the options row last.
    let field_rows = if replace { 2 } else { 1 };
    if row < field_rows {
        // The field owns the whole row, label included: clicking near a field means clicking it.
        return Some(FocusId::Field(row));
    }
    if y != options_row(app) {
        // The error row carries nothing clickable.
        return None;
    }
    let (_, spans) = options_spans(app);
    let (_, _, hit) = spans
        .into_iter()
        .find(|(start, w, _)| offset >= *start && offset < start.saturating_add(*w))?;
    match hit {
        OptionHit::Checkbox(i) => Some(FocusId::Checkbox(i)),
        // Close answers with its own focus identity so the mouse layer can tell it apart from a
        // search command without knowing about CLOSE_INDEX.
        OptionHit::Button(CLOSE_INDEX) => Some(FocusId::CloseStrip),
        OptionHit::Button(i) => Some(FocusId::Button(i)),
    }
}

/// Draw the strip.
pub fn draw(app: &App, buf: &mut Buffer) {
    let strip = strip_rect(app);
    if strip.w < 8 || strip.h == 0 {
        return;
    }
    let area = rect(strip);
    let width = area.width as usize;
    let strip_palette = app.highlight.palette();
    draw_chrome_band(buf, strip, &strip_palette);

    let needle_field = FocusId::Field(0);
    let status = match &app.find.error {
        Some(msg) => format!("! {msg}"),
        None if !app.find.needle.is_empty() => format!("{} match(es)", app.find.marks.len()),
        None => String::new(),
    };
    draw_line(
        buf,
        &FieldRow {
            x: area.x,
            y: area.y,
            width,
            text: format!("Find : {}", app.find.needle),
            status: &status,
            focused: app.focused_id() == Some(needle_field),
            palette: &strip_palette,
        },
    );

    if app.find.replace_mode && strip.h > FIND_ROWS {
        let y = area.y + 1;
        draw_line(
            buf,
            &FieldRow {
                x: area.x,
                y,
                width,
                text: format!("Repl : {}", app.find.replacement),
                status: "",
                focused: app.focused_id() == Some(FocusId::Field(1)),
                palette: &strip_palette,
            },
        );
    }

    if showing_error(app) {
        // Full width, and its own row: see ERROR_ROWS.
        let y = area.y + strip.h.saturating_sub(1 + ERROR_ROWS);
        let msg = app.find.error.clone().unwrap_or_default();
        draw_str(
            buf,
            area.x,
            y,
            &truncate(&format!("! {msg}"), width),
            crate::style::error_on(&strip_palette, strip_palette.chrome_bg)
                .add_modifier(Modifier::BOLD),
        );
    }

    let options_y = options_row(app);
    let (options, _spans) = options_spans(app);
    draw_str(
        buf,
        area.x,
        options_y,
        &truncate(&options, width),
        crate::style::chrome(&strip_palette),
    );
}

/// One field row, with the match count right-aligned in the same row.
///
/// The count used to sit at the bottom of the floating dialog, the last thing visible before
/// the box covered the file. On a strip it belongs beside the query the user just typed, and
/// it is dropped rather than truncated when the strip runs out of room -- a count without its
/// number says nothing.
fn draw_line(buf: &mut Buffer, row: &FieldRow) {
    let palette = row.palette;
    // The strip's ground is the chrome band; text on it must carry the band's ink rather
    // than the terminal's default, which is what made these lines unreadable on a light terminal.
    let mut style = crate::style::chrome(palette);
    if row.focused {
        // Bold alone was the whole focus signal, and it was not enough: on an empty query there is
        // no text to embolden, and bold on a small terminal font is hard to tell from regular.
        // The ground is the same one the Find in Files panel uses, so a focused field looks the
        // same on every strip.
        style = style
            .add_modifier(Modifier::BOLD)
            .bg(crate::style::to_color(palette.field_focus_bg));
        crate::ui::framework::draw_field_focus_ground(
            buf,
            row.x,
            row.y,
            row.x + row.width as u16,
            palette,
        );
    }
    let shown = truncate(&row.text, row.width);
    draw_str(buf, row.x, row.y, &shown, style);
    if row.status.is_empty() {
        return;
    }
    let need = row.status.chars().count() + 1;
    if shown.chars().count() + need <= row.width {
        let sx = row.x + (row.width - need) as u16;
        draw_str(
            buf,
            sx,
            row.y,
            row.status,
            crate::style::chrome_dim(palette),
        );
    }
}

/// A field row and everything the painter needs to draw it.
///
/// Grouped because the positional version reached eight arguments, which is a signature where
/// two `u16`s can be swapped without any type complaining.
struct FieldRow<'a> {
    x: u16,
    y: u16,
    width: usize,
    text: String,
    status: &'a str,
    focused: bool,
    /// The theme palette in force. Carried on the row rather than read from `App` inside
    /// `draw_line`, so the row painter stays a pure function of what it is handed.
    palette: &'a np_core::ThemePalette,
}

/// Clip to the strip width.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}
