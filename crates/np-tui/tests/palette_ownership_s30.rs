//! Every painted cell must name both of its colors (S30).
//!
//! # The bug this pins
//!
//! A ratatui cell may leave its foreground or background unset, and an unset channel is not
//! "a color that matches the theme" -- it is a color the *terminal* owns. Chrome, dialogs and
//! the text area all set a background and left the foreground (or the reverse) to the emulator,
//! so switching the terminal from a dark to a light palette repainted half of npt in a color
//! nobody had chosen: dark labels on the app's own dark menu band, a light caret on a light
//! field, syntax spans sitting on the emulator's default ground.
//!
//! # The invariant
//!
//! After a real `draw`, no cell in the frame may have `fg == None` or `bg == None`. That is a
//! stronger and simpler claim than checking individual widgets, and it is the actual property
//! the user asked for: the theme controls every pixel, so nothing is left to delegate.
//!
//! The test walks the whole screen for every theme and every overlay, because one unconverted
//! widget anywhere fails it. That is deliberate: the failure mode is a single line of code in a
//! single painter, and a test that only checked the menu bar would let the other forty sites
//! regress unnoticed.

use np_core::{Buffer, Config, LineCol, ThemePalette};
use np_tui::app::{DialogKind, Mode};
use np_tui::ui::render_to_buffer;
use np_tui::App;

const W: u16 = 100;
const H: u16 = 30;

/// Every bundled theme plus the explicit "no theme" spelling.
///
/// `none` matters most: with highlighting off the engine supplies no span colors at all, so
/// every visible color on screen has to come from the palette, and that is the configuration
/// where an unset channel is easiest to leave behind.
const THEMES: &[&str] = &[
    "base16-ocean.dark",
    "base16-ocean.light",
    "Solarized (dark)",
    "Solarized (light)",
    "Dracula",
    "Monokai",
    "Nord",
    "One Dark",
    "GitHub Light",
    "InspiredGitHub",
    "none",
];

fn app_with(theme: &str, text: &str) -> App {
    let mut cfg = Config::default();
    cfg.ui.theme = theme.to_string();
    let mut app = App::new_headless(cfg, (W, H));
    let doc = app.doc_mut().expect("a document");
    doc.buffer = Buffer::from_str_lossy(text);
    doc.set_cursor(LineCol::new(0, 0), false);
    app.recompute_geometry(W, H);
    app.dirty = true;
    app
}

/// Cells whose foreground or background is unset, as `(x, y, what)`.
fn unowned(buf: &ratatui::buffer::Buffer) -> Vec<(u16, u16, &'static str)> {
    let mut bad = Vec::new();
    for y in buf.area.y..buf.area.bottom() {
        for x in buf.area.x..buf.area.right() {
            let Some(cell) = buf.cell(ratatui::layout::Position { x, y }) else {
                continue;
            };
            // A continuation cell of a double-width glyph holds no symbol of its own and is never
            // emitted -- the leading cell's style governs the pair -- so it is not ours to own.
            // Detected by empty symbol rather than the `skip` field, which ratatui 0.30 deprecates
            // in favour of `set_diff_option(CellDiffOption::Skip)`.
            if cell.symbol().is_empty() {
                continue;
            }
            if cell.style().fg.is_none() {
                bad.push((x, y, "foreground"));
            } else if cell.style().bg.is_none() {
                bad.push((x, y, "background"));
            }
        }
    }
    bad
}

fn describe(bad: &[(u16, u16, &str)], theme: &str, surface: &str) -> String {
    let sample: Vec<String> = bad
        .iter()
        .take(8)
        .map(|(x, y, what)| format!("({x},{y}) missing {what}"))
        .collect();
    format!(
        "theme {theme:?} / {surface}: {} cells do not name both colors: {}",
        bad.len(),
        sample.join(", ")
    )
}

