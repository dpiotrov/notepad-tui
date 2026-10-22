//! S29: the themes shipped on top of syntect's defaults.
//!
//! The user asked for "more good looking code highlighting themes, for example like Dracula in
//! VS-Code and in CLion". The failure modes here are all silent ones: a `.tmTheme` that syntect
//! refuses to parse is logged and skipped, so the theme simply does not appear and everything else
//! keeps working; a theme that parses but omits a scope renders that scope in the default
//! foreground, so comments look like code and nobody can tell a palette bug from a grammar bug.
//!
//! Both are asserted directly. The palette *values* are checked against the published schemes for
//! a few representative scopes, because a theme that loads but has the wrong colors is exactly the
//! thing a user notices and a reviewer cannot see in a diff of generated XML.

use np_core::highlight::{HighlightEngine, LineSource, THEME_NONE};
use np_core::CoreStyle;

/// Every theme this project adds, by picker name.
const BUNDLED: [&str; 6] = [
    "Dracula",
    "Monokai",
    "One Dark",
    "Nord",
    "Solarized Osaka (dark)",
    "GitHub Light",
];

fn engine() -> HighlightEngine {
    HighlightEngine::load()
}

struct Lines(Vec<String>);
impl LineSource for Lines {
    fn line_text(&self, line: usize) -> Option<String> {
        self.0.get(line).cloned()
    }
    fn line_count(&self) -> usize {
        self.0.len().max(1)
    }
}

/// A foreground color as it reaches the renderer: `(r, g, b)`.
type Rgb = (u8, u8, u8);

fn is_styled(st: &CoreStyle) -> bool {
    st.fg.is_some() || st.bg.is_some() || st.bold || st.italic || st.underline || st.dim
}

/// The distinct foreground colors a language produces under `theme`, one per styled span.
fn foregrounds(engine: HighlightEngine, theme: &str, lang: &str, src: &str) -> Vec<Rgb> {
    let mut e = engine;
    assert!(
        e.set_theme(theme),
        "set_theme rejected {theme}: the theme is not in the set"
    );
    let lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut cache = e.new_cache(Some(lang));
    let mut out = Vec::new();
    for i in 0..lines.len() {
        for (st, _frag) in e.line_at(&mut cache, i, &Lines(lines.clone())) {
            // Themes are hex, so they always arrive as truecolor; an indexed color here would
            // mean something upstream had changed how theme colors reach the renderer.
            if let Some(rgb) = st.fg.and_then(|c| c.rgb) {
                out.push(rgb);
            }
        }
    }
    out
}

#[test]
fn every_bundled_theme_is_listed_and_selectable() {
    // The load path logs and skips a theme syntect rejects, which is the right failure policy and
    // also invisible: the picker just has a hole where Dracula should be.
    let e = engine();
    let names = e.theme_names();
    for want in BUNDLED {
        assert!(
            names.iter().any(|n| n == want),
            "{want} is not in the theme list. Either the asset failed to parse (check the log) \
             or its name in BUNDLED_THEMES does not match this test. Available: {names:?}"
        );
    }
    // set_theme is the gate config and the picker both go through.
    for want in BUNDLED {
        let mut e = engine();
        assert!(e.set_theme(want), "{want} listed but set_theme refused it");
        assert_eq!(e.theme_name(), want);
    }
}

#[test]
fn the_bundled_defaults_are_still_present() {
    // A theme table that overwrote `base16-ocean.dark` would silently restyle every existing
    // config.toml. The loader logs and skips a collision; this asserts the collision never
    // happens in the first place.
    let e = engine();
    let names = e.theme_names();
    for keep in [
        THEME_NONE,
        "base16-ocean.dark",
        "base16-ocean.light",
        "base16-eighties.dark",
        "base16-mocha.dark",
        "Solarized (dark)",
        "Solarized (light)",
        "InspiredGitHub",
    ] {
        assert!(
            names.iter().any(|n| n == keep),
            "syntect's own theme {keep} disappeared: {names:?}"
        );
    }
}

