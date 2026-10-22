//! S13 — fold detection and fold state (FR-V5).
//!
//! Fold detection is the part of this stage a reader can check by eye, which makes it easy to
//! believe it works. The cases that actually decide whether the feature is usable are the ugly
//! ones: a stray brace in a string, a `def` whose body is indented with tabs, a block that ends
//! one line before its sibling's blank separator. Each test below names the failure a user would
//! see, because a wrong fold range does not announce itself -- it just hides the wrong lines.

use np_core::{
    compute_folds, is_brace_language, merge_folds, set_all_collapsed, shift_folds_insert,
    shift_folds_remove, toggle_fold_at, unfold_containing, FoldRange,
};

fn range(start: usize, end: usize) -> FoldRange {
    FoldRange {
        start,
        end,
        collapsed: false,
    }
}

fn closed(start: usize, end: usize) -> FoldRange {
    FoldRange {
        start,
        end,
        collapsed: true,
    }
}

fn spans(folds: &[FoldRange]) -> Vec<(usize, usize)> {
    folds.iter().map(|f| (f.start, f.end)).collect()
}

fn has(folds: &[FoldRange], start: usize, end: usize) -> bool {
    folds.iter().any(|f| f.start == start && f.end == end)
}

// ------------------------------------------------------------- language scope

#[test]
fn brace_folding_is_limited_to_brace_languages() {
    for lang in [
        "Rust",
        "C",
        "C++",
        "Java",
        "JavaScript",
        "Go",
        "TypeScript",
        "json",
    ] {
        assert!(is_brace_language(lang), "{lang} should fold on braces");
    }
    for lang in ["", "Plain Text", "Python", "YAML", "Markdown", "ini"] {
        assert!(!is_brace_language(lang), "{lang} should not fold on braces");
    }
}

// ---------------------------------------------------------- indentation folds

/// A Python-shaped fixture: nesting is the only structure, and blank lines sit between
/// siblings.
fn python() -> &'static str {
    "def outer():\n    a = 1\n    if a:\n        b = 2\n\ndef other():\n    c = 3\nx = 4\n"
}

#[test]
fn indentation_folds_a_python_block() {
    let folds = compute_folds(python(), "Python");
    // line 0 `def outer():` folds lines 1-3; line 2 `if a:` folds line 3;
    // line 5 `def other():` folds line 6.
    assert!(has(&folds, 0, 3), "outer body: {:?}", spans(&folds));
    assert!(has(&folds, 2, 3), "if body: {:?}", spans(&folds));
    assert!(has(&folds, 5, 6), "second def: {:?}", spans(&folds));
    // Line 4 is the blank between the two defs and line 7 is dedented top level: neither is a
    // header, and neither may be swallowed into the fold above it.
    assert!(
        !folds.iter().any(|f| f.start == 4),
        "a blank line is not a header"
    );
    assert!(
        !folds.iter().any(|f| f.start == 0 && f.end >= 4),
        "a fold must not drag the blank separator between siblings into the block above: {:?}",
        spans(&folds)
    );
}

#[test]
fn indentation_folding_runs_for_plain_text_too() {
    // An outline made of indentation is foldable even with no language at all. Not a nicety:
    // the majority of files opened are plain text, and a fold that only works in a recognized
    // language is a fold the user never finds.
    let text = "Chapter one\n  section a\n  section b\nChapter two\n  section c\n";
    let folds = compute_folds(text, "");
    assert!(has(&folds, 0, 2), "outline: {:?}", spans(&folds));
    assert!(has(&folds, 3, 4), "outline: {:?}", spans(&folds));
}

#[test]
fn tab_indented_and_space_indented_files_fold_the_same_way() {
    let spaces = "def f():\n    a = 1\n    b = 2\n";
    let tabs = "def f():\n\ta = 1\n\tb = 2\n";
    assert_eq!(
        spans(&compute_folds(spaces, "Python")),
        spans(&compute_folds(tabs, "Python")),
        "indent unit must not change the fold structure"
    );
}

#[test]
fn a_line_with_nothing_deeper_below_it_is_not_a_fold() {
    let text = "top\n    indented\n";
    let folds = compute_folds(text, "");
    assert_eq!(
        spans(&folds),
        vec![(0, 1)],
        "one fold, its header, and that is all"
    );
    // The indented line itself has nothing under it.
    assert!(!folds.iter().any(|f| f.start == 1));
}

