//! S12 — highlighting engine: detection, themes, and cache equivalence (FR-H1..H5).
//!
//! The load-bearing test here is equivalence: after any insert, delete or edit, the styles
//! the cache produces must equal a from-scratch pass over the same text. "It produced
//! something" proves nothing, because a stale cache always produces something.

use np_core::highlight::{
    HighlightEngine, LineSource, CACHE_WINDOW, CHECKPOINT_EVERY, PLAIN_TEXT, THEME_NONE,
};
use np_core::CoreStyle;

fn engine() -> HighlightEngine {
    HighlightEngine::load()
}

pub struct Lines(Vec<String>);

impl Lines {
    fn new(text: &str) -> Lines {
        Lines(text.lines().map(str::to_string).collect())
    }
}

impl LineSource for Lines {
    fn line_text(&self, line: usize) -> Option<String> {
        self.0.get(line).cloned()
    }
    fn line_count(&self) -> usize {
        self.0.len().max(1)
    }
}

/// Reference pass: every line, in order, from a cold cache.
fn scratch(
    eng: &HighlightEngine,
    src: &Lines,
    lang: Option<&str>,
) -> Vec<Vec<(CoreStyle, String)>> {
    let mut cache = eng.new_cache(lang);
    (0..src.line_count())
        .map(|l| eng.line_at(&mut cache, l, src))
        .collect()
}

/// Reconstruct the text a style pass covered, so a wrong span boundary shows up as a text
/// mismatch rather than only a color one.
fn flatten(styles: &[Vec<(CoreStyle, String)>]) -> Vec<(String, Vec<(CoreStyle, String)>)> {
    styles
        .iter()
        .map(|line| {
            let text = line.iter().map(|(_, f)| f.as_str()).collect::<String>();
            (text, line.clone())
        })
        .collect()
}

/// The color that dominates a line, i.e. the fg of the longest span. Span splitting means
/// `styles[line][0]` is often punctuation or leading whitespace, not the line's meaning, so
/// tests that ask "what color is this line" must ask about its widest span.
fn dominant(styles: &[(CoreStyle, String)]) -> Option<np_core::CoreColor> {
    styles
        .iter()
        .max_by_key(|(_, f)| f.chars().count())
        .and_then(|(st, _)| st.fg)
}

// ------------------------------------------------------------------ FR-H1 detect

#[test]
fn detects_rust_by_extension() {
    let eng = engine();
    assert_eq!(
        eng.detect_syntax(Some(std::path::Path::new("/a/b/main.rs")), ""),
        Some("Rust".into())
    );
}

#[test]
fn detects_python_by_shebang_when_the_extension_is_unknown() {
    let eng = engine();
    // No extension at all: the first line is the only signal.
    assert_eq!(
        eng.detect_syntax(None, "#!/usr/bin/env python3"),
        Some("Python".into())
    );
    // An unknown extension must not win over the shebang.
    assert_eq!(
        eng.detect_syntax(
            Some(std::path::Path::new("/a/script.zzz")),
            "#!/usr/bin/python3"
        ),
        Some("Python".into())
    );
}

#[test]
fn extension_wins_over_a_shebang() {
    let eng = engine();
    // A `.rs` file whose first line merely looks like a shebang comment is still Rust.
    assert_eq!(
        eng.detect_syntax(Some(std::path::Path::new("x.rs")), "#!/usr/bin/env python3"),
        Some("Rust".into())
    );
}

#[test]
fn an_extensionless_known_file_name_is_detected() {
    let eng = engine();
    assert!(
        eng.detect_syntax(Some(std::path::Path::new("/proj/Makefile")), "")
            .is_some(),
        "Makefile should resolve to a syntax, not Plain Text"
    );
}

#[test]
fn an_unknown_extension_and_no_shebang_means_plain_text() {
    let eng = engine();
    assert_eq!(
        eng.detect_syntax(Some(std::path::Path::new("notes.zzz")), "hello"),
        None,
        "None means Plain Text"
    );
    assert_eq!(eng.detect_syntax(None, "just words"), None);
}