#[test]
fn the_plain_editor_frame_names_both_colors_everywhere() {
    // Text, gutter, tab bar, menu bar, status bar, scrollbar tracks and the empty ground past
    // the end of each line.
    let text = "fn main() {\n    // a comment\n    let x = 1;\n}\n";
    for theme in THEMES {
        let mut app = app_with(theme, text);
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(bad.is_empty(), "{}", describe(&bad, theme, "plain frame"));
    }
}

#[test]
fn a_selection_and_the_current_line_keep_both_colors() {
    for theme in THEMES {
        let mut app = app_with(theme, "alpha beta gamma\ndelta\n");
        app.execute(np_tui::Action::SelectAll);
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(
            bad.is_empty(),
            "{}",
            describe(&bad, theme, "selection + current line")
        );
    }
}

#[test]
fn every_dialog_names_both_colors() {
    // Each of these paints its own box, list, fields, buttons and hints, and each was converted
    // separately -- so each gets its own sweep.
    let kinds = [
        DialogKind::Open,
        DialogKind::SaveAs,
        DialogKind::GoTo,
        DialogKind::Confirm,
        DialogKind::Preferences,
        DialogKind::About,
        DialogKind::LanguagePicker,
        DialogKind::ThemePicker,
        DialogKind::DocumentList,
        DialogKind::EncodingPicker,
        DialogKind::EolMenu,
        DialogKind::RecentFiles,
    ];
    for theme in THEMES {
        for kind in kinds {
            let mut app = app_with(theme, "one\ntwo\nthree\n");
            app.push_mode(Mode::Dialog(kind));
            let buf = render_to_buffer(&mut app, W, H);
            let bad = unowned(&buf);
            assert!(
                bad.is_empty(),
                "{}",
                describe(&bad, theme, &format!("{kind:?} dialog"))
            );
        }
    }
}

#[test]
fn the_find_and_replace_strips_name_both_colors() {
    for theme in THEMES {
        for kind in [DialogKind::Find, DialogKind::Replace] {
            let mut app = app_with(theme, "needle in a haystack\nanother needle\n");
            app.push_mode(Mode::Dialog(kind));
            let buf = render_to_buffer(&mut app, W, H);
            let bad = unowned(&buf);
            assert!(
                bad.is_empty(),
                "{}",
                describe(&bad, theme, &format!("{kind:?} strip"))
            );
        }
    }
}

#[test]
fn an_open_menu_and_submenu_name_both_colors() {
    use np_tui::actions::MenuId;
    for theme in THEMES {
        for menu in [
            MenuId::File,
            MenuId::Edit,
            MenuId::Search,
            MenuId::View,
            MenuId::Encoding,
            MenuId::Language,
            MenuId::Settings,
            MenuId::Window,
            MenuId::Help,
        ] {
            let mut app = app_with(theme, "menu probe\n");
            app.push_mode(Mode::Menu {
                open: menu,
                item: 0,
                parent: None,
            });
            let buf = render_to_buffer(&mut app, W, H);
            let bad = unowned(&buf);
            assert!(
                bad.is_empty(),
                "{}",
                describe(&bad, theme, &format!("{menu:?} dropdown"))
            );
        }
    }
}

#[test]
fn the_command_palette_and_context_menu_name_both_colors() {
    for theme in THEMES {
        let mut app = app_with(theme, "palette probe\n");
        app.push_mode(Mode::Palette);
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(
            bad.is_empty(),
            "{}",
            describe(&bad, theme, "command palette")
        );

        let mut app = app_with(theme, "context probe\n");
        app.push_mode(Mode::ContextMenu(np_tui::app::ContextKind::Editor));
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(bad.is_empty(), "{}", describe(&bad, theme, "context menu"));

        let mut app = app_with(theme, "help probe\n");
        app.push_mode(Mode::HelpOverlay);
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(
            bad.is_empty(),
            "{}",
            describe(&bad, theme, "keyboard help overlay")
        );
    }
}

