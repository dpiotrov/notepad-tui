//! Incremental syntax highlighting on syntect (spec §6.6, contract §7).
//!
//! Two types. [`HighlightEngine`] owns the syntax and theme sets and is built once at
//! startup. [`HighlightCache`] is per-document: styled lines plus the parse-state
//! checkpoints that make re-highlighting after an edit cheap.
//!
//! # Why replay, and why checkpoints are the whole design
//!
//! Syntax is stateful. `/*` colors every line after it, and a Rust raw string can run two
//! hundred lines. Lexing a viewport in isolation would color the middle of a block comment
//! as code, which is the most visibly wrong thing a highlighter can do. So a line's style is
//! only produced by replaying the parser from a saved state at or above the line, and
//! FR-H4's "invalidate from the edited line to the next checkpoint" is the same fact seen
//! from the write side.
//!
//! Checkpoints bound a *cold* replay at `CHECKPOINT_EVERY` lines. That alone is not enough,
//! and the trap is worth stating plainly: a checkpoint is only useful if the text between it
//! and the target has already been parsed, so a replay that starts at a checkpoint must
//! still read every line between there and the target. Reaching line 500 000 from the
//! nearest checkpoint costs up to 128 lexes; reaching it with *no* checkpoint above line 0
//! costs 500 000. Hence two things the contract does not mention:
//!
//! - a [`LineSource`], so a replay can read the lines it walks over rather than only the one
//!   it was asked for. Without it the cache cannot replay at all, and "highlight line 500
//!   000" would either be a lie or a full-document scan;
//! - a forward **anchor** on the cache, the first line the last walk left off at, whose
//!   parser state is carried in a checkpoint. Scroll-driven access is monotone, so a walk
//!   resumes from the anchor and a scroll down a 10 MB file lexes each line once.
//!
//! # Memory
//!
//! Cached styles are evicted outside a window around the viewport ([`CACHE_WINDOW`]), so the
//! cache tracks what the user has looked at rather than the size of the file. Checkpoints
//! are the exception: they are `Clone` but not `Serialize`, and a bounded count is what
//! guarantees a bounded replay, so they persist. Both are stated in the numbers below.
//!
//! # What is *not* here
//!
//! No threads, and no syntect state leaves this module. `np-tui` receives
//! `Vec<(CoreStyle, String)>`, which is ratatui-free by construction.

use std::collections::BTreeMap;
use std::path::Path;

use syntect::dumps::from_uncompressed_data;
use syntect::easy::HighlightLines;
use syntect::highlighting::{HighlightState, Theme, ThemeSet};
use syntect::parsing::{ParseState, SyntaxReference, SyntaxSet};

use crate::style::{self, CoreStyle};

/// Lines between parse-state checkpoints (FR-H4).
pub const CHECKPOINT_EVERY: usize = 128;

/// Theme name meaning "no highlighting" (FR-H3).
pub const THEME_NONE: &str = "none";

/// The language name that means "no highlighting" in the picker (FR-H1, FR-H2).
pub const PLAIN_TEXT: &str = "Plain Text";

/// The language name for CMake scripts (FR-H1).
///
/// The name is what the picker lists and what a `LanguageId::Named` value must carry, so it has
/// to match the `name:` field of the vendored grammar exactly. It is asserted against the loaded
/// syntax set in `cmake_grammar_s28.rs` rather than trusted.
pub const CMAKE_LANGUAGE: &str = "CMake";

