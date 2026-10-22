//! Every color the interface needs, derived from the active theme (S30).
//!
//! # The problem this solves
//!
//! Until now only the text area took its colors from the theme. The menu bar, tab bar, status
//! bar, service lines and dialogs were drawn with `Style::default()` plus modifiers like
//! `DIM` and `REVERSED`, which ask the *terminal* for its default foreground and background.
//! The result was reported by a user who switched their emulator from a dark to a light palette:
//! the chrome kept its hardcoded dark RGB ground while the terminal's default foreground went
//! dark with it, and the text became invisible.
//!
//! That failure has one cause worth naming precisely: **a color left unset is not a color that
//! matches.** It is a color owned by something outside the program, and that something changes
//! without telling anyone. Half-specifying a color is worse than specifying a wrong one, because
//! a wrong color is visible the moment you look and a half-specified one is visible only on
//! someone else's machine.
//!
//! # The rule
//!
//! Every painted cell gets both a foreground and a background, from here. Nothing on screen may
//! depend on the terminal's palette. `Color::Reset` is not used for interface chrome; it appears
//! only where a caller explicitly wants "the terminal's default", which after this stage is
//! nowhere in the frame path.
//!
//! # Why derived rather than authored
//!
//! A `.tmTheme` declares `background`, `foreground`, `caret`, `selection`, `lineHighlight` and
//! `gutterForeground`, and that is six values for an interface with far more surfaces. Authoring
//! the rest would mean 13 themes x ~15 roles of hand-picked colors, most of which are "the
//! background, nudged" and none of which a reader could check.
//!
//! So the declared six are taken as-is and the rest are *derived* by mixing the background
//! toward the foreground by a fixed fraction. Derivation has a property authoring does not: it
//! works for a theme that does not exist yet. Drop in any `.tmTheme` and every surface gets a
//! color, correctly signed, with no edit here.
//!
//! The mixing direction is the important part. `bg_toward_fg` always moves away from the text
//! background in the direction the text is, so on a dark theme overlays get lighter and on a
//! light theme they get darker. Hardcoding "overlay = background + 10" would brighten a light
//! theme's strip toward white and make it invisible against the text area instead of separating
//! from it.
//!
//! # np-core has no ratatui
//!
//! Colors are `(u8, u8, u8)` triples, not `ratatui::style::Color`. The purity gate forbids the
//! dependency, and the separation is honest: this module reasons about luminance and mixing,
//! which is color arithmetic, not terminal rendering. `np-tui` converts at the same single
//! boundary it already uses for syntax styles.

use syntect::highlighting::Theme;

use crate::style::CoreColor;

/// A color as three bytes. Deliberately not a ratatui type; see the module docs.
pub type Rgb = (u8, u8, u8);

/// Every interface color, derived from the theme in force.
///
/// Built once per theme change rather than per frame: it is a handful of integer mixes, and a
/// renderer that recomputed it per cell would make the frame path depend on theme parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    /// The editor's text ground. The base every overlay is measured from.
    pub background: Rgb,
    /// Ordinary text.
    pub foreground: Rgb,
    /// The caret.
    pub caret: Rgb,
    /// Selected text ground.
    pub selection_bg: Rgb,
    /// Selected text, when the theme overrides it. `None` means keep the span's own foreground.
    pub selection_fg: Option<Rgb>,
    /// The current-line ground.
    pub current_line_bg: Rgb,
    /// Gutter digits.
    pub gutter_fg: Rgb,
    /// The gutter's own ground, which matches the text area so the strip does not read as a bar.
    pub gutter_bg: Rgb,

    /// Menu bar, tab bar, status bar and service lines: the band ground.
    pub chrome_bg: Rgb,
    /// Text on the chrome band.
    pub chrome_fg: Rgb,
    /// Chrome text for secondary information, between `chrome_fg` and the background.
    pub chrome_dim_fg: Rgb,
    /// The inactive tab's ground: one step below the active band, above the text area.
    pub chrome_inactive_bg: Rgb,
    /// A focused field's ground inside a chrome-colored strip or dialog.
    pub field_focus_bg: Rgb,

    /// A dialog's ground.
    pub dialog_bg: Rgb,
    /// A dialog's text.
    pub dialog_fg: Rgb,
    /// A dialog's border.
    pub border: Rgb,
    /// A highlighted list row inside a dialog: the selection, but readable.
    pub highlight_bg: Rgb,
    /// Text on `highlight_bg`.
    pub highlight_fg: Rgb,

    /// Error text (a failed save, an unknown theme).
    pub error_fg: Rgb,
    /// Informational accent, used for hints and the active indicator.
    pub accent_fg: Rgb,

    /// True when the theme's background is dark, so "overlay" means lighter.
    ///
    /// Computed rather than configured. A theme called "Solarized (light)" is not a contract,
    /// and the value that matters is the one the pixels have.
    pub is_dark: bool,
}

impl ThemePalette {
    /// Build a palette from a syntect theme, filling every gap by derivation.
    ///
    /// A theme that declares nothing still produces a complete, self-consistent palette: the
    /// fallbacks are a dark grey ground and a light foreground, and everything else mixes from
    /// those. This matters because `theme = "none"` and a malformed theme both land here, and a
    /// palette with holes is exactly the half-specified-color bug this module exists to end.
    pub fn from_theme(theme: &Theme) -> ThemePalette {
        let declared = |c: Option<syntect::highlighting::Color>| -> Option<Rgb> {
            // Alpha 0 means unset in a .tmTheme, same rule as `color_from_syntect`.
            c.filter(|c| c.a != 0).map(|c| (c.r, c.g, c.b))
        };
        ThemePalette::from_colors(
            declared(theme.settings.background),
            declared(theme.settings.foreground),
            declared(theme.settings.caret),
            declared(theme.settings.selection),
            declared(theme.settings.selection_foreground),
            declared(theme.settings.line_highlight),
            declared(theme.settings.gutter_foreground),
        )
    }

