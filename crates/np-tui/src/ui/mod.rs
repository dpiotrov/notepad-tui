//! The four screen regions and the frame that composes them (spec §5.4).
//!
//! Stacking order is fixed by the spec: menu bar, tab bar, editor, status bar. `App`
//! computes the rectangles in `recompute_geometry` and stores them in `App::regions`, so
//! the renderer and S6's hit-testing agree on one set of coordinates rather than each
//! deriving their own and drifting.
//!
//! Dialogs, the palette, the help overlay and context menus draw on top of these and arrive
//! in S14; this module only lays down the base frame.

pub mod chrome;
pub mod context_menu;
pub mod dialog_keys;
pub mod dialogs;
pub mod editor_view;
pub mod file_picker;
pub mod find_bar;
pub mod find_in_files;
pub mod framework;
pub mod gutter;
pub mod help;
pub mod layers;
pub mod layout;
pub mod menu_bar;
pub mod menus;
pub mod palette;
pub mod prefs;
pub mod status_bar;
pub mod tab_bar;
pub mod text_input;
pub mod toast;

use ratatui::buffer::Buffer;

use crate::app::{App, DialogKind, Mode};

/// Draw the whole base frame into `buf`.
///
/// Regions with zero height are skipped rather than drawing a one-row sliver: on a
/// deliberately tiny terminal the layout degrades gracefully, and the snapshot tests rely
/// on that instead of on clamping.
pub fn draw(app: &App, buf: &mut Buffer) {
    if app.regions.menu.h > 0 {
        menu_bar::draw(app, buf);
    }
    if app.regions.tabs.h > 0 {
        tab_bar::draw(app, buf);
    }
    if app.regions.editor.h > 0 {
        editor_view::draw(app, buf);
    }
    if app.regions.status.h > 0 {
        status_bar::draw(app, buf);
    }
    // S14 overlays, highest last. Each one dims or clears what it covers, so the order
    // between them only matters where two can coexist: a context menu over an open dialog
    // cannot happen (the dialog is modal), but a toast can sit on top of anything, so it
    // draws last and stays readable.
    // Find in Files is a docked panel, not an overlay: it belongs to the base frame because
    // it coexists with the editor, and any dialog summoned while it is open draws over it.
    if app.find_panel_height > 0 {
        find_in_files::draw(app, buf);
    }
    // Find and Replace dock in the same place Find in Files does, so they draw with it rather
    // than as an overlay: the rows are reserved out of the text area, and a modal box over the
    // buffer is what the user could not see past.
    if find_bar::is_open(app) {
        find_bar::draw(app, buf);
    }
    draw_overlays(app, buf);

    // The edit range is read by three caches during the frame -- line widths, row counts and
    // highlighting -- so whoever draws clears it once, at the end. Consumers must not clear it
    // themselves: the first one to do so leaves the other two re-walking the whole document,
    // which is the 250 ms per keystroke this replaced.
    if let Some(doc) = app.doc() {
        doc.buffer.clear_dirty_range();
    }
}

/// Render `app` into a ratatui `Buffer` of the given size.
///
/// The single place a frame is produced for tests and for the S4 draw path, so both see
/// identical layout. Geometry is recomputed first: `App` may have been driven by
/// `execute()` at a size it never saw.
pub fn render_to_buffer(app: &mut App, width: u16, height: u16) -> Buffer {
    app.recompute_geometry(width, height);
    let mut buf = Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width,
        height,
    });
    draw(app, &mut buf);
    buf
}

/// Read a single cell's text, for assertions in tests.
pub fn cell_text(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell(ratatui::layout::Position { x, y })
        .map(|c| c.symbol().to_string())
        .unwrap_or_default()
}

/// Read a horizontal run of cells as one string.
pub fn row_text(buf: &Buffer, y: u16, from: u16, to: u16) -> String {
    (from..to).map(|x| cell_text(buf, x, y)).collect()
}

/// The S14 overlay stack (FR-U1, U4, U6, U7, U8, U9, U10).
///
/// Split from [`draw`] so the base frame stays the four-region function the S4..S13 tests
/// describe, and so a reader can see the whole overlay order in one place.
fn draw_overlays(app: &App, buf: &mut Buffer) {
    // The banner strip sits *under the tab bar*, which is where S6 already reserves a row for
    // its hit-testing (`mouse::banner_region`), so the two agree without a shared constant.
    if app.banner.is_some() {
        dialogs::draw_banner(app, buf);
    }
    match app.mode() {
        Mode::Menu { .. } => {
            menu_bar::draw_dropdown(app, buf);
        }
        Mode::Palette => {
            palette::draw_palette(app, buf);
        }
        Mode::HelpOverlay => {
            help::draw(app, buf);
        }
        Mode::ContextMenu(_) => {
            context_menu::draw(app, buf);
        }
        // Find and Replace are docked service lines, drawn with the base frame above; the mode
        // stays a dialog so its keyboard model is untouched.
        Mode::Dialog(DialogKind::Find) | Mode::Dialog(DialogKind::Replace) => {}
        Mode::Dialog(kind) => {
            dialogs::draw_dialog(app, buf, kind);
        }
        Mode::Editing | Mode::FindPanel => {}
    }
    // Toasts last: they must be readable over a dialog, because a config write that fails
    // while Preferences is open is exactly the case where the message matters most.
    if !app.timed_toasts.is_empty() {
        toast::draw(
            buf,
            &app.timed_toasts,
            app.regions.menu.w,
            app.regions.status.y,
            &app.palette(),
        );
    }
}