/// The syntax set this build ships, as prebuilt syntect dump bytes.
///
/// syntect's default set carries no CMake grammar: its bundled syntaxes have none, and
/// `find_syntax_by_extension("cmake")` answers `None`. So CMake has to be merged into the set,
/// and this is where the merged result lives -- see `HighlightEngine::build_syntax_set` for why
/// the merge happens offline in `tools/gen_syntax_set` rather than here at startup.
///
/// # The chain from upstream to these bytes
///
/// Upstream (twxs/vs.language.cmake, MIT, Copyright (c) 2015 Nicolas MARTIN) publishes a VSCode
/// TextMate plist. syntect parses Sublime `.sublime-syntax` shape and its `plist-load` feature
/// is wired to *metadata* loading only, so the plist cannot be loaded directly -- syntect
/// reports only `MissingMandatoryKey("contexts")`. So there are two steps, both reproducible:
///
/// 1. `tools/convert_textmate_grammar.py` rewrites the plist into
///    `assets/CMake.sublime-syntax.yaml`, mapping the three shape differences that matter
///    (`scopeName`/`scope`, `fileTypes`/`file_extensions`, `patterns`/`contexts.main`, and each
///    pattern's `name`/`scope`), expanding `begin`/`end` into push/pop contexts, and
///    case-folding the regexes as described below.
/// 2. `tools/gen_syntax_set` merges that YAML into syntect's defaults and writes
///    `assets/syntax_set.packdump`, the file embedded here.
///
/// Both must be re-run, in that order, when upstream changes. `gen_syntax_set` exits nonzero if
/// the set it produces has lost CMake or any of a probe list of bundled languages, so a broken
/// asset cannot be committed quietly -- but it cannot detect a *stale* one, so the test in
/// `cmake_grammar_s28.rs` checks behavior rather than timestamps.
///
/// # Why `CMakeLists.txt` is not detected
///
/// The grammar lists the `cmake` extension only. `CMakeLists.txt` carries `txt`, and claiming
/// `txt` for CMake would relabel every text file in the tree, so it opens as Plain Text and the
/// user chooses CMake from the language picker. That is the requested behavior, and it is
/// asserted rather than assumed.
///
/// # Why the patterns are lowercase
///
/// fancy-regex takes about 76 ms per pattern to compile a long alternation carrying a
/// case-insensitivity flag. This grammar has thirteen of them -- CMake keyword lists of up to
/// 115 alternatives, deliberately case-insensitive because CMake commands are -- and loading
/// them as upstream wrote them costs 1.13 s against the 1.2 ms the rest of the set takes. Even
/// prebuilt, that cost would be paid on first use, so folding is not made redundant by the dump.
///
/// The converter therefore emits patterns already-lowercased with `(?i:` rewritten to `(?:`,
/// and `for_syntax` feeds syntect a lowercased line. Equivalence was measured, not assumed: the
/// folded grammar over lowercased input reproduces the original grammar's scope stack exactly
/// across bracket comments, block entry and exit, and mixed-case keywords, and across every
/// pattern in this grammar against 40 probes (560 pairs, zero mismatches).
///
/// Two patterns are deliberately *not* folded: the terminators of CMake bracket comments, which
/// use a numbered backreference. Folding those would compare folded pattern text against case
/// taken from the document, and fancy-regex cannot compile such a reference in any case.
const SYNTAX_SET: &[u8] = include_bytes!("../assets/syntax_set.packdump");

/// The themes this project ships, on top of syntect's seven.
///
/// Each entry is `(picker name, theme text)`. The name is what the picker lists, what
/// `config.toml` stores, and what [`HighlightEngine::set_theme`] looks up, so it is the public
/// identity of the theme and is written out here rather than derived from a file name -- a
/// rename would otherwise be an invisible config-breaking change.
///
/// # Why embedded text rather than a folder on disk
///
/// syntect has `ThemeSet::load_from_folder`, and it is the obvious call. It reads the user's
/// filesystem, which means the set of themes available depends on what happens to be installed,
/// and a missing folder is invisible until someone looks for a theme that is not there. Parsing
/// committed text costs about 85 us per theme -- 508 us for all six, against the 377 us
/// `ThemeSet::load_defaults()` already takes -- so embedding buys a deterministic feature list
/// for a cost that does not register.
///
/// # Where they come from
///
/// Every palette is a published color scheme, not an invention, and the values are the ones those
/// schemes publish for these scopes: Dracula (dracula.io), Monokai (Wimer Hazenberg), One Dark
/// (the Atom editor), Nord (Arctic Ice Studio), a Solarized-revision dark, and GitHub Light. The
/// list is deliberately not all-dark: a bright terminal needs a bright theme, and every dark
/// palette on a light background is unreadable in a way no keyword colouring fixes.
///
/// Generated by `tools/gen_themes.py`, which keeps the palette readable and the plist markup out
/// of review's way. Re-run that after editing a palette.
///
/// # The scope set is identical across all of them
///
/// Same scopes, same order, colors differing only by value. A theme that omits `comment` does not
/// fail to load -- it falls back to the theme foreground, so comments render as code. That is a
/// plausible-looking result that would never send anyone to look at the palette, so the generator
/// builds every theme from one shared scope list and a missing color is impossible by construction.
const BUNDLED_THEMES: &[(&str, &str)] = &[
    ("Dracula", include_str!("../assets/themes/Dracula.tmTheme")),
    ("Monokai", include_str!("../assets/themes/Monokai.tmTheme")),
    (
        "One Dark",
        include_str!("../assets/themes/One Dark.tmTheme"),
    ),
    ("Nord", include_str!("../assets/themes/Nord.tmTheme")),
    (
        "Solarized Osaka (dark)",
        include_str!("../assets/themes/Solarized Osaka (dark).tmTheme"),
    ),
    (
        "GitHub Light",
        include_str!("../assets/themes/GitHub Light.tmTheme"),
    ),
];

/// Parse the bundled themes into `into`, leaving syntect's defaults in place.
///
/// A theme that fails to parse is logged and skipped rather than aborting the load: losing one
/// theme out of thirteen is a worse outcome than not starting, and the picker still shows the
/// rest. The name is taken from this table rather than from the theme's own `name` field, so two
/// themes cannot silently collide on a name the file happens to carry -- and a collision against
/// one of syntect's defaults is logged, because overwriting `base16-ocean.dark` would change
/// every existing user's configured theme.
fn add_bundled_themes(into: &mut ThemeSet) {
    for (name, text) in BUNDLED_THEMES {
        match ThemeSet::load_from_reader(&mut std::io::Cursor::new(text.as_bytes())) {
            Ok(theme) => {
                if into.themes.contains_key(*name) {
                    log::warn!("bundled theme {name} collides with an existing theme; skipped");
                    continue;
                }
                into.themes.insert((*name).to_string(), theme);
            }
            Err(e) => log::warn!("could not load bundled theme {name}: {e}"),
        }
    }
}

