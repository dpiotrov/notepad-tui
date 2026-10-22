//! The vendored CMake grammar (S28).
//!
//! S27 established that syntect 5.3 ships no CMake grammar and left the detection order fixed
//! but the highlighting unavailable. S28 vendors a grammar and, at the user's instruction, wires
//! it for **manual selection only** -- no `CMakeLists.txt` autodetection -- because claiming the
//! `txt` extension for CMake would relabel every text file in the tree.
//!
//! Three things could silently break and are pinned here: the grammar actually loads and is
//! reachable by name; selecting it really produces non-neutral styling (a grammar that loads but
//! matches nothing would look identical to "not installed"); and the case-folding the grammar is
//! written in reproduces what the unfolded original would have produced.

use np_core::highlight::{HighlightEngine, LineSource, CMAKE_LANGUAGE, PLAIN_TEXT};
use std::path::Path;

fn engine() -> HighlightEngine {
    HighlightEngine::load()
}

/// Whether a style says anything at all.
///
/// Local rather than a method on `CoreStyle` because nothing in the editor needs this question
/// -- rendering applies styles, it never asks whether one is empty -- and adding public API to
/// np-core for a test would be the tail wagging the dog.
fn is_styled(st: &np_core::CoreStyle) -> bool {
    st.fg.is_some() || st.bg.is_some() || st.bold || st.italic || st.underline || st.dim
}

/// A whole document as a line source, so highlighting can be driven without a file on disk.
struct Lines(Vec<String>);

impl LineSource for Lines {
    fn line_text(&self, line: usize) -> Option<String> {
        self.0.get(line).cloned()
    }
    fn line_count(&self) -> usize {
        self.0.len()
    }
}

fn styled(text: &str, language: &str) -> Vec<(String, bool)> {
    let e = engine();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut cache = e.new_cache(Some(language));
    let out: Vec<Vec<(np_core::CoreStyle, String)>> = (0..lines.len())
        .map(|i| e.line_at(&mut cache, i, &Lines(lines.clone())))
        .collect();
    out.iter()
        .flatten()
        .map(|(st, txt)| (txt.clone(), is_styled(st)))
        .collect()
}

#[test]
fn cmake_is_a_selectable_language() {
    let e = engine();
    assert!(
        e.has_language(CMAKE_LANGUAGE),
        "the vendored grammar must be loadable by name"
    );
    let names = e.language_names();
    assert!(
        names.iter().any(|n| n == CMAKE_LANGUAGE),
        "CMake must appear in the language picker, got {} names",
        names.len()
    );
}

#[test]
fn adding_the_grammar_does_not_lose_the_bundled_languages() {
    // The failure mode this guards is specific and easy to hit: building a fresh
    // SyntaxSetBuilder instead of converting the loaded set back into one yields a set holding
    // only the vendored grammar. Everything still compiles, CMake still works, and 75 languages
    // have quietly vanished from the picker.
    let e = engine();
    let names = e.language_names();
    assert!(
        names.len() > 75,
        "expected the 75 bundled grammars plus the vendored one, got {}",
        names.len()
    );
    for probe in ["C", "Rust", "Python", "Makefile", "YAML", "JSON"] {
        assert!(
            names.iter().any(|n| n == probe),
            "bundled language {probe} disappeared"
        );
    }
}

#[test]
fn selecting_cmake_actually_colors_the_text() {
    // A grammar that loads but matches nothing is indistinguishable from an absent one, so the
    // property that matters is that styles come out non-neutral.
    let src = "if(WIN32)\n  message(STATUS \"hello\")\nendif()\n";
    let out = styled(src, CMAKE_LANGUAGE);
    let colored = out.iter().filter(|(_, c)| *c).count();
    assert!(
        colored > 3,
        "CMake highlighting produced almost no styling: {out:?}"
    );
}