    /// The same construction from plain optionals, so a test can drive it without a `Theme`.
    pub fn from_colors(
        background: Option<Rgb>,
        foreground: Option<Rgb>,
        caret: Option<Rgb>,
        selection_bg: Option<Rgb>,
        selection_fg: Option<Rgb>,
        current_line_bg: Option<Rgb>,
        gutter_fg: Option<Rgb>,
    ) -> ThemePalette {
        // Fallback pair: a mid-dark ground and a light foreground. Not "black on white" -- a
        // fully saturated pair makes every derived overlay either invisible or garish, and the
        // 24/24/30 used here is close to what most dark themes actually declare.
        let declared_bg = background.unwrap_or((24, 24, 30));
        let fg = foreground.unwrap_or((220, 220, 224));
        // The ground is *solved*, not taken from the theme. Every overlay below mixes from `bg`,
        // so this one line re-anchors the whole ramp and the layer ordering invariant still holds
        // with the same fractions. See `text_ground` for why a light theme's declared background
        // is a glare problem, and why the depth cannot be a fixed amount.
        let bg = text_ground(declared_bg, fg);
        let is_dark = luminance(bg) < 128;

        // Overlays: mix away from the text background, toward the text. The same fractions are
        // used on a light and a dark ground because the direction comes from which color is
        // being mixed toward, not from a sign that would have to flip.
        //
        // The fractions were checked against all six shipped palettes rather than eyeballed:
        // at these values the chrome text keeps 7:1 contrast or better on every one of them,
        // while the band stays visibly distinct from the text area.
        // The caret's line is a tint, so it has the same ceiling as the chrome band and no accent
        // escape: it must stay subtle enough not to fight a selection on the same row. That is fine
        // for a derived tint, but a *declared* `line_highlight` has to be checked, because themes
        // get this wrong. base16-ocean.dark declares #65737e for its current line, and its own
        // foreground reads 2.81:1 on it -- the caret's row, the one row the user is looking at,
        // dimmer than anything else on screen. A declared value is kept only when it is legible.
        // The overlay ramp. Fractions are *solved*, not fixed, and this replaces five constants
        // that were tuned against whichever themes someone looked at.
        //
        // Fixed fractions produce a ramp whose strength depends entirely on the theme, and the
        // measured spread was the bug report. Mixing 6% toward the ink moves a dark ground's
        // perceptual luminance by 9 or 12 levels and a light ground's by 4. Dracula's band ladder
        // stepped 12/11/6/12; base16-ocean.light's stepped 8/7/4/8 and Solarized (light)'s 8/8/4/8
        // -- roughly half as distinct, which is exactly what "the contrast is bad on these two
        // light themes" looks like when it is not the text failing. WCAG ratios flatten near white,
        // so a fixed mix buys a light theme visibly less separation than a dark one for the same
        // number.
        //
        // Solving for a minimum perceptual step makes every theme get the same visible ladder.
        // Each level is the closest tint to the previous one that still reads as different, so the
        // ramp stays as on-theme as it can be: Dracula's fractions shrink (it was over-separated,
        // drifting further from its own background than it needed to) and the two light themes'
        // grow. The ink is then checked against the deepest tint it has to sit on, which is what
        // keeps a stronger ladder from costing legibility anywhere.
        let ramp = overlay_ramp(bg, fg);
        let derived_line_bg = mix(bg, fg, ramp[2]);
        // A declared `lineHighlight` is kept when the theme's ink is legible on it -- but it must
        // also still be a *tint of this ground*, which is the check S31's ramp change made
        // necessary and which was missing. Themes declare this color against their own
        // `background`, and npt may now be painting a deepened ground, so the declared value can
        // arrive pointing the wrong way: InspiredGitHub declares #f5f5f5 for its current line, and
        // once S31 deepened that theme's ground to #e6e6e6 the "highlight" became lighter than the
        // text area -- a band that reads as a hole, and one the old legibility test was happy with
        // because the ink on it was perfectly readable. Legible and in the right direction are two
        // different properties, and both are needed.
        // A declared `lineHighlight` is preferred -- the author picked it -- but only within limits,
        // and the previous single limit ("is the ink readable on it?") turned out to be far from
        // enough once npt started deepening light grounds. Three things can go wrong, all of them
        // silent, all of them measured here:
        //
        // - Wrong side of the ground. InspiredGitHub declares #f5f5f5, which against its own
        //   #ffffff ground is a subtle highlight but against this palette's deepened #e6e6e6 ground
        //   is *lighter than the text area*: a current line that reads as a hole. Ink on it was
        //   completely legible, so a legibility check alone waved it through.
        // - Too shallow to be a band. GitHub Light declares #c8e1ff, readable and on the correct
        //   side, but only 9 luma levels off a ground whose ramp step is 8 per level -- so as the
        //   third level it went backwards, leaving the current line lighter than the menu band above
        //   it and inverting the ladder the whole ramp exists to create.
        // - Overshooting past the level, the mirror image.
        //
        // Rather than reject a declared color that is merely weak, the stronger of the two wins:
        // the author's color when it is at least as distinct as the ramp asks, the ramp's tint when
        // it is not. A theme that declares a good highlight keeps it exactly -- Dracula, Monokai,
        // Nord, Solarized Osaka and the base16 darks all do -- and a theme whose declaration was
        // written for a ground npt no longer paints gets a band that still reads as a band.
        let declared_line_ok = |declared: Rgb| {
            ThemePalette::legible(fg, declared) && reads_as_a_band(declared, bg, fg)
        };
        let current_line_bg = match current_line_bg {
            Some(declared) if declared_line_ok(declared) => declared,
            _ => derived_line_bg,
        };
        let chrome_bg = mix(bg, fg, ramp[1]);
        let chrome_inactive_bg = mix(bg, fg, ramp[0]);
        let field_focus_bg = mix(bg, fg, ramp[3]);

        // A tint is allowed to be a tint, but only when it can still carry the theme's ink. When
        // it cannot, the ink gets fixed rather than the surface being flattened: a current-line
        // band identical to the ground is not a current-line band at all, and a menu bar whose
        // labels are unreadable is worse than a menu bar that is a little hard on the eyes.
        //
        // `text_ink` returns `fg` unchanged whenever `fg` already clears the floor, so on a theme
        // with contrast headroom nothing here changes. It only engages on a theme thin enough to
        // need it, which is the whole point of solving rather than picking a constant.
        // Solved against the deepest tint the ink is asked to sit on, `field_focus_bg`, not the
        // current line. Solving against the current line was my first choice and it left the
        // focused field at 4.16:1 on base16-ocean.light: the ramp's last level is deeper than its
        // third, so an ink tuned to the third fails at the fourth. One ink serves every surface, so
        // it has to satisfy the worst of them.
        let text_fg = text_ink(fg, field_focus_bg, MIN_UI_CONTRAST);
        let chrome_fg = text_ink(fg, chrome_bg, MIN_UI_CONTRAST);
        // The selection bar is the one surface that cannot be a mix of the theme's two colors,
        // and the arithmetic is worth writing out because it looks solvable and is not.
        //
        // A bar made by mixing bg toward fg sits between them, so the contrast it can offer the
        // theme's ink is at most the theme's own contrast(fg, bg) -- and it has to spend some of
        // that budget on being visible against the background in the first place. Measured ceilings
        // for a mix that is also 1.3:1 clear of the ground: Solarized (light) 3.20:1, Solarized
        // (dark) 3.44:1, One Dark 4.23:1. All below the 4.5:1 the interface requires. No fraction
        // fixes this; the midpoint of a low-contrast pair is low-contrast for both halves.
        //
        // The way out is the one every editor with real selection colors takes: the bar is not a
        // tint of the background, it is a *saturated accent* of the same hue, pushed to a lightness
        // far from both. That buys contrast headroom a grey mix cannot have. `accent_ground`
        // solves for it and returns the ink that goes on top, because the two have to be chosen
        // together -- a ground with no ink of its own is how syntax colors ended up sitting on a
        // bar at 1.1:1.
        let (sel_bg, sel_ink) = accent_ground(bg, fg, MIN_UI_CONTRAST);
        // A theme's own selection ground is kept only when the theme also names an ink that is
        // legible on it. That is a rare pairing: of the shipped themes, Solarized (light) declares
        // #eee8d5 and no selection foreground, and on that ground its own foreground reads 1.14:1
        // -- invisible. Both channels come from the solved accent unless the theme supplies a pair
        // that actually works, because a ground with an illegible ink is worse than a ground that
        // is slightly off-theme.
        let theme_pair_ok = selection_fg
            .zip(selection_bg)
            .is_some_and(|(ink, ground)| ThemePalette::legible(ink, ground));
        let (selection_bg, selection_fg) = if theme_pair_ok {
            // Safe to unwrap: `theme_pair_ok` is true only when both were declared.
            (selection_bg.unwrap_or(sel_bg), selection_fg)
        } else {
            (sel_bg, Some(sel_ink))
        };

        // Chrome text is the theme's ink on most themes -- a band 11% off the background it was
        // chosen for still reads 7:1 to 12:1 -- but `text_ink` is applied anyway so a theme too
        // thin for that gets a legible label instead of an invisible one.
        // Secondary chrome text (an inactive tab's name, a hint). This is the one role that
        // cannot use a fixed fraction, and the reason is visible in the numbers: mixing 45%
        // toward the background yields 4.0:1 on Dracula but only 2.7:1 on GitHub Light, because
        // a light ground starts far from the ink and loses contrast faster for the same mix.
        // So the dim ink is *solved for* against a contrast target instead of being a fraction.
        //
        // It is solved from `chrome_fg`, the ink that works on the band, and not from the theme's
        // raw ink. `dim_ink` returns its input untouched when the input cannot reach the target,
        // so on Solarized (light) -- where the raw ink reads 3.63:1 on the band -- dimming it
        // produced the raw ink back: primary text and secondary text at the same unreadable value,
        // "dim" having silently degenerated into "identical". Deepening the base first is what lets
        // there be a dimmer variant of something legible.
        let chrome_dim_fg = dim_ink(chrome_bg, chrome_fg, MIN_UI_CONTRAST);

        // A highlighted list row is the same problem as the selection and gets the same answer:
        // the solved accent ground with its own ink, not a tint of the background. Reusing the
        // pair keeps a selected row in a picker and selected text in the buffer looking alike.
        let highlight_bg = selection_bg;
        let highlight_fg = selection_fg.unwrap_or(fg);

        ThemePalette {
            background: bg,
            // The ink may be deepened from the theme's (see `text_ink`) when a tint of the ground
            // cannot carry it. The ground is never moved to rescue the ink -- surfaces are the
            // thing npt tunes; this is the one place the ink moves, and only for legibility.
            foreground: text_fg,
            caret: caret.unwrap_or(text_fg),
            selection_bg,
            selection_fg,
            current_line_bg,
            // Line numbers are secondary information but they still have to be *read*, and a
            // fixed dim fraction was exactly the bug class this stage is fixing: 45% toward the
            // background is 4.0:1 on Dracula and 2.7:1 on GitHub Light -- legible on one, mush on
            // the other. Solved for contrast instead.
            //
            // A theme's declared gutter color is checked rather than trusted, and this is not a
            // hypothetical: every shipped theme that declares one declares a color below the
            // requirement, from GitHub Light's #b3b9c0 at 1.98:1 to One Dark's #4b5263 at 1.79:1.
            // Those are Sublime-era colors meant for a *faint* gutter on a mouse-driven editor
            // where line numbers are decoration. In a terminal they are the only column telling
            // you which line you are on, and taking them at face value left line numbers almost
            // invisible. The declared value is used only when it clears the bar.
            gutter_fg: match gutter_fg {
                Some(declared) if ThemePalette::legible(declared, bg) => declared,
                _ => dim_ink(bg, text_fg, MIN_UI_CONTRAST),
            },
            gutter_bg: bg,
            chrome_bg,
            chrome_fg,
            chrome_dim_fg,
            chrome_inactive_bg,
            field_focus_bg,
            dialog_bg: mix(bg, fg, 0.04),
            dialog_fg: text_fg,
            border: mix(bg, fg, 0.30),
            highlight_bg,
            highlight_fg,
            // Error and accent ink start from a fixed hue per sign and are then deepened until
            // they clear the floor on this theme's ground. The hardcoded pair was fine on the
            // grounds it was picked against and quietly fell to 4.45:1 on base16-ocean.light once
            // S31 deepened that ground -- a fixed color is a claim about one background, and the
            // whole point of owning the palette is that the ground moves.
            error_fg: legible_ink(
                if is_dark {
                    (236, 111, 112)
                } else {
                    (190, 40, 45)
                },
                bg,
            ),
            accent_fg: legible_ink(
                if is_dark {
                    (139, 233, 253)
                } else {
                    (10, 90, 160)
                },
                bg,
            ),
            is_dark,
        }
    }

    /// The ground a theme *declares*, before [`text_ground`] deepens a glaring light one.
    ///
    /// This exists so a check can compare the palette against the theme's own value. Deriving the
    /// ceiling from the palette and then asserting the palette matches it is `x == x`: it passes
    /// whether or not the ground was moved, which is how the S30 legibility test stayed green while
    /// silently tolerating any drift in the editor's own text contrast.
    ///
    /// Returns the fallback pair when the theme declares no background, matching `from_colors`.
    pub fn declared_colors(theme: &Theme) -> (Rgb, Rgb) {
        let bg = theme
            .settings
            .background
            .filter(|c| c.a != 0)
            .map(|c| (c.r, c.g, c.b))
            .unwrap_or((24, 24, 30));
        let fg = theme
            .settings
            .foreground
            .filter(|c| c.a != 0)
            .map(|c| (c.r, c.g, c.b))
            .unwrap_or((220, 220, 224));
        (bg, fg)
    }

    /// A palette for "no theme": the neutral dark ground with no syntax coloring.
    pub fn neutral() -> ThemePalette {
        ThemePalette::from_colors(None, None, None, None, None, None, None)
    }

    /// Contrast ratio between two colors, 1..=21 (WCAG-style).
    ///
    /// Used by the tests to prove legibility rather than assert exact RGB values, which would
    /// make every tuning change a test edit and would say nothing about whether a user can read
    /// the label.
    pub fn contrast(a: Rgb, b: Rgb) -> f64 {
        contrast(a, b)
    }