/// Fallback theme when config names one that does not exist.
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

/// How far outside the requested line cached styles are kept.
///
/// Without this a cache grows with the file, and scrolling to the end of a 100 MB log breaks
/// spec §9's "4 x file size" rule. Eviction keeps the cache proportional to what has been
/// displayed, which is bounded by the session rather than by the document.
pub const CACHE_WINDOW: usize = 2_000;

/// Read-only view of the document text, so the cache can replay without owning a `Buffer`.
///
/// A replay needs the lines it walks over, not just the requested one. Keeping this a trait
/// means a test can hand the highlighter a `Vec<String>` and the engine never learns that a
/// `Buffer` is a rope.
pub trait LineSource {
    /// The line's text without its end-of-line terminator, or `None` past the end.
    fn line_text(&self, line: usize) -> Option<String>;
    /// Total line count, at least 1.
    fn line_count(&self) -> usize;
}

impl LineSource for crate::buffer::Buffer {
    fn line_text(&self, line: usize) -> Option<String> {
        if line < self.line_count() {
            Some(self.line_string(line))
        } else {
            None
        }
    }

    fn line_count(&self) -> usize {
        self.line_count()
    }
}

/// Syntax and theme state, built once at startup (FR-H1).
///
/// Loading both sets costs about 1.2 ms, which is cheaper than spawning a thread to do it,
/// so this runs on the UI thread during startup rather than in a worker.
pub struct HighlightEngine {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    theme_name: String,
}

impl Default for HighlightEngine {
    fn default() -> Self {
        Self::load()
    }
}

/// Summary form. A syntect `SyntaxSet` holds every bundled grammar, so a derived `Debug`
/// would dump roughly the whole crate's data into a test failure message.
impl std::fmt::Debug for HighlightEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightEngine")
            .field("theme", &self.theme_name)
            .field("syntaxes", &self.syntax_set.syntaxes().len())
            .field("themes", &self.theme_set.themes.len())
            .finish()
    }
}

impl HighlightEngine {
    /// Load the bundled syntax and theme sets, plus the grammars this project vendors.
    ///
    /// # Why a builder pass rather than the default set as-is
    ///
    /// Load the syntax set, the theme set, and the themes this project ships.
    ///
    /// Both sets come from embedded bytes rather than from disk, so a user's `~/.config` and
    /// the machine's network state cannot change what highlighting is available. The syntax set
    /// is a prebuilt dump for the reasons in [`Self::build_syntax_set`]; the themes are parsed
    /// from committed `.tmTheme` text, which costs about 85 us each.
    ///
    /// # Why a bad asset must not be fatal
    ///
    /// `include_str!`/`include_bytes!` make a *missing* file a compile error, so it cannot
    /// happen. A *malformed* one can: a bad edit, or a re-run of a generator emitting something
    /// syntect rejects. Dropping the asset costs one language or one theme in a picker;
    /// propagating the error would cost starting the editor at all, over a style file. The loss
    /// is logged rather than swallowed, and the tests in `cmake_grammar_s28.rs` and
    /// `themes_s29.rs` assert the shipped sets really contain what they claim, so a silent
    /// fallback cannot pass review.
    pub fn load() -> Self {
        let syntax_set = Self::build_syntax_set();
        let mut theme_set = ThemeSet::load_defaults();
        add_bundled_themes(&mut theme_set);
        HighlightEngine {
            syntax_set,
            theme_set,
            theme_name: DEFAULT_THEME.to_string(),
        }
    }