#[test]
fn a_toast_names_both_colors() {
    // A toast is drawn by a function that does not receive `App`, which is exactly how it lost
    // its colors: it had to be handed the palette explicitly.
    for theme in THEMES {
        let mut app = app_with(theme, "toast probe\n");
        app.timed_toasts.push(np_tui::ui::toast::TimedToast::info(
            "saved".to_string(),
            app.clock,
        ));
        let buf = render_to_buffer(&mut app, W, H);
        let bad = unowned(&buf);
        assert!(bad.is_empty(), "{}", describe(&bad, theme, "toast"));
    }
}

#[test]
fn the_too_small_screen_names_both_colors() {
    // Drawn before the editor can be laid out, so it is the one surface that could plausibly be
    // excused from owning its colors. It is not excused: the user sees it right after resizing,
    // and an unset ground there sits on whatever the emulator has.
    use ratatui::buffer::Buffer as RBuffer;
    for theme in THEMES {
        let app = app_with(theme, "");
        let pal = app.palette();
        for (w, h) in [(20, 4), (20, 10), (40, 8)] {
            let mut buf = RBuffer::empty(ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            });
            np_tui::event::draw_too_small(&mut buf, w, h, &pal);
            // Only the rows that were actually painted are claimed; the rest of a `Buffer::empty`
            // is untouched space that this screen never owns.
            let mut bad = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let Some(cell) = buf.cell(ratatui::layout::Position { x, y }) else {
                        continue;
                    };
                    if cell.symbol().is_empty() || cell.symbol() == " " {
                        continue;
                    }
                    if cell.style().fg.is_none() || cell.style().bg.is_none() {
                        bad.push((x, y));
                    }
                }
            }
            assert!(
                bad.is_empty(),
                "theme {theme:?} / too-small {w}x{h}: {bad:?} name no color"
            );
        }
    }
}

#[test]
fn the_frame_uses_theme_colors_not_the_terminal_defaults() {
    // The sharper half of the claim. A cell can name `Color::Reset` and still be terminal-owned,
    // so assert that every named color is a concrete RGB value from the palette family rather
    // than a reset or an ANSI index.
    for theme in THEMES {
        let mut app = app_with(theme, "reset probe\nsecond line\n");
        let buf = render_to_buffer(&mut app, W, H);
        for y in buf.area.y..buf.area.bottom() {
            for x in buf.area.x..buf.area.right() {
                let Some(cell) = buf.cell(ratatui::layout::Position { x, y }) else {
                    continue;
                };
                if cell.symbol().is_empty() {
                    continue;
                }
                for (what, c) in [("fg", cell.style().fg), ("bg", cell.style().bg)] {
                    match c {
                        Some(ratatui::style::Color::Rgb(..)) => {}
                        other => panic!(
                            "theme {theme:?}: cell ({x},{y}) {what} is {other:?}, not an RGB \
                             color from the theme"
                        ),
                    }
                }
            }
        }
    }
}

#[test]
fn switching_theme_changes_the_chrome_colors() {
    // Guards against a palette that ignores the theme: if every theme produced the same frame
    // colors, the tests above would pass while a light theme was still unreadable.
    let dark = {
        let mut app = app_with("Dracula", "probe\n");
        let buf = render_to_buffer(&mut app, W, H);
        buf.cell(ratatui::layout::Position { x: 2, y: 0 })
            .map(|c| (c.style().fg, c.style().bg))
    };
    let light = {
        let mut app = app_with("GitHub Light", "probe\n");
        let buf = render_to_buffer(&mut app, W, H);
        buf.cell(ratatui::layout::Position { x: 2, y: 0 })
            .map(|c| (c.style().fg, c.style().bg))
    };
    assert_ne!(
        dark, light,
        "the menu bar painted identically for both themes"
    );
}

