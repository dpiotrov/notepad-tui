//! The tab bar: one entry per open document, with markers and a close affordance (FR-U2).
//!
//! Every span comes from [`crate::tabs::tab_spans`]. The renderer deliberately computes
//! nothing itself -- not the label, not the width, not the scroll offset -- because the
//! mouse hit-tester calls the same function, and any layout logic invented here would put
//! clicks on the wrong document as soon as a name was elided or the bar scrolled.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use crate::app::{App, HoverTarget};
use crate::tabs::{self, TabSpan};
use crate::ui::menu_bar::{draw_str, rect};

/// The labels the tab bar will draw, in document order.
///
/// The close button is decided here rather than inside `tab_spans`, because whether a tab is
/// hovered is `App` state and the layout function stays pure. The consequence is that a
/// hovered tab is one cell wider than an unhovered one, which is what moves the tabs to its
/// right -- the same behavior every real tab bar has, and the reason the spans are
/// recomputed per frame rather than cached.
pub fn labels(app: &App) -> Vec<String> {
    app.documents
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let show_close = i == app.active || app.hover == Some(HoverTarget::Tab(i));
            tabs::tab_label(doc, show_close)
        })
        .collect()
}

/// The spans for the current frame. Shared with `mouse::tab_hit_at`.
pub fn spans(app: &App) -> Vec<TabSpan> {
    tabs::tab_spans(
        &labels(app),
        app.regions.tabs.x,
        app.regions.tabs.w,
        app.active,
    )
}

/// The exact text drawn for one span, elided to the span's own width.
///
/// The renderer draws *this* rather than the raw label. A span's width is the width of the
/// elided label, so drawing the unelided label inside it would spill the overflow onto the
/// next tab's cells -- which is exactly what a caller sees as a name cut off mid-word with
/// no ellipsis.
pub fn span_text(app: &App, span: &TabSpan) -> String {
    let label = labels(app).into_iter().nth(span.index).unwrap_or_default();
    let width = span.width as usize;
    if label.chars().count() <= width {
        return label;
    }
    tabs::elide(&label, width)
}

/// Draw the tab row into `App::regions.tabs`.
pub fn draw(app: &App, buf: &mut Buffer) {
    let area = rect(app.regions.tabs);
    if area.width == 0 || area.height == 0 {
        return;
    }
    // A band behind the labels: without it the tab row is the same colour as the buffer and
    // reads as one more line of the file.
    let pal = app.highlight.palette();
    crate::ui::framework::draw_chrome_band(buf, app.regions.tabs, &pal);
    for span in spans(app) {
        let active = span.index == app.active;
        let label = span_text(app, &span);
        let label = label.as_str();
        // FR-U2: the active tab is the obvious one; a hovered tab gets a lighter treatment
        // so a pointer resting on the bar does not look like a selection change.
        // The band behind the labels is already a distinct ground, so the active tab cannot
        // use REVERSED the way it did on the plain buffer: reversed out of the band's grey it
        // came back looking like the band itself, and the row lost the one tab it was meant to
        // point at. Bold plus an underline reads as "this one" against either ground, and stays
        // visibly different from a hover, which is underline only.
        // Every branch carries both colors. A label drawn with `Style::default()` does not
        // merely "leave the band alone" -- it overwrites the band's cell style, so the tab's text
        // reverted to the terminal's defaults and the band showed only behind the gaps.
        //
        // Inactive tabs are dimmed *and* sit on a slightly different ground. Either signal alone
        // can be defeated by a theme whose greys are close together; requiring both means the
        // active tab is identifiable even on a palette where one of the two is indistinct.
        let style = if active {
            crate::style::chrome_active(&pal)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED)
        } else if app.hover == Some(HoverTarget::Tab(span.index)) {
            crate::style::chrome_inactive(&pal).add_modifier(Modifier::UNDERLINED)
        } else {
            crate::style::chrome_inactive(&pal)
        };
        draw_str(buf, span.x, area.y, label, style);
    }
}