    /// The syntax set this build ships: syntect's defaults plus the vendored grammars.
    ///
    /// # Why the bytes are prebuilt rather than merged here
    ///
    /// The obvious code is
    /// `SyntaxSet::load_defaults_newlines().into_builder().add(def).build()`, and it is wrong
    /// for a startup path. `build()` rebuilds the scope and context maps of every grammar it
    /// holds. Measured in release on one machine:
    ///
    /// ```text
    ///   load_defaults_newlines()                    483 us
    ///   into_builder().build(), nothing added     82_806 us
    ///   defaults + CMake through into_builder    ~193_000 us
    ///   from_uncompressed_data(include_bytes!)      475 us
    /// ```
    ///
    /// So merging at startup is a 40x regression against a budget (spec section 9) that already
    /// counts startup time, and it buys nothing over doing the same merge once, offline, in
    /// `tools/gen_syntax_set`, then embedding the result. That is also how syntect loads its own
    /// defaults: `load_defaults_newlines()` is literally
    /// `from_uncompressed_data(include_bytes!(...))`.
    ///
    /// # Why a bad asset must not be fatal
    ///
    /// A committed asset can drift from its generator -- the YAML edited without re-running the
    /// tool, or the file truncated. Dropping to the default set costs one language in the picker;
    /// propagating the error would cost starting the editor at all, over a style file. The loss
    /// is logged rather than swallowed, and `cmake_grammar_s28.rs` asserts the embedded set
    /// really carries CMake, so a silent fallback cannot pass review.
    fn build_syntax_set() -> SyntaxSet {
        match from_uncompressed_data::<SyntaxSet>(SYNTAX_SET) {
            Ok(set) if set.find_syntax_by_name(CMAKE_LANGUAGE).is_some() => set,
            Ok(_) => {
                log::warn!(
                    "embedded syntax set has no {CMAKE_LANGUAGE}; re-run tools/gen_syntax_set"
                );
                SyntaxSet::load_defaults_newlines()
            }
            Err(e) => {
                log::warn!("embedded syntax set is unreadable ({e}); using syntect defaults");
                SyntaxSet::load_defaults_newlines()
            }
        }
    }

