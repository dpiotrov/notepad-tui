//! Toasts (FR-U6): bottom-right, 3 s auto-dismiss, errors sticky.
//!
//! # Timing lives here, the queue lives on `App`
//!
//! `ToastQueue` has existed since S3 as a bounded `Vec<Toast>` with no notion of time, and
//! every error path in the app already pushes into it. Replacing it would mean touching
//! forty call sites for no behavioral gain, so instead this module stamps each toast with
//! the `App::clock` value at the moment it was queued and expires it later. The queue stays
//! the single write point; only the reading end learned about time.
//!
//! # Injected clock
//!
//! Expiry is measured against `App::clock`, which tests move with `advance_clock`. A toast
//! that read `Instant::now()` directly could not be tested without sleeping, and a suite
//! that sleeps for three seconds per case is a suite nobody runs.
//!
//! # Stacking and the cap
//!
//! A burst of failures -- a Find in Files run over an unreadable tree produces one per
//! directory -- must not pile into an unreadable wall. `ToastQueue`'s capacity already drops
//! the oldest, and the renderer draws at most [`MAX_VISIBLE`] newest toasts bottom-up, so
//! the screen keeps working rows underneath them. Errors outrank info when the visible
//! window is chosen: a sticky error should never be pushed off screen by later chatty
//! info toasts.

use std::time::{Duration, Instant};

/// How long an info toast stays up (FR-U6).
pub const INFO_TTL: Duration = Duration::from_secs(3);

/// At most this many toasts on screen at once. The queue may hold more; the overflow waits
/// its turn rather than being drawn in a pile.
pub const MAX_VISIBLE: usize = 3;

/// A toast with the clock reading that queued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedToast {
    pub message: String,
    pub error: bool,
    /// `App::clock` at push time.
    pub since: Instant,
}

impl TimedToast {
    /// An info toast.
    pub fn info(message: impl Into<String>, now: Instant) -> Self {
        TimedToast {
            message: message.into(),
            error: false,
            since: now,
        }
    }

    /// An error toast, which never expires on its own.
    pub fn error(message: impl Into<String>, now: Instant) -> Self {
        TimedToast {
            message: message.into(),
            error: true,
            since: now,
        }
    }

    /// Whether an info toast has lived out its TTL. Errors answer `false` forever.
    pub fn expired(&self, now: Instant) -> bool {
        if self.error {
            return false;
        }
        now.saturating_duration_since(self.since) >= INFO_TTL
    }
}

/// The toasts to draw, newest at index 0, capped at [`MAX_VISIBLE`].
///
/// Errors are collected first so a burst of info cannot evict a sticky error from the
/// visible window. Within each class the newest is first, which is what a stack that grows
/// upward reads like.
pub fn visible(items: &[TimedToast]) -> Vec<&TimedToast> {
    let mut errors: Vec<&TimedToast> = items.iter().filter(|t| t.error).collect();
    let mut info: Vec<&TimedToast> = items.iter().filter(|t| !t.error).collect();
    errors.reverse();
    info.reverse();
    let mut out = errors;
    out.extend(info);
    out.truncate(MAX_VISIBLE);
    out
}

/// Drop expired info toasts, keeping every error. Returns how many were removed.
pub fn expire(items: &mut Vec<TimedToast>, now: Instant) -> usize {
    let before = items.len();
    items.retain(|t| !t.expired(now));
    before - items.len()
}

/// The rectangle a toast stack occupies: bottom-right, above the status bar.
///
/// The status row is excluded because the toast must not hide the readout that tells you
/// where the caret is, and because a click on the status bar should hit the status bar.
pub fn stack_rect(screen_w: u16, status_top: u16, count: usize) -> np_core::RectCore {
    let w = TOAST_W.min(screen_w.saturating_sub(2)).max(1);
    let h = (count as u16).saturating_add(1).min(status_top.max(1));
    let y = status_top.saturating_sub(h);
    np_core::RectCore::new(screen_w.saturating_sub(w + 1), y, w, h)
}

/// Preferred toast width in cells. Long messages are truncated rather than wrapped: a
/// notification that grows to six rows stops being a notification.
pub const TOAST_W: u16 = 44;

/// Draw the stack. Bottom row is the newest, so a new toast appears at the bottom edge and
/// older ones ride upward.
pub fn draw(
    buf: &mut ratatui::buffer::Buffer,
    items: &[TimedToast],
    screen_w: u16,
    status_top: u16,
    palette: &np_core::ThemePalette,
) {
    let shown = visible(items);
    if shown.is_empty() {
        return;
    }
    let area = stack_rect(screen_w, status_top, shown.len());
    let layout = crate::ui::framework::DialogLayout::centered(area, area.w, area.h);
    // No title: a toast is a bare bordered strip, and a title would cost a row of the
    // message.
    layout.draw_box(buf, "", palette);
    for (i, toast) in shown.iter().enumerate() {
        // Index 0 is newest and goes at the bottom of the stack.
        let row = layout.inner.h.saturating_sub(1 - i as u16);
        let line = layout.row(row.saturating_sub(1));
        // A toast sits over whatever it is reporting on, so it wears dialog colors: it is a
        // box with a border, and its text has to be legible on that box rather than on the
        // terminal. `Color::LightRed` used to pin one channel to the emulator's palette.
        let style = if toast.error {
            crate::ui::framework::error_style(palette)
        } else {
            crate::style::dialog(palette)
        };
        let text = crate::ui::framework::fit(&toast.message, line.w as usize);
        crate::ui::menu_bar::draw_str(buf, line.x, line.y, &text, style);
    }
}