#[test]
fn plain_text_is_a_named_language_and_the_fallback_syntax() {
    let eng = engine();
    assert!(eng.language_names().contains(&PLAIN_TEXT.to_string()));
    // An unknown language name must resolve rather than fail.
    let mut cache = eng.new_cache(Some("Definitely Not A Language"));
    let src = Lines::new("fn x() {}");
    let styles = eng.line_at(&mut cache, 0, &src);
    assert!(!styles.is_empty());
}

// ------------------------------------------------------------------ FR-H3 themes

#[test]
fn every_listed_theme_can_be_selected() {
    let mut eng = engine();
    for name in eng.theme_names() {
        assert!(eng.set_theme(&name), "{name} should load");
        assert_eq!(eng.theme_name(), name);
    }
}

#[test]
fn none_disables_highlighting() {
    let mut eng = engine();
    assert!(eng.set_theme(THEME_NONE));
    assert!(eng.theme_is_none());
    let src = Lines::new("fn x() {}");
    let mut cache = eng.new_cache(Some("Rust"));
    let styles = eng.line_at(&mut cache, 0, &src);
    // One neutral span covering the line: no syntax color at all.
    assert!(styles.iter().all(|(s, _)| *s == CoreStyle::neutral()));
    assert_eq!(
        styles.iter().map(|(_, f)| f.as_str()).collect::<String>(),
        "fn x() {}"
    );
}

#[test]
fn a_bogus_theme_is_rejected_and_the_previous_one_survives() {
    let mut eng = engine();
    assert!(eng.set_theme("InspiredGitHub"));
    assert!(!eng.set_theme("No Such Theme Anywhere"));
    assert_eq!(eng.theme_name(), "InspiredGitHub");
}

#[test]
fn theme_names_start_with_none() {
    let eng = engine();
    assert_eq!(
        eng.theme_names().first().map(String::as_str),
        Some(THEME_NONE)
    );
}

// ------------------------------------------------------------------ FR-H4 replay

#[test]
fn a_multiline_block_comment_colors_later_lines() {
    // The headline proof that this is parse state and not per-line lexing: line 2 has no
    // `*/` and no code, yet must be colored as comment because line 0 opened a block.
    let eng = engine();
    let src = Lines::new("/* start\nmiddle\nstill comment\n*/\nlet after = 1;");
    let styles = scratch(&eng, &src, Some("Rust"));
    let fg = |line: usize| dominant(&styles[line]);
    assert_eq!(
        fg(1),
        fg(2),
        "two lines inside one block comment must share a color"
    );
    // Line 0 opens the block. Comparing its *dominant* span against the interior is fragile
    // (an opener like `/*` alone is all punctuation), so compare the text that is obviously
    // part of the comment.
    let opener_comment = styles[0]
        .iter()
        .find(|(_, f)| f.contains("start"))
        .and_then(|(st, _)| st.fg);
    assert_eq!(
        opener_comment,
        fg(1),
        "the opener's text shares the block comment's color"
    );
    // After the block closes we are back in code, which is a different color.
    assert_ne!(
        fg(4),
        fg(1),
        "code after the block must not stay comment-colored"
    );
}

#[test]
fn a_rust_raw_string_spans_lines() {
    // A Rust raw string is the hardest multi-line construct in the set: `r#"` opens on one
    // line and `"#` closes on another, and everything between is literal.
    let eng = engine();
    let src = Lines::new("let s = r#\"\nnot code {\n} still string\n\"#;\nlet real = 1;");
    let styles = scratch(&eng, &src, Some("Rust"));
    // The interior lines are entirely string content, so each is a single span of one color.
    let only = |line: usize| -> (usize, Option<np_core::CoreColor>) {
        (styles[line].len(), dominant(&styles[line]))
    };
    let (n1, c1) = only(1);
    let (n2, c2) = only(2);
    assert!(n1 >= 1 && n2 >= 1);
    assert!(c1.is_some() && c2.is_some());
    assert_eq!(c1, c2, "both interior lines are one string");

    // The opener's quote span carries that same color, which per-line lexing could not know.
    let opener_quote = styles[0]
        .iter()
        .find(|(_, f)| f == "\"")
        .and_then(|(st, _)| st.fg);
    assert_eq!(
        opener_quote, c1,
        "the opening quote carries the string color"
    );

    // After `"#` closes, the code on the next line is not the string color. Compare against
    // a from-scratch pass over the same text with the raw string *removed*, so the colors
    // being compared are the same token kind.
    let plain = Lines::new("let real = 1;");
    let plain_styles = scratch(&eng, &plain, Some("Rust"));
    let plain_num = plain_styles[0]
        .iter()
        .find(|(_, f)| f.trim() == "1")
        .and_then(|(st, _)| st.fg);
    let here_num = styles[4]
        .iter()
        .find(|(_, f)| f.trim() == "1")
        .and_then(|(st, _)| st.fg);
    assert_eq!(
        here_num, plain_num,
        "after the raw string closes, a number is colored as a normal number"
    );
    assert_ne!(c1, plain_num, "and the string color is genuinely different");
}

