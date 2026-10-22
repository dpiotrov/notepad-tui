//! Geometry for every dialog, in one place (FR-U4).
//!
//! # Why layouts are their own types
//!
//! A dialog's keyboard handling needs to know where its widgets are -- to route a click, to
//! draw a focus ring, to lay out a button row -- and the renderer needs the same numbers.
//! When those two compute rectangles independently they drift, and the failure is a click
//! that activates the widget to the left of the one under the pointer. So each dialog has a
//! layout type built once from the frame, and both the renderer and the focus list ask it.
//! Every accessor is `&self` and derived, so a resize is just a rebuild.
//!
//! # The shared shape
//!
//! [`Frame`] is the common part: a [`DialogLayout`] plus the interior row helper. The per-
//! dialog types wrap it and add the specifics (a checkbox grid, a button row, a list area).
//! That keeps "centered box, one row per control" written once while letting each dialog
//! state its own row count, which is the only part that genuinely differs.

use np_core::RectCore;

use crate::app::App;
use crate::ui::framework::{button_row, DialogLayout};

/// A dialog box plus its interior rows.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub layout: DialogLayout,
}

impl Frame {
    /// A box of the requested size centered over `app`'s frame.
    pub fn new(app: &App, w: u16, h: u16) -> Frame {
        Frame {
            layout: DialogLayout::for_app(app, w, h),
        }
    }

    /// Interior row `i`, full interior width.
    pub fn row(&self, i: u16) -> RectCore {
        self.layout.row(i)
    }

    /// Interior rows `i..i+n` as one region.
    pub fn rows(&self, i: u16, n: u16) -> RectCore {
        RectCore::new(
            self.layout.inner.x,
            self.layout.inner.y + i,
            self.layout.inner.w,
            n,
        )
    }

    /// The list area: from interior row `i` to the bottom of the interior.
    pub fn list_from(&self, i: u16) -> RectCore {
        let y = self.layout.inner.y.saturating_add(i);
        let h = self.layout.inner.bottom().saturating_sub(y);
        RectCore::new(self.layout.inner.x, y, self.layout.inner.w, h)
    }
}

/// The Open / Save As picker (FR-F2, FR-U5).
///
/// Rows: path field, then the listing, then `[Open] [Hidden]`. The listing gets whatever is
/// left, which is why its height is derived rather than declared.
#[derive(Debug, Clone, Copy)]
pub struct FilePicker {
    pub frame: Frame,
}

impl FilePicker {
    pub fn new(app: &App) -> FilePicker {
        // Field + separator + listing + buttons + error line.
        let h = 16u16.min(app.regions.status.bottom().max(6));
        FilePicker {
            frame: Frame::new(app, 62, h),
        }
    }

    /// The path field row.
    pub fn field_row(&self) -> RectCore {
        self.frame.row(0)
    }

    /// The listing region, between the field and the button row.
    pub fn list_area(&self) -> RectCore {
        // One row of gap under the field, two rows (buttons + error) above the bottom.
        let top = self.frame.layout.inner.y + 1;
        let bottom = self.frame.layout.inner.bottom().saturating_sub(2);
        RectCore::new(
            self.frame.layout.inner.x,
            top,
            self.frame.layout.inner.w,
            bottom.saturating_sub(top),
        )
    }

    /// The button row.
    pub fn buttons_row(&self) -> RectCore {
        self.frame.row(self.frame.layout.inner.h.saturating_sub(2))
    }

    /// Button `i`'s rectangle. `i` beyond the button count lands on the row's right edge
    /// with zero width rather than panicking, because the focus list asks for a fixed count.
    pub fn button_row(&self, i: u16) -> RectCore {
        let rects = button_row(self.buttons_row(), &["Open", "Hidden"]);
        rects
            .get(i as usize)
            .map(|(r, _)| *r)
            .unwrap_or(RectCore::new(0, 0, 0, 0))
    }