#[test]
fn theme_names_are_unique() {
    let names = engine().theme_names();
    let mut sorted = names.clone();
    sorted.sort();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate theme names in {names:?}");
}

#[test]
fn each_bundled_theme_actually_colors_a_file() {
    // A theme can parse, load, and still be useless: if its scope entries never match, every
    // span comes back with no foreground and the editor looks like highlighting is off.
    let src = "// comment\nint x = 42;\nconst char* s = \"text\";\n";
    for theme in BUNDLED {
        let colors = foregrounds(engine(), theme, "C", src);
        assert!(
            colors.len() > 3,
            "{theme} produced almost no colored spans for a C file: {colors:?}",
        );
    }
}

#[test]
fn comments_are_distinguishable_from_the_base_foreground_in_every_theme() {
    // The specific silent failure the shared scope list exists to prevent. A palette missing its
    // `comment` entry does not fail to load: syntect falls back to the theme's base foreground,
    // so comments render as ordinary text. The result looks plausible, and nobody comparing a
    // single file would think to inspect a palette.
    //
    // The assertion is therefore made against the theme's own declared base foreground rather
    // than against "some code color" -- comparing against another span's color is what made the
    // first version of this test pass while a mutated palette painted comments in the base
    // foreground. The base foreground is read straight out of the parsed theme, so the two sides
    // of the comparison come from the same file and cannot drift.
    let src = "// only a comment\n";
    for theme in BUNDLED {
        let base = base_foreground(theme);
        let comment = foregrounds(engine(), theme, "C", src);
        assert!(
            !comment.is_empty(),
            "{theme}: a comment line came back with no foreground at all"
        );
        assert!(
            !comment.contains(&base),
            "{theme}: comment is painted in the theme's base foreground {base:?}, so comments \
             and code are indistinguishable -- the palette is probably missing its comment entry"
        );
    }
}