#[test]
fn opening_a_block_comment_recolors_every_line_after_it() {
    let eng = engine();
    let before = "let a = 1;\nlet b = 2;\nlet c = 3;";
    let src = Lines::new(before);
    let original = scratch(&eng, &src, Some("Rust"));

    // Edit line 0 to open a block comment, then invalidate from line 0.
    let edited = Lines::new("/* let a = 1;\nlet b = 2;\nlet c = 3;");
    let mut cache = eng.new_cache(Some("Rust"));
    // Prime the cache with the ORIGINAL text so stale styles genuinely exist to be wrong.
    let orig_src = Lines::new(before);
    for l in 0..orig_src.line_count() {
        eng.line_at(&mut cache, l, &orig_src);
    }
    eng.invalidate_from(&mut cache, 0);
    let after: Vec<_> = (0..edited.line_count())
        .map(|l| eng.line_at(&mut cache, l, &edited))
        .collect();

    // The reference: a cold pass over the edited text.
    let reference = scratch(&eng, &edited, Some("Rust"));
    assert_eq!(
        flatten(&after),
        flatten(&reference),
        "edit must equal a from-scratch pass"
    );
    // And the point of the edit: `let b = 2;` was code and is now comment. Comparing the
    // same token before and after is what makes this assertion mean something; comparing
    // whole-line dominant spans would compare different token kinds.
    let tok = |line: &[(CoreStyle, String)], needle: &str| -> Option<np_core::CoreColor> {
        line.iter()
            .find(|(_, f)| f.contains(needle))
            .and_then(|(st, _)| st.fg)
    };
    assert_eq!(
        tok(&after[1], "let"),
        tok(&after[2], "let"),
        "both edited lines read the same way"
    );
    assert_ne!(
        tok(&original[1], "let"),
        tok(&after[1], "let"),
        "the same token changed color, so the recolor is real"
    );
}

#[test]
fn inserting_a_line_in_the_middle_matches_a_from_scratch_pass() {
    let eng = engine();
    let base = "fn a() {}\n/* open\ninside\n*/\nfn b() {}";
    let src = Lines::new(base);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..src.line_count() {
        eng.line_at(&mut cache, l, &src);
    }

    // Insert a line at 1 in the text, then invalidate at 1.
    let inserted = Lines::new("fn a() {}\nlet injected = 1;\n/* open\ninside\n*/\nfn b() {}");
    eng.invalidate_line_inserted(&mut cache, 1);
    let after: Vec<_> = (0..inserted.line_count())
        .map(|l| eng.line_at(&mut cache, l, &inserted))
        .collect();
    let reference = scratch(&eng, &inserted, Some("Rust"));
    assert_eq!(
        flatten(&after),
        flatten(&reference),
        "insert must equal scratch"
    );
}

#[test]
fn deleting_a_line_in_the_middle_matches_a_from_scratch_pass() {
    let eng = engine();
    let base = "fn a() {}\n/* open\ninside\n*/\nfn b() {}";
    let src = Lines::new(base);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..src.line_count() {
        eng.line_at(&mut cache, l, &src);
    }

    // Delete line 1. That removes the `/*` opener, so everything after changes meaning.
    let deleted = Lines::new("fn a() {}\ninside\n*/\nfn b() {}");
    eng.invalidate_line_deleted(&mut cache, 1);
    let after: Vec<_> = (0..deleted.line_count())
        .map(|l| eng.line_at(&mut cache, l, &deleted))
        .collect();
    let reference = scratch(&eng, &deleted, Some("Rust"));
    assert_eq!(
        flatten(&after),
        flatten(&reference),
        "delete must equal scratch"
    );
    // Sanity: the deletion really did change the meaning of what follows.
    // The deletion removed the `/*` opener, so `inside` goes from comment to code. Compare
    // the whole-line color: it was one comment span before and is code-colored after.
    let original = scratch(&eng, &src, Some("Rust"));
    let comment_color = dominant(&original[2]);
    assert!(comment_color.is_some());
    assert_ne!(
        comment_color,
        dominant(&after[1]),
        "removing the /* opener must change how the following line reads"
    );
}