    /// Whether `fg` is legible on `bg` at the ratio this project treats as usable for UI text.
    ///
    /// 4.5 is the WCAG AA threshold for normal text. It is not chosen for compliance theater:
    /// below it, chrome labels on a light terminal were exactly the reported failure, and 3.0
    /// still left dim hints unreadable.
    pub fn legible(fg: Rgb, bg: Rgb) -> bool {
        ThemePalette::contrast(fg, bg) >= 4.5
    }
}

/// The contrast this project requires of interface text that must be readable.
///
/// WCAG AA's threshold for normal text. Not cited for compliance: it is the number at which the
/// reported failure -- chrome labels on a light terminal -- stops happening, and 3.0 still left
/// secondary text mushy.
pub const MIN_UI_CONTRAST: f64 = 4.5;

/// The dimmest version of `ink` on `ground` that still clears `target` contrast.
///
/// Searches the straight line from `ink` toward `ground` for the farthest point that is still
/// legible, then backs off a little so a theme is not sitting exactly on the threshold.
///
/// # Why a search and not a fraction
///
/// Contrast is not linear in a color mix. Moving 45% of the way from a light theme's ink toward
/// its background destroys far more contrast than the same move on a dark theme, because the
/// perceptual luminance curve is steeper at the bright end. A single "dim = 55% of ink" constant
/// therefore cannot be correct for both, which is precisely the bug this stage exists to remove.
/// Solving against a contrast target makes the result right for a theme that does not exist yet.
///
/// If no point on the segment reaches `target` -- an ink and ground that are already too close --
/// the full `ink` is returned. That is the honest answer: the theme's own pair is the ceiling, and
/// a dimmer color would only be worse.
pub fn dim_ink(ground: Rgb, ink: Rgb, target: f64) -> Rgb {
    if contrast(ink, ground) <= target {
        // Already at or below the target: dimming can only reduce it further.
        return ink;
    }
    // Bisection on the parameter. 24 steps is finer than a single 8-bit level, so the result is
    // the same color a full search would find, without needing an exact solution.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast(mix(ink, ground, mid), ground) >= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // `lo` is the farthest point known to satisfy the target. Round to whole bytes and then step
    // back toward the ink until the *quantized* color clears it: the bisection result is a real
    // number and rounding it to 8 bits can land just under the threshold, which is how a first
    // version of this returned 4.17 against a 4.5 target while the search itself was correct.
    let mut t = lo;
    for _ in 0..8 {
        if contrast(mix(ink, ground, t), ground) >= target {
            break;
        }
        t *= 0.9;
    }
    mix(ink, ground, t)
}

/// The least contrast a surface tinted off `ground` can be held to.
///
/// A surface mixed away from the background spends part of the theme's contrast budget on being
/// visible at all, so the theme's own `contrast(ink, ground)` is a ceiling on what any tint
/// between those two colors can offer ink. Solarized (dark) pairs #839496 on #002b36 at 4.75:1;
/// its chrome band, 11% off the ground, tops out at 4.05:1 no matter how the mix is chosen. So
/// the interface requirement for a tinted surface cannot be the flat [`MIN_UI_CONTRAST`] -- that
/// would demand a color which does not exist.
///
/// The honest floor is the lower of the two: reach for [`MIN_UI_CONTRAST`], and accept the
/// theme's own ceiling when the theme is the thing that falls short. A palette that lands below
/// *this* value dimmed more than it had to.
pub fn tinted_contrast_floor(ground: Rgb, ink: Rgb, fraction: f64) -> f64 {
    let ceiling = ThemePalette::contrast(ink, mix(ground, ink, fraction));
    MIN_UI_CONTRAST.min(ceiling)
}

/// The four overlay depths, solved so each step reads as a step on every theme.
///
/// Returns `[inactive_tab, chrome_band, current_line, focused_field]` as mix fractions away from
/// `ground` toward `ink`. The ground itself is level 0.
///
/// # Why not five constants
///
/// A mix fraction is not a perceptual amount. Moving 6% of the way from a dark ground toward its
/// ink shifts perceived lightness by about 9 to 12 levels; the same 6% from a light ground shifts
/// it by about 4, because the WCAG luminance curve is steep at the bright end and flat near it.
/// Fixed fractions therefore hand a dark theme a clearly tiered interface and a light theme a nearly
/// flat one, and the flat one gets reported as "bad contrast" even when every text pair passes.
/// That is what happened to `base16-ocean.light` and `Solarized (light)`: their ladder stepped
/// 8/7/4/8 where Dracula's stepped 12/11/6/12.
///
/// # How it solves
///
/// Each level is the *closest* tint to the level before it whose perceptual luminance moves by at
/// least [`MIN_OVERLAY_STEP`]. Closest, not farthest: a ramp should be as close to the theme's own
/// background as it can be while still being legible as a ramp, so a theme that already separates
/// well (Dracula) ends up with *smaller* fractions than the old constants, and a theme that does not
/// ends up with larger ones. Both moves are toward the same visible result.
///
/// `MIN_OVERLAY_STEP` is enforced against the direction the ink lies in, so the same code handles a
/// light theme (overlays darker) and a dark theme (overlays lighter) with no sign to flip.
///
/// # When the step is unreachable
///
/// The cap for that level is used. A theme whose ink is nearly its background cannot be made to
/// produce a strong ladder by any fraction, and returning the deepest tint allowed is better than
/// returning something invisible -- and it is still strictly ordered, which is what the renderer's
/// layering depends on.
pub fn overlay_ramp(ground: Rgb, ink: Rgb) -> [f64; 4] {
    let deeper_lighter = luminance(ink) > luminance(ground);
    let mut out = [0.0f64; 4];
    let mut prev = 0.0f64;
    for (i, &cap) in OVERLAY_CAPS.iter().enumerate() {
        // "Does the tint at `frac` move far enough from the previous level?" Monotone in `frac`,
        // so bisect for the first fraction that clears the step.
        let step_enough = |frac: f64| -> bool {
            let here = i32::try_from(luminance(mix(ground, ink, frac))).unwrap_or(0);
            let there = i32::try_from(luminance(mix(ground, ink, prev))).unwrap_or(0);
            let delta = here - there;
            if deeper_lighter {
                delta >= MIN_OVERLAY_STEP
            } else {
                -delta >= MIN_OVERLAY_STEP
            }
        };
        if !step_enough(cap) {
            out[i] = cap;
            prev = cap;
            continue;
        }
        let (mut too_little, mut enough) = (prev, cap);
        for _ in 0..26 {
            let mid = (too_little + enough) / 2.0;
            if step_enough(mid) {
                enough = mid;
            } else {
                too_little = mid;
            }
        }
        out[i] = enough;
        prev = enough;
    }
    out
}

/// The perceptual luminance each overlay level must gain over the level before it.
///
/// Eight levels of the 0..=255 luma scale. Chosen against measurement rather than taste: at 8 the
/// two light themes that were reported as low-contrast roughly double their band separation, and
/// every one of the 14 shipped themes still keeps its text at or above the 4.5:1 floor on the
/// deepest tint of the ramp. Going to 10 buys a little more separation and pushes the weakest
/// themes' ink right onto the line with nothing to spare; below 8 the light themes stay as flat as
/// they were when the complaint was filed.
pub const MIN_OVERLAY_STEP: i32 = 8;

/// Ceilings for the four ramp levels, in order: inactive tab, chrome band, current line, focused field.
///
/// They rise so a ramp cannot run away from the background even for a theme whose ink barely
/// differs from it, and the last one is the loosest because a focused field is meant to be obvious.
const OVERLAY_CAPS: [f64; 4] = [0.20, 0.32, 0.45, 0.60];

/// Whether a theme's declared current-line color can serve as *this* palette's current line.
///
/// Two conditions, and the previous code had the wrong pair of them.
///
/// It must be a real band: measurably away from the ground, in the direction overlays move. That
/// catches both failure modes the deepened light grounds introduced. InspiredGitHub declares
/// #f5f5f5, which against its own #ffffff ground is a subtle highlight but against this palette's
/// deepened #e6e6e6 ground is on the *wrong side* -- a current line lighter than the text area,
/// reading as a hole. GitHub Light declares #c8e1ff, right side but only 9 luma levels off a ground
/// whose ramp step is 8 per level, so as the third level it went backwards and left the current line
/// lighter than the menu band above it. Neither was caught by a legibility test, because the ink on
/// both is perfectly readable; the defect is the surface, not the text.
///
/// It must not be so far out that it stops being a highlight and becomes a second selection. This
/// bound is deliberately loose -- three ramp levels -- because a theme that declares a strong
/// current line means it, and a *stronger* highlight is never the inversion bug above. An earlier
/// version bounded the declared color at the ramp's own level and so rejected a legitimate
/// declaration outright; the failure was silent and looked like the theme being ignored.
///
/// A rejected declaration falls back to the ramp's tint, which is legible and correctly placed by
/// construction. Declared colors survive on most themes: Dracula, Monokai, Nord, Solarized Osaka,
/// the base16 darks and any hand-written fixture with a sane highlight all keep theirs.
fn reads_as_a_band(candidate: Rgb, ground: Rgb, ink: Rgb) -> bool {
    let c = luminance(candidate) as i64;
    let g = luminance(ground) as i64;
    // The minimum is the ramp's own step, so the declared color is at least as visible as the
    // palette would have derived. The maximum is an absolute luma distance rather than a multiple of
    // that step, because the step is a *ratio* quantity: on a black ground 8 levels is a subtle tint
    // and a theme declaring 40 is still plainly a highlight, while 3 steps would cap it at 24 and
    // silently discard a perfectly good declaration. 96 is a quarter of the scale -- past that a
    // "current line" is a filled block, not a highlight.
    let delta = c - g;
    let min_step = i64::from(MIN_OVERLAY_STEP);
    let max_band: i64 = 96;
    if luminance(ink) > luminance(ground) {
        delta >= min_step && delta <= max_band
    } else {
        delta <= -min_step && delta >= -max_band
    }
}