#[test]
fn no_fold_range_is_shorter_than_two_lines() {
    // Spec: never produce a range shorter than 2 lines. A one-line range hides nothing but
    // still draws a marker, which reads as a control that does nothing when clicked.
    for text in [
        "a\n",
        "a\nb\n",
        "  \n",
        "\n\n\n",
        "def f():\n    x\n",
        "a\n  b\n  c\n",
    ] {
        for lang in ["", "Rust", "Python"] {
            for fold in compute_folds(text, lang) {
                assert!(
                    fold.end > fold.start,
                    "{lang:?} over {text:?} produced a degenerate {fold:?}"
                );
            }
        }
    }
}

// -------------------------------------------------------------- brace folds

#[test]
fn brace_folds_a_c_like_block() {
    let text = "fn main() {\n    let a = 1;\n    if a > 0 {\n        println!(\"x\");\n    }\n}\nfn other() {}\n";
    let folds = compute_folds(text, "Rust");
    assert!(has(&folds, 0, 5), "main body: {:?}", spans(&folds));
    assert!(has(&folds, 2, 4), "if body: {:?}", spans(&folds));
    // A fold's `start` is the header line -- the one carrying the `{` -- because that line has
    // to stay on screen for the marker to be clickable.
    for fold in &folds {
        let header = text.lines().nth(fold.start).unwrap_or("");
        assert!(
            header.contains('{'),
            "fold {fold:?} starts on a line with no opening brace: {header:?}"
        );
    }
}

#[test]
fn an_unbalanced_open_brace_does_not_fold_the_rest_of_the_file() {
    // The single most important case. A stray `{` with no `}` must not swallow everything
    // below it -- that is an editor that appears to lose the user's text.
    let text = "fn broken() {\n    let a = 1;\n}\n\nfn fine() {\n    let b = 2;\n";
    let folds = compute_folds(text, "Rust");
    assert!(
        !folds.iter().any(|f| f.start == 0 && f.end >= 3),
        "the unclosed brace at line 0 folded past its real end: {:?}",
        spans(&folds)
    );
    assert!(
        !folds.iter().any(|f| f.end == 6 && f.start == 0),
        "a fold ran to the end of the file, which is the signature of an unbalanced opener"
    );
}

#[test]
fn an_unbalanced_close_brace_does_not_fold_backwards() {
    let text = "let a = 1;\n}\nlet b = 2;\nfn ok() {\n    let c = 3;\n}\n";
    let folds = compute_folds(text, "Rust");
    for fold in &folds {
        assert!(
            fold.start < fold.end,
            "a closer with nothing on the stack produced a backwards {fold:?}"
        );
    }
    assert!(
        has(&folds, 3, 5),
        "the balanced block still folds: {:?}",
        spans(&folds)
    );
}

#[test]
fn braces_inside_strings_and_comments_are_ignored() {
    // `"{"` in a string is the classic way a brace *counter* produces a fold that eats half a
    // file. Detection is a state scanner for exactly this reason.
    let text = "fn a() {\n    let s = \"{\";\n    let t = \"}\";\n    // }\n    /* { } */\n    let u = '}';\n}\n";
    let folds = compute_folds(text, "Rust");
    assert_eq!(
        spans(&folds),
        vec![(0, 6)],
        "exactly one fold for the one real block: {:?}",
        spans(&folds)
    );
}

#[test]
fn a_block_comment_spanning_lines_does_not_leak_braces() {
    let text = "fn a() {\n    /* {\n       }\n    */\n    let x = 1;\n}\n";
    let folds = compute_folds(text, "Rust");
    assert_eq!(
        spans(&folds),
        vec![(0, 5)],
        "comment braces ignored: {:?}",
        spans(&folds)
    );
}

#[test]
fn a_rust_lifetime_apostrophe_does_not_swallow_following_braces() {
    // `'` is a string delimiter in C-like languages and a lifetime in Rust. Carrying an
    // unterminated `'` across a line boundary would ignore every brace after it.
    let text = "fn f<'a>(x: &'a str) {\n    let s = x;\n    let t = '{';\n}\n";
    let folds = compute_folds(text, "Rust");
    assert!(
        has(&folds, 0, 3),
        "lifetime must not break folding: {:?}",
        spans(&folds)
    );
}

#[test]
fn nested_brace_folds_are_all_reported() {
    let text = "a {\n b {\n  c {\n   d\n  }\n }\n}\n";
    let folds = compute_folds(text, "C");
    assert!(has(&folds, 0, 6), "outer: {:?}", spans(&folds));
    assert!(has(&folds, 1, 5), "middle: {:?}", spans(&folds));
    assert!(has(&folds, 2, 4), "inner: {:?}", spans(&folds));
}

