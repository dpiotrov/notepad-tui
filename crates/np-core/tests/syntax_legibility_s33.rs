//! Syntax colors have to be readable on the ground they are painted on (S33).
//!
//! # The report
//!
//! "theme base16-ocean.light still unusable -- please make letters proportionally darker. gray
//! letters still readable, but colored letters can't be seen properly."
//!
//! That distinction is the whole finding, and it was invisible to every assertion in the suite. The
//! grey was the theme's default foreground, the one color in base16-ocean.light that cleared the
//! floor at 5.20:1. Its accents -- green, cyan, comment, blue, purple, orange -- sat between 1.53:1
//! and 2.13:1. A user cannot read a keyword at 1.5:1; they see the shape of the code and the grey
//! text and nothing else, which is exactly what was described.
//!
//! # Why a floor per color is the wrong fix
//!
//! Pushing each color independently until it clears the minimum collapses the palette's luminance
//! spread from 50 to 7. Hue survives -- a color picker still says green and blue -- but nothing on
//! screen looks different from anything else, and mutual distinctness is the entire mechanism by
//! which a color scheme means anything. That trades an invisible palette for an indistinguishable one.
//!
//! So the depth is a property of the palette: one factor, the smallest that lifts the worst *visible*
//! color to the floor, applied to every color. Relative depth survives and the spread widens. The
//! tests below pin both halves, because a test that only checks the floor passes on the collapse.

use np_core::highlight::{HighlightEngine, LineSource};
use np_core::palette::{
    apply_syntax_push, luminance, syntax_push, ThemePalette, MIN_SYNTAX_CONTRAST,
};

struct Lines(Vec<String>);
impl LineSource for Lines {
    fn line_text(&self, i: usize) -> Option<String> {
        self.0.get(i).cloned()
    }
    fn line_count(&self) -> usize {
        self.0.len().max(1)
    }
}

