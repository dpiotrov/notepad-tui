//! The help overlay and the About dialog (FR-U9).
//!
//! # The cheatsheet is generated, never written
//!
//! A hand-maintained key reference is wrong the first time someone remaps a binding, and it
//! is wrong silently. So every line here comes from `app.keymap` -- the live table, after
//! `[keymap]` overrides have been applied -- paired with `Action::label()`. If the overlay
//! says `Ctrl+S  Save`, that is because the keymap actually resolves that chord to that
//! action right now. The test in `tests/help_s14.rs` asserts against
//! `keymap::default_bindings()` for exactly this reason: the document, the overlay and the
//! binding table cannot diverge without a test failing.
//!
//! # Grouping
//!
//! Lines are grouped by the menu each action belongs to, because "where is this command" is
//! the question a cheatsheet answers, and the menu is the answer a user can act on. Actions
//! with no menu entry land in a final "Other" group rather than being dropped -- an orphaned
//! binding with no visible command is precisely the thing a cheatsheet exists to reveal.

use crate::actions::MenuId;
use crate::app::App;

/// One rendered line of the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The key chord, or empty for a section header.
    pub key: String,
    /// The command's label, or the section title.
    pub text: String,
    pub header: bool,
}

impl Line {
    /// Render as `"key   text"`, with the chord in a fixed-width column.
    pub fn render(&self) -> String {
        if self.header {
            format!("{}:", self.text)
        } else {
            format!("{:<16}{}", self.key, self.text)
        }
    }
}