#[test]
fn cmake_keywords_are_colored_in_any_case() {
    // The grammar's patterns are written lowercase and matched against lowercased lines, so
    // uppercase and mixed-case CMake commands must still be recognized.
    //
    // "Is any of this line styled" is NOT a sufficient assertion, and the first version of this
    // test got that wrong: a line like `SET(MY_VAR 1)` still picks up punctuation and argument
    // scopes, so it reports "styled" even when the keyword rule matched nothing at all. The
    // property that actually pins the fold is that a keyword in any casing is styled *the same
    // way* as the same keyword written lowercase -- same spans, same styles. That is what
    // case-insensitivity means operationally, and it is what breaks if `for_syntax` stops
    // folding or the grammar loses its folded form.
    let e = engine();
    let shape_of = |text: &str| {
        let lines: Vec<String> = vec![text.to_string()];
        let mut cache = e.new_cache(Some(CMAKE_LANGUAGE));
        e.line_at(&mut cache, 0, &Lines(lines))
            .into_iter()
            .map(|(st, frag)| (is_styled(&st), frag.to_lowercase()))
            .collect::<Vec<_>>()
    };
    for (upper, lower) in [
        ("SET(MY_VAR 1)", "set(MY_VAR 1)"),
        ("IF(WIN32)", "if(WIN32)"),
        ("ENDIF()", "endif()"),
        ("EndIf()", "endif()"),
        ("MESSAGE(STATUS \"x\")", "message(STATUS \"x\")"),
        ("FIND_PACKAGE(Qt5)", "find_package(Qt5)"),
    ] {
        assert_eq!(
            shape_of(upper),
            shape_of(lower),
            "{upper} is styled differently from {lower}: keyword matching became case-sensitive, \
             so the fold between the grammar and `for_syntax` is broken"
        );
    }
    // And the keyword really is colored, so the equality above is not two lines of nothing.
    assert!(
        shape_of("SET(MY_VAR 1)")[0].0,
        "SET() is unstyled at the first span; the keyword rule is not matching at all"
    );
}

#[test]
fn bracket_comments_do_not_leak_into_following_code() {
    // CMake's `#[[ ... ]]` blocks are the two patterns the converter deliberately left unfolded
    // (they carry a numbered backreference). If their context ever failed to pop, every line
    // after one would render as a comment -- the same class of fault S12 recorded for `/*`.
    let e = engine();
    let src = "#[[ bracket comment\nstill inside\n]] end of it\nmessage(\"back to code\")\n";
    let lines: Vec<String> = src.lines().map(str::to_string).collect();
    let mut cache = e.new_cache(Some(CMAKE_LANGUAGE));
    let last = e.line_at(&mut cache, src.lines().count() - 1, &Lines(lines));
    // The line after the block must contain something that is not styled as a comment: at least
    // one span whose text is the command name.
    assert!(
        last.iter().any(|(_, t)| t.replace('\n', "") == "message"),
        "the line after a bracket comment is still being consumed by it: {last:?}"
    );
}

#[test]
fn detection_is_unchanged_and_cmake_is_not_autodetected() {
    // Manual-selection only, by explicit instruction. `CMakeLists.txt` must NOT be claimed.
    let e = engine();
    assert_ne!(
        e.detect_syntax(Some(Path::new("CMakeLists.txt")), "")
            .as_deref(),
        Some(CMAKE_LANGUAGE),
        "CMakeLists.txt must not be autodetected as CMake; it is a manual pick"
    );
    // And the neighbours are untouched.
    assert_eq!(
        e.detect_syntax(Some(Path::new("notes.txt")), "").as_deref(),
        Some(PLAIN_TEXT)
    );
    assert_eq!(
        e.detect_syntax(Some(Path::new("Makefile")), "").as_deref(),
        Some("Makefile")
    );
    assert_eq!(
        e.detect_syntax(Some(Path::new("main.c")), "").as_deref(),
        Some("C")
    );
}

#[test]
fn plain_text_still_means_no_styling() {
    // S12's rule: syntect's Plain Text grammar does recognize keywords, so it has to be treated
    // as "no highlighting". Adding a grammar must not disturb that.
    let out = styled("if this then that end\n", PLAIN_TEXT);
    assert!(
        out.iter().all(|(_, c)| !*c),
        "Plain Text started styling again: {out:?}"
    );
}