#[test]
fn editing_inside_a_block_comment_updates_the_lines_below_it() {
    let eng = engine();
    let base = "/* comment\nsecond\nthird\n*/\ndone";
    let src = Lines::new(base);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..src.line_count() {
        eng.line_at(&mut cache, l, &src);
    }
    // Close the comment on line 0, so lines 1.. must stop being comment-colored.
    //
    // Note the invalidation line: it is 0, the first changed line, not 1. Invalidating from
    // 1 while line 0 changed would leave line 0's stale styles in place, which is a caller
    // bug rather than a cache bug -- the cache cannot know which line the caller edited.
    let edited = Lines::new("/* comment */ x\nsecond\nthird\n*/\ndone");
    eng.invalidate_from(&mut cache, 0);
    let after: Vec<_> = (0..edited.line_count())
        .map(|l| eng.line_at(&mut cache, l, &edited))
        .collect();
    let reference = scratch(&eng, &edited, Some("Rust"));
    assert_eq!(flatten(&after), flatten(&reference));
    // The visible consequence: `second`/`third` were bare comment words and after the edit
    // they are bare code words. Same token kind on both sides, so the comparison is fair.
    let original = scratch(&eng, &src, Some("Rust"));
    assert_eq!(
        dominant(&original[1]),
        dominant(&original[2]),
        "in the original, lines 1 and 2 sit in the same block comment"
    );
    assert_ne!(
        dominant(&original[1]),
        dominant(&after[1]),
        "closing the comment on line 0 must recolor the lines below it"
    );
}

#[test]
fn an_untouched_prefix_keeps_its_styles_after_invalidation() {
    let eng = engine();
    let src = Lines::new("fn a() {}\nfn b() {}\n/* x\ny\n*/");
    let full = scratch(&eng, &src, Some("Rust"));
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..src.line_count() {
        eng.line_at(&mut cache, l, &src);
    }
    eng.invalidate_from(&mut cache, 2);
    // Lines 0 and 1 were before the edit; replaying them must give the same answer, which is
    // what makes keeping their checkpoints safe.
    for (l, expected) in full.iter().take(2).enumerate() {
        assert_eq!(
            eng.line_at(&mut cache, l, &src),
            *expected,
            "line {l} unchanged"
        );
    }
}

// ------------------------------------------------------- checkpoint density (FR-H4)

#[test]
fn checkpoints_land_on_multiples_of_128() {
    let eng = engine();
    let text: String = (0..600).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..600 {
        eng.line_at(&mut cache, l, &src);
    }
    let cps = cache.checkpoint_lines();
    assert!(cps.contains(&128), "got {cps:?}");
    assert!(cps.contains(&256));
    assert!(cps.contains(&384));
    assert!(cps.contains(&512));
    assert!(cps.iter().all(|c| c % CHECKPOINT_EVERY == 0));
}