    /// Theme names, `none` first then sorted (FR-H3).
    pub fn theme_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.theme_set.themes.keys().cloned().collect();
        names.sort();
        let mut out = Vec::with_capacity(names.len() + 1);
        out.push(THEME_NONE.to_string());
        out.extend(names);
        out
    }

    /// Select a theme. An unknown name returns `false` and leaves the previous theme in
    /// place, because a typo in `config.toml` must not silently strip all color.
    pub fn set_theme(&mut self, name: &str) -> bool {
        if name == THEME_NONE || self.theme_set.themes.contains_key(name) {
            self.theme_name = name.to_string();
            return true;
        }
        false
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// The push depth that makes the current theme's syntax palette readable on its own ground.
    ///
    /// Computed from every foreground color the theme can emit, because the depth is a property of
    /// the palette as a whole: solving each color against the floor independently collapses their
    /// relative depths and leaves a scheme whose colors no longer look different from one another.
    /// See [`crate::palette::syntax_push`].
    ///
    /// Resolved once, where the cache is built, rather than per line: a theme carries hundreds of
    /// scope entries, and a frame asks per span. The depth then travels on the cache it produced.
    pub fn syntax_push_depth(&self) -> f64 {
        match self.theme() {
            None => 0.0,
            Some(theme) => {
                let ground = crate::palette::ThemePalette::from_theme(theme).background;
                // Only scopes that can actually put glyphs on the text area. This filter matters
                // more than it looks: driving the depth off every declared foreground produced a
                // 0.35 push on the base16 darks caused entirely by `markup.*.git_gutter` markers --
                // which npt has no gutter for -- and by `invalid.illegal`, whose "foreground" is
                // declared as the theme's own background because it is meant to be read as a
                // background block. Those two alone set the depth for every color the user reads,
                // pushing a whole palette far darker than anything on screen required.
                let mut colors: Vec<crate::palette::Rgb> = theme
                    .scopes
                    .iter()
                    .filter(|item| declares_visible_text(&item.scope))
                    .filter_map(|item| item.style.foreground)
                    .filter(|c| c.a != 0)
                    .map(|c| (c.r, c.g, c.b))
                    .collect();
                // The default foreground is itself a syntax color on this ground, and a theme whose
                // accents are fine but whose base ink is weak needs the same treatment.
                colors.extend(
                    theme
                        .settings
                        .foreground
                        .filter(|c| c.a != 0)
                        .map(|c| (c.r, c.g, c.b)),
                );
                crate::palette::syntax_push(&colors, ground)
            }
        }
    }

    /// True when highlighting is switched off.
    pub fn theme_is_none(&self) -> bool {
        self.theme_name == THEME_NONE
    }

    /// Language names for the picker (FR-H2), sorted, with Plain Text first so it is
    /// reachable without typing.
    pub fn language_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .syntax_set
            .syntaxes()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        names.sort();
        names.dedup();
        if let Some(pos) = names.iter().position(|n| n == PLAIN_TEXT) {
            names.remove(pos);
        }
        names.insert(0, PLAIN_TEXT.to_string());
        names
    }

    /// Detect a language from a path and the first line (FR-H1).
    ///
    /// Extension first, then the whole file name for extensionless files such as
    /// `Makefile`, then a name-based rule for files whose extension belongs to a *different*
    /// language, then the first line, which is how a shebang finds Python. `None` means
    /// Plain Text.
    pub fn detect_syntax(&self, path: Option<&Path>, first_line: &str) -> Option<String> {
        if let Some(p) = path {
            if let Some(syn) = p
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|e| self.syntax_set.find_syntax_by_extension(e))
            {
                return Some(syn.name.clone());
            }
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if let Some(syn) = self.syntax_set.find_syntax_by_token(name) {
                    return Some(syn.name.clone());
                }
            }
        }
        self.syntax_set
            .find_syntax_by_first_line(first_line)
            .map(|s| s.name.clone())
    }

    /// The syntax for a language name, falling back to Plain Text for an unknown one.
    ///
    /// Degrading rather than failing matters: a session can name a language that a different
    /// syntect version does not ship, and losing the file over a style would be absurd.
    fn syntax_for(&self, name: Option<&str>) -> &SyntaxReference {
        name.and_then(|n| self.syntax_set.find_syntax_by_name(n))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }

    /// The theme to use, or `None` when highlighting is off. Falls back to
    /// [`DEFAULT_THEME`] if the selected name vanished.
    ///
    /// Public because the palette is derived from it and a caller inspecting "what colors is
    /// the editor actually using" needs the source, not a re-derivation of it.
    pub fn theme(&self) -> Option<&Theme> {
        if self.theme_is_none() {
            return None;
        }
        self.theme_set
            .themes
            .get(&self.theme_name)
            .or_else(|| self.theme_set.themes.get(DEFAULT_THEME))
    }

    /// The ground and ink the current theme *declares*, before any palette adjustment.
    ///
    /// The counterpart of [`HighlightEngine::palette`] for tests: comparing a palette against
    /// itself proves nothing, so a check that wants to know whether npt moved the editor's ground
    /// has to read the value the theme actually shipped. `None` when highlighting is off.
    pub fn declared_colors(&self) -> Option<(crate::palette::Rgb, crate::palette::Rgb)> {
        self.theme()
            .map(crate::palette::ThemePalette::declared_colors)
    }

    /// Every interface color for the theme currently in force (S30).
    ///
    /// # Why this exists
    ///
    /// The menu bar, tab bar, status bar, service lines and dialogs used `Style::default()`,
    /// which leaves the foreground and background to the *terminal*. Switching the emulator from
    /// a dark to a light palette moved one of the two and left the other hardcoded, and chrome
    /// text went invisible. A color left unset is not a color that matches; it is a color someone
    /// else owns. This returns a value for every surface so nothing on screen can depend on the
    /// terminal's palette.
    ///
    /// # `theme = "none"`
    ///
    /// Returns the neutral palette rather than "no colors". Turning syntax *highlighting* off is
    /// not a request to make the interface unstyled -- it still needs a ground and ink, and the
    /// neutral palette is the same one a malformed theme falls back to.
    pub fn palette(&self) -> crate::palette::ThemePalette {
        match self.theme() {
            Some(theme) => crate::palette::ThemePalette::from_theme(theme),
            None => crate::palette::ThemePalette::neutral(),
        }
    }

    /// A fresh cache for a language (FR-H4).
    pub fn new_cache(&self, syntax: Option<&str>) -> HighlightCache {
        HighlightCache {
            syntax_name: syntax.map(str::to_string),
            lines: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            anchor: None,
            generation: 0,
            lexes: 0,
            syntax_push: self.syntax_push_depth(),
        }
    }

    /// Style one line, replaying as needed and filling the cache on the way (FR-H4).
    ///
    /// `src` supplies the text of the lines the replay walks over. Call in viewport order
    /// for the cheapest behavior: consecutive requests then hit the cache or extend the
    /// anchor, and each line is lexed at most once per invalidation.
    pub fn line_at(
        &self,
        cache: &mut HighlightCache,
        line: usize,
        src: &dyn LineSource,
    ) -> Vec<(CoreStyle, String)> {
        let neutral = |text: &str| vec![(CoreStyle::neutral(), text.to_string())];
        if self.theme_is_none() {
            return neutral(&src.line_text(line).unwrap_or_default());
        }
        // Plain Text is a real syntect grammar that still recognizes keywords, so selecting
        // it in the picker would keep coloring the text the user just asked to be unstyled
        // (FR-H1's fallback, FR-H2's explicit choice). Treat it as "no highlighting".
        if cache
            .syntax_name
            .as_deref()
            .is_some_and(|n| n == PLAIN_TEXT)
        {
            return neutral(&src.line_text(line).unwrap_or_default());
        }
        if let Some(styles) = cache.lines.get(&line) {
            return styles.clone();
        }
        let Some(theme) = self.theme() else {
            return neutral(&src.line_text(line).unwrap_or_default());
        };
        // The ground the editor will actually paint, which is what syntax colors have to be readable
        // against. From the palette rather than from the theme's raw `background`, because the two
        // differ on any light theme npt has deepened.
        let editor_ground = crate::palette::ThemePalette::from_theme(theme).background;
        let syntax = self.syntax_for(cache.syntax_name.as_deref());
        let total = src.line_count();
        let target = line.min(total.saturating_sub(1));

        // Resume from the anchor when it is usable, otherwise from the nearest checkpoint at
        // or below the target. Both carry the state they claim: resuming at line `n` requires
        // the state *at* n, so a source that is merely near n is not usable. This is the
        // invariant whose violation made every line after a block comment render as code.
        // The anchor carries its own proof: it is the state *at* line `a`, produced by the
        // walk that reached `a`, so nothing else has to still be cached for it to be valid.
        // Requiring the preceding line to be cached was both wrong at `a == 0` and wrong once
        // eviction had swept past the anchor, and it silently turned every scroll into a walk
        // back to the nearest checkpoint.
        let resumed = match &cache.anchor {
            Some((a, ps, hs)) if *a <= target => Some((*a, ps.clone(), hs.clone())),
            _ => None,
        };
        let (mut hl, walk_start) =
            match resumed.or_else(|| nearest_checkpoint(&cache.checkpoints, target)) {
                Some((at, ps, hs)) => (HighlightLines::from_state(theme, hs, ps), at),
                None => (HighlightLines::new(syntax, theme), 0),
            };
        for l in walk_start..=target {
            let text = src.line_text(l).unwrap_or_default();
            cache.lexes += 1;
            let line_for_syntax = for_syntax(&text);
            let Ok(styles) = hl.highlight_line(&line_for_syntax, &self.syntax_set) else {
                // A parse error means the parser could not continue on this line. Keep what
                // we have and stop the walk rather than emit styles derived from a state we
                // no longer trust.
                break;
            };
            // The terminators were added for the parser (see `for_syntax`); they are not part
            // of the line, and the render contract is that span text concatenates back to the
            // line exactly. Trimmed by count rather than stripped of `\n`, because a
            // multi-line scope -- an unterminated string, a block comment's opening line --
            // absorbs the terminator into its own fragment, and a `strip_suffix` would leave
            // that fragment one character too long.
            let core: Vec<(CoreStyle, String)> = trim_to_text_len(
                styles,
                text.chars().count(),
                editor_ground,
                cache.syntax_push,
            );
            cache.lines.insert(l, core);
            let (hs2, ps2) = hl.state();
            if (l + 1) % CHECKPOINT_EVERY == 0 {
                cache.checkpoints.insert(l + 1, (ps2.clone(), hs2.clone()));
            }
            // Carry the state forward so the next request resumes here. Storing it every line
            // is what keeps a scroll at one lex per line; it is two clones of a small struct.
            cache.anchor = Some((l + 1, ps2.clone(), hs2.clone()));
            hl = HighlightLines::from_state(theme, hs2, ps2);
        }
        cache.evict_around(line);
        cache
            .lines
            .get(&line)
            .cloned()
            .unwrap_or_else(|| neutral(&src.line_text(line).unwrap_or_default()))
    }

    /// Drop everything an edit at `line` could have invalidated (FR-H4).
    ///
    /// Styles from `line` onward are wrong. So is every checkpoint at or after `line`: a
    /// checkpoint at `line` is the state *before* `line`, which a pure overwrite inside that
    /// line would not change, but an inserted or deleted line shifts what `line` even is and
    /// the caller cannot distinguish the two from here. The spec asks for correctness over
    /// cleverness, so the boundary checkpoint goes too. Checkpoints strictly below `line`
    /// are kept and are what keeps the next replay short.
    pub fn invalidate_from(&self, cache: &mut HighlightCache, line: usize) {
        cache.lines.split_off(&line);
        cache.checkpoints.split_off(&line);
        cache.drop_anchor_at_or_after(line);
        cache.generation = cache.generation.wrapping_add(1);
    }

    /// Line insert at `inserted_line`. Drops rather than reindexes.
    ///
    /// Shifting a cache and its checkpoints by one is precisely the cleverness that yields a
    /// one-line-off color nobody can reproduce, and the replay rebuilds the dropped range on
    /// the next frame for a viewport's worth of lexes.
    pub fn invalidate_line_inserted(&self, cache: &mut HighlightCache, inserted_line: usize) {
        self.invalidate_from(cache, inserted_line);
    }

    /// Line delete at `deleted_line`.
    pub fn invalidate_line_deleted(&self, cache: &mut HighlightCache, deleted_line: usize) {
        self.invalidate_from(cache, deleted_line);
    }

    /// Repoint the cache at another language, dropping all derived state.
    pub fn set_language(&self, cache: &mut HighlightCache, syntax: Option<&str>) {
        cache.syntax_name = syntax.map(str::to_string);
        cache.clear(self);
    }

    /// Clear every cached style and checkpoint. Needed on a theme switch, since styles, not
    /// just parse state, are derived from the theme.
    pub fn clear_cache(&self, cache: &mut HighlightCache) {
        cache.clear(self);
    }

    /// Number of syntect line lexes performed on this cache. Used by the checkpoint-density
    /// test to prove a distant line is reached by replay and not by a full-document scan.
    pub fn lex_count(cache: &HighlightCache) -> usize {
        cache.lexes
    }

    /// Syntaxes in the bundled set.
    pub fn syntax_count(&self) -> usize {
        self.syntax_set.syntaxes().len()
    }

    /// Resolve a language name to whether the set knows it. Used to reject a bad session
    /// value instead of silently switching language.
    pub fn has_language(&self, name: &str) -> bool {
        name == PLAIN_TEXT || self.syntax_set.find_syntax_by_name(name).is_some()
    }
}