#[test]
fn ink_is_legible_on_every_surface_the_theme_can_support() {
    // The property the user actually experiences: ink you can read. Checked through the palette
    // the renderer uses, not a re-derivation of it.
    //
    // Two different standards apply, and the difference is the finding worth keeping. Surfaces
    // that own both of their colors -- the selection bar, a highlighted row -- must clear the full
    // 4.5:1, and do, because the palette solves for an accent that reaches it. Surfaces that are a
    // *tint* of the theme's own background cannot: a tint spends part of the theme's contrast
    // budget on being visible at all, so Solarized (dark)'s 4.75:1 pair yields at most 4.05:1 on
    // the chrome band. Demanding 4.5 there would be asking for a color that does not exist, so
    // those assertions use the theme's own ceiling via `tinted_contrast_floor`.
    for theme in THEMES {
        if theme.eq_ignore_ascii_case("none") {
            continue;
        }
        let app = app_with(theme, "legibility probe\n");
        let pal = app.palette();
        // The ceiling comes from the theme's *declared* pair, not from the palette under test.
        // Reading it off `pal` makes the drift assertion below `x - x < 0.01`: true whatever the
        // palette does. That is exactly how S30 shipped a check that could not fail, and S31's
        // light-ground deepening -- a real change to this contrast -- passed through it unnoticed.
        let (declared_bg, declared_fg) = app
            .highlight
            .declared_colors()
            .unwrap_or(((24, 24, 30), (220, 220, 224)));
        let ceiling = np_core::ThemePalette::contrast(declared_fg, declared_bg);

        // The selection bar carries the full requirement: the palette chooses both of its colors,
        // so there is no theme limitation to blame if it is unreadable.
        assert!(
            np_core::ThemePalette::legible(pal.highlight_fg, pal.highlight_bg),
            "{theme}: selection-bar ink is not legible ({:?} on {:?}, ratio {:.2})",
            pal.highlight_fg,
            pal.highlight_bg,
            np_core::ThemePalette::contrast(pal.highlight_fg, pal.highlight_bg),
        );

        // Editor text vs the theme's declared pair. The rule changed in S31 and is narrow: a
        // *light* ground may be deepened for reading comfort, and only as far as the theme's own
        // ink still clears the legibility floor. Anything else is drift.
        //
        // So the check is not "unchanged" (that would fail the fix it is meant to permit) and not
        // "equal to the palette" (that proves nothing). It is: the ground is the theme's, or the
        // ground is exactly what `text_ground` would derive from the theme's, and in both cases the
        // ink is still at least as readable as the theme allowed.
        // Hardcoded, on purpose. Computing the expected ground with `text_ground` makes this
        // assert that a function equals itself: raising the depth constant to 0.45 and deleting the
        // below-floor guard -- which does crush Solarized (light) to 3.1:1 -- passed a version of
        // this check written that way. Only a literal pins the value.
        let expected_bg: (u8, u8, u8) = match *theme {
            // Light grounds with contrast budget: deepened for reading comfort.
            "GitHub Light" => (229, 229, 230),
            "InspiredGitHub" => (230, 230, 230),
            "base16-ocean.light" => (220, 223, 228),
            // Light ground under the floor already: must keep the author's cream ground.
            "Solarized (light)" => (253, 246, 227),
            // Dark grounds at or below the luminance ceiling: never moved.
            "base16-ocean.dark" => (43, 48, 59),
            "Solarized (dark)" => (0, 43, 54),
            "Dracula" => (40, 42, 54),
            "Monokai" => (39, 40, 34),
            "One Dark" => (40, 44, 52),
            // Dark grounds above the ceiling, pulled down to it. Nord and base16-mocha.dark were the
            // two outliers at luminance 51; the themes the user reads without complaint sit at 39-47.
            "Nord" => (41, 47, 58),
            "base16-mocha.dark" => (53, 45, 36),
            _ => declared_bg,
        };
        assert_eq!(
            pal.background, expected_bg,
            "{theme}: editor ground is {:?}, expected {expected_bg:?} (theme declared \
             {declared_bg:?})",
            pal.background,
        );
        // The ink. S31 said npt would not touch it; S32 had to walk that back, because a deeper
        // overlay ramp -- which is what the reported "bad contrast" actually needed -- put tints on
        // screen that a thin theme's ink cannot carry. The rule as it now stands is narrower and
        // worth stating exactly: npt may deepen the ink, only when the theme's own ink cannot reach
        // the legibility floor on a surface npt drew, and only toward darker/lighter, never in hue.
        //
        // So this asserts the *permitted* relation rather than equality, plus the specific case
        // where no permission exists: a theme with contrast headroom must keep its ink exactly.
        let ink_moved = pal.foreground != declared_fg;
        // The surface that decides is the deepest tint in the ramp, not the plain ground: an ink can
        // read fine on the ground and fail on the current-line band, which is precisely the case
        // that justifies deepening it. Checking the ground alone would forbid the rescue while
        // claiming to permit it.
        let deepest = np_core::mix(pal.background, declared_fg, 0.45);
        let theme_ink_carries_the_ramp = ThemePalette::legible(declared_fg, pal.current_line_bg)
            && ThemePalette::legible(declared_fg, deepest);
        if theme_ink_carries_the_ramp {
            assert!(
                !ink_moved,
                "{theme}: npt changed the theme's ink ({declared_fg:?} -> {:?}) when the theme's \
                 own ink was already legible on the ground. Deepening is a legibility rescue, not a \
                 style preference.",
                pal.foreground,
            );
        }
        assert!(
            ThemePalette::contrast(pal.foreground, pal.background)
                >= ThemePalette::contrast(declared_fg, pal.background) - 0.01,
            "{theme}: deepening the ink made it *less* legible than the theme's own",
        );
        let ink_now = np_core::ThemePalette::contrast(pal.foreground, pal.background);
        if ceiling < np_core::MIN_UI_CONTRAST {
            // A theme that ships below the floor -- Solarized (light) declares 4.13:1, a deliberate
            // low-glare choice -- must not be pushed further down. Its ground is untouchable.
            assert!(
                ink_now + 0.01 >= ceiling,
                "{theme}: editor text got worse than the theme's own {:.2}:1",
                ceiling,
            );
        } else {
            assert!(
                ink_now + 0.01 >= np_core::MIN_UI_CONTRAST,
                "{theme}: deepened ground left editor text at {ink_now:.2}:1, below the \
                 {:.1}:1 floor (theme ceiling {ceiling:.2}:1)",
                np_core::MIN_UI_CONTRAST,
            );
        }

        // Ceiling-relative requirement: these are tints off the editor ground.
        for (what, fg, bg, fraction) in [
            ("menu/tab/status ink", pal.chrome_fg, pal.chrome_bg, 0.11),
            ("inactive tab ink", pal.chrome_dim_fg, pal.chrome_bg, 0.11),
            ("dialog ink", pal.dialog_fg, pal.dialog_bg, 0.04),
            ("line numbers", pal.gutter_fg, pal.gutter_bg, 0.0),
            (
                "current-line ink",
                pal.foreground,
                pal.current_line_bg,
                0.14,
            ),
            ("focused-field ink", pal.dialog_fg, pal.field_focus_bg, 0.20),
        ] {
            // The floor is 4.5:1 for a theme with room to spare, and the theme's own ceiling for
            // one without. A result above the floor is fine -- `dim_ink` stops at the target
            // rather than maximizing, so most surfaces land near 4.5 by design.
            let floor = np_core::tinted_contrast_floor(pal.background, pal.foreground, fraction);
            let got = np_core::ThemePalette::contrast(fg, bg);
            assert!(
                got + 0.05 >= floor,
                "{theme}: {what} reads {got:.2}:1 on {:?}, below the {floor:.2}:1 this theme \
                 allows (theme ceiling {ceiling:.2}:1)",
                bg,
            );
        }
    }
}