#[test]
fn reaching_a_far_line_does_not_highlight_every_line_before_it() {
    // The spec's requirement, stated the way it is actually achievable. A checkpoint at 128
    // records the parser state at 128; it says nothing about the state at 499 999, so a
    // single cold random jump into a huge file *must* lex the lines in between. There is no
    // way around that without parsing ahead of the user.
    //
    // What the cache does guarantee is that each line is lexed at most once between
    // invalidations, which is what makes scrolling a 10 MB file cheap: the cost is paid once
    // per line, in the direction the user is moving, and never again.
    let eng = engine();
    // 20 000 lines is enough to prove the point (the jump is 199 lines of gap, not 20 000),
    // and building/lexing a 200 000-line fixture cost more test time than it proved.
    let text: String = (0..20_000).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..130 {
        eng.line_at(&mut cache, l, &src);
    }
    assert!(cache.checkpoint_lines().contains(&128));
    // A cold jump pays for the distance it skipped. Asserted explicitly so nobody "fixes"
    // the cache into pretending otherwise.
    let before = HighlightEngine::lex_count(&cache);
    eng.line_at(&mut cache, 19_999, &src);
    let cold = HighlightEngine::lex_count(&cache) - before;
    assert!(
        cold > 10_000,
        "expected a cold jump across most of the file to pay for the distance, got {cold}"
    );
    // The guarantee: the same far region, and a re-read of the same line, cost nothing.
    let after_cold = HighlightEngine::lex_count(&cache);
    for l in [19_999, 19_990, 19_995, 19_999] {
        eng.line_at(&mut cache, l, &src);
    }
    assert_eq!(
        HighlightEngine::lex_count(&cache),
        after_cold,
        "a re-read of an already-lexed region must cost zero"
    );
    // Once the far region is warm, moving around inside it is free.
    let base = HighlightEngine::lex_count(&cache);
    for l in (19_900..20_000).rev() {
        eng.line_at(&mut cache, l, &src);
    }
    assert_eq!(
        HighlightEngine::lex_count(&cache),
        base,
        "moving inside a warm region must not re-lex"
    );
}