/// Convert a syntect span style into a *syntax layer* style.
///
/// The foreground and attributes come straight through. The background does not, and that is
/// deliberate: syntect's default style carries the theme's own background color, so every
/// span of every line arrives with `bg = Some(theme_background)`. Passing that through means
/// the syntax layer states an opinion about every cell in the editor, which then beats the
/// current-line highlight (FR-H5) and any editor decoration that wants a background -- the
/// layer stack is "highest layer with a color wins", and syntax would be it everywhere.
///
/// The theme background is a single editor-wide default, not a per-span fact. Keeping it out
/// of the per-span style is what lets the layers compose at all.
fn syntax_style(
    st: &syntect::highlighting::Style,
    ground: crate::palette::Rgb,
    push: f64,
) -> CoreStyle {
    let mut out = style::from_syntect(st);
    out.bg = None;
    // A theme's syntax palette is its own business, but it is painted on npt's ground, and the two
    // are not guaranteed to be compatible: base16-ocean.light ships accents at 1.5:1 against its own
    // text area. The depth is a palette-wide property, so it arrives already solved -- see
    // `palette::syntax_push` for why each color is not solved on its own.
    //
    // Only a truecolor foreground can be adjusted: an indexed color's actual RGB belongs to the
    // terminal's palette, which npt does not own and cannot measure, so it passes through untouched.
    // Same reason nothing is invented where the theme declared no color.
    out.fg = out.fg.and_then(|c| {
        let rgb = c.rgb?;
        let fixed = crate::palette::apply_syntax_push(rgb, ground, push);
        Some(crate::style::CoreColor::rgb(fixed.0, fixed.1, fixed.2))
    });
    out
}

