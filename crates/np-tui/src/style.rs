//! The one and only style conversion (contract §7).
//!
//! `np-core` describes colors with `CoreStyle` because it must not depend on ratatui.
//! Everything that reaches the screen goes through this file, which is what makes "why did
//! that render the wrong color" a single-file investigation instead of a scavenger hunt
//! across widgets.
//!
//! # Why a trait instead of `impl From<CoreStyle> for Style`
//!
//! Contract §7 asks for `impl From<CoreStyle> for ratatui::style::Style` here. That impl is
//! not legal Rust: the orphan rule permits `From<Foreign> for Local` or `From<Local> for
//! Foreign`, and inside `np-tui` *both* `CoreStyle` (defined in np-core) and `Style`
//! (defined in ratatui) are foreign. The only ways to satisfy the letter of the contract
//! are to make np-core depend on ratatui, which the purity gate forbids, or to put the impl
//! in ratatui itself. So the conversion lives on a trait local to this crate, which keeps
//! exactly the property the contract is actually protecting: one place where a core style
//! becomes a terminal style. See `docs/DEVIATIONS.md`.
//!
//! # Index versus RGB
//!
//! A palette index is passed through as `Color::Indexed` rather than expanded to RGB.
//! Collapsing it looks the same on a truecolor terminal and wrong on a 16-color one, where
//! the emulator's own palette is the only correct source. RGB is likewise never downgraded
//! to an index.

use np_core::style::{CoreColor, CoreStyle};
use ratatui::style::{Color, Modifier, Style};

/// Resolve a core color, preferring the more specific representation when a caller
/// somehow supplied both.
fn core_color(c: CoreColor) -> Option<Color> {
    if let Some((r, g, b)) = c.rgb {
        return Some(Color::Rgb(r, g, b));
    }
    c.indexed.map(Color::Indexed)
}

/// Conversion to ratatui's color type.
pub trait ToRatatuiColor {
    fn to_ratatui_color(&self) -> Color;
}

impl ToRatatuiColor for CoreColor {
    fn to_ratatui_color(&self) -> Color {
        // A color carrying neither shape cannot be rendered. `Reset` asks the terminal for
        // its default rather than inventing black, which is what "no color" should mean.
        core_color(*self).unwrap_or(Color::Reset)
    }
}

/// Conversion to ratatui's style type. The single conversion point for the whole app.
pub trait ToRatatuiStyle {
    fn to_ratatui_style(&self) -> Style;
}

impl ToRatatuiStyle for CoreStyle {
    fn to_ratatui_style(&self) -> Style {
        let mut style = Style::default();
        if let Some(fg) = self.fg.and_then(core_color) {
            style = style.fg(fg);
        }
        if let Some(bg) = self.bg.and_then(core_color) {
            style = style.bg(bg);
        }
        let mut modifiers = Modifier::empty();
        if self.bold {
            modifiers |= Modifier::BOLD;
        }
        if self.italic {
            modifiers |= Modifier::ITALIC;
        }
        if self.underline {
            modifiers |= Modifier::UNDERLINED;
        }
        if self.dim {
            modifiers |= Modifier::DIM;
        }
        style.add_modifier(modifiers)
    }
}

// ------------------------------------------------------- theme palette helpers (S30)
//
// The interface's own colors, as opposed to a syntax span's. Both come from the theme, but they
// arrive differently: a syntax span may legitimately leave its background unset so the editor
// ground shows through, while an interface surface may not, because "unset" means "whatever the
// terminal's default is" and that default changes when the user switches their emulator from a
// dark to a light palette. Every helper below sets both channels.

use np_core::palette::{Rgb, ThemePalette};

/// A style whose foreground and background are both explicit.
///
/// The type most of the chrome should be built from. Taking `Option` colors here would reintroduce
/// exactly the bug this exists to prevent, so the signature makes an unset channel impossible.
pub fn ink(fg: Rgb, bg: Rgb) -> Style {
    Style::default().fg(to_rgb(fg)).bg(to_rgb(bg))
}

/// A style for text on the chrome band: menu bar, tab bar, status bar, service lines.
pub fn chrome(palette: &ThemePalette) -> Style {
    ink(palette.chrome_fg, palette.chrome_bg)
}

/// Secondary text on the chrome band: an inactive tab's name, a hint, a separator.
pub fn chrome_dim(palette: &ThemePalette) -> Style {
    ink(palette.chrome_dim_fg, palette.chrome_bg)
}

/// The active tab: full-strength ink on the band.
pub fn chrome_active(palette: &ThemePalette) -> Style {
    ink(palette.chrome_fg, palette.chrome_bg)
}