/// The whole cheatsheet, in display order.
pub fn help_lines(app: &App) -> Vec<Line> {
    let mut out = Vec::new();
    // Group the actions the menu registry knows about, in bar order.
    for menu in MenuId::ALL {
        let items = crate::ui::menus::menu_items(app, menu);
        let rows: Vec<(String, String)> = items
            .iter()
            .filter(|it| it.action.is_some() && !it.hint.is_empty())
            .filter_map(|it| {
                let action = it.action?;
                if is_encoding_payload(&hint_of(it)) {
                    return None;
                }
                Some((it.hint.clone(), action.label().to_string()))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(Line {
            key: String::new(),
            text: menu.title().to_string(),
            header: true,
        });
        for (key, text) in rows {
            out.push(Line {
                key,
                text,
                header: false,
            });
        }
    }
    // Every remaining chord the keymap holds, so nothing bound is absent. The filter is
    // "this exact (chord, action) pair is not already on screen", not "this action has no
    // menu entry": an action with two chords -- Ctrl+Y and Ctrl+Shift+Z for Redo, or the
    // nine Alt+<letter> menu accelerators -- would otherwise show one and hide the other,
    // and a hidden binding in a key reference is exactly the staleness this dialog exists
    // to prevent.
    let mut shown: Vec<(String, String)> = out
        .iter()
        .filter(|l| !l.header)
        .map(|l| (l.key.clone(), l.text.clone()))
        .collect();
    let mut other: Vec<(String, String)> = app
        .keymap
        .table()
        .into_iter()
        .filter(|(key, a)| {
            let pair = (key.clone(), a.label().to_string());
            !shown.contains(&pair)
        })
        .map(|(key, a)| (key, a.label().to_string()))
        .collect();
    other.sort();
    other.dedup();
    shown.extend(other.iter().cloned());
    if !other.is_empty() {
        out.push(Line {
            key: String::new(),
            text: "Other".to_string(),
            header: true,
        });
        for (key, text) in other {
            out.push(Line {
                key,
                text,
                header: false,
            });
        }
    }
    out
}

fn hint_of(item: &crate::ui::menus::MenuItem) -> String {
    item.hint.clone()
}

/// The Encoding lists stash a codec index in `hint`; it is not a chord and must not appear
/// in a key reference.
fn is_encoding_payload(hint: &str) -> bool {
    !hint.is_empty() && hint.chars().all(|c| c.is_ascii_digit())
}

/// Draw the overlay: a scrollable cheatsheet over a dimmed backdrop.
pub fn draw(app: &App, buf: &mut ratatui::buffer::Buffer) {
    use crate::ui::framework::{dim_frame, draw_hint, DialogLayout};
    use np_core::RectCore;
    dim_frame(buf, app);
    let pal = app.palette();
    let screen_h = app.regions.status.bottom();
    let screen_w = app.regions.menu.w;
    let h = screen_h.saturating_sub(2).max(3);
    let w = 64u16.min(screen_w.saturating_sub(2).max(10));
    let area = RectCore::new(
        screen_w.saturating_sub(w) / 2,
        screen_h.saturating_sub(h) / 2,
        w,
        h,
    );
    let layout = DialogLayout::centered(area, w, h);
    layout.draw_box(buf, "Keyboard Help", &pal);
    let lines = help_lines(app);
    let visible = layout.inner.h.saturating_sub(1) as usize;
    let start = app.help_scroll.min(lines.len().saturating_sub(1));
    for (i, line) in lines.iter().skip(start).take(visible).enumerate() {
        let row = layout.row(i as u16);
        let text = line.render();
        if line.header {
            crate::ui::menu_bar::draw_str(
                buf,
                row.x,
                row.y,
                &crate::ui::framework::fit(&text, row.w as usize),
                crate::ui::framework::title_style(&pal),
            );
        } else {
            crate::ui::menu_bar::draw_str(
                buf,
                row.x,
                row.y,
                &crate::ui::framework::fit(&text, row.w as usize),
                crate::style::dialog(&pal),
            );
        }
    }
    // A scroll hint on the last interior row, so it is visible whenever there is more to see.
    if lines.len() > visible {
        let hint = format!(
            "-- {} of {} -- arrows scroll, Esc closes",
            start + 1,
            lines.len()
        );
        let row = layout.row(layout.inner.h.saturating_sub(1));
        draw_hint(buf, row, &hint, &pal);
    }
}

/// The About dialog's lines: name, version, terminal size (FR-U9).
///
/// The count of these lines is load-bearing for [`crate::ui::layout::About`], which sizes the
/// frame and places the OK button from it: a line added here without a matching frame row lands on
/// top of the button, which is exactly the bug this comment exists to prevent. The layout derives
/// its height from `about_lines().len()` rather than hardcoding a number, so the two cannot drift
/// apart again -- but that only holds if the dialog keeps calling this one function for both jobs.
pub fn about_lines(app: &App) -> Vec<String> {
    let (w, h) = app.frame_size();
    let mut lines = about_banner();
    lines.push(format!("Terminal: {w} x {h}"));
    lines.push(format!("Documents open: {}", app.documents.len()));
    lines.push(format!("Theme: {}", app.config.ui.theme));
    lines
}

/// The fixed identity block at the top of the About dialog: name, version, tagline, authorship,
/// license, and a blank separator.
///
/// Split from [`about_lines`] because the dialog's geometry needs the line count without an `App`
/// in hand, and because the count must not depend on session state: the three dynamic lines below
/// are always exactly three, whatever their values. So `about_banner().len() + 3` is stable, and a
/// line added here moves the OK button rather than overlapping it.
pub fn about_banner() -> Vec<String> {
    vec![
        format!("Notepad-TUI {}", env!("CARGO_PKG_VERSION")),
        "A text editor for the terminal".to_string(),
        String::new(),
        "Created by Dpiotrov and Inorganic Beings".to_string(),
        "MIT License".to_string(),
        String::new(),
    ]
}

/// Total rows the About dialog draws. One source of truth for the frame height and the button row.
pub fn about_row_count() -> u16 {
    // The three dynamic lines in `about_lines` are always three; see `about_banner`.
    (about_banner().len() + 3) as u16
}

/// Draw the About dialog.
pub fn draw_about(app: &App, buf: &mut ratatui::buffer::Buffer) {
    use crate::ui::framework::{dim_frame, FocusId};
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = crate::ui::dialogs::about_layout(app);
    layout.frame.layout.draw_box(buf, "About", &pal);
    let lines = about_lines(app);
    for (i, line) in lines.iter().enumerate() {
        let row = layout.frame.row(i as u16);
        let style = if i == 0 {
            crate::ui::framework::title_style(&pal)
        } else {
            crate::style::dialog(&pal)
        };
        crate::ui::menu_bar::draw_str(
            buf,
            row.x,
            row.y,
            &crate::ui::framework::fit(line, row.w as usize),
            style,
        );
    }
    // The OK button, focused since it is the dialog's only child and the default action.
    let focused = app.focused_id() == Some(FocusId::Button(0));
    let btn = layout.button_row(0);
    crate::ui::framework::draw_button(
        buf,
        btn,
        "OK",
        if focused {
            crate::ui::framework::ButtonState::Focused
        } else {
            crate::ui::framework::ButtonState::Default
        },
        &pal,
    );
}