/// Whether a theme scope entry can put readable text on the editor's text area.
///
/// A theme declares colors for a lot of things that are not text the user reads in the buffer, and
/// including them in the legibility calculation lets something invisible set the depth of something
/// prominent. The two that actually caused this:
///
/// - `invalid.illegal` is frequently declared with the theme's own background as its foreground,
///   because the intent is a filled block behind the text. As a foreground it is 1.00:1 against that
///   background by design, and it dominated the calculation completely.
/// - `markup.*.git_gutter` and friends are gutter-icon colors. npt draws no VCS gutter, so they can
///   never appear, yet they are dark-on-dark in the base16 schemes and were the sole reason those
///   themes were being pushed 35% lighter.
///
/// Anything unrecognized is treated as visible, so the filter can only ever *reduce* a push, never
/// manufacture one, and a scope nobody has heard of still gets its color corrected.
fn declares_visible_text(scope: &syntect::highlighting::ScopeSelectors) -> bool {
    let text = format!("{scope:?}");
    const INVISIBLE: &[&str] = &[
        "invalid.illegal",
        "git_gutter",
        "gutter",
        "markup.deleted",
        "markup.inserted",
        "markup.changed",
        "markup.ignored",
        "markup.untracked",
        "brackethighlighter",
        "unmatched",
    ];
    !INVISIBLE.iter().any(|needle| text.contains(needle))
}

/// Clip highlighted fragments back to `len` characters, dropping the parser-only terminator.
///
/// Drops from the end, walking whole fragments and then trimming a partial one, which
/// preserves fragment order and keeps every kept character's style intact.
fn trim_to_text_len(
    styles: Vec<(syntect::highlighting::Style, &str)>,
    len: usize,
    ground: crate::palette::Rgb,
    push: f64,
) -> Vec<(CoreStyle, String)> {
    let mut out: Vec<(CoreStyle, String)> = Vec::with_capacity(styles.len());
    let mut remaining = len;
    for (st, frag) in styles {
        if remaining == 0 {
            break;
        }
        let take = frag.chars().count().min(remaining);
        let kept: String = frag.chars().take(take).collect();
        remaining -= take;
        if kept.is_empty() {
            continue;
        }
        out.push((syntax_style(&st, ground, push), kept));
    }
    out
}

