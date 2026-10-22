//! A line comment must not paint the rest of the file (FR-H1).
//!
//! Reported from a real terminal: in a C++ file, after the first `//`, every following line
//! was highlighted as a comment until the end of the file.
//!
//! # The cause, in one paragraph
//!
//! The syntax set is loaded with `SyntaxSet::load_defaults_newlines()`, whose grammars match
//! the line terminator explicitly -- a C++ line comment ends at `\n`. `LineSource` hands over
//! a line *without* its terminator, because that is what every other consumer of a line wants
//! and what `Buffer::line_string` has always returned. Feed syntect a terminator-less line and
//! the comment scope never closes; the parse state carries it forward and every later line
//! inherits comment color.
//!
//! # Why the S12 suite could not see it
//!
//! `highlight_s12.rs` checks the incremental cache against a from-scratch reference pass. Both
//! sides fed the source the same terminator-less way, so they agreed -- on being wrong. That
//! is the general lesson: an equivalence test between two consumers of one wrong assumption
//! proves consistency and nothing else. The tests below assert about the *colors*, which is
//! the level the user sees.

use np_core::highlight::{HighlightEngine, LineSource};

struct Lines(Vec<String>);

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

/// The default theme's comment foreground as the theme declares it: `base16-ocean.dark` grey.
///
/// Resolved through the engine rather than pinned as a literal, because this test is about scope
/// *reach* -- does a block color the lines under it -- and a hardcoded RGB makes it also assert the
/// exact palette value. S33 adjusts syntax colors for legibility against the ground, so the literal
/// drifts for reasons that have nothing to do with scope matching, and a highlighting-scope test
/// failing on a contrast tweak sends the reader to the wrong file.
fn comment_rgb() -> Option<(u8, u8, u8)> {
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let src = Lines::new("// c\n");
    let mut cache = engine.new_cache(Some(&syntax));
    engine
        .line_at(&mut cache, 0, &src)
        .first()
        .and_then(|(s, _)| s.fg.as_ref())
        .and_then(|c| c.rgb)
}

fn is_comment(style: &np_core::CoreStyle) -> bool {
    let want = comment_rgb().expect("the default theme colors a line comment");
    style.fg.as_ref().and_then(|c| c.rgb) == Some(want)
}

const CPP: &str = "\
int main() {
    // a line comment
    int x = 1;
    return x;
}
void other() {}
// trailing comment
int after = 2;
";

/// The whole point: no line after a `//` is still inside the comment.
fn comment_leak(lines: &[String], engine: &HighlightEngine, syntax: &str) -> Vec<usize> {
    let src = Lines::new(&lines.join("\n"));
    let mut cache = engine.new_cache(Some(syntax));
    let mut leaked = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let styles = engine.line_at(&mut cache, i, &src);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // A non-comment line whose every fragment is comment-colored is a leaked scope. The
        // test is "every", not "any": a line that *contains* a comment legitimately has one
        // grey fragment, and that is correct highlighting, not a bug.
        let all_comment = !styles.is_empty() && styles.iter().all(|(s, _)| is_comment(s));
        if all_comment {
            leaked.push(i);
        }
    }
    leaked
}

#[test]
fn a_line_comment_does_not_leak_past_its_line() {
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let lines: Vec<String> = CPP.lines().map(str::to_string).collect();
    let leaked = comment_leak(&lines, &engine, &syntax);
    assert!(
        leaked.is_empty(),
        "{} non-comment lines are entirely comment-colored: {:?}",
        leaked.len(),
        leaked.iter().map(|i| &lines[*i]).collect::<Vec<_>>()
    );
}

