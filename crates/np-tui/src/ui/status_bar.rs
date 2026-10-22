//! Status bar: the complete M1 readout (spec §6.7, FR-U4).
//!
//! Format, left to right:
//!
//! ```text
//!  path ● [lock] | Ln L, Col C, Sel N | LF | UTF-8 | Lang | INS/OVR | chars / lines
//! ```
//!
//! # Segment offsets are shared with hit-testing
//!
//! `status_segments` produces the labeled spans and is the only place this row's text is
//! assembled. `mouse::status_segment` asks it which segment a click landed in, so a click on
//! the encoding opens the encoding menu rather than whatever happens to sit under a
//! hand-counted column. Any change to the layout is automatically correct on both sides.
//!
//! # Counts come from the rope
//!
//! Character and line counts go through `Buffer::char_count()` and `Buffer::line_count()`.
//! `text_string()` would materialize the whole document on every frame, which on a 100 MB
//! file is a frame-time allocation the size of the file, for a number the rope already knows.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use np_core::SelectionKind;

use crate::app::App;
use crate::mouse::StatusSegment;
use crate::ui::menu_bar::{draw_str, rect};

/// One labeled segment of the status row, with its horizontal span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSpan {
    pub segment: StatusSegment,
    pub text: String,
    /// Screen column where the text starts.
    pub x: u16,
    /// Width in cells.
    pub width: u16,
}

/// The status row as segments, laid out left-aligned from `area.x`.
///
/// Segments are joined by `" | "`, and that separator belongs to the row rather than to any
/// segment, so the hit-test boundaries include the separator with the segment on its left.
/// That is the convention that makes "click near the encoding" behave predictably.
pub fn status_segments(app: &App) -> Vec<StatusSpan> {
    let Some(doc) = app.doc() else {
        return Vec::new();
    };
    let area = app.regions.status;
    let pos = doc.cursor.pos;

    // Left: the document name, a modified dot, and a lock when the file is read-only.
    let mut name = doc.display_name();
    if doc.modified {
        name.push_str(" \u{25cf}");
    }
    if doc.readonly {
        name.push_str(" [lock]");
    }

    // Position. `Sel N` appears only with a selection, so the common case stays narrow.
    let sel_count = doc
        .selection
        .filter(|s| !s.is_empty())
        .map(|s| selection_cell_count(doc, &s))
        .unwrap_or(0);
    let position = if sel_count > 0 {
        format!(
            "Ln {}, Col {}, Sel {}",
            pos.line + 1,
            pos.col + 1,
            sel_count
        )
    } else {
        format!("Ln {}, Col {}", pos.line + 1, pos.col + 1)
    };

    let eol = doc.eol.label().to_string();
    let encoding = doc.encoding.label().to_string();
    // Highlighting is S12; until then the language is whatever the file extension implied.
    let language = match &doc.language {
        np_core::LanguageId::Named(n) => n.clone(),
        np_core::LanguageId::Auto => "None".to_string(),
    };
    let overtype = if doc.overtype { "OVR" } else { "INS" };
    // Rope-derived, never `text_string()` (see the module docs).
    let counts = format!("{} / {}", doc.buffer.char_count(), doc.buffer.line_count());

    let parts: Vec<(StatusSegment, String)> = vec![
        (StatusSegment::Document, name),
        (StatusSegment::Position, position),
        (StatusSegment::Eol, eol),
        (StatusSegment::Encoding, encoding),
        (StatusSegment::Language, language),
        (StatusSegment::Overtype, overtype.to_string()),
        (StatusSegment::Counts, counts),
    ];

    layout(area.x, area.w, parts)
}

