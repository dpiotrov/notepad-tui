//! Ratatui-free style types (contract §7).
//!
//! `np-core` must not depend on ratatui, but the highlighter has to describe colors and
//! attributes. These structs are that vocabulary. `np-tui::style` owns the single
//! `From<CoreStyle> for ratatui::style::Style` conversion; nothing else in the codebase
//! converts styles.
//!
//! # Why two color shapes
//!
//! A syntect theme can request either a truecolor RGB value or an 8/16/256-color palette
//! index. They are not interchangeable: on a 16-color terminal an index renders correctly
//! while the same color flattened to RGB gets quantized by the emulator anyway, and on a
//! truecolor terminal an index has to be expanded through a palette we do not own. So the
//! distinction is preserved rather than collapsed, and exactly one of the two is `Some`.

/// A color requested either as truecolor or as a palette index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoreColor {
    /// 24-bit RGB.
    pub rgb: Option<(u8, u8, u8)>,
    /// 8/16/256-color palette index.
    pub indexed: Option<u8>,
}

impl CoreColor {
    /// A truecolor color.
    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        CoreColor {
            rgb: Some((r, g, b)),
            indexed: None,
        }
    }

    /// A palette-indexed color.
    pub fn indexed(i: u8) -> Self {
        CoreColor {
            rgb: None,
            indexed: Some(i),
        }
    }

    /// True when this carries no color information at all.
    pub fn is_none(&self) -> bool {
        self.rgb.is_none() && self.indexed.is_none()
    }
}

/// Text style: colors plus the four boolean attributes the editor uses.
///
/// `Default` is "inherit whatever the widget decided", which is what unstyled text needs:
/// every field is `None`/`false` rather than an explicit white-on-black.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoreStyle {
    pub fg: Option<CoreColor>,
    pub bg: Option<CoreColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Dim/faint, used for secondary text such as line-number gutters and EOL marks.
    pub dim: bool,
}

impl CoreStyle {
    /// A style with no color and no attributes.
    pub fn neutral() -> Self {
        CoreStyle::default()
    }

    /// A foreground-only style, the common case for a syntax span.
    pub fn fg(color: CoreColor) -> Self {
        CoreStyle {
            fg: Some(color),
            ..CoreStyle::default()
        }
    }

    /// Overlay another style's set fields on top of this one.
    ///
    /// Used to compose syntax color with editor decorations such as a search-match
    /// background or a selection, where the decoration wins but the syntax color shows
    /// through wherever the decoration is silent.
    pub fn merged(self, over: CoreStyle) -> CoreStyle {
        CoreStyle {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            bold: self.bold || over.bold,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            dim: self.dim || over.dim,
        }
    }
}

/// Convert a syntect style into the core vocabulary (FR-H1).
///
/// Lives here rather than in `highlight.rs` so the only two places that define what a
/// style *is* sit together, and `highlight.rs` stays about parsing.
///
/// # Transparency
///
/// A syntect `Color` carries an alpha channel, and syntect's own ANSI writer blends a
/// partial alpha over the background instead of emitting it. Alpha 0 means "no color was
/// specified here", which must become `None` (inherit). Painting it as opaque black would
/// give every unstyled span a black background.
///
/// # Why `indexed` is never produced
///
/// syntect 5.3's `Color` is an RGBA struct with no palette-index representation at all:
/// theme files are hex, and `.tmTheme` has no indexed color syntax. So `CoreColor::indexed`
/// can never come from a theme, only from editor-chosen decoration. That is a property of
/// the crate, not a gap in this mapping; contract §7 assumed an indexed path exists.
///
/// # Why `dim` is never produced
///
/// `FontStyle` has exactly three bits: BOLD, UNDERLINE, ITALIC. There is no DIM, so the
/// core field stays `false` for every syntax span. Dim remains available to the editor's
/// own decorations, which is what it is used for.
pub fn from_syntect(style: &syntect::highlighting::Style) -> CoreStyle {
    CoreStyle {
        fg: color_from_syntect(style.foreground),
        bg: color_from_syntect(style.background),
        bold: style
            .font_style
            .contains(syntect::highlighting::FontStyle::BOLD),
        italic: style
            .font_style
            .contains(syntect::highlighting::FontStyle::ITALIC),
        underline: style
            .font_style
            .contains(syntect::highlighting::FontStyle::UNDERLINE),
        dim: false,
    }
}

/// Map a syntect color, treating full transparency as "not specified".
pub fn color_from_syntect(c: syntect::highlighting::Color) -> Option<CoreColor> {
    if c.a == 0 {
        return None;
    }
    Some(CoreColor::rgb(c.r, c.g, c.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_carries_no_information() {
        let s = CoreStyle::default();
        assert!(s.fg.is_none() && s.bg.is_none());
        assert!(!s.bold && !s.italic && !s.underline && !s.dim);
    }

    #[test]
    fn color_constructors_are_exclusive() {
        let rgb = CoreColor::rgb(1, 2, 3);
        assert_eq!(rgb.rgb, Some((1, 2, 3)));
        assert!(rgb.indexed.is_none());
        let idx = CoreColor::indexed(9);
        assert_eq!(idx.indexed, Some(9));
        assert!(idx.rgb.is_none());
        assert!(CoreColor::default().is_none());
    }

    #[test]
    fn merge_lets_the_overlay_win_per_field() {
        let base = CoreStyle {
            fg: Some(CoreColor::rgb(10, 20, 30)),
            bg: Some(CoreColor::rgb(40, 50, 60)),
            bold: true,
            ..CoreStyle::default()
        };
        let over = CoreStyle {
            bg: Some(CoreColor::indexed(4)),
            underline: true,
            ..CoreStyle::default()
        };
        let m = base.merged(over);
        // fg untouched by the overlay -> base survives.
        assert_eq!(m.fg, Some(CoreColor::rgb(10, 20, 30)));
        // bg replaced by the overlay.
        assert_eq!(m.bg, Some(CoreColor::indexed(4)));
        // attributes OR together.
        assert!(m.bold && m.underline);
    }

    #[test]
    fn merging_with_neutral_is_identity() {
        let base = CoreStyle {
            fg: Some(CoreColor::rgb(1, 1, 1)),
            italic: true,
            ..CoreStyle::default()
        };
        assert_eq!(base.merged(CoreStyle::neutral()), base);
    }
}