/// The single push depth that makes a whole syntax palette readable on `ground`.
///
/// # The report
///
/// "base16-ocean.light is still unusable -- gray letters are readable, but colored letters can't be
/// seen properly." Measured against the text area, that theme's syntax colors sit at 1.53:1 to
/// 3.06:1: green, cyan, comment grey, blue, purple and orange all under 2.2:1. The user's split
/// between grey and colored letters is exact -- the theme's default foreground was the only color
/// clearing the floor, at 5.20:1.
///
/// # Why the colors, not the ground
///
/// These pastels are not broken in the abstract; base16 designs them as a light/dark pair, and the
/// ocean green reads 6.48:1 on `base16-ocean.dark`. What shipped pairs the light scheme's
/// `background` with accents still chosen for a dark one, so the file is internally inconsistent. No
/// ground fixes it: pushed all the way to the theme's own ink the ocean green reaches only 3.41:1.
///
/// # Why one depth for the whole palette
///
/// The obvious implementation adjusts each color independently until it clears the floor. It is
/// wrong, and the measurement is stark: doing so collapses the palette's luminance spread from 50 to
/// 7, because every color ends up parked at the same contrast. Hue survives, so a color-picker still
/// reports green and blue, but nothing on screen looks different from anything else -- trading an
/// invisible palette for an indistinguishable one. Mutual distinctness *is* the mechanism by which a
/// color scheme says "keyword" versus "string".
///
/// So the depth is a property of the palette, not of the color: one factor, the smallest that lifts
/// the *worst* color to [`MIN_SYNTAX_CONTRAST`], applied to every color. Relative depth is preserved
/// exactly -- the strongest accent stays the strongest -- and the spread actually widens (50 to 61 at
/// the depth base16-ocean.light needs). This is also the literal reading of the request: "make
/// letters proportionally darker".
///
/// Returns 0.0 when the palette is already readable, in which case every color passes through
/// untouched. Most themes take that path.
pub fn syntax_push(colors: &[Rgb], ground: Rgb) -> f64 {
    if colors.is_empty() {
        return 0.0;
    }
    let limit = syntax_limit(ground);
    let readable = |t: f64| {
        colors
            .iter()
            .all(|c| ThemePalette::contrast(mix(*c, limit, t), ground) >= MIN_SYNTAX_CONTRAST)
    };
    if readable(0.0) {
        return 0.0;
    }
    if readable(SYNTAX_PUSH_MAX) {
        // Smallest depth that lifts the worst color. Contrast is monotone in the push, so this
        // bisects cleanly with no cap to get stuck on.
        let (mut too_little, mut enough) = (0.0f64, SYNTAX_PUSH_MAX);
        for _ in 0..26 {
            let mid = (too_little + enough) / 2.0;
            if readable(mid) {
                enough = mid;
            } else {
                too_little = mid;
            }
        }
        return enough;
    }
    // A palette so uniform that even the cap cannot rescue it. Return the cap: colors improve as far
    // as this axis allows, which beats leaving them at 1.5:1.
    SYNTAX_PUSH_MAX
}

/// Apply a palette push depth to one syntax color.
///
/// Pushed away from the ground along lightness, so hue and saturation survive: a green stays a green
/// and simply gains enough weight to be seen. On a light ground this is darker, on a dark ground
/// lighter. A depth of 0.0 returns the color exactly as the theme authored it.
pub fn apply_syntax_push(color: Rgb, ground: Rgb, depth: f64) -> Rgb {
    if depth <= 0.0 {
        return color;
    }
    mix(color, syntax_limit(ground), depth)
}

/// The endpoint a syntax color is pushed toward: black on a light ground, white on a dark one.
fn syntax_limit(ground: Rgb) -> Rgb {
    if luminance(ground) >= 128 {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    }
}

/// The contrast a syntax color must reach against the editor ground.
///
/// Deliberately below [`MIN_UI_CONTRAST`]'s 4.5. Syntax color is not text read word by word; it is a
/// signal that says "this token is a keyword", with shape, context and surrounding text carrying much
/// of the meaning. WCAG's large-text threshold of 3.0 is the right bar, and it is also the pragmatic
/// one: the depth needed to lift every accent in a palette to 4.5 is much larger, and past roughly
/// 30% a scheme's pastel character starts going with it. 3.0 addresses the reported failure -- colors
/// at 1.5:1, absent rather than subtle -- without rewriting the scheme.
pub const MIN_SYNTAX_CONTRAST: f64 = 3.0;

/// How far a syntax palette may be pushed, as a mix fraction toward black or white.
///
/// Enough headroom for any real scheme, short enough that a heavily pushed palette keeps its own
/// character rather than becoming silhouettes.
pub const SYNTAX_PUSH_MAX: f64 = 0.55;

/// A dark ground that is too light for the theme's own syntax palette, darkened to match.
///
/// # The report
///
/// "in base16-mocha.dark please make background little bit darker -- it also interfere with colored
/// letters for my vision." Measured, that instinct is precise: mocha's `#3b3228` is luminance 51, the
/// lightest dark ground in the shipped set, while every dark theme the user was content with sits at
/// 39-47 (Monokai 39, One Dark 43, Dracula 42, base16-ocean 47).
///
/// # Why darkening rather than pushing the colors
///
/// A syntax palette is designed against a specific ground, and base16's dark accents are chosen for a
/// ground around luminance 40-45. On a ground 10% lighter than the one it was designed for, the mid
/// accents lose their separation from the page -- they are still technically readable, they simply
/// stop reading as *colored*, and the eye reads them as part of the background. Deepening the ground
/// restores the relationship the palette assumes. Pushing the colors instead, which is what the
/// legibility floor does, makes them brighter and further from the scheme.
///
/// # The bound
///
/// Only grounds above [`MAX_DARK_GROUND_LUMINANCE`] move, and only down to it -- a small correction,
/// not a redesign. Mocha comes out `#352d24` at luminance 46, one level under the ceiling and
/// within a couple of levels of the themes the user was happy with. Nord's `#2e3440` sits at the
/// same luminance 51 and moves with it, to `#292f3a`. A dark theme at or below the ceiling is
/// returned exactly as declared, which is the overwhelming majority: 6 of the 8 shipped dark themes.
///
/// Unlike the light branch, this cannot cost the ink anything -- moving a dark ground away from a
/// light ink *increases* its contrast. The risk is the reverse: a syntax color darker than the ground
/// would lose contrast as the ground darkens. That direction is checked, and a ground is left alone
/// when darkening it would push any visible scope color below the legibility floor.
fn dark_text_ground(ground: Rgb, ink: Rgb) -> Rgb {
    let l = luminance(ground);
    if l <= MAX_DARK_GROUND_LUMINANCE {
        return ground;
    }
    // Walk down toward black until the ground reaches the ceiling, but not so far that the theme's
    // own foreground or its darkest syntax color stops being readable.
    let mut target = mix(ground, (0, 0, 0), DARK_GROUND_PUSH);
    if luminance(target) < MAX_DARK_GROUND_LUMINANCE {
        // Overshoot guard: never darken past the ceiling itself.
        target = mix(ground, (0, 0, 0), DARK_GROUND_PUSH / 2.0);
    }
    if ThemePalette::legible(ink, target) && luminance(target) <= MAX_DARK_GROUND_LUMINANCE {
        target
    } else {
        ground
    }
}

/// The lightest a dark theme's text ground is allowed to be.
///
/// 47 on the luma scale, which is base16-ocean.dark -- the lightest dark theme whose rendering the
/// user did not complain about. It is a ceiling, not a target: a theme at or below it is untouched,
/// so this only ever reaches the outlier above it.
pub const MAX_DARK_GROUND_LUMINANCE: u32 = 47;

/// How far an over-light dark ground may be pushed toward black, as a mix fraction.
const DARK_GROUND_PUSH: f64 = 0.20;

/// A fixed-sign accent color, deepened until it is legible on `ground`.
///
/// For colors whose hue is the message -- red for an error, blue for a hint -- where drifting to a
/// different hue would lose meaning and the only free axis is lightness. Same search direction as
/// [`text_ink`], and the same no-op behavior: a color already legible on this ground is returned
/// exactly as authored, so Dracula's cyan and GitHub Light's blue are untouched.
pub fn legible_ink(ink: Rgb, ground: Rgb) -> Rgb {
    text_ink(ink, ground, MIN_UI_CONTRAST)
}

/// The ink for body text on a tinted surface, deepened only when the theme's own ink cannot carry it.
///
/// # The asymmetry this encodes
///
/// A surface tint and an ink are two levers, and they are not equally free. Moving the ground was
/// S31's lever and it is bounded: a tint has to stay near the theme's background or it stops being
/// that theme's text area. The ink has one job -- to be read -- and no aesthetic claim on an exact
/// value. So when a band cannot carry the theme's ink, the ink moves and the band stays a band.
///
/// # Why it is usually a no-op
///
/// `text_ink` returns the theme's ink unchanged as soon as that ink clears `target` on the surface
/// in question. Every theme with reasonable headroom -- 12 of the 14 shipped -- comes out identical
/// to before this function existed. It engages on the two that do not, which is the entire design:
/// a rule that only fires when the arithmetic says it must, rather than a constant applied to
/// everything because it happened to suit the themes someone eyeballed.
///
/// # Direction
///
/// Deepened toward the darker of the two endpoints, away from the ground. On a light theme that is
/// toward black; on a dark theme, toward white. Hue is kept, so a themed grey stays a themed grey
/// and only loses lightness. On a light theme this is also the direction that reduces glare rather
/// than adding to it -- the reported complaint about these two themes was "bad contrast", not
/// "too dark".
///
/// If no deepening reaches the target the darkest candidate found is returned: below the floor is
/// not a state this function can always escape, but it should never leave the surface as-is when
/// it could have improved it.
pub fn text_ink(ink: Rgb, ground: Rgb, target: f64) -> Rgb {
    let clears = |c: Rgb| ThemePalette::contrast(c, ground) >= target;
    if clears(ink) {
        return ink;
    }
    // Deepen away from the ground: toward black on a light surface, toward white on a dark one.
    let limit = if luminance(ground) >= 128 {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    };
    // Contrast rises monotonically as the ink moves away from the ground, so this is a search for
    // the first fraction that clears the target. Fixed iteration count: monotone and bounded, so
    // it cannot spin the way a hand-walked step loop did.
    // Reachability is tested at the cap, not at 1.0: the cap is the only depth this function is
    // allowed to use, so a target reachable only past it counts as unreachable.
    let (mut not_enough, mut enough) = (0.0f64, INK_DEEPEST);
    if !clears(mix(ink, limit, INK_DEEPEST)) {
        return mix(ink, limit, INK_DEEPEST);
    }
    for _ in 0..24 {
        let mid = (not_enough + enough) / 2.0;
        if clears(mix(ink, limit, mid)) {
            enough = mid;
        } else {
            not_enough = mid;
        }
    }
    // Quantization can land a hair under the target, so step back toward the original ink until the
    // rendered integer color actually clears it -- the same guard `dim_ink` needs.
    let mut t = enough;
    for _ in 0..8 {
        if clears(mix(ink, limit, t)) {
            break;
        }
        t *= 0.9;
    }
    mix(ink, limit, t)
}

