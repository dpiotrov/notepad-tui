//! The single place style layers are composed (spec §6.6, FR-H5).
//!
//! Six layers paint the editor text, and the order is the whole design: a search match must
//! beat the current-line background or the user cannot see where the match is, the current
//! line must beat syntax or the caret's row disappears in a colorful file, and syntax must
//! beat the plain default or it does nothing. Spread across six call sites that order becomes
//! folklore, and the first stage to add a layer gets it wrong. So every cell's final style is
//! produced here, from one function, with the precedence written out.
//!
//! # How composition works
//!
//! [`Layer`] is a decoration with an opinion about some cells. Each carries an optional style,
//! and `None` means "this layer has no view here", so a layer never erases a lower one by
//! accident. Fields are merged one at a time with `or`, which is why a selection can supply a
//! background while leaving syntax's foreground alone: the two coexist.
//!
//! Attributes deliberately do not participate in the `or` chain. Bold from syntax and bold
//! from a match are not competing claims about the same property, so they OR together; colors
//! are claims about the same cell and therefore override.

use np_core::CoreStyle;

/// A decoration in the stack, lowest to highest.
///
/// Kept as plain data rather than a iterator of styles so the precedence is visible in the
/// struct's field order: `read_field` below takes `fg` from the first layer that has one, and
/// the field order *is* the priority order.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layers<'a> {
    /// Syntax highlighting for this cell (FR-H1).
    pub syntax: Option<&'a CoreStyle>,
    /// The caret's line (FR-H5).
    pub current_line: Option<&'a CoreStyle>,
    /// Editor selection.
    pub selection: Option<&'a CoreStyle>,
    /// A search match, current or marked (FR-S2).
    pub search_match: Option<&'a CoreStyle>,
    /// Whitespace/EOL annotation (FR-E14). Highest, because a marker must stay legible over a
    /// highlighted match or the annotation is lost exactly when it is being used.
    pub annotation: Option<&'a CoreStyle>,
}

impl Layers<'_> {
    /// Compose into one style.
    ///
    /// Colors: the highest layer that expresses a color wins, per channel. Foreground and
    /// background resolve independently, which is what lets a selection recolor the
    /// background and still show the keyword's own color.
    ///
    /// Attributes: OR across all layers.
    pub fn compose(&self) -> CoreStyle {
        let all = [
            self.syntax,
            self.current_line,
            self.selection,
            self.search_match,
            self.annotation,
        ];
        // Highest priority first: the last entry is the topmost layer, so walk backwards.
        let fg = all.iter().rev().copied().flatten().find_map(|s| s.fg);
        let bg = all.iter().rev().copied().flatten().find_map(|s| s.bg);
        let any = |f: fn(&CoreStyle) -> bool| all.iter().copied().flatten().any(f);
        CoreStyle {
            fg,
            bg,
            bold: any(|s| s.bold),
            italic: any(|s| s.italic),
            underline: any(|s| s.underline),
            dim: any(|s| s.dim),
        }
    }
}

/// The current-line background (FR-H5).
///
/// Subtle by intent: a background strong enough to see clearly in a dark theme is also strong
/// enough to fight a selection, and the selection has to stay readable. A single dim
/// foreground-inverting band is the least intrusive thing that still reads as "this row".
pub const CURRENT_LINE_BG: (u8, u8, u8) = (58, 64, 76);

/// A selection background, chosen so a syntax foreground stays legible on top of it.
pub const SELECTION_BG: (u8, u8, u8) = (79, 110, 156);