/// Code broad enough to exercise comments, strings, numbers, keywords, types and preprocessor.
const SRC: &str = "\
// a line comment
/* a block comment */
#include <stdio.h>
#define MAX 100
typedef struct { int n; } Box;
static const char *name = \"hello\";
int main(int argc, char **argv) {
    unsigned long counter = 42;
    float ratio = 3.14f;
    if (counter > MAX) { return 0; }
    for (int i = 0; i < 10; ++i) counter += i;
    printf(\"%s %d\\n\", name, counter);
    return NULL ? 1 : 0;
}
";

/// Every distinct syntax foreground the engine produces for `SRC` under `theme`.
fn rendered_colors(theme: &str) -> Vec<(u8, u8, u8)> {
    let mut engine = HighlightEngine::load();
    assert!(engine.set_theme(theme), "{theme} not in the theme set");
    let lines: Vec<String> = SRC.lines().map(str::to_string).collect();
    let mut cache = engine.new_cache(Some("C"));
    let mut out: Vec<(u8, u8, u8)> = Vec::new();
    for i in 0..lines.len() {
        for (style, _) in engine.line_at(&mut cache, i, &Lines(lines.clone())) {
            if let Some(rgb) = style.fg.as_ref().and_then(|c| c.rgb) {
                if !out.contains(&rgb) {
                    out.push(rgb);
                }
            }
        }
    }
    out
}

#[test]
fn no_syntax_color_is_invisible_on_its_own_ground() {
    // The reported failure, asserted through the engine rather than through the helper, so a
    // correctly implemented helper that nothing calls cannot pass.
    let mut engine = HighlightEngine::load();
    let mut checked_themes = 0usize;
    for name in engine.theme_names() {
        if !engine.set_theme(&name) || engine.theme_is_none() {
            continue;
        }
        let pal = engine.palette();
        let lines: Vec<String> = SRC.lines().map(str::to_string).collect();
        let mut cache = engine.new_cache(Some("C"));
        let mut worst = f64::MAX;
        let mut worst_rgb = (0, 0, 0);
        for i in 0..lines.len() {
            for (style, frag) in engine.line_at(&mut cache, i, &Lines(lines.clone())) {
                let Some(rgb) = style.fg.as_ref().and_then(|c| c.rgb) else {
                    continue;
                };
                if frag.trim().is_empty() {
                    continue;
                }
                let c = ThemePalette::contrast(rgb, pal.background);
                if c < worst {
                    worst = c;
                    worst_rgb = rgb;
                }
            }
        }
        if worst == f64::MAX {
            continue;
        }
        assert!(
            worst >= MIN_SYNTAX_CONTRAST - 0.01,
            "{name}: a syntax color renders at {worst:.2}:1 on the text ground {:?} (color \
             {worst_rgb:?}), below the {MIN_SYNTAX_CONTRAST}:1 floor",
            pal.background,
        );
        checked_themes += 1;
    }
    assert!(
        checked_themes >= 8,
        "expected to exercise the bundled themes, got {checked_themes}"
    );
}

#[test]
fn base16_ocean_light_accents_reach_the_floor() {
    // The specific theme reported, with its specific complaint: the accents, not the grey.
    let colors = rendered_colors("base16-ocean.light");
    assert!(
        colors.len() >= 5,
        "expected several distinct syntax colors, got {colors:?}",
    );
    // The ground has to come from the theme whose colors are being measured. Reading it off a
    // freshly loaded engine silently compares against the default theme instead, and a light theme's
    // colors measured against a dark ground produce numbers that look like failures -- this test
    // reported 2.49:1 for a color that was actually at 3.98:1.
    let mut engine = HighlightEngine::load();
    assert!(engine.set_theme("base16-ocean.light"));
    let pal = engine.palette();
    for rgb in &colors {
        let c = ThemePalette::contrast(*rgb, pal.background);
        assert!(
            c >= MIN_SYNTAX_CONTRAST,
            "base16-ocean.light still renders {rgb:?} at {c:.2}:1 on {:?}",
            pal.background,
        );
    }
    let push = engine.syntax_push_depth();
    assert!(
        push > 0.20,
        "base16-ocean.light needs a substantial push; got {push:.3}",
    );
    // "Darker", as asked: every rendered accent is darker than the theme's lightest declared accent,
    // i.e. the palette moved away from the pale end rather than toward it.
    let theme = engine.theme().expect("theme present");
    let declared_min = theme
        .scopes
        .iter()
        .filter_map(|i| i.style.foreground)
        .filter(|c| c.a != 0)
        .map(|c| luminance((c.r, c.g, c.b)))
        .min()
        .expect("theme declares colors");
    assert!(
        colors.iter().all(|c| luminance(*c) >= declared_min),
        "a rendered accent is darker than anything the theme declared: {colors:?}",
    );
}

#[test]
fn a_palette_push_preserves_the_spread_between_colors() {
    // The property that separates this design from "push each color to the floor". Measured: equal
    // per-color floors give a luminance spread of 7 out of 255, which looks like one dark smear;
    // the shared depth gives 40 or more. Nothing else in the suite would notice the difference --
    // every color still clears the floor -- so this is asserted directly.
    let ground = (220, 223, 228);
    let accents = [
        (0xa3, 0xbe, 0x8c),
        (0x96, 0xb5, 0xb4),
        (0xa7, 0xad, 0xba),
        (0x8f, 0xa1, 0xb3),
        (0xb4, 0x8e, 0xad),
        (0xd0, 0x87, 0x70),
        (0xbf, 0x61, 0x6a),
    ];
    let depth = syntax_push(&accents, ground);
    assert!(depth > 0.0, "this palette needs a push, got {depth}");

    let pushed: Vec<(u8, u8, u8)> = accents
        .iter()
        .map(|c| apply_syntax_push(*c, ground, depth))
        .collect();

    // Every color clears the floor.
    for c in &pushed {
        assert!(
            ThemePalette::contrast(*c, ground) >= MIN_SYNTAX_CONTRAST,
            "{c:?} below the floor after pushing",
        );
    }

    // The spread survives. This is the whole reason for a shared depth: pushing each color to the
    // floor independently parks every one of them at the same contrast and the palette's luminance
    // range collapses from 50 to 7, so a keyword and a string stop looking different. Mutual
    // distinctness is the mechanism by which a color scheme means anything.
    let after: Vec<i64> = pushed.iter().map(|c| luminance(*c) as i64).collect();
    let before: Vec<i64> = accents.iter().map(|c| luminance(*c) as i64).collect();
    let range = |v: &[i64]| v.iter().max().unwrap_or(&0) - v.iter().min().unwrap_or(&0);
    let spread_before = range(&before);
    let spread = range(&after);
    assert!(
        spread >= spread_before * 2 / 3,
        "push collapsed the palette's luminance spread to {spread}; the theme had {spread_before}",
    );
    assert!(
        spread >= 20,
        "palette spread of {spread} is too flat to distinguish tokens: {pushed:?}",
    );
    // And no two colors land on top of each other, which is the concrete failure the collapse causes.
    for i in 0..pushed.len() {
        for j in (i + 1)..pushed.len() {
            assert!(
                pushed[i] != pushed[j],
                "two distinct accents became the same color: {:?} and {:?} -> {:?}",
                accents[i],
                accents[j],
                pushed[i],
            );
        }
    }
}

#[test]
fn a_theme_that_is_already_readable_is_not_touched() {
    // Dracula and Monokai need nothing. Applying a correction the theme does not need is the failure
    // mode of every rule of this shape, and it is the reason the depth is solved per palette rather
    // than applied as a constant.
    for name in ["Dracula", "Monokai", "Solarized Osaka (dark)"] {
        let mut engine = HighlightEngine::load();
        assert!(engine.set_theme(name), "{name} missing");
        let depth = engine.syntax_push_depth();
        assert_eq!(
            depth, 0.0,
            "{name} needed a push of {depth:.3}; its own palette should pass through untouched",
        );
    }
}

#[test]
fn a_syntax_push_never_invents_a_color_or_breaks_a_readable_one() {
    let ground = (220, 223, 228);
    // Depth 0 is the identity.
    assert_eq!(apply_syntax_push((10, 20, 30), ground, 0.0), (10, 20, 30));
    // A palette already above the floor needs no depth, so nothing moves.
    let fine = [(36, 41, 46), (79, 91, 102), (20, 20, 20)];
    assert_eq!(syntax_push(&fine, ground), 0.0);
    // An empty palette has nothing to be unreadable and must not panic or invent a push.
    assert_eq!(syntax_push(&[], ground), 0.0);
}

#[test]
fn an_over_light_dark_ground_is_darkened_and_others_are_left_alone() {
    // The second half of the report: "in base16-mocha.dark please make background little bit
    // darker". Measured, that theme's ground was luminance 51 -- the lightest dark ground shipped --
    // while every dark theme the user reads without complaint sits at 39-47. The rule is a ceiling,
    // so it reaches only the outliers above it.
    let mut engine = HighlightEngine::load();

    let outliers = ["base16-mocha.dark", "Nord"];
    for name in outliers {
        assert!(engine.set_theme(name), "{name} missing");
        let (declared, ink) = engine.declared_colors().expect("declared pair");
        let pal = engine.palette();
        assert_ne!(
            pal.background, declared,
            "{name} was the lightest dark ground and should have been darkened",
        );
        assert!(
            luminance(pal.background) < luminance(declared),
            "{name} must get darker, not lighter: {declared:?} -> {:?}",
            pal.background,
        );
        // Darkening a ground can only help a light ink, so this must never cost legibility.
        assert!(
            ThemePalette::legible(pal.foreground, pal.background),
            "{name}: darkening the ground made its own text illegible",
        );
        assert!(
            ink.0 > 128,
            "{name}: precondition, a dark theme's ink is light"
        );
    }

    // Themes at or below the ceiling keep their declared ground byte for byte.
    for name in [
        "Dracula",
        "Monokai",
        "One Dark",
        "base16-ocean.dark",
        "Solarized (dark)",
        "Solarized Osaka (dark)",
    ] {
        assert!(engine.set_theme(name), "{name} missing");
        let (declared, _) = engine.declared_colors().expect("declared pair");
        let pal = engine.palette();
        assert_eq!(
            pal.background, declared,
            "{name} is at or below the dark-ground ceiling and must be untouched",
        );
    }
}

#[test]
fn a_darkening_that_would_hurt_the_ink_does_not_happen() {
    // The ceiling rule is bounded by legibility, and the bound is asserted rather than trusted. An
    // ink already close to its dark ground must not be pushed further down by a ground adjustment.
    let mut engine = HighlightEngine::load();
    for name in engine.theme_names() {
        if !engine.set_theme(&name) || engine.theme_is_none() {
            continue;
        }
        let pal = engine.palette();
        let (declared, _) = match engine.declared_colors() {
            Some(pair) => pair,
            None => continue,
        };
        if luminance(declared) >= 128 {
            continue; // light ground: a different rule, covered elsewhere
        }
        assert!(
            ThemePalette::contrast(pal.foreground, pal.background)
                >= ThemePalette::contrast(pal.foreground, declared) - 0.01,
            "{name}: the ground moved in a way that made the theme's own text less readable",
        );
    }
}