/// The leak was reported for a scroll down a file, which is the monotone access path, and
/// for a jump, which is not. Both go through the anchor and the checkpoints, and they take
/// different routes through the parser state, so both are asserted.
#[test]
fn no_leak_under_reverse_or_repeated_access() {
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let lines: Vec<String> = CPP.lines().map(str::to_string).collect();
    let src = Lines::new(&lines.join("\n"));

    // Reverse: last line first, walking back to the top.
    let mut cache = engine.new_cache(Some(&syntax));
    let mut leaked = Vec::new();
    for (i, line) in lines.iter().enumerate().rev() {
        let styles = engine.line_at(&mut cache, i, &src);
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !styles.is_empty()
            && styles.iter().all(|(s, _)| is_comment(s))
        {
            leaked.push(i);
        }
    }
    assert!(leaked.is_empty(), "reverse walk leaked on lines {leaked:?}");

    // A fresh cache per line: no carried state at all, the "open the file at line 40" case.
    let mut leaked = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let mut fresh = engine.new_cache(Some(&syntax));
        let styles = engine.line_at(&mut fresh, i, &src);
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !styles.is_empty()
            && styles.iter().all(|(s, _)| is_comment(s))
        {
            leaked.push(i);
        }
    }
    assert!(
        leaked.is_empty(),
        "fresh-cache walk leaked on lines {leaked:?}"
    );
}

/// The fix appends a `\n` for the parser and clips the fragments back afterwards. If that
/// clip is wrong the user sees a phantom character or a truncated line, so the fragments
/// must reconstruct the source exactly -- no extra newline, nothing dropped.
#[test]
fn spans_reconstruct_the_line_exactly() {
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let lines: Vec<String> = CPP.lines().map(str::to_string).collect();
    let src = Lines::new(&lines.join("\n"));
    let mut cache = engine.new_cache(Some(&syntax));
    for (i, line) in lines.iter().enumerate() {
        let styles = engine.line_at(&mut cache, i, &src);
        let joined: String = styles.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(
            &joined, line,
            "line {i} fragments must equal the source exactly"
        );
        for (_, frag) in &styles {
            assert!(
                !frag.contains('\n'),
                "line {i} carries a fragment with a newline in it: {frag:?}"
            );
        }
    }
}

/// The scope that legitimately *spans* lines is the harder half of the same fix. A block
/// comment or an unterminated string absorbs the appended `\n` into its own fragment, so a
/// clip written as `strip_suffix("\n")` would leave a stray newline on those lines while
/// working on every ordinary one.
#[test]
fn a_multiline_scope_still_reconstructs_exactly() {
    let src_text = "\
/* block start
 * still inside the block
 */
int ok = 1;
let s = \"unterminated string
int after = 2;
";
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let lines: Vec<String> = src_text.lines().map(str::to_string).collect();
    let src = Lines::new(src_text);
    let mut cache = engine.new_cache(Some(&syntax));
    for (i, line) in lines.iter().enumerate() {
        let styles = engine.line_at(&mut cache, i, &src);
        let joined: String = styles.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(
            joined, *line,
            "line {i} inside a multiline scope must not gain or lose characters"
        );
    }
}

/// The block comment must actually color the lines it covers. Asserting only the negative
/// ("no leak") would pass on an engine that colored nothing at all, which is the failure mode
/// a highlighting test is most likely to hide.
#[test]
fn a_block_comment_does_reach_the_lines_it_covers() {
    let src_text = "/* open\nint inside = 1;\nclosed */\nint outside = 2;\n";
    let engine = HighlightEngine::load();
    let syntax = engine
        .detect_syntax(Some(std::path::Path::new("/tmp/x.cpp")), "")
        .expect("c++ syntax available");
    let src = Lines::new(src_text);
    let mut cache = engine.new_cache(Some(&syntax));
    let inside = engine.line_at(&mut cache, 1, &src);
    assert!(
        !inside.is_empty() && inside.iter().all(|(s, _)| is_comment(s)),
        "line 1 is inside a block comment and should be entirely comment-colored, got {inside:?}"
    );
    let outside = engine.line_at(&mut cache, 3, &src);
    assert!(
        !outside.is_empty() && !outside.iter().any(|(s, _)| is_comment(s)),
        "line 3 is after the block closes and should not be comment-colored, got {outside:?}"
    );
}