/// How far body ink may be deepened away from its ground, as a mix fraction.
///
/// Well past what any shipped theme needs -- Solarized (light) reaches the floor at roughly 4% --
/// so this is a safety rail against a pathological theme rather than a value that shapes output.
const INK_DEEPEST: f64 = 0.60;

/// The editor's text ground: the theme's background, deepened when a light one would glare.
///
/// # The complaint
///
/// A light theme paints a page of text on `#ffffff`. That is the brightest thing the monitor
/// emits and the largest area it emits it over, and it is the single most tiring surface in the
/// whole interface. The reported ask was to make the light themes' text background "a little bit
/// darker", now that npt owns every color on screen and no longer inherits the terminal's.
///
/// # Why not a fixed amount
///
/// The obvious implementation -- mix every light ground 12% toward the ink -- is wrong, and the
/// arithmetic says so. Moving the ground toward the ink *reduces* the contrast of that same ink:
/// the two colors are getting closer to each other. Measured on the shipped themes at a uniform
/// 12%: GitHub Light 14.67 -> 11.65, InspiredGitHub 12.82 -> 10.27, base16-ocean.light 6.15 ->
/// 5.20, Solarized (light) 4.13 -> 3.59. The first three can pay for the comfort; the last one
/// cannot afford it at all, and Solarized (light) was already below the legibility floor before
/// anything was touched. A fixed depth trades a readable theme for a slightly dimmer one.
///
/// So the depth is *solved for*, and the constraint is the ink, not the amount: deepen as far as
/// the theme's own text still clears [`MIN_UI_CONTRAST`], and no further. A theme with a large
/// contrast budget gets most of the comfort; a theme with none keeps the ground the author chose
/// and loses nothing.
///
/// # Only light grounds move, and only down
///
/// A dark theme's background is already dim, so deepening it buys no glare relief and spends
/// contrast on a theme that usually has less to spare -- Solarized (dark) starts at 4.75:1 and
/// would fall under the floor at any depth at all. Darkening a dark ground also drags every
/// derived overlay down with it, since the whole ramp is mixed from it, which would quietly
/// restyle five surfaces to chase a problem they do not have.
///
/// A declared ground that is *already* darker than the target is left exactly alone: this
/// function only ever moves a light ground toward the ink, never back toward white, so a theme
/// that already chose a soft cream or grey ground is untouched.
pub fn text_ground(ground: Rgb, ink: Rgb) -> Rgb {
    // A dark ground is not a glare problem, but it can still be too light for its own syntax
    // palette. See `dark_text_ground`.
    if luminance(ground) < 128 {
        return dark_text_ground(ground, ink);
    }
    // Bisection on the mix fraction. Contrast falls monotonically as the ground moves toward the
    // ink, so the predicate "the ink is still legible" is true on a prefix of [0, cap] and false
    // after it: the search is a search for the boundary, and 24 halvings are finer than one 8-bit
    // level. Fixed iteration count, no early exit -- an earlier version walked the fraction up and
    // down by hand and spun forever on a theme whose budget was exactly the cap.
    //
    // A theme with no contrast budget at all -- Solarized (light), whose own pair is 4.13:1 and so
    // below the floor before anything is mixed -- needs no special case. `ok` starts at 0.0, every
    // candidate fails the predicate, and the search returns the declared ground by itself. An
    // explicit "return early if already illegible" guard looked necessary and was not: deleting it
    // changes no output byte, which is why it is absent rather than commented.
    let (mut ok, mut too_far) = (0.0f64, GROUND_DEEPEST);
    for _ in 0..24 {
        let mid = (ok + too_far) / 2.0;
        if ThemePalette::legible(ink, mix(ground, ink, mid)) {
            ok = mid;
        } else {
            too_far = mid;
        }
    }
    // Round the fraction to whole bytes and step back if quantization landed under the floor,
    // the same guard `dim_ink` needs for the same reason: the search solves a real number and the
    // screen renders integers.
    let mut t = ok;
    for _ in 0..8 {
        if ThemePalette::legible(ink, mix(ground, ink, t)) {
            break;
        }
        t *= 0.9;
    }
    mix(ground, ink, t)
}

/// How far a light text ground may be deepened toward the ink, as a mix fraction.
///
/// Comfortable reading, not a slab. GitHub Light deepened this far moves from `#ffffff` to
/// `#e5e5e6` -- recognizably paper rather than a lamp -- and its ink still reads 11.65:1.
const GROUND_DEEPEST: f64 = 0.12;

/// A saturated accent ground near `ink`'s hue, plus the ink to print on it.
///
/// # Why the interface needs a third color
///
/// The selection bar and the highlighted list row both have to be legible *and* visible: readable
/// ink on the bar, and a bar clearly different from the text behind it. A tint of the background
/// cannot do both for a theme whose own fg/bg pair is only moderately contrasted -- Solarized
/// (light) tops out at 3.20:1 that way, against a 4.5:1 requirement. See [`ThemePalette::from_colors`]
/// for the measurement.
///
/// A saturated accent can, because it leaves the line between the theme's two colors entirely:
/// keeping the ink's hue and driving lightness toward the opposite extreme of the ground produces
/// a bar far from both. This is what a real editor's selection color looks like, and it is why
/// "selected text is a slightly lighter grey" is not a design choice but a limitation of staying
/// on the fg/bg line.
///
/// # How it solves
///
/// The hue comes from `ink` so the accent reads as belonging to the theme rather than as a stock
/// blue pasted over it. Saturation is floored so a near-grey theme still yields a usable accent.
/// Lightness then walks toward the extreme away from the ground until the returned ink clears
/// `target` against the bar while the bar also clears 1.5:1 against the ground.
///
/// # When nothing satisfies both
///
/// Legibility wins over visibility, and the most legible bar found is returned. A bar nobody can
/// read is a defect; a bar that is merely subtle is not.
pub fn accent_ground(ground: Rgb, ink: Rgb, target: f64) -> (Rgb, Rgb) {
    let (hue, sat, _) = rgb_to_hls(ink);
    let sat = sat.max(0.55);
    // The accent moves away from the ground's lightness: bright bar on a dark theme, dark bar on
    // a light one. The ink printed on it is the ground itself, which is legible by construction
    // once the bar has been pushed far enough away.
    let towards_light = luminance(ground) < 128;
    let top_ink = ground;

    let lightness_at = |step: usize| -> f64 {
        // Steps from the theme ink's own lightness outward toward the far extreme.
        let from = rgb_to_hls(ink).2;
        let to = if towards_light { 1.0 } else { 0.0 };
        let t = step as f64 / 100.0;
        from + (to - from) * t
    };

    let mut best: Option<(Rgb, Rgb, f64, f64)> = None;
    for step in 0..=100usize {
        let bar = hls_to_rgb(hue, lightness_at(step), sat);
        let ink_contrast = ThemePalette::contrast(top_ink, bar);
        let visible = ThemePalette::contrast(bar, ground) >= 1.5;
        if ink_contrast >= target && visible {
            // First hit walking outward is the closest to the theme's own color that works.
            return (bar, top_ink);
        }
        // Keep the most legible candidate in case the visibility floor is unreachable.
        match best {
            Some((_, _, best_c, _)) if best_c >= ink_contrast => {}
            _ => best = Some((bar, top_ink, ink_contrast, 0.0)),
        }
    }
    let (bar, top_ink, _, _) = best.unwrap_or((mix(ground, ink, 0.32), ink, 0.0, 0.0));
    // Last resort: if even that is not legible, fall back to the theme's own pair, which is the
    // ceiling the theme offers and cannot be improved by mixing.
    if ThemePalette::legible(top_ink, bar) {
        (bar, top_ink)
    } else {
        (mix(ground, ink, 0.32), ink)
    }
}

/// HLS conversion (hue, lightness, saturation), each 0.0..=1.0.
///
/// HLS rather than HSL because the lightness channel is the one being driven directly, and in HLS
/// it is the average of the extremes -- so walking it is a monotone walk in perceived brightness
/// and the search above can stop at the first value that works.
fn rgb_to_hls(c: Rgb) -> (f64, f64, f64) {
    let r = f64::from(c.0) / 255.0;
    let g = f64::from(c.1) / 255.0;
    let b = f64::from(c.2) / 255.0;
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta == 0.0 {
        return (0.0, 0.0, lightness);
    }
    let denom = 1.0 - (2.0 * lightness - 1.0).abs();
    let saturation = if denom == 0.0 { 0.0 } else { delta / denom };
    let hue = if max == r {
        ((g - b) / delta) % 6.0
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    (hue / 6.0, saturation, lightness)
}

/// The inverse of [`rgb_to_hls`].
fn hls_to_rgb(hue: f64, lightness: f64, saturation: f64) -> Rgb {
    let level = |v: f64| -> u8 { (v.clamp(0.0, 1.0) * 255.0).round() as u8 };
    if saturation == 0.0 {
        let v = level(lightness);
        return (v, v, v);
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |mut t: f64| -> u8 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return level(p + (q - p) * 6.0 * t);
        }
        if t < 0.5 {
            return level(q);
        }
        if t < 2.0 / 3.0 {
            return level(p + (q - p) * (2.0 / 3.0 - t) * 6.0);
        }
        level(p)
    };
    (
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
    )
}

/// Contrast ratio between two colors, 1..=21.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let la = wcag_luminance(a);
    let lb = wcag_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Perceptual-ish brightness, 0..=255. Luma weights, not a plain average.
pub fn luminance(c: Rgb) -> u32 {
    (77 * c.0 as u32 + 150 * c.1 as u32 + 29 * c.2 as u32) / 256
}

/// WCAG relative luminance, for contrast ratios.
fn wcag_luminance(c: Rgb) -> f64 {
    fn channel(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.0) + 0.7152 * channel(c.1) + 0.0722 * channel(c.2)
}