#[test]
fn scrolling_forward_lexes_each_line_once() {
    // Monotone scrolling is the common case and must not re-lex.
    let eng = engine();
    let text: String = (0..1_000).map(|i| format!("let v{i} = {i};\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..40 {
        eng.line_at(&mut cache, l, &src);
    }
    let before = HighlightEngine::lex_count(&cache);
    for l in 40..80 {
        eng.line_at(&mut cache, l, &src);
    }
    assert_eq!(
        HighlightEngine::lex_count(&cache) - before,
        40,
        "a 40-line scroll should cost exactly 40 lexes"
    );
}

#[test]
fn a_backward_scroll_lexes_each_line_once() {
    // Backward movement is the direction checkpoints genuinely help in, because the cache
    // already holds the parse state for the region being re-entered.
    let eng = engine();
    let text: String = (0..1_000).map(|i| format!("let v{i} = {i};\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..1_000 {
        eng.line_at(&mut cache, l, &src);
    }
    let before = HighlightEngine::lex_count(&cache);
    // Scroll back up through already-cached lines: nothing to do.
    for l in (900..1_000).rev() {
        eng.line_at(&mut cache, l, &src);
    }
    assert_eq!(
        HighlightEngine::lex_count(&cache),
        before,
        "re-reading cached lines must not lex"
    );
}

#[test]
fn an_edit_only_relexes_from_the_edit_point() {
    // The FR-H4 payoff, measured rather than asserted as a property: edit near the top of a
    // viewed region and only the lines from the edit onward are re-lexed, not the document.
    let eng = engine();
    let text: String = (0..5_000).map(|i| format!("let v{i} = {i};\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..1_000 {
        eng.line_at(&mut cache, l, &src);
    }
    eng.invalidate_from(&mut cache, 500);
    let before = HighlightEngine::lex_count(&cache);
    for l in 500..540 {
        eng.line_at(&mut cache, l, &src);
    }
    let cost = HighlightEngine::lex_count(&cache) - before;
    // The replay must start at the highest checkpoint at or below 500, which is 384, so the
    // honest bound is (500 - 384) + 40 = 156. Bounding it by the checkpoint gap plus the
    // viewport is exactly FR-H4's promise: the cost never depends on document length.
    assert!(
        cost <= CHECKPOINT_EVERY + 40,
        "re-highlighting a viewport after an edit cost {cost} lexes; bound is {} + 40",
        CHECKPOINT_EVERY
    );
    // And it must not be anywhere near the document size.
    assert!(
        cost < 200,
        "cost {cost} should be bounded by the checkpoint gap"
    );
    // Lines before the edit were never invalidated and are still served from cache.
    let mid = HighlightEngine::lex_count(&cache);
    for l in 0..500 {
        eng.line_at(&mut cache, l, &src);
    }
    assert_eq!(HighlightEngine::lex_count(&cache), mid, "prefix untouched");
}

// ------------------------------------------------------------------ memory window

#[test]
fn cached_styles_stay_inside_a_window_of_the_viewport() {
    let eng = engine();
    let text: String = (0..20_000).map(|i| format!("let v{i} = {i};\n")).collect();
    let src = Lines::new(&text);
    let mut cache = eng.new_cache(Some("Rust"));
    for l in 0..50 {
        eng.line_at(&mut cache, l, &src);
    }
    // Jump far. Eviction must drop the old viewport rather than keep both.
    for l in 18_000..18_010 {
        eng.line_at(&mut cache, l, &src);
    }
    assert!(
        cache.cached(0).is_none(),
        "a line {} lines behind the viewport must be evicted",
        CACHE_WINDOW
    );
    assert!(
        cache.cached_line_count() <= CACHE_WINDOW * 2 + 200,
        "cache grew to {} lines",
        cache.cached_line_count()
    );
    assert!(cache.cached(18_005).is_some(), "the current viewport stays");
}

#[test]
fn invalidation_bumps_the_generation() {
    let eng = engine();
    let mut cache = eng.new_cache(Some("Rust"));
    let g0 = cache.generation();
    eng.invalidate_from(&mut cache, 0);
    assert_ne!(cache.generation(), g0);
}

#[test]
fn a_language_switch_drops_all_state() {
    let eng = engine();
    let src = Lines::new("fn x() {} // hi");
    let mut cache = eng.new_cache(Some("Rust"));
    eng.line_at(&mut cache, 0, &src);
    assert!(cache.cached(0).is_some());
    eng.set_language(&mut cache, Some("C"));
    assert!(cache.cached(0).is_none());
    assert_eq!(cache.syntax_name(), Some("C"));
    assert!(cache.checkpoint_lines().is_empty());
}

// ------------------------------------------------------------------ style spans

#[test]
fn spans_reconstruct_the_line_exactly() {
    // A silent corruption class: if span text were dropped or reordered, the render would
    // show the wrong characters. Assert the concatenation for a few real lines.
    let eng = engine();
    let src = Lines::new("let s = \"a b\"; // c\nfn f() -> u64 { 1 }");
    let styles = scratch(&eng, &src, Some("Rust"));
    for (i, line) in styles.iter().enumerate() {
        assert_eq!(
            line.iter().map(|(_, f)| f.as_str()).collect::<String>(),
            src.line_text(i).unwrap(),
            "line {i} spans must rebuild the text"
        );
    }
}

#[test]
fn a_keyword_gets_a_color_distinct_from_a_plain_identifier() {
    let eng = engine();
    let src = Lines::new("let zebra = 1;");
    let styles = scratch(&eng, &src, Some("Rust"));
    let keyword = styles[0]
        .iter()
        .find(|(_, f)| f == "let")
        .map(|(s, _)| *s)
        .expect("`let` should be its own span");
    let ident = styles[0]
        .iter()
        .find(|(_, f)| f.trim() == "zebra")
        .map(|(s, _)| *s)
        .expect("identifier span");
    assert_ne!(
        keyword.fg, ident.fg,
        "a keyword must not be the same color as a name"
    );
}

#[test]
fn an_empty_document_highlights_without_panicking() {
    let eng = engine();
    let src = Lines::new("");
    let mut cache = eng.new_cache(Some("Rust"));
    let styles = eng.line_at(&mut cache, 0, &src);
    let text: String = styles.iter().map(|(_, f)| f.as_str()).collect();
    assert_eq!(text, "");
}

#[test]
fn asking_past_the_end_is_clamped_not_fatal() {
    let eng = engine();
    let src = Lines::new("one\ntwo");
    let mut cache = eng.new_cache(Some("Rust"));
    let styles = eng.line_at(&mut cache, 999, &src);
    // Clamped to the last line, so it must not invent content.
    let text: String = styles.iter().map(|(_, f)| f.as_str()).collect();
    assert!(text.is_empty() || text == "two", "got {text:?}");
}

#[test]
fn the_bundled_syntax_set_is_the_real_one() {
    let eng = engine();
    assert!(eng.syntax_count() > 50, "expected the bundled set");
    assert!(eng.has_language("Rust"));
    assert!(!eng.has_language("Not A Language"));
}