/// An inactive tab: dimmer ink on a ground between the band and the text area, so the active tab
/// is distinguished by *two* channels rather than one. Either alone survives a bad theme; together
/// they cannot both fail.
pub fn chrome_inactive(palette: &ThemePalette) -> Style {
    ink(palette.chrome_dim_fg, palette.chrome_inactive_bg)
}

/// A dialog's body.
pub fn dialog(palette: &ThemePalette) -> Style {
    ink(palette.dialog_fg, palette.dialog_bg)
}

/// A highlighted list row.
pub fn highlight(palette: &ThemePalette) -> Style {
    ink(palette.highlight_fg, palette.highlight_bg)
}

/// Error text on a dialog or the chrome band.
pub fn error_on(palette: &ThemePalette, bg: Rgb) -> Style {
    ink(palette.error_fg, bg)
}

/// A dialog border. The symbol is drawn in this color; no text rides on it.
pub fn border(palette: &ThemePalette) -> Style {
    ink(palette.border, palette.dialog_bg)
}

/// The editor ground with ordinary text on it. The base every overlay is measured from.
pub fn text(palette: &ThemePalette) -> Style {
    ink(palette.foreground, palette.background)
}

/// A `CoreStyle` over the theme's text ground.
///
/// The one place a syntax span's unset background is resolved. Leaving it unset was correct while
/// the terminal supplied the ground; now that the app owns the ground, a span with no background
/// has to be told what to sit on, or it shows whatever the emulator has -- the same reported bug,
/// relocated to the text area.
///
/// An unset *foreground* resolves to the theme's own foreground rather than a computed ink. The
/// theme chose that color to sit on that background, so it is both the semantically right answer
/// ("this span has no special color") and one already known to be legible -- inventing a fallback
/// here could only be worse.
pub fn span_over(style: &CoreStyle, palette: &ThemePalette) -> Style {
    // `unwrap_or`, never `or`-then-overwrite. Writing `base.bg(to_rgb(background))` unconditionally
    // looks equivalent and is not: it replaces the background the layer stack just chose, so the
    // selection, the caret's line and a search match all silently lose their ground and every cell
    // comes back painted with the plain editor background. The fallback applies only where the
    // composed style genuinely named nothing.
    let base = style.to_ratatui_style();
    base.bg(base.bg.unwrap_or_else(|| to_rgb(palette.background)))
        .fg(base.fg.unwrap_or_else(|| to_rgb(palette.foreground)))
}

/// Convert a palette triple for callers that need a bare `Color` rather than a `Style`.
pub fn to_color(c: Rgb) -> Color {
    to_rgb(c)
}

/// Convert a palette triple. Kept local so callers never hand-write a `Color::Rgb`.
fn to_rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_style_leaves_colors_unset() {
        let s = CoreStyle::neutral().to_ratatui_style();
        assert_eq!(s.fg, None);
        assert_eq!(s.bg, None);
        assert!(s.add_modifier.is_empty());
    }

    #[test]
    fn rgb_survives_as_rgb() {
        let s = CoreStyle::fg(CoreColor::rgb(12, 34, 56)).to_ratatui_style();
        assert_eq!(s.fg, Some(Color::Rgb(12, 34, 56)));
    }

    #[test]
    fn indexed_survives_as_indexed_not_rgb() {
        let s = CoreStyle::fg(CoreColor::indexed(4)).to_ratatui_style();
        assert_eq!(s.fg, Some(Color::Indexed(4)));
    }

    #[test]
    fn all_four_attributes_map() {
        let core = CoreStyle {
            bold: true,
            italic: true,
            underline: true,
            dim: true,
            ..CoreStyle::default()
        };
        let s = core.to_ratatui_style();
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::ITALIC));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        assert!(s.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn empty_core_color_becomes_reset() {
        assert_eq!(CoreColor::default().to_ratatui_color(), Color::Reset);
    }

    #[test]
    fn rgb_wins_when_both_shapes_are_present() {
        let both = CoreColor {
            rgb: Some((1, 2, 3)),
            indexed: Some(9),
        };
        assert_eq!(both.to_ratatui_color(), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn fg_and_bg_convert_independently() {
        let core = CoreStyle {
            fg: Some(CoreColor::rgb(1, 1, 1)),
            bg: Some(CoreColor::indexed(2)),
            ..CoreStyle::default()
        };
        let s = core.to_ratatui_style();
        assert_eq!(s.fg, Some(Color::Rgb(1, 1, 1)));
        assert_eq!(s.bg, Some(Color::Indexed(2)));
    }
}