#[test]
fn indent_and_brace_agreement_produces_no_duplicate_range() {
    // A brace body is also an indented body. Both strategies report it, so the union has to be
    // deduplicated -- otherwise a caller keying fold state on (start, end) sees one of two
    // identical ranges win arbitrarily.
    let text = "fn f() {\n    let a = 1;\n}\n";
    let folds = compute_folds(text, "Rust");
    let mut seen = spans(&folds);
    let len = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), len, "duplicate ranges: {:?}", spans(&folds));
}

// ------------------------------------------------------------- fold state

#[test]
fn merge_folds_keeps_the_users_collapsed_choice() {
    let computed = vec![range(0, 5), range(2, 4), range(7, 9)];
    let kept = vec![closed(0, 5)];
    let merged = merge_folds(&computed, &kept);
    assert_eq!(spans(&merged), vec![(0, 5), (2, 4), (7, 9)]);
    assert!(merged[0].collapsed, "the fold the user closed stays closed");
    assert!(!merged[1].collapsed && !merged[2].collapsed);
}

#[test]
fn merge_folds_does_not_guess_when_a_range_moved() {
    // A shifted range is a *different* range. Matching on fuzziness is how an editor ends up
    // with a collapsed block hiding the wrong lines, which is far worse than an open fold.
    let computed = vec![range(1, 6)];
    let kept = vec![closed(0, 5)];
    let merged = merge_folds(&computed, &kept);
    assert_eq!(merged.len(), 1);
    assert!(
        !merged[0].collapsed,
        "a moved range must not inherit collapsed"
    );
}

#[test]
fn merge_folds_drops_ranges_that_no_longer_exist() {
    let computed = vec![range(0, 3)];
    let kept = vec![closed(0, 3), closed(10, 20)];
    let merged = merge_folds(&computed, &kept);
    assert_eq!(
        spans(&merged),
        vec![(0, 3)],
        "the deleted block's fold is gone"
    );
}

#[test]
fn fold_state_survives_a_recompute_after_an_unrelated_edit() {
    // The scenario a user actually hits: fold a function, type somewhere else in the file, and
    // the fold must still be folded. Detection is recomputed; only the *collapsed* set is
    // carried across, by exact range match.
    let text = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    let y = 2;\n}\n";
    let computed = compute_folds(text, "Rust");
    let mut state = merge_folds(&computed, &[]);
    assert!(toggle_fold_at(&mut state, 0), "folding a()");

    // An edit below both folds: add a line at the end of the document.
    let edited = format!("{text}\n// note\n");
    let recomputed = compute_folds(&edited, "Rust");
    let after = merge_folds(&recomputed, &state);
    let fold_a = after
        .iter()
        .find(|f| f.start == 0)
        .expect("fold a still detected");
    assert!(
        fold_a.collapsed,
        "an unrelated edit must not open a fold: {after:?}"
    );
}

#[test]
fn set_all_collapsed_covers_every_range() {
    let mut folds = compute_folds("fn a() {\n x\n}\nfn b() {\n y\n}\n", "Rust");
    set_all_collapsed(&mut folds, true);
    assert!(folds.iter().all(|f| f.collapsed), "fold all");
    set_all_collapsed(&mut folds, false);
    assert!(folds.iter().all(|f| !f.collapsed), "unfold all");
    assert!(!folds.is_empty(), "fixture must have folds to set");
}

#[test]
fn toggle_only_touches_the_fold_that_starts_on_that_line() {
    let mut folds = vec![range(0, 9), range(3, 6)];
    assert!(toggle_fold_at(&mut folds, 3), "inner fold toggles");
    assert!(
        folds
            .iter()
            .find(|f| f.start == 3)
            .expect("inner")
            .collapsed
    );
    assert!(
        !folds
            .iter()
            .find(|f| f.start == 0)
            .expect("outer")
            .collapsed,
        "a marker controls the block that begins on its line, never one that merely contains it"
    );
}

#[test]
fn toggling_a_shared_header_moves_every_range_with_that_header() {
    // A brace block whose body is also an indent block can end one line apart. Half-collapsing
    // it would leave a stub of the block on screen.
    let mut folds = vec![range(0, 9), range(0, 8)];
    assert!(toggle_fold_at(&mut folds, 0));
    assert!(
        folds.iter().all(|f| f.collapsed),
        "both ranges sharing the header moved together: {folds:?}"
    );
    assert!(toggle_fold_at(&mut folds, 0));
    assert!(folds.iter().all(|f| !f.collapsed));
}