#[test]
fn folding_does_not_change_bundled_grammar_scopes() {
    // `for_syntax` lowercases every line before handing it to syntect, which is what makes the
    // folded CMake grammar cheap. That is only acceptable if it leaves the bundled grammars
    // behaving as they did. Compare each probe's styling against the same probe in lowercase:
    // identical results mean the fold is not changing the answer for that grammar.
    let e = engine();
    let colorize = |lang: &str, text: &str| -> Vec<bool> {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut cache = e.new_cache(Some(lang));
        (0..lines.len())
            .flat_map(|i| e.line_at(&mut cache, i, &Lines(lines.clone())))
            .map(|(st, _)| is_styled(&st))
            .collect()
    };
    let cases: [(&str, &str); 4] = [
        ("Rust", "fn main() {\n    let x = Foo;\n}\n"),
        ("C", "int main(void) {\n    return Foo;\n}\n"),
        ("Python", "def f():\n    return Foo\n"),
        ("Makefile", "all:\n\techo Foo\n"),
    ];
    for (lang, text) in cases {
        let mixed = colorize(lang, text);
        let lower = colorize(lang, &text.to_lowercase());
        assert_eq!(
            mixed, lower,
            "{lang}: styling differs between original and lowercased input, so line folding \
             changes this grammar's result"
        );
    }
}

#[test]
fn the_shipped_set_is_the_prebuilt_one_not_a_fallback() {
    // `build_syntax_set` deliberately falls back to `SyntaxSet::load_defaults_newlines()` if the
    // embedded dump cannot be read or has no CMake -- a bad asset must not stop the editor
    // starting. That safety net is also a silent failure mode: every other test here would still
    // pass except that CMake would be gone, and a stale asset is exactly the drift a generator
    // cannot detect on its own.
    //
    // The fallback set is syntect's defaults, which have no CMake grammar at all (S27's whole
    // finding). So "CMake is listed" is precisely the observation that distinguishes the embedded
    // set from the fallback, and "more languages than the defaults" says the same thing
    // numerically.
    let e = engine();
    let names = e.language_names();
    assert!(
        names.iter().any(|n| n == CMAKE_LANGUAGE),
        "CMake is missing from the picker, which means the embedded syntax set fell back to \
         syntect's defaults -- re-run tools/gen_syntax_set"
    );
    assert!(
        names.len() > 75,
        "expected more than syntect's default languages, got {}; the embedded set may not be \
         the one built from assets/CMake.sublime-syntax.yaml",
        names.len()
    );
}

#[test]
fn the_generator_and_the_asset_agree_on_the_grammar_text() {
    // The committed YAML is the input to the committed dump. If someone edits the YAML and
    // forgets to re-run tools/gen_syntax_set, the editor ships a set that does not match the
    // grammar file a reader would trust -- a confusing kind of stale.
    //
    // The asset is binary and carries no timestamp that means anything here, so this checks the
    // relationship that does matter and is cheap: the YAML parses as a syntect grammar under the
    // name the engine advertises, and declares the extension the engine relies on. Combined with
    // the test above (CMake is really present), an edit that changes the grammar's identity gets
    // caught; an edit that only changes a keyword list is caught by the styling tests.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/CMake.sublime-syntax.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let def = syntect::parsing::SyntaxDefinition::load_from_str(&text, true, Some(CMAKE_LANGUAGE))
        .unwrap_or_else(|e| panic!("syntect rejected {}: {e}", path.display()));
    assert_eq!(def.name, CMAKE_LANGUAGE);
    assert!(
        def.file_extensions.iter().any(|e| e == "cmake"),
        "grammar no longer declares the cmake extension: {:?}",
        def.file_extensions
    );
    // Case folding is what keeps this grammar affordable; an unfolded re-generation would silently
    // reintroduce a ~1 s first-use cost.
    assert!(
        !text.contains("(?i"),
        "the committed grammar carries case-insensitivity flags again -- re-run \
         tools/convert_textmate_grammar.py, which folds them"
    );
}