    /// The error line under the buttons.
    pub fn error_row(&self) -> RectCore {
        self.frame.row(self.frame.layout.inner.h.saturating_sub(1))
    }
}

/// Find / Replace (FR-S1).
#[derive(Debug, Clone)]
pub struct FindDialog {
    pub frame: Frame,
    pub replace_mode: bool,
    /// Rectangles of the five option checkboxes, in display order.
    pub checkboxes: [RectCore; 5],
    /// Rectangles of the action buttons, in display order.
    pub buttons: Vec<RectCore>,
}

/// The checkbox labels, in the order the dialog shows them.
pub const CHECK_LABELS: [&str; 5] = [
    "Match case",
    "Whole word",
    "Regular expression",
    "Wrap around",
    "In selection",
];

/// The button labels, in display order.
pub const BUTTON_LABELS: [&str; 5] = ["Find Next", "Find Prev", "Replace", "Replace All", "Count"];

impl FindDialog {
    pub fn new(app: &App) -> FindDialog {
        let replace = app.find.replace_mode;
        let h = if replace { 12 } else { 11 };
        let frame = Frame::new(app, 58, h);
        // Two checkboxes per row keeps the dialog short, matching the pre-S14 layout so the
        // existing snapshot expectations still describe the same screen.
        let base = if replace { 2 } else { 1 };
        let mut checkboxes = [RectCore::new(0, 0, 0, 0); 5];
        for (i, slot) in checkboxes.iter_mut().enumerate() {
            let pair = (i / 2) as u16;
            let col = (i % 2) as u16;
            let row = frame.row(base + pair);
            let half = frame.layout.inner.w / 2;
            *slot = RectCore::new(row.x + col * half, row.y, half, 1);
        }
        let buttons_row = frame.row(base + 3);
        let buttons: Vec<RectCore> = button_row(buttons_row, &BUTTON_LABELS)
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        FindDialog {
            frame,
            replace_mode: replace,
            checkboxes,
            buttons,
        }
    }

    /// The row a numbered checkbox occupies.
    pub fn checkbox_rows(&self) -> Vec<RectCore> {
        self.checkboxes.to_vec()
    }

    /// The button rectangles.
    pub fn button_rects(&self) -> Vec<RectCore> {
        self.buttons.clone()
    }

    /// Which button `Enter` runs: Replace in Replace mode, Find Next otherwise.
    pub fn default_button(&self) -> usize {
        if self.replace_mode {
            2
        } else {
            0
        }
    }

    /// The Find field row.
    pub fn find_field(&self) -> RectCore {
        self.frame.row(0)
    }

    /// The Replace field row.
    pub fn replace_field(&self) -> RectCore {
        self.frame.row(1)
    }

    /// The status/error line at the bottom.
    pub fn status_row(&self) -> RectCore {
        self.frame.row(self.frame.layout.inner.h.saturating_sub(1))
    }
}

/// Go To Line (FR-S6).
#[derive(Debug, Clone, Copy)]
pub struct GoTo {
    pub frame: Frame,
    buttons: [RectCore; 1],
}

impl GoTo {
    pub fn new(app: &App) -> GoTo {
        let frame = Frame::new(app, 42, 6);
        let buttons = button_row(frame.row(3), &["Go"])
            .into_iter()
            .map(|(r, _)| r)
            .collect::<Vec<_>>();
        let first = buttons
            .first()
            .copied()
            .unwrap_or(RectCore::new(0, 0, 0, 0));
        GoTo {
            frame,
            buttons: [first],
        }
    }

    /// The field row.
    pub fn field_row(&self) -> RectCore {
        self.frame.row(1)
    }

    /// Button `i`.
    pub fn button_row(&self, i: u16) -> RectCore {
        self.buttons
            .get(i as usize)
            .copied()
            .unwrap_or(RectCore::new(0, 0, 0, 0))
    }

    /// The inline error row.
    pub fn error_row(&self) -> RectCore {
        self.frame.row(4)
    }
}