/// A non-current search match: keeps syntax color and adds an underline plus a faint
/// background, so marks stay visible on a line the user is not looking at.
pub fn match_style(current: bool) -> CoreStyle {
    if current {
        // The match under the caret inverts, the way a selection does. This is the one layer
        // allowed to override a selection's background, and after a successful find the two
        // cover the same cells anyway.
        CoreStyle {
            bg: Some(np_core::CoreColor::rgb(236, 111, 112)),
            fg: Some(np_core::CoreColor::rgb(40, 44, 52)),
            bold: true,
            ..CoreStyle::default()
        }
    } else {
        CoreStyle {
            bg: Some(np_core::CoreColor::rgb(67, 76, 94)),
            underline: true,
            ..CoreStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use np_core::CoreColor;

    fn style(fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>) -> CoreStyle {
        CoreStyle {
            fg: fg.map(|(r, g, b)| CoreColor::rgb(r, g, b)),
            bg: bg.map(|(r, g, b)| CoreColor::rgb(r, g, b)),
            ..CoreStyle::default()
        }
    }

    #[test]
    fn empty_layers_compose_to_neutral() {
        assert_eq!(Layers::default().compose(), CoreStyle::neutral());
    }

    #[test]
    fn the_topmost_layer_with_a_color_wins() {
        let syntax = style(Some((1, 1, 1)), Some((2, 2, 2)));
        let matched = style(Some((9, 9, 9)), None);
        let out = Layers {
            syntax: Some(&syntax),
            search_match: Some(&matched),
            ..Default::default()
        }
        .compose();
        assert_eq!(out.fg.map(|c| c.rgb), Some(Some((9, 9, 9))));
        // The match had no background, so syntax's survives -- exactly the independence we want.
        assert_eq!(out.bg.map(|c| c.rgb), Some(Some((2, 2, 2))));
    }

    #[test]
    fn match_beats_current_line_and_current_line_beats_syntax() {
        let syntax = style(Some((1, 1, 1)), Some((10, 10, 10)));
        let line = style(None, Some((20, 20, 20)));
        let matched = style(None, Some((30, 30, 30)));
        let out = Layers {
            syntax: Some(&syntax),
            current_line: Some(&line),
            search_match: Some(&matched),
            ..Default::default()
        }
        .compose();
        assert_eq!(
            out.bg.map(|c| c.rgb),
            Some(Some((30, 30, 30))),
            "match wins"
        );
        // Drop the match and the current line takes its background.
        let out2 = Layers {
            syntax: Some(&syntax),
            current_line: Some(&line),
            ..Default::default()
        }
        .compose();
        assert_eq!(out2.bg.map(|c| c.rgb), Some(Some((20, 20, 20))));
        // With no decoration at all, syntax's background shows.
        let out3 = Layers {
            syntax: Some(&syntax),
            ..Default::default()
        }
        .compose();
        assert_eq!(out3.bg.map(|c| c.rgb), Some(Some((10, 10, 10))));
    }

    #[test]
    fn selection_beats_current_line() {
        let line = style(None, Some((20, 20, 20)));
        let sel = style(None, Some((40, 40, 40)));
        let out = Layers {
            current_line: Some(&line),
            selection: Some(&sel),
            ..Default::default()
        }
        .compose();
        assert_eq!(out.bg.map(|c| c.rgb), Some(Some((40, 40, 40))));
    }

    #[test]
    fn annotation_is_the_top_layer() {
        let matched = style(Some((9, 9, 9)), Some((30, 30, 30)));
        let anno = style(Some((5, 5, 5)), None);
        let out = Layers {
            search_match: Some(&matched),
            annotation: Some(&anno),
            ..Default::default()
        }
        .compose();
        assert_eq!(out.fg.map(|c| c.rgb), Some(Some((5, 5, 5))));
    }

    #[test]
    fn attributes_or_across_layers() {
        let syntax = CoreStyle {
            italic: true,
            ..Default::default()
        };
        let matched = match_style(true);
        let out = Layers {
            syntax: Some(&syntax),
            search_match: Some(&matched),
            ..Default::default()
        }
        .compose();
        assert!(out.italic, "syntax italic survives the match layer");
        assert!(out.bold, "and the match's bold is added");
    }

    #[test]
    fn a_current_match_is_stronger_than_a_mark() {
        assert_ne!(match_style(true).bg, match_style(false).bg);
        assert!(match_style(false).underline);
    }
}
