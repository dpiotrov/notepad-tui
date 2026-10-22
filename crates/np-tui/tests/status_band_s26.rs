//! The status row is chrome, not text (S26).
//!
//! The menu bar, the tab bar and the docked service lines all paint a background band through
//! `framework::draw_chrome_band`. The status row was the one strip that never did: it drew its
//! spans with `Style::default()` over whatever was already in the buffer, so the bottom row of
//! the screen was exactly the colour of the text area and read as another line of the file.
//!
//! These tests pin the three properties that matter:
//!
//! 1. the whole status row carries the chrome ground, in every column, not just under glyphs;
//! 2. that ground is the *same* colour the menu and tab rows use -- one shared constant;
//! 3. it is still different from the editor body, so the strip separation from S14 survives.

use np_core::Config;
use np_tui::app::App;

const W: u16 = 100;
const H: u16 = 24;

/// The ground colour a cell carries, as an `(r, g, b)` triple.
fn bg(buf: &ratatui::buffer::Buffer, x: u16, y: u16) -> Option<(u8, u8, u8)> {
    match buf.cell(ratatui::layout::Position { x, y })?.style().bg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    }
}

fn chrome(app: &App) -> (u8, u8, u8) {
    // Derived from the app's theme. The band used to be a single `CHROME_BG` constant, which is
    // precisely what made it unreadable on a light terminal; the colour now comes from the palette
    // the app is running with, so the test asks that palette instead of a literal.
    app.palette().chrome_bg
}

fn with_doc(w: u16, h: u16) -> App {
    let mut app = App::new_headless(Config::default(), (w, h));
    let doc = np_core::Document::from_decoded(
        std::path::PathBuf::from("/tmp/status_band.txt"),
        "hello\nworld\n",
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.documents.clear();
    app.documents.push(doc);
    app.active = 0;
    app
}

/// Premise: the frame really has a status row and an editor row to compare.
///
/// Without this the tests below could pass against a layout that has no status region at all.
#[test]
fn frame_has_a_status_row_below_the_editor() {
    let mut app = with_doc(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert_eq!(
        app.regions.status.h, 1,
        "the status row should be one row tall"
    );
    assert_eq!(
        app.regions.status.bottom(),
        H,
        "the status row should be the last row on screen"
    );
    assert!(
        app.regions.editor.h > 1,
        "the editor should have room to differ"
    );
}

#[test]
fn status_row_is_the_same_colour_as_the_menu_and_tab_rows() {
    let mut app = with_doc(W, H);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);

    let menu_y = app.regions.menu.y;
    let tabs_y = app.regions.tabs.y;
    let status_y = app.regions.status.y;

    assert_eq!(
        bg(&buf, 0, menu_y),
        Some(chrome(&app)),
        "premise: menu is chrome"
    );
    assert_eq!(
        bg(&buf, 0, tabs_y),
        Some(chrome(&app)),
        "premise: tabs is chrome"
    );
    assert_eq!(
        bg(&buf, 0, status_y),
        Some(chrome(&app)),
        "the status row must sit on the same ground as the menu and tab rows"
    );
}

/// The band has to cover the row, not just the glyphs.
///
/// A column past the last segment is the interesting case: drawing text spans with a background
/// style would leave the right-hand tail of the row at the default colour, which is exactly the
/// "half a strip" look the user sees as a broken border.
#[test]
fn every_column_of_the_status_row_is_painted() {
    let mut app = with_doc(W, H);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let y = app.regions.status.y;
    let want = chrome(&app);
    for x in 0..W {
        assert_eq!(
            bg(&buf, x, y),
            Some(want),
            "column {x} of the status row is not the chrome ground"
        );
    }
}

/// Separation from the buffer is the point of the band, so it must not collapse.
#[test]
fn status_row_still_differs_from_the_editor_body() {
    let mut app = with_doc(W, H);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let editor_y = app.regions.editor.y;
    let status_y = app.regions.status.y;
    assert_ne!(
        bg(&buf, 0, status_y),
        bg(&buf, 0, editor_y),
        "if the status row matches the editor ground it stops reading as a strip"
    );
}

/// A status row with nothing to show is still chrome.
///
/// `status_segments` returns nothing when there is no document, and `draw` used to return at
/// that point. Painting the band before the check keeps an empty row looking like a strip
/// instead of a hole in the frame.
#[test]
fn an_empty_status_row_is_still_chrome() {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.documents.clear();
    app.active = 0;
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let y = app.regions.status.y;
    for x in 0..W {
        assert_eq!(
            bg(&buf, x, y),
            Some(chrome(&app)),
            "empty status row column {x} lost its ground"
        );
    }
}

/// Text drawn over the band keeps its own style; only the ground is shared.
///
/// The read-only marker is bold, and the segment text is drawn with the caller's style on
/// purpose so selection and dim rules elsewhere keep working. The band must not eat the glyphs.
#[test]
fn status_text_still_renders_over_the_band() {
    let mut app = with_doc(W, H);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let y = app.regions.status.y;
    let row = np_tui::ui::row_text(&buf, y, 0, W);
    assert!(
        row.contains("status_band.txt"),
        "the document name should still be on the status row, got {row:?}"
    );
    assert!(
        row.contains("Ln 1, Col 1"),
        "the caret readout should still be on the status row, got {row:?}"
    );
}

/// A terminal too narrow for any segment still gets a ground.
///
/// At 30 columns the layout drops the whole run, so this is the other route through the early
/// return, and it also checks the band does not bleed into the editor above.
#[test]
fn narrow_terminal_keeps_the_band_inside_the_status_row() {
    let (w, h) = (30u16, 5u16);
    let mut app = with_doc(w, h);
    let buf = np_tui::ui::render_to_buffer(&mut app, w, h);
    let status_y = app.regions.status.y;
    for x in 0..w {
        assert_eq!(
            bg(&buf, x, status_y),
            Some(chrome(&app)),
            "narrow status row column {x} lost its ground"
        );
    }
    assert_ne!(
        bg(&buf, 0, app.regions.editor.y),
        Some(chrome(&app)),
        "the band must stay inside the status row"
    );
}