/// The theme's own declared base foreground, read from the asset on disk.
///
/// Read from the file rather than from the engine, and not through a new public accessor: the
/// point is to compare what the palette *declares* against what the engine *renders*, so pulling
/// both from the same loaded engine would make the comparison tautological. `BUNDLED_THEMES` is
/// deliberately private and this test has no business widening it.
fn base_foreground(theme: &str) -> Rgb {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("themes")
        .join(format!("{theme}.tmTheme"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let parsed: syntect::highlighting::Theme = syntect::highlighting::ThemeSet::load_from_reader(
        &mut std::io::Cursor::new(text.as_bytes()),
    )
    .unwrap_or_else(|e| panic!("{theme} does not parse: {e}"));
    let fg = parsed
        .settings
        .foreground
        .unwrap_or_else(|| panic!("{theme} declares no base foreground"));
    (fg.r, fg.g, fg.b)
}

#[test]
fn themes_are_not_all_the_same_colors() {
    // Guards against a palette copy-paste that left two picker entries rendering identically.
    let src = "// comment\nint x = 42;\nreturn \"text\";\n";
    let mut seen: Vec<(String, Vec<Rgb>)> = Vec::new();
    for theme in BUNDLED {
        let colors = foregrounds(engine(), theme, "C", src);
        for (other, prior) in &seen {
            assert_ne!(
                *prior, colors,
                "{theme} and {other} render identically; one of the palettes is a copy"
            );
        }
        seen.push((theme.to_string(), colors));
    }
}

#[test]
fn dracula_uses_its_published_palette() {
    // Dracula is the theme the user named, so its identity is checked against the published
    // values rather than against "some purple". Published: background #282a36, foreground
    // #f8f8f2, pink #ff79c6 (keywords), cyan #8be9fd (types), green #50fa7b (strings in the
    // original are yellow #f1fa8c), comment #6272a4.
    let kw = foregrounds(engine(), "Dracula", "C", "if (x) { return 1; }\n");
    assert!(
        kw.contains(&(255, 121, 198)),
        "Dracula keyword pink #ff79c6 not present; got {kw:?}",
    );
    let cm = foregrounds(engine(), "Dracula", "C", "// c\n");
    assert!(
        cm.contains(&(98, 114, 164)),
        "Dracula comment #6272a4 not present; got {cm:?}",
    );
    let st = foregrounds(engine(), "Dracula", "C", "const char* s = \"hi\";\n");
    assert!(
        st.contains(&(241, 250, 140)),
        "Dracula string yellow #f1fa8c not present; got {st:?}",
    );
}

#[test]
fn setting_a_theme_does_not_change_the_language_list() {
    // Themes and grammars are independent sets. A theme switch that disturbed the syntax set would
    // show up as languages vanishing mid-session.
    let mut e = engine();
    let before = e.language_names();
    e.set_theme("Monokai");
    assert_eq!(e.language_names(), before);
    e.set_theme(THEME_NONE);
    assert_eq!(e.language_names(), before);
}

#[test]
fn an_unknown_theme_is_still_refused() {
    // The pre-existing contract: a typo in config.toml must not strip all color.
    let mut e = engine();
    assert!(
        !e.set_theme("Drakula"),
        "a misspelling of Dracula was accepted"
    );
    assert!(!e.set_theme(""));
}

#[test]
fn theme_none_still_removes_all_color() {
    let mut none = engine();
    none.set_theme(THEME_NONE);
    let src = "// comment\nint x = 1;\n";
    let lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut cache = none.new_cache(Some("C"));
    for i in 0..lines.len() {
        let spans = none.line_at(&mut cache, i, &Lines(lines.clone()));
        assert!(
            spans.iter().all(|(st, _)| !is_styled(st)),
            "theme none still produced styling under Dracula-era set: {spans:?}"
        );
    }
    // And the same source is styled under a real theme, so the assertion above is not vacuous.
    let mut dracula = engine();
    dracula.set_theme("Dracula");
    let mut cache = dracula.new_cache(Some("C"));
    let any = (0..lines.len()).any(|i| {
        dracula
            .line_at(&mut cache, i, &Lines(lines.clone()))
            .iter()
            .any(|(st, _)| is_styled(st))
    });
    assert!(
        any,
        "Dracula produced no styling at all, which would make the `none` check above vacuous"
    );
}

#[test]
fn a_bundled_theme_name_survives_a_config_round_trip() {
    // Theme names are user-visible strings that land in `config.toml`, and several of the new
    // ones contain spaces and parentheses -- "One Dark", "Solarized Osaka (dark)". TOML quotes
    // them, so this should be a formality, but a theme that fails to come back is a user whose
    // editor silently reverts to the default on every start, and that is worth one test.
    for name in BUNDLED {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut cfg = np_core::Config::default();
        cfg.ui.theme = name.to_string();
        cfg.save(&path).expect("save");
        let (reloaded, warns) = np_core::Config::load(Some(&path));
        assert!(warns.is_empty(), "{name}: {warns:?}");
        assert_eq!(reloaded.ui.theme, name, "theme name did not round-trip");
        // And the reloaded name is one the engine will actually accept.
        let mut e = engine();
        assert!(
            e.set_theme(&reloaded.ui.theme),
            "{name} round-tripped but the engine refuses it"
        );
    }
}

#[test]
fn the_default_theme_is_still_the_documented_one() {
    // `DEFAULT_THEME` is the value an App falls back to when config names nothing, and several
    // tests elsewhere assert on its specific colors. Pinned so adding themes cannot quietly
    // change what a fresh install looks like.
    let e = engine();
    assert_eq!(e.theme_name(), np_core::highlight::DEFAULT_THEME);
    assert_eq!(np_core::highlight::DEFAULT_THEME, "base16-ocean.dark");
}