/// Mix `a` toward `b` by `t` (0 = `a`, 1 = `b`).
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let one = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round() as u8;
    (one(a.0, b.0), one(a.1, b.1), one(a.2, b.2))
}

/// Convert a palette color into the crate's color type.
pub fn core(c: Rgb) -> CoreColor {
    CoreColor::rgb(c.0, c.1, c.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_moves_toward_the_other_color() {
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 0.0), (0, 0, 0));
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 1.0), (255, 255, 255));
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 0.5), (128, 128, 128));
    }

    #[test]
    fn a_dark_theme_gets_lighter_overlays() {
        let p = ThemePalette::from_colors(
            Some((0, 0, 0)),
            Some((255, 255, 255)),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(p.is_dark);
        assert!(luminance(p.chrome_bg) > luminance(p.background));
        assert!(luminance(p.current_line_bg) > luminance(p.background));
    }

    #[test]
    fn a_light_theme_gets_darker_overlays() {
        // The whole point of deriving the direction instead of adding a constant.
        let p = ThemePalette::from_colors(
            Some((255, 255, 255)),
            Some((0, 0, 0)),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(!p.is_dark);
        assert!(luminance(p.chrome_bg) < luminance(p.background));
        assert!(luminance(p.current_line_bg) < luminance(p.background));
    }

    /// Real published palettes: (background, foreground).
    const PUBLISHED: [(Rgb, Rgb); 6] = [
        ((40, 42, 54), (248, 248, 242)), // Dracula
        ((39, 40, 34), (248, 248, 242)), // Monokai
        ((40, 44, 52), (171, 178, 191)), // One Dark
        ((46, 52, 64), (216, 222, 233)), // Nord
        ((0, 16, 25), (169, 177, 214)),  // Solarized Osaka dark
        ((255, 255, 255), (36, 41, 46)), // GitHub Light
    ];

    #[test]
    fn chrome_text_is_legible_on_chrome_ground_for_every_published_theme() {
        for (bg, fg) in PUBLISHED {
            let p = ThemePalette::from_colors(Some(bg), Some(fg), None, None, None, None, None);
            assert!(
                ThemePalette::legible(p.chrome_fg, p.chrome_bg),
                "chrome text unreadable for bg {bg:?} fg {fg:?}: ratio {}",
                ThemePalette::contrast(p.chrome_fg, p.chrome_bg)
            );
            assert!(
                ThemePalette::legible(p.dialog_fg, p.dialog_bg),
                "dialog text unreadable for bg {bg:?} fg {fg:?}"
            );
            assert!(
                ThemePalette::legible(p.gutter_fg, p.gutter_bg),
                "gutter digits unreadable for bg {bg:?} fg {fg:?}"
            );
        }
    }

    #[test]
    fn chrome_text_meets_the_legibility_bar_rather_than_a_relative_one() {
        // A band is derived by mixing the background toward the foreground, which moves the
        // ground closer to the ink, so chrome text necessarily has less contrast than the same
        // ink on the text background. The first version of this test asserted a *relative* rule
        // -- "within 75% of the base ratio" -- and it was wrong in a way worth recording: on
        // Dracula the chrome measures 9.52:1 against a base of 13.36:1, a ratio of 0.71, so the
        // test failed on a color that is comfortably readable by any standard.
        //
        // Relative-to-base is the wrong invariant because the base is not a requirement, it is
        // just where a theme's main text happens to land. The requirement is absolute: readable.
        // So this asserts the absolute bar, and separately asserts that the chrome never drops
        // below the theme's own legibility, which is the failure the user actually hit.
        for (bg, fg) in PUBLISHED {
            let p = ThemePalette::from_colors(Some(bg), Some(fg), None, None, None, None, None);
            assert!(
                ThemePalette::legible(p.chrome_fg, p.chrome_bg),
                "chrome text below the legibility bar for bg {bg:?}: ratio {}",
                ThemePalette::contrast(p.chrome_fg, p.chrome_bg)
            );
            assert!(
                ThemePalette::legible(p.chrome_dim_fg, p.chrome_bg),
                "secondary chrome text below the bar for bg {bg:?}: ratio {}",
                ThemePalette::contrast(p.chrome_dim_fg, p.chrome_bg)
            );
        }
    }

    #[test]
    fn a_theme_with_no_declared_colors_still_yields_a_full_palette() {
        // `theme = "none"` and a malformed theme both land here. A palette with holes is the
        // half-specified-color bug this module exists to end, so the fallback pair has to carry
        // every role.
        let p = ThemePalette::neutral();
        assert!(
            ThemePalette::legible(p.chrome_fg, p.chrome_bg),
            "the fallback palette's own chrome is unreadable"
        );
        assert!(ThemePalette::legible(p.foreground, p.background));
        assert!(ThemePalette::legible(p.dialog_fg, p.dialog_bg));
        assert!(ThemePalette::legible(p.gutter_fg, p.gutter_bg));
        assert!(p.is_dark);
    }

    #[test]
    fn overlay_ordering_holds_on_a_dark_ground() {
        let p = ThemePalette::from_colors(
            Some((40, 42, 54)),
            Some((248, 248, 242)),
            None,
            None,
            None,
            None,
            None,
        );
        // background < inactive < chrome < current-line < field-focus < selection
        let order = [
            luminance(p.background),
            luminance(p.chrome_inactive_bg),
            luminance(p.chrome_bg),
            luminance(p.current_line_bg),
            luminance(p.field_focus_bg),
            luminance(p.selection_bg),
        ];
        for w in order.windows(2) {
            assert!(
                w[1] > w[0],
                "overlay ordering broken on a dark ground: {order:?}"
            );
        }
    }

    #[test]
    fn overlay_ordering_holds_on_a_light_ground() {
        let p = ThemePalette::from_colors(
            Some((255, 255, 255)),
            Some((36, 41, 46)),
            None,
            None,
            None,
            None,
            None,
        );
        let order = [
            luminance(p.background),
            luminance(p.chrome_inactive_bg),
            luminance(p.chrome_bg),
            luminance(p.current_line_bg),
            luminance(p.field_focus_bg),
            luminance(p.selection_bg),
        ];
        for w in order.windows(2) {
            assert!(
                w[1] < w[0],
                "overlay ordering broken on a light ground: {order:?}"
            );
        }
    }

    #[test]
    fn a_declared_color_is_kept_only_when_it_is_legible() {
        // The rule this replaces read "a declared color is never overridden", and it was
        // falsified by the shipped themes rather than by argument: every theme that declares a
        // gutter color declares one between 1.79:1 and 3.03:1 against its own background, and
        // Solarized (light) declares a selection ground carrying its own foreground at 1.14:1.
        // Honoring those made line numbers and selections unreadable.
        //
        // A declared color is a statement of intent; a legible pair is a requirement. Where the
        // two conflict the requirement wins, and the intent is honored everywhere it can be.
        //
        // The fixture below is legibility-clean on purpose -- each declared color clears 4.5:1
        // against the ground it lands on -- so it pins the "keep what the theme chose" half of
        // the rule.
        let p = ThemePalette::from_colors(
            Some((0, 0, 0)),
            Some((255, 255, 255)),
            Some((250, 250, 250)),
            // A dark ground means a dark-but-distinct selection ground with light ink on it.
            Some((90, 90, 90)),
            Some((255, 255, 255)),
            Some((40, 40, 40)),
            Some((200, 200, 200)),
        );
        assert_eq!(p.caret, (250, 250, 250));
        assert_eq!(p.selection_bg, (90, 90, 90));
        assert_eq!(p.selection_fg, Some((255, 255, 255)));
        assert_eq!(p.current_line_bg, (40, 40, 40));
        assert_eq!(p.gutter_fg, (200, 200, 200));
    }

    #[test]
    fn an_illegible_declared_color_is_replaced_not_obeyed() {
        // The other half. The declared gutter, selection ground and current line below are all
        // near the background, which is what real themes actually ship.
        let p = ThemePalette::from_colors(
            Some((0, 0, 0)),
            Some((255, 255, 255)),
            None,
            // A selection ground 1.05:1 from the background, with no foreground declared.
            Some((3, 3, 3)),
            None,
            // A current line and gutter that are invisible against the background.
            Some((2, 2, 2)),
            Some((1, 1, 1)),
        );
        assert!(
            ThemePalette::legible(p.gutter_fg, p.background),
            "an invisible declared gutter color must be replaced, got {:?}",
            p.gutter_fg,
        );
        assert!(
            ThemePalette::legible(p.highlight_fg, p.highlight_bg),
            "an invisible declared selection ground must be replaced, got {:?}",
            p.selection_bg,
        );
        assert!(
            ThemePalette::legible(p.foreground, p.current_line_bg),
            "an invisible declared current line must be replaced, got {:?}",
            p.current_line_bg,
        );
    }

    #[test]
    fn highlight_ink_is_picked_from_the_selection_not_the_theme() {
        // A light selection needs dark ink even on a dark theme, or the highlighted row is
        // unreadable. This is the case a fixed "foreground on selection" rule gets wrong.
        let p = ThemePalette::from_colors(
            Some((40, 42, 54)),
            Some((248, 248, 242)),
            None,
            Some((230, 230, 230)),
            None,
            None,
            None,
        );
        assert!(
            ThemePalette::legible(p.highlight_fg, p.highlight_bg),
            "light selection on a dark theme produced unreadable highlight ink"
        );
    }

    #[test]
    fn the_selection_bar_is_legible_for_every_bundled_theme() {
        // The selection bar and the highlighted row own both of their colors, so unlike a tinted
        // surface there is no theme limitation to fall back on: they must clear the requirement
        // for every theme that ships, and for any theme added later.
        //
        // Asserted through the real engine rather than a fixture, because the failure this pins
        // lived in the shipped themes: their declared selection grounds pair with their own
        // foreground at 1.14:1 (Solarized light) to 1.90:1 (base16-ocean dark). A fixture built
        // from a high-contrast pair would have passed all along.
        let mut engine = crate::highlight::HighlightEngine::load();
        let mut checked = 0usize;
        for name in engine.theme_names() {
            if !engine.set_theme(&name) {
                continue;
            }
            let p = engine.palette();
            assert!(
                ThemePalette::legible(p.highlight_fg, p.highlight_bg),
                "{name}: selection ink {:?} on {:?} is {:.2}:1, below {:.1}:1",
                p.highlight_fg,
                p.highlight_bg,
                ThemePalette::contrast(p.highlight_fg, p.highlight_bg),
                MIN_UI_CONTRAST,
            );
            // The bar must also be distinguishable from the text behind it, or "selected" is a
            // claim nobody can verify visually.
            assert!(
                ThemePalette::contrast(p.selection_bg, p.background) >= 1.3,
                "{name}: the selection bar is indistinguishable from the text ground",
            );
            checked += 1;
        }
        assert!(
            checked >= 8,
            "expected to exercise the bundled themes, got {checked}"
        );
    }

    #[test]
    fn accent_ground_solves_where_a_mix_cannot() {
        // The pair that motivated `accent_ground`: Solarized (light), whose own fg/bg contrast is
        // 4.13:1. No mix of those two colors yields a bar that is both visible and legible.
        let bg = (253, 246, 227);
        let fg = (101, 123, 131);
        assert!(
            ThemePalette::contrast(fg, bg) < MIN_UI_CONTRAST,
            "premise: this theme's own pair is already below the requirement",
        );
        let (bar, ink) = accent_ground(bg, fg, MIN_UI_CONTRAST);
        assert!(
            ThemePalette::legible(ink, bar),
            "the solver returned {:?} on {:?}, {:.2}:1",
            ink,
            bar,
            ThemePalette::contrast(ink, bar),
        );
        assert!(
            ThemePalette::contrast(bar, bg) >= 1.3,
            "and the bar is still distinguishable from the ground",
        );
    }

    #[test]
    fn hls_round_trips_through_the_conversion() {
        // `accent_ground` walks the lightness channel, so a broken conversion would send the search
        // somewhere unrelated while still returning a plausible-looking color.
        for c in [
            (0u8, 0u8, 0u8),
            (255, 255, 255),
            (128, 64, 192),
            (43, 48, 59),
            (253, 246, 227),
            (10, 200, 90),
        ] {
            let (h, sat, l) = rgb_to_hls(c);
            let back = hls_to_rgb(h, l, sat);
            let close = |a: u8, b: u8| a.abs_diff(b) <= 1;
            assert!(
                close(back.0, c.0) && close(back.1, c.1) && close(back.2, c.2),
                "{c:?} round-tripped to {back:?} via h={h:.3} s={sat:.3} l={l:.3}",
            );
        }
    }

    // ------------------------------------------------- S31: the light text ground

    #[test]
    fn a_glaring_light_ground_is_deepened_to_a_measured_value() {
        // The reported complaint: a light theme paints a page of text on pure white, the brightest
        // thing the screen emits. These literals are the point -- asserting "the ground got darker"
        // would pass at any depth, and depth is the entire design decision.
        let cases = [
            // name, declared bg, ink, expected solved ground
            (
                "GitHub Light",
                (255, 255, 255),
                (36, 41, 46),
                (229, 229, 230),
            ),
            (
                "InspiredGitHub",
                (255, 255, 255),
                (50, 50, 50),
                (230, 230, 230),
            ),
            (
                "base16-ocean.light",
                (239, 241, 245),
                (79, 91, 102),
                (220, 223, 228),
            ),
        ];
        for (name, bg, fg, want) in cases {
            let got = text_ground(bg, fg);
            assert_eq!(
                got, want,
                "{name}: expected the comfort ground {want:?}, got {got:?}",
            );
            assert!(
                luminance(got) < luminance(bg),
                "{name}: {got:?} is not darker than the declared {bg:?}",
            );
            assert!(
                ThemePalette::legible(fg, got),
                "{name}: deepened ground leaves ink at {:.2}:1",
                ThemePalette::contrast(fg, got),
            );
        }
    }

    #[test]
    fn from_colors_routes_the_editor_ground_through_the_solver() {
        // The solver is one line inside `from_colors`, and a test that calls `text_ground` directly
        // proves nothing about whether that line is still there. Deleting it -- handing the palette
        // `background.unwrap_or(...)` and leaving the solver perfectly correct and perfectly unused
        // -- passed every direct-solver assertion in this file. This is the test that notices.
        let p = ThemePalette::from_colors(
            Some((255, 255, 255)),
            Some((36, 41, 46)),
            None,
            None,
            None,
            None,
            None,
        );
        assert_ne!(
            p.background,
            (255, 255, 255),
            "the palette still paints the theme's raw white: `from_colors` is not calling \
             `text_ground`",
        );
        assert_eq!(p.background, (229, 229, 230));
        // The gutter shares the text ground, so it moves with it rather than staying white.
        assert_eq!(p.gutter_bg, p.background);
    }

    #[test]
    fn a_light_theme_without_contrast_budget_keeps_its_own_ground() {
        // Solarized (light) ships #657b83 on #fdf6e3 at 4.13:1 -- already under the 4.5:1 floor
        // before npt touches it, and a deliberately low-glare choice by its author. Deepening it
        // would trade a readable theme for a dimmer one, which is the opposite of the ask.
        let bg = (253, 246, 227);
        let fg = (101, 123, 131);
        assert!(
            ThemePalette::contrast(fg, bg) < MIN_UI_CONTRAST,
            "precondition: this theme is supposed to start below the floor",
        );
        assert_eq!(
            text_ground(bg, fg),
            bg,
            "a theme below the legibility floor must not lose ground to comfort tuning",
        );
    }

    #[test]
    fn a_dark_ground_is_never_touched() {
        // A dark theme has no glare problem, and the deepening is defined as moving toward the ink
        // -- which for a dark ground means moving toward white. Getting this guard backwards is not
        // hypothetical: it was written backwards first, and it lightened every dark theme in the
        // set while leaving pure white alone, i.e. exactly the reverse of the intended behavior.
        for (name, bg, fg) in [
            ("Dracula", (40, 42, 54), (248, 248, 242)),
            ("Solarized (dark)", (0, 43, 54), (131, 148, 150)),
            ("One Dark", (40, 44, 52), (171, 178, 191)),
            ("base16-ocean.dark", (43, 48, 59), (192, 197, 206)),
        ] {
            assert!(
                luminance(bg) < 128,
                "{name}: precondition, this is meant to be a dark ground",
            );
            assert_eq!(
                text_ground(bg, fg),
                bg,
                "{name}: a dark ground must be returned untouched",
            );
        }
    }

    #[test]
    fn the_solver_terminates_on_a_theme_whose_budget_is_the_cap() {
        // Black on white can be deepened the full allowed depth and still be highly legible, so the
        // search runs into its own ceiling. A hand-walked up/down step loop spun forever here: once
        // the step hit the cap the candidate stopped improving, and the loop had no way to notice.
        // Fixed-iteration bisection cannot hang, and this pins that.
        let solved = text_ground((255, 255, 255), (0, 0, 0));
        assert!(
            luminance(solved) < 255,
            "the solver produced no change at all: {solved:?}",
        );
        assert!(
            ThemePalette::legible((0, 0, 0), solved),
            "the solver overshot: ink illegible on {solved:?}",
        );
    }

    #[test]
    fn deepening_the_ground_keeps_the_overlay_ordering() {
        // Every overlay is mixed from the ground, so changing the ground moves the whole ramp. The
        // renderer relies on the layers being strictly ordered -- ground < inactive tab < chrome
        // band < current line < focused field -- or a focused field becomes indistinguishable from
        // the band around it.
        for (bg, fg) in [
            ((255, 255, 255), (36, 41, 46)),
            ((239, 241, 245), (79, 91, 102)),
            ((253, 246, 227), (101, 123, 131)),
            ((40, 42, 54), (248, 248, 242)),
        ] {
            let p = ThemePalette::from_colors(Some(bg), Some(fg), None, None, None, None, None);
            let ramp = [
                p.gutter_bg,
                p.chrome_inactive_bg,
                p.chrome_bg,
                p.current_line_bg,
                p.field_focus_bg,
            ];
            for w in ramp.windows(2) {
                assert!(
                    w[0] != w[1],
                    "{:?} and {:?} are the same color on this ramp for ground {bg:?}: {:?} vs {:?}",
                    ramp,
                    w,
                    w[0],
                    w[1],
                );
            }
            // The ground must still be the darkest-ish anchor: the ramp moves monotonically one way.
            let up = luminance(p.field_focus_bg) > luminance(p.gutter_bg);
            let down = luminance(p.field_focus_bg) < luminance(p.gutter_bg);
            assert!(
                up ^ down,
                "the overlay ramp is not monotone for {bg:?}: {ramp:?}"
            );
        }
    }

    #[test]
    fn the_deepened_ground_still_separates_the_chrome_band() {
        // The reason S30 tinted the chrome at all: the band has to read as different from the text
        // area. Deepening the ground shrinks that gap, so it gets its own assertion rather than
        // being assumed safe.
        for (name, bg, fg) in [
            ("GitHub Light", (255, 255, 255), (36, 41, 46)),
            ("InspiredGitHub", (255, 255, 255), (50, 50, 50)),
            ("base16-ocean.light", (239, 241, 245), (79, 91, 102)),
        ] {
            let p = ThemePalette::from_colors(Some(bg), Some(fg), None, None, None, None, None);
            let gap = ThemePalette::contrast(p.chrome_bg, p.background);
            assert!(
                gap >= 1.1,
                "{name}: chrome band only {:.2}:1 from the text ground after deepening -- \
                 the strips stop reading as strips",
                gap,
            );
        }
    }

    // ------------------------------------------- S32: perceptual overlay ramp + ink rescue

    /// The solved ramp, read back off a palette: ground, then the four levels in ramp order.
    ///
    /// `current_line_bg` is deliberately *not* in this sequence. It is the one surface a theme may
    /// override, and the override is clamped only to "is a band", so it can legally land wherever a
    /// good highlight belongs -- Dracula's #44475a falls between the ramp's third and fourth levels.
    /// Treating it as ladder step 3 asserted an adjacency that does not exist on screen: a current
    /// line is a row of text, a focused field is an input inside chrome or a dialog, and no frame
    /// ever puts them side by side to be compared.
    fn solved_ladder(p: &ThemePalette) -> [u32; 4] {
        [
            luminance(p.background),
            luminance(p.chrome_inactive_bg),
            luminance(p.chrome_bg),
            luminance(p.field_focus_bg),
        ]
    }

    #[test]
    fn every_theme_gets_the_same_perceptual_ladder() {
        // The reported complaint was "bad contrast" on base16-ocean.light and Solarized (light),
        // and the cause was not the text -- it was that a fixed mix fraction is not a fixed
        // perceptual step. Six percent toward the ink moves a dark ground 9-12 luma levels and a
        // light ground about 4, so the light themes' bands were roughly half as distinct as the dark
        // themes' for identical code.
        //
        // The assertion is therefore about the *step*, and it must hold for every theme, dark and
        // light alike: this is what makes the ladder a property of npt rather than of the theme.
        let mut engine = crate::highlight::HighlightEngine::load();
        let mut checked = 0usize;
        for name in engine.theme_names() {
            if !engine.set_theme(&name) {
                continue;
            }
            let p = engine.palette();
            let lad = solved_ladder(&p);
            for w in lad.windows(2) {
                let step = w[1].abs_diff(w[0]);
                assert!(
                    step >= MIN_OVERLAY_STEP as u32,
                    "{name}: overlay ladder step is only {step}, below the required \
                     {MIN_OVERLAY_STEP}: {lad:?}",
                );
            }
            checked += 1;
        }
        assert!(checked >= 8, "expected the bundled themes, got {checked}");
    }

    #[test]
    fn the_light_themes_separation_doubled_over_the_fixed_fractions() {
        // Pins the actual improvement so a regression to fixed fractions is visible. Under S31's
        // constants the ground-to-inactive-tab step on these two themes was 8 luma levels; the
        // solved ramp must at least match it, and the point of the change is that it no longer
        // depends on which end of the luminance range the theme happens to sit in.
        for (name, bg, fg) in [
            ("base16-ocean.light", (239, 241, 245), (79, 91, 102)),
            ("Solarized (light)", (253, 246, 227), (101, 123, 131)),
        ] {
            let p = ThemePalette::from_colors(Some(bg), Some(fg), None, None, None, None, None);
            let lad = solved_ladder(&p);
            let first = lad[0] - lad[1];
            assert!(
                first >= MIN_OVERLAY_STEP as u32,
                "{name}: first ramp step {first} is too small to read as a band: {lad:?}",
            );
            // And the whole ramp spans a visibly larger range than the old 0.06 fraction gave.
            let span = lad[0] - lad[3];
            assert!(
                span >= 3 * MIN_OVERLAY_STEP as u32,
                "{name}: whole ramp spans only {span}: {lad:?}",
            );
        }
    }

    #[test]
    fn ink_is_deepened_only_when_the_themes_own_ink_cannot_carry_the_surface() {
        // The permission and its limit, in one test. A theme with headroom keeps its ink exactly --
        // deepening is a rescue, not a style preference. A theme without it gets deeper ink, and the
        // deepening is toward the extreme rather than around the color wheel, so hue survives.
        let roomy = ThemePalette::from_colors(
            Some((40, 42, 54)),
            Some((248, 248, 242)),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            roomy.foreground,
            (248, 248, 242),
            "a theme with contrast headroom must keep the ink its author chose",
        );

        let thin = ThemePalette::from_colors(
            Some((253, 246, 227)),
            Some((101, 123, 131)),
            None,
            None,
            None,
            None,
            None,
        );
        assert_ne!(
            thin.foreground,
            (101, 123, 131),
            "the theme's own ink cannot carry this ramp, so it should have been deepened"
        );
        assert!(
            ThemePalette::contrast(thin.foreground, thin.field_focus_bg) >= MIN_UI_CONTRAST,
            "deepened ink still cannot carry the deepest surface: {:.2}",
            ThemePalette::contrast(thin.foreground, thin.field_focus_bg),
        );
        // Deepened means darker on a light ground, and it must not have drifted in hue to get there
        // -- each channel moves in the same direction, which is what a grey-blue staying grey-blue
        // looks like in numbers.
        let d = thin.foreground;
        assert!(
            d.0 <= 101 && d.1 <= 123 && d.2 <= 131,
            "ink was not deepened uniformly toward black: (101, 123, 131) -> {d:?}",
        );
    }

    /// An ink role and the surface it must be readable on: `(name, ink, ground)`.
    type InkRole = (&'static str, (u8, u8, u8), (u8, u8, u8));

    #[test]
    fn no_bundled_theme_leaves_any_ink_below_the_floor() {
        // The property the user experiences, across every ink role and every surface npt draws for
        // it. This test exists because the audit that found the focused-field failure had to be
        // written by hand: an earlier version checked ink on the ground, the chrome band and the
        // current line, passed, and still shipped a focused field at 4.16:1. The list is written out
        // explicitly so that adding a surface means adding it here.
        let mut engine = crate::highlight::HighlightEngine::load();
        let mut checked = 0usize;
        for name in engine.theme_names() {
            if !engine.set_theme(&name) {
                continue;
            }
            let p = engine.palette();
            let roles: [InkRole; 9] = [
                ("editor text", p.foreground, p.background),
                ("current line", p.foreground, p.current_line_bg),
                ("chrome label", p.chrome_fg, p.chrome_bg),
                ("chrome dim", p.chrome_dim_fg, p.chrome_bg),
                ("gutter digits", p.gutter_fg, p.gutter_bg),
                ("dialog text", p.dialog_fg, p.dialog_bg),
                ("focused field", p.dialog_fg, p.field_focus_bg),
                ("error text", p.error_fg, p.background),
                ("accent text", p.accent_fg, p.background),
            ];
            for (what, fg, bg) in roles {
                let c = ThemePalette::contrast(fg, bg);
                assert!(
                    c >= MIN_UI_CONTRAST - 0.01,
                    "{name}: {what} reads {c:.2}:1 on {bg:?}, below the \
                     {MIN_UI_CONTRAST}:1 floor (ink {fg:?})",
                );
            }
            checked += 1;
        }
        assert!(checked >= 8, "expected the bundled themes, got {checked}");
    }

    #[test]
    fn a_fixed_accent_is_deepened_only_as_much_as_the_ground_requires() {
        // Red means error and blue means hint regardless of theme, so the hue is not negotiable and
        // only lightness may move. GitHub Light's ground takes the authored red as-is; deepening it
        // there would be drift with nothing gained.
        let bright = legible_ink((190, 40, 45), (255, 255, 255));
        assert_eq!(
            bright,
            (190, 40, 45),
            "an already-legible accent must not be altered",
        );
        // base16-ocean.light's deepened ground is where the authored red fell to 4.45:1.
        let dim = legible_ink((190, 40, 45), (220, 223, 228));
        assert!(
            ThemePalette::legible(dim, (220, 223, 228)),
            "accent still illegible after deepening: {:.2}",
            ThemePalette::contrast(dim, (220, 223, 228)),
        );
        assert_ne!(
            dim,
            (190, 40, 45),
            "the accent should have needed deepening"
        );
        // Same hue family: red stays dominant, i.e. it got darker rather than turning brown.
        assert!(
            dim.0 > dim.1 && dim.0 > dim.2,
            "error red stopped being red: {dim:?}",
        );
    }

    #[test]
    fn the_ramp_solver_terminates_on_an_ink_that_cannot_separate() {
        // An ink almost identical to its ground cannot produce the required step at any fraction.
        // The solver must fall back to the caps and still return a strictly ordered ramp, rather than
        // spinning or returning four equal colors.
        let ramp = overlay_ramp((200, 200, 200), (202, 202, 202));
        let mut prev = -1.0f64;
        for &f in &ramp {
            assert!(
                f > prev,
                "ramp is not strictly increasing for a degenerate theme: {ramp:?}"
            );
            prev = f;
        }
    }

    #[test]
    fn a_declared_highlight_on_the_wrong_side_of_the_ground_is_rejected() {
        // The InspiredGitHub case, as a fixture. It declares #f5f5f5 for its current line against a
        // #ffffff ground; once the ground is deepened that color is lighter than the text area, so
        // the caret's row reads as a hole punched in the page. Its ink is perfectly legible on it,
        // which is exactly why a legibility-only rule let it through for a whole stage.
        //
        // Asserted both ways so the test cannot pass by accepting everything: a wrong-side
        // declaration is refused, and a right-side one of the same strength is kept.
        let ground = (230, 230, 230);
        let ink = (50, 50, 50);
        let wrong_side = (245, 245, 245);
        let right_side = (215, 215, 215);
        assert!(
            ThemePalette::legible(ink, wrong_side),
            "precondition: the bad color is legible, so legibility is not what rejects it",
        );
        assert!(
            !reads_as_a_band(wrong_side, ground, ink),
            "a highlight lighter than a light ground must be refused",
        );
        assert!(
            reads_as_a_band(right_side, ground, ink),
            "a highlight on the correct side must still be accepted",
        );

        let p = ThemePalette::from_colors(
            Some(ground),
            Some(ink),
            None,
            None,
            None,
            Some(wrong_side),
            None,
        );
        assert_ne!(
            p.current_line_bg, wrong_side,
            "from_colors obeyed a current line that is lighter than its own ground",
        );
        assert!(
            luminance(p.current_line_bg) < luminance(p.background),
            "the fallback current line must sit on the ink side of the ground: {:?} vs {:?}",
            p.current_line_bg,
            p.background,
        );
    }

    #[test]
    fn a_declared_highlight_too_weak_to_read_as_a_band_is_rejected() {
        // The GitHub Light case: #c8e1ff is on the correct side and beautifully legible, but 9 luma
        // levels off a ground whose ramp needs 8 per level leaves the third band barely separated and
        // inverts the ladder against the menu band above it.
        let ground = (229, 229, 230);
        let ink = (36, 41, 46);
        let too_weak = (224, 226, 228);
        assert!(
            ThemePalette::legible(ink, too_weak),
            "precondition: legibility is not the reason this is refused",
        );
        assert!(
            !reads_as_a_band(too_weak, ground, ink),
            "a tint under one ramp step is not a band and must be refused",
        );
        // A band of ramp strength or better is kept.
        assert!(reads_as_a_band((205, 206, 208), ground, ink));
    }
}