#[test]
fn toggle_reports_false_when_no_fold_starts_there() {
    let mut folds = vec![range(0, 9)];
    assert!(
        !toggle_fold_at(&mut folds, 5),
        "line 5 is inside, not a header"
    );
    assert!(!folds[0].collapsed, "nothing changed");
}

#[test]
fn unfold_containing_opens_every_collapsed_range_over_the_line() {
    let mut folds = vec![closed(0, 9), closed(3, 6)];
    assert!(unfold_containing(&mut folds, 4));
    assert!(
        folds.iter().all(|f| !f.collapsed),
        "both nested folds opened"
    );
}

#[test]
fn unfold_containing_is_false_when_nothing_was_collapsed() {
    let mut folds = vec![range(0, 9)];
    assert!(!unfold_containing(&mut folds, 4));
}

// ------------------------------------------------------------- fold shifting

#[test]
fn inserting_lines_below_a_fold_moves_it_down() {
    let mut folds = vec![range(10, 20)];
    shift_folds_insert(&mut folds, 0, 5);
    assert_eq!(spans(&folds), vec![(15, 25)]);
}

#[test]
fn inserting_lines_inside_a_fold_grows_it() {
    let mut folds = vec![range(10, 20)];
    shift_folds_insert(&mut folds, 12, 3);
    assert_eq!(spans(&folds), vec![(10, 23)], "header fixed, body longer");
}

#[test]
fn inserting_at_a_header_keeps_the_header() {
    // "Type at the start of a function body": the header line is unchanged and the block got
    // longer. Moving the header would move the fold marker off the line the user is looking at.
    let mut folds = vec![range(10, 20)];
    shift_folds_insert(&mut folds, 10, 2);
    assert_eq!(spans(&folds), vec![(10, 22)]);
}

#[test]
fn deleting_lines_above_a_fold_pulls_it_up() {
    // Five lines removed from the top: the fold's header was line 10 and is now line 5.
    let mut folds = vec![range(10, 20)];
    shift_folds_remove(&mut folds, 0, 4);
    assert_eq!(spans(&folds), vec![(5, 15)]);
}

#[test]
fn deleting_a_fold_whole_removes_the_fold() {
    let mut folds = vec![range(0, 5), range(10, 20)];
    shift_folds_remove(&mut folds, 10, 20);
    assert_eq!(
        spans(&folds),
        vec![(0, 5)],
        "the deleted block's fold is gone, not clamped to nothing"
    );
}

#[test]
fn a_fold_left_shorter_than_two_lines_is_dropped() {
    // A fold that hides nothing must not keep a marker.
    let mut folds = vec![range(5, 7)];
    shift_folds_remove(&mut folds, 6, 7);
    assert!(folds.is_empty(), "degenerate fold survived: {folds:?}");
}

#[test]
fn a_fold_partially_overlapping_a_deletion_shrinks() {
    let mut folds = vec![range(5, 20)];
    shift_folds_remove(&mut folds, 8, 12);
    assert_eq!(spans(&folds), vec![(5, 15)], "clamped to what survived");
}

#[test]
fn shifting_is_a_no_op_for_an_empty_span() {
    let mut folds = vec![range(5, 10)];
    shift_folds_insert(&mut folds, 7, 0);
    shift_folds_remove(&mut folds, 9, 3);
    assert_eq!(spans(&folds), vec![(5, 10)]);
}

// ------------------------------------------------------------- detection cost

#[test]
fn detection_is_pure_and_stable_across_repeats() {
    let text = "fn a() {\n  if b {\n    c\n  }\n}\n";
    let first = compute_folds(text, "Rust");
    let second = compute_folds(text, "Rust");
    assert_eq!(
        spans(&first),
        spans(&second),
        "detection must be a pure function"
    );
    assert!(
        first.iter().all(|f| !f.collapsed),
        "detection never decides state"
    );
}

#[test]
fn detection_on_empty_and_whitespace_input_is_empty_not_panicky() {
    for text in ["", "\n", "   \n\t\n", "\n\n\n"] {
        assert!(
            compute_folds(text, "Rust").is_empty(),
            "{text:?} produced folds"
        );
        assert!(compute_folds(text, "").is_empty());
    }
}