/// The text handed to the syntax parser for one logical line: `text` plus a line terminator.
///
/// This is the fix for "after the first `//` the rest of the file is a comment", and the
/// reason it happens is a mismatch between two conventions that are each reasonable alone.
/// The syntax set is loaded with `SyntaxSet::load_defaults_newlines()`, whose patterns match
/// the line terminator explicitly -- the C++ line comment ends at `\n`. `LineSource` hands
/// over text *without* the terminator, because that is what every other consumer of a line
/// wants. Feed syntect a terminator-less line and a `//` comment never terminates: the parser
/// stays inside the comment scope, and every following line inherits comment color to the end
/// of the file.
///
/// Verified against syntect's own documented `HighlightLines` pattern: the same C++ line
/// yields 7 correctly scoped fragments with a trailing `\n` and 1 comment-colored fragment
/// without one. No headless test caught it because the reference pass in `highlight_s12.rs`
/// fed the source the same terminator-less way, so cache and reference agreed on being wrong.
///
/// The trailing fragment is harmless downstream: the renderer walks a fixed character count
/// per row and clips to the row width, so a `\n` past the last visible cell is never drawn.
fn for_syntax(text: &str) -> String {
    // Lowercased as well as newline-terminated. See `SYNTAX_SET` for why the vendored
    // grammar is written in already-lowercase form.
    //
    // # Why this is safe for every grammar, not just CMake
    //
    // The value is used for one thing: matching. The span *text* the renderer draws comes from
    // the document, never from here, so casing here cannot change what the user reads -- only
    // which scope a run of characters is assigned. Every grammar in the bundled set is a
    // programming-language grammar whose keywords are matched case-insensitively by intent, and
    // folding was verified to reproduce the original grammar's scopes exactly on the CMake one.
    //
    // # Where it could bite, stated honestly
    //
    // A grammar with genuinely case-sensitive literals -- one distinguishing `Foo` from `foo` --
    // would match differently under this. None of the 75 bundled grammars or the vendored CMake
    // one does, and the test `folding_does_not_change_bundled_grammar_scopes` pins that for a
    // sample of them, so a future grammar that needs case would fail a test rather than quietly
    // mis-highlight.
    let folded = text.to_lowercase();
    if folded.ends_with('\n') {
        folded
    } else {
        format!("{folded}\n")
    }
}

/// The checkpoint at or below `line`, or `None` when there is none above it.
///
/// The returned line number is where the state is valid, so a caller resuming at `at` may
/// not skip lines between `at` and its target: the state says nothing about them.
fn nearest_checkpoint(
    checkpoints: &BTreeMap<usize, (ParseState, HighlightState)>,
    line: usize,
) -> Option<(usize, ParseState, HighlightState)> {
    checkpoints
        .range(..=line)
        .next_back()
        .map(|(k, (ps, hs))| (*k, ps.clone(), hs.clone()))
}

/// Per-document styles and checkpoints (contract §7).
#[derive(Debug)]
pub struct HighlightCache {
    syntax_name: Option<String>,
    lines: BTreeMap<usize, Vec<(CoreStyle, String)>>,
    checkpoints: BTreeMap<usize, (ParseState, HighlightState)>,
    /// The parser state just past the most recently lexed line, plus that line number.
    ///
    /// This is a checkpoint that exists for exactly one position, and it is what makes a
    /// scroll cheap: consecutive access resumes here instead of walking back to the nearest
    /// 128-line checkpoint on every request. Without it, scrolling a long file re-walks the
    /// gap each line and the cost is quadratic in lines scrolled.
    anchor: Option<(usize, ParseState, HighlightState)>,
    generation: u64,
    lexes: usize,
    /// Syntax-palette push depth in force when this cache was built.
    ///
    /// Lives here rather than being recomputed per line because it is a property of the theme, and a
    /// cache is already per-theme: `App` builds a fresh cache when the theme changes, which is the
    /// same event that would invalidate these styles anyway. Carrying it also means a cached style
    /// and the depth that produced it can never disagree.
    syntax_push: f64,
}

impl HighlightCache {
    /// Bumped on every invalidation, so a consumer holding a derived value can notice.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn syntax_name(&self) -> Option<&str> {
        self.syntax_name.as_deref()
    }

    /// Cached styles for `line`, letting a renderer skip the engine for a line it just drew.
    pub fn cached(&self, line: usize) -> Option<&[(CoreStyle, String)]> {
        self.lines.get(&line).map(Vec::as_slice)
    }

    pub fn cached_line_count(&self) -> usize {
        self.lines.len()
    }

    /// Which line numbers currently hold a checkpoint.
    pub fn checkpoint_lines(&self) -> Vec<usize> {
        self.checkpoints.keys().copied().collect()
    }

    /// The current replay anchor line, for tests and diagnostics.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor.as_ref().map(|(l, _, _)| *l)
    }

    fn drop_anchor_at_or_after(&mut self, line: usize) {
        if self.anchor.as_ref().is_some_and(|(a, _, _)| *a >= line) {
            self.anchor = None;
        }
    }

    fn clear(&mut self, _engine: &HighlightEngine) {
        self.lines.clear();
        self.checkpoints.clear();
        self.anchor = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drop cached styles outside `line +- CACHE_WINDOW`.
    fn evict_around(&mut self, line: usize) {
        let lo = line.saturating_sub(CACHE_WINDOW);
        let hi = line.saturating_add(CACHE_WINDOW);
        let stale: Vec<usize> = self
            .lines
            .range(..lo)
            .chain(self.lines.range(hi.saturating_add(1)..))
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            self.lines.remove(&k);
        }
    }
}

// No unit tests here on purpose. Everything worth asserting about this module needs a real
// syntax set and a multi-line document, and is in `tests/highlight_s12.rs`, where the
// reference implementation a cache must match (`scratch`) lives next to the tests that use it.
// Helpers kept here would be dead code from the lib's point of view.