/// Place the segments on one row, and produce the spans the hit-tester reads.
///
/// # The row is laid out once, and the renderer draws what this returns
///
/// An earlier version laid the segments out from the left here and had `draw` re-join the
/// strings and right-align the tail when the row overflowed. At 80 columns the two agreed by
/// luck; at 40 they did not, and the result was a click on the visible `INS` landing on
/// nothing while the real `INS` span sat off-screen. So this function owns the positions,
/// takes the available width, and `draw` paints exactly these spans at exactly these `x`s.
/// There is no second layout to disagree with.
///
/// # Narrow-width rule
///
/// Segments fill the row from the right, newest-information-first: the caret position, EOL,
/// encoding, language, overtype and counts all stay readable, and the document name -- the
/// one thing the tab bar already shows -- is what gives way. `..` order is preserved, spans
/// never overlap, and every segment that is drawn is inside `width`.
fn layout(origin: u16, width: u16, parts: Vec<(StatusSegment, String)>) -> Vec<StatusSpan> {
    let sep = 3u16; // " | "
    let measured: Vec<(StatusSegment, String, u16)> = parts
        .into_iter()
        .map(|(segment, text)| {
            let w = text.chars().count() as u16;
            (segment, text, w)
        })
        .collect();

    // Walk from the right, keeping every segment that fits. The separator in front of a
    // kept segment costs 3 cells unless it is the leftmost kept one.
    let mut keep: Vec<(usize, u16)> = Vec::with_capacity(measured.len());
    let mut used = 0u16;
    for i in (0..measured.len()).rev() {
        let w = measured[i].2;
        let cost = if keep.is_empty() { w } else { w + sep };
        if used.saturating_add(cost) > width {
            break;
        }
        used = used.saturating_add(cost);
        keep.push((i, w));
    }
    keep.reverse();
    if keep.is_empty() {
        // Not even one segment fits. Report that nothing is clickable rather than laying
        // out a row that draws nothing.
        return Vec::new();
    }

    // Now assign positions left to right, starting where the kept run begins.
    let mut spans = Vec::with_capacity(keep.len());
    let mut x = origin.saturating_add(width.saturating_sub(used));
    for (n, (i, w)) in keep.iter().enumerate() {
        if n > 0 {
            x = x.saturating_add(sep);
        }
        let (segment, text, _) = &measured[*i];
        spans.push(StatusSpan {
            segment: *segment,
            text: text.clone(),
            x,
            width: *w,
        });
        x = x.saturating_add(*w);
    }
    spans
}

/// Characters covered by the selection, counted the way the user counts them.
///
/// A block selection covers a rectangle, so its "size" is the rectangle's character count
/// including the padding the copy would produce, which is what `selected_text` already
/// returns. A range selection is the plain span.
fn selection_cell_count(doc: &np_core::Document, sel: &np_core::Selection) -> usize {
    match sel.kind {
        SelectionKind::Range => {
            let (s, e) = sel.bounds();
            let a = doc.buffer.char_offset(s).unwrap_or(0);
            let b = doc.buffer.char_offset(e).unwrap_or(0);
            b.saturating_sub(a)
        }
        SelectionKind::Block => doc.selected_text().map(|t| t.chars().count()).unwrap_or(0),
    }
}

/// Draw the status row into `App::regions.status`.
pub fn draw(app: &App, buf: &mut Buffer) {
    let area = rect(app.regions.status);
    // The ground first, and before anything can bail out. This row sits against the buffer with
    // no border of its own, so without the band it is exactly the colour of the text area and
    // reads as another line of the file -- the same complaint already settled for the menu bar,
    // the tab bar and the docked service lines. Painting it here rather than in `ui::draw`
    // keeps the strip and its ground in one function, and painting it before the empty-segment
    // return means an untitled-with-no-document row still looks like chrome.
    let pal = app.highlight.palette();
    crate::ui::framework::draw_chrome_band(buf, app.regions.status, &pal);
    let spans = status_segments(app);
    if spans.is_empty() {
        return;
    }
    // The read-only marker gets emphasis: it is the one piece of information that changes
    // what a keystroke will do.
    // Explicit ink, for the reason recorded on the tab bar: drawing over the band with
    // `Style::default()` replaces the band's colors rather than blending with them, which is how
    // the status row ended up with the terminal's foreground on the app's dark ground.
    let base = if app.doc().is_some_and(|d| d.readonly) {
        crate::style::chrome(&pal).add_modifier(Modifier::BOLD)
    } else {
        crate::style::chrome(&pal)
    };
    let limit = area.x.saturating_add(area.width);
    for (i, span) in spans.iter().enumerate() {
        if span.x >= limit {
            continue;
        }
        if i > 0 {
            // The separator belongs to the gap the layout left in front of this span.
            draw_str(buf, span.x.saturating_sub(3), area.y, " | ", base);
        }
        draw_str(buf, span.x, area.y, &span.text, base);
    }
}