/// About (FR-U9).
#[derive(Debug, Clone, Copy)]
pub struct About {
    pub frame: Frame,
    buttons: [RectCore; 1],
}

impl About {
    /// Rows of body text the About dialog renders.
    ///
    /// Derived from `about_lines` rather than written down, because the two were hardcoded
    /// independently and drifted: the frame reserved 5 body rows while `about_lines` grew to 6, so
    /// the last line drew straight through the OK button. The dialog and its geometry now share one
    /// source of truth, and adding a line to the dialog moves the button instead of burying it.
    fn body_rows() -> u16 {
        crate::ui::help::about_row_count()
    }

    pub fn new(app: &App) -> About {
        let rows = Self::body_rows();
        // The interior is height minus the two borders: body rows, one blank, the button row.
        let frame = Frame::new(app, 46, rows + 4);
        let buttons = button_row(frame.row(rows + 1), &["OK"])
            .into_iter()
            .map(|(r, _)| r)
            .collect::<Vec<_>>();
        let first = buttons
            .first()
            .copied()
            .unwrap_or(RectCore::new(0, 0, 0, 0));
        About {
            frame,
            buttons: [first],
        }
    }

    /// Button `i`.
    pub fn button_row(&self, i: u16) -> RectCore {
        self.buttons
            .get(i as usize)
            .copied()
            .unwrap_or(RectCore::new(0, 0, 0, 0))
    }

    /// The text block.
    pub fn body(&self) -> RectCore {
        self.frame.rows(0, Self::body_rows())
    }
}

/// The save-changes and binary confirmations (FR-F5, FR-N5).
#[derive(Debug, Clone)]
pub struct Confirm {
    pub frame: Frame,
    buttons: Vec<RectCore>,
}

impl Confirm {
    pub fn new(app: &App) -> Confirm {
        let labels: Vec<&str> = app
            .confirm
            .as_ref()
            .map(|c| c.buttons.iter().map(|b| b.label()).collect())
            .unwrap_or_else(|| vec!["OK"]);
        let width = labels.iter().map(|l| l.chars().count() + 2).sum::<usize>() + labels.len() + 4;
        let frame = Frame::new(app, width.max(36) as u16, 6);
        let buttons = button_row(frame.row(2), &labels)
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        Confirm { frame, buttons }
    }

    /// The prompt row.
    pub fn prompt_row(&self) -> RectCore {
        self.frame.row(0)
    }

    /// The button rectangles.
    pub fn button_rects(&self) -> Vec<RectCore> {
        self.buttons.clone()
    }
}

/// The Document List and the filtered Language / Theme pickers (FR-U4, FR-H2, FR-H3).
#[derive(Debug, Clone, Copy)]
pub struct Picker {
    pub frame: Frame,
    /// Rows consumed above the list (a filter line, or just a gap).
    pub header_rows: u16,
}

impl Picker {
    /// `count` is the number of rows that will be listed; it only sizes the box.
    pub fn sized(app: &App, count: usize, want_w: u16) -> Picker {
        let screen_h = app.regions.status.bottom();
        let wanted = count.saturating_add(3) as u16;
        let h = wanted.min(screen_h * 2 / 3).max(5);
        Picker {
            frame: Frame::new(app, want_w, h),
            header_rows: 1,
        }
    }

    /// The filter field row, for the pickers that have one.
    pub fn field_row(&self) -> RectCore {
        self.frame.row(0)
    }

    /// The list region.
    pub fn list_area(&self) -> RectCore {
        self.frame.list_from(self.header_rows)
    }
}

/// A flat choice list: Encoding, EOL, Recent Files.
#[derive(Debug, Clone, Copy)]
pub struct Choice {
    pub picker: Picker,
}

impl Choice {
    pub fn new(app: &App, count: usize) -> Choice {
        Choice {
            picker: Picker::sized(app, count, 40),
        }
    }

    /// The list region.
    pub fn list_area(&self) -> RectCore {
        // No filter line on a choice list, so the list starts at row 0.
        self.picker.frame.list_from(0)
    }
}
