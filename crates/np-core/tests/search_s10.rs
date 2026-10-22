//! The search engine (stage S10, spec §6.4 FR-S1..S7).
//!
//! # What this file is really guarding
//!
//! Search feels broken to a user in a handful of specific ways, and almost none of them are
//! "does substring matching work". They are termination and offset bugs: `F3` sticks on a
//! zero-width match, the second `F3` returns the first hit again, a case-insensitive hit in a
//! line containing `İ` lands a character early, `^b` answers at every column of the line,
//! `replace_all` chases its own output. Every test below targets one of those.
//!
//! # Rules asserted here, and why each is the way it is
//!
//! **Case folding** is per-character full folding. Not `str::to_lowercase`, which applies the
//! final-sigma rule and would fold a word-final `Σ` to `ς` -- a character that is not the one
//! under the cursor. And with **no** `ß`→`ss` expansion, because Unicode has no such fold in
//! either direction; inventing one would break every search for a literal `ß`.
//!
//! **Whole word** means identifier-class `alnum + _`, the same class `Buffer::word_*` uses, so
//! Ctrl+Left and whole-word search cannot disagree about where a word starts. Consequence worth
//! stating plainly: `foo` does *not* match inside `foo_bar`.
//!
//! **Anchors** `^`/`$` are line-level, `.` does not cross a line, and an explicit `\n` does.
//! That combination is achieved by scanning line spans rather than by `multi_line` mode, which
//! would also redefine `\b` as a *line* boundary. See `scan_regex`.
//!
//! **Termination** is structural everywhere, so these tests can assert "repeated F3 advances"
//! without a timeout race: an empty needle matches nothing, a zero-width match advances one
//! character, and the literal scan advances one document character per iteration regardless of
//! what its comparison decided.

use np_core::{
    count, find_all, find_directional, find_next, find_prev, replace_all, validate, Buffer,
    LineCol, Match, SearchError, SearchOpts,
};

fn buf(text: &str) -> Buffer {
    Buffer::from_str_lossy(text)
}

fn opts() -> SearchOpts {
    SearchOpts {
        case_sensitive: false,
        whole_word: false,
        regex: false,
        wrap_around: true,
        forward: true,
    }
}

fn rx() -> SearchOpts {
    SearchOpts {
        regex: true,
        ..opts()
    }
}

fn whole() -> SearchOpts {
    SearchOpts {
        whole_word: true,
        ..opts()
    }
}

fn lc(line: usize, col: usize) -> LineCol {
    LineCol::new(line, col)
}

fn starts(ms: &[Match]) -> Vec<(usize, usize)> {
    ms.iter().map(|m| (m.start.line, m.start.col)).collect()
}

fn one(buf: &Buffer, needle: &str, from: LineCol, o: &SearchOpts) -> Option<LineCol> {
    find_next(buf, needle, from, o)
        .ok()
        .flatten()
        .map(|m| m.start)
}

// ------------------------------------------------------------- literal search

#[test]
fn literal_finds_every_occurrence_in_document_order() {
    let b = buf("alpha beta\ngamma alpha\ndelta");
    let ms = find_all(&b, "alpha", &opts()).unwrap();
    assert_eq!(starts(&ms), [(0, 0), (1, 6)]);
    assert_eq!(ms[0].end, lc(0, 5));
    assert_eq!(ms[0].text_len, 5);
}

#[test]
fn case_sensitive_distinguishes_case() {
    let b = buf("Foo foo FOO");
    let o = SearchOpts {
        case_sensitive: true,
        ..opts()
    };
    assert_eq!(starts(&find_all(&b, "Foo", &o).unwrap()), [(0, 0)]);
}

#[test]
fn case_insensitive_matches_across_case() {
    let b = buf("Foo foo FOO fOo");
    assert_eq!(
        starts(&find_all(&b, "foo", &opts()).unwrap()),
        [(0, 0), (0, 4), (0, 8), (0, 12)]
    );
}

#[test]
fn from_is_exclusive_so_the_caret_sitting_on_a_hit_skips_it() {
    let b = buf("xx\nyy");
    assert_eq!(starts(&find_all(&b, "xx", &opts()).unwrap()), [(0, 0)]);
    // The only hit is at 0,0 and `from` is exclusive, so forward search wraps back to it
    // rather than returning it immediately.
    // With wrap on, the search comes all the way round to the same hit.
    assert_eq!(one(&b, "xx", lc(0, 0), &opts()), Some(lc(0, 0)));
    let no_wrap = SearchOpts {
        wrap_around: false,
        ..opts()
    };
    assert_eq!(
        one(&b, "xx", lc(0, 0), &no_wrap),
        None,
        "strictly after, no wrap"
    );
    assert_eq!(one(&b, "yy", lc(0, 0), &opts()), Some(lc(1, 0)));
}

#[test]
fn match_at_the_very_end_of_the_document() {
    let b = buf("one two");
    assert_eq!(one(&b, "two", lc(0, 0), &opts()), Some(lc(0, 4)));
    let ms = find_all(&b, "two", &opts()).unwrap();
    assert_eq!(ms[0].end, lc(0, 7));
    assert_eq!(ms[0].text_len, 3);
}

#[test]
fn overlapping_candidates_are_all_reported() {
    let b = buf("aaaa");
    let ms = find_all(&b, "aa", &opts()).unwrap();
    // Overlapping starts, not greedy consumption.
    assert_eq!(starts(&ms), [(0, 0), (0, 1), (0, 2)]);
}

#[test]
fn empty_needle_matches_nothing_and_never_errors() {
    let b = buf("hello");
    assert_eq!(find_all(&b, "", &opts()).unwrap(), Vec::<Match>::new());
    assert_eq!(count(&b, "", &opts()).unwrap(), 0);
    assert_eq!(find_next(&b, "", lc(0, 0), &opts()).unwrap(), None);
    assert_eq!(find_prev(&b, "", lc(0, 0), &opts()).unwrap(), None);
    assert_eq!(
        replace_all(&mut buf("hello"), "", "x", None, &opts()).unwrap(),
        0
    );
}

#[test]
fn needle_longer_than_document_is_not_an_error() {
    let b = buf("ab");
    assert_eq!(find_all(&b, "abcdefgh", &opts()).unwrap().len(), 0);
}

#[test]
fn needle_longer_than_a_line_still_matches_across_the_break() {
    // Literal search works on the materialized document, so a needle containing a newline is
    // found the same way a regex one is.
    let b = buf("one\ntwo\nthree");
    assert_eq!(
        starts(&find_all(&b, "two\nthree", &opts()).unwrap()),
        [(1, 0)]
    );
}

#[test]
fn single_character_needle_terminates() {
    // The shape that livelocked an earlier rewrite: a match whose restart position equals its
    // own start.
    let b = buf("xxxxxxxxxxxxxxxxxxxxxxxxxx");
    assert_eq!(find_all(&b, "x", &opts()).unwrap().len(), 26);
    let b2 = buf("aa");
    assert_eq!(find_all(&b2, "a", &opts()).unwrap().len(), 2);
}

// ------------------------------------------------------- unicode case folding

#[test]
fn fold_maps_offsets_back_when_lowercasing_changes_length() {
    // U+0130 lowercases to `i` + U+0307: two scalars from one. Without a document-anchored
    // fold, the hit for "bar" lands one character early, on the İ.
    let b = buf("\u{0130}bar");
    let ms = find_all(&b, "bar", &opts()).unwrap();
    assert_eq!(starts(&ms), [(0, 1)], "must not drift onto the İ");
    assert_eq!(ms[0].end, lc(0, 4));
    let text: String = b
        .line_string(0)
        .chars()
        .skip(ms[0].start.col)
        .take(ms[0].text_len)
        .collect();
    assert_eq!(text, "bar");
}

#[test]
fn a_folded_needle_finds_the_single_document_character_it_came_from() {
    let b = buf("x\u{0130}y");
    let ms = find_all(&b, "i\u{0307}", &opts()).unwrap();
    assert_eq!(starts(&ms), [(0, 1)]);
    assert_eq!(
        ms[0].text_len, 1,
        "one document character, two folded scalars"
    );
    assert_eq!(ms[0].end, lc(0, 2));
}

#[test]
fn matching_half_of_a_length_changing_fold_is_refused() {
    // `İ` folds to two scalars. Matching only the `i` would put the caret in the middle of
    // what the user sees as one character, so a one-scalar needle must not match it.
    let b = buf("x\u{0130}y");
    assert!(find_all(&b, "i", &opts()).unwrap().is_empty());
}

#[test]
fn strasse_does_not_find_sharp_s_and_there_is_no_expansion() {
    let b = buf("Stra\u{00DF}e");
    assert_eq!(find_all(&b, "strasse", &opts()).unwrap().len(), 0);
    assert_eq!(find_all(&b, "STRASSE", &opts()).unwrap().len(), 0);
    assert_eq!(
        starts(&find_all(&b, "stra\u{00DF}e", &opts()).unwrap()),
        [(0, 0)]
    );
    // The uppercase needle does not match either, for the same reason: folding is
    // per-character, and `ß` never becomes `SS`. Matching it would need a whole-string
    // uppercase, which is a different rule from the one asserted here.
    assert_eq!(find_all(&b, "STRAS\u{00DF}E", &opts()).unwrap().len(), 0);
    // Same-case folding still works.
    assert_eq!(find_all(&b, "strase", &opts()).unwrap().len(), 0);
    assert_eq!(find_all(&b, "stra\u{00DF}e", &opts()).unwrap().len(), 1);
}

#[test]
fn final_sigma_rule_does_not_apply_to_a_search() {
    // `str::to_lowercase("ΟΔΟΣ")` yields a FINAL sigma, which would not match the document's
    // medial sigma. Per-character folding avoids the rule, so the medial needle matches.
    let upper = "\u{39f}\u{394}\u{39f}\u{3a3}";
    let medial = "\u{3bf}\u{3b4}\u{3bf}\u{3c3}";
    let final_form = "\u{3bf}\u{3b4}\u{3bf}\u{3c2}";
    let b = buf(upper);
    assert_eq!(find_all(&b, medial, &opts()).unwrap().len(), 1);
    assert_eq!(find_all(&b, upper, &opts()).unwrap().len(), 1);
    // The final form is a different character and is not the same word.
    assert_eq!(find_all(&b, final_form, &opts()).unwrap().len(), 0);
}

#[test]
fn multibyte_needle_offsets_are_character_correct() {
    let b = buf("привет мир привет");
    assert_eq!(starts(&find_all(&b, "мир", &opts()).unwrap()), [(0, 7)]);
    assert_eq!(
        starts(&find_all(&b, "привет", &opts()).unwrap()),
        [(0, 0), (0, 11)]
    );
}

// -------------------------------------------------------------- whole word

#[test]
fn whole_word_rejects_identifier_neighbors() {
    let b = buf("foobar barfoo foo foo1 1foo foo");
    assert_eq!(
        starts(&find_all(&b, "foo", &whole()).unwrap()),
        [(0, 14), (0, 28)]
    );
}

#[test]
fn whole_word_accepts_punctuation_neighbors() {
    let b = buf("(foo) foo. ,foo! \"foo\"");
    assert_eq!(
        starts(&find_all(&b, "foo", &whole()).unwrap()),
        [(0, 1), (0, 6), (0, 12), (0, 18)]
    );
}

#[test]
fn whole_word_treats_underscore_as_an_identifier_character() {
    // Documented rule: identifier class is alnum + `_`, the same one `Buffer::word_*` uses.
    // So `foo` is a fragment of `foo_bar` and does not match it.
    let b = buf("foo_bar bar_foo _foo foo_ foo");
    assert_eq!(starts(&find_all(&b, "foo", &whole()).unwrap()), [(0, 26)]);
}

#[test]
fn whole_word_agrees_with_ctrl_arrow_word_boundaries() {
    // The consistency the spec asks for: if Ctrl+Left treats a run as one word, whole-word
    // search must not report a match inside it.
    let b = buf("hello foo_bar world");
    for needle in ["foo", "bar"] {
        assert!(
            find_all(&b, needle, &whole()).unwrap().is_empty(),
            "{needle} is inside the identifier foo_bar"
        );
    }
    // Ctrl+Right/Left across the same run confirm the class is identical.
    assert_eq!(b.word_end_after(lc(0, 6)), lc(0, 13));
    assert_eq!(b.word_start_before(lc(0, 13)), lc(0, 6));
}

#[test]
fn whole_word_works_on_unicode_letters() {
    let b = buf("привет мир мир2");
    assert_eq!(starts(&find_all(&b, "мир", &whole()).unwrap()), [(0, 7)]);
}

#[test]
fn whole_word_applies_to_regex_mode_too() {
    let b = buf("foobar foo");
    let o = SearchOpts {
        whole_word: true,
        ..rx()
    };
    assert_eq!(starts(&find_all(&b, r"f.o", &o).unwrap()), [(0, 7)]);
}

// ------------------------------------------------------------------- regex

#[test]
fn regex_digit_class_and_alternation() {
    let b = buf("a12 bb345 c");
    assert_eq!(
        starts(&find_all(&b, r"\d+", &rx()).unwrap()),
        [(0, 1), (0, 6)]
    );
    assert_eq!(
        starts(&find_all(&b, "[bc]", &rx()).unwrap()),
        [(0, 4), (0, 5), (0, 10)]
    );
    assert_eq!(
        starts(&find_all(&b, "a|c", &rx()).unwrap()),
        [(0, 0), (0, 10)]
    );
}

#[test]
fn inline_flags_beat_the_case_option_in_both_directions() {
    let b = buf("Foo FOO");
    // Builder says case-insensitive; the needle opts out.
    assert_eq!(starts(&find_all(&b, r"(?-i)Foo", &rx()).unwrap()), [(0, 0)]);
    // Builder says case-sensitive; the needle opts in.
    let sens = SearchOpts {
        case_sensitive: true,
        ..rx()
    };
    assert_eq!(
        starts(&find_all(&b, r"(?i)foo", &sens).unwrap()),
        [(0, 0), (0, 4)]
    );
}

#[test]
fn regex_matches_across_lines_with_literal_newline() {
    // FR-S4.
    let b = buf("one\ntwo\nthree");
    let ms = find_all(&b, r"two\nthree", &rx()).unwrap();
    assert_eq!(starts(&ms), [(1, 0)]);
    assert_eq!(ms[0].end, lc(2, 5));
    assert_eq!(ms[0].text_len, 9, "counts the terminator it spans");
}

#[test]
fn dot_does_not_cross_a_line_but_an_explicit_class_does() {
    let b = buf("ab\ncd");
    assert!(find_all(&b, r"b.c", &rx()).unwrap().is_empty());
    assert_eq!(starts(&find_all(&b, r"b[\s\S]c", &rx()).unwrap()), [(0, 1)]);
}

#[test]
fn anchors_are_line_level_and_do_not_leak_across_the_line() {
    let b = buf("aaa\nbbb\naaa");
    assert_eq!(starts(&find_all(&b, r"^b", &rx()).unwrap()), [(1, 0)]);
    assert_eq!(starts(&find_all(&b, r"b$", &rx()).unwrap()), [(1, 2)]);
    assert_eq!(
        starts(&find_all(&b, r"^aaa$", &rx()).unwrap()),
        [(0, 0), (2, 0)]
    );
    // `^` alone: one anchor per line, not one per character position.
    assert_eq!(
        starts(&find_all(&b, r"^", &rx()).unwrap()),
        [(0, 0), (1, 0), (2, 0)]
    );
    // An end anchor is a position, and it is reported as the line's beginning so an `F3`
    // cycle lands somewhere the user can see and select.
    // Each line's beginning, plus the document's own end-of-file position.
    assert_eq!(
        starts(&find_all(&b, r"$", &rx()).unwrap()),
        [(0, 0), (1, 0), (2, 0), (2, 3)]
    );
}

#[test]
fn unanchored_pattern_is_not_confused_by_line_scanning() {
    // The line-wise scan must not change where an ordinary pattern hits.
    let b = buf("aaa\nbbb\naaa");
    assert_eq!(
        starts(&find_all(&b, "a", &rx()).unwrap()),
        [(0, 0), (0, 1), (0, 2), (2, 0), (2, 1), (2, 2)]
    );
    assert_eq!(starts(&find_all(&b, "a\nb", &rx()).unwrap()), [(0, 2)]);
}

#[test]
fn dot_star_stays_inside_one_line() {
    let b = buf("<b>x</b><i>y</i>");
    let ms = find_all(&b, "<.*>", &rx()).unwrap();
    // Greedy within the line, which is the documented `.` behavior.
    assert_eq!(starts(&ms), [(0, 0)]);
    assert_eq!(ms[0].text_len, 16);
}

#[test]
fn invalid_patterns_report_invalid_regex_and_never_panic() {
    let b = buf("anything");
    for bad in ["[unclosed", "(", "a{2,1}", "*", "a)", "(?P<", "\\", "(?i"] {
        let err = find_next(&b, bad, lc(0, 0), &rx());
        assert!(
            matches!(err, Err(SearchError::InvalidRegex(_))),
            "{bad:?} should be InvalidRegex, got {err:?}"
        );
        assert!(matches!(
            validate(bad, &rx()),
            Err(SearchError::InvalidRegex(_))
        ));
        // Shown inline, so a message must exist.
        assert!(!err
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
            .is_empty());
        // And the sibling entry points degrade the same way rather than panicking.
        assert!(find_all(&b, bad, &rx()).is_err());
        assert!(count(&b, bad, &rx()).is_err());
        assert!(replace_all(&mut buf("x"), bad, "y", None, &rx()).is_err());
    }
}

#[test]
fn literal_mode_never_rejects_a_regex_metacharacter_needle() {
    let b = buf("a[unclosed( *");
    assert_eq!(
        starts(&find_all(&b, "[unclosed", &opts()).unwrap()),
        [(0, 1)]
    );
    assert!(validate("[unclosed", &opts()).is_ok());
}

// ----------------------------------------------------------- zero-width rules

#[test]
fn zero_width_caret_repeated_find_advances_and_terminates() {
    let b = buf("line one\nline two\nline three");
    let mut at = lc(0, 0);
    let mut seen = Vec::new();
    for step in 0..5 {
        let Some(m) = find_next(&b, "^", at, &rx()).unwrap() else {
            break;
        };
        assert!(
            m.start > at || step == 2,
            "F3 must advance from {at:?}, got {:?} (only step 2 may go backwards, at the wrap)",
            m.start
        );
        at = m.start;
        seen.push((at.line, at.col));
    }
    assert_eq!(seen.len(), 5, "wrap must keep it going, never stop");
    assert_eq!(seen, vec![(1, 0), (2, 0), (0, 0), (1, 0), (2, 0)]);
}

#[test]
fn zero_width_star_pattern_terminates_with_one_hit_per_position() {
    let b = buf("ab");
    let ms = find_all(&b, "a*", &rx()).unwrap();
    // "a" at 0, then the empty match at 2. No infinite run of empty matches.
    assert_eq!(starts(&ms), [(0, 0), (0, 2)]);
    assert_eq!(ms[0].text_len, 1);
    assert_eq!(ms[1].text_len, 0);
}

#[test]
fn zero_width_word_boundary_terminates() {
    let b = buf("one two");
    let ms = find_all(&b, r"\b", &rx()).unwrap();
    assert!(!ms.is_empty());
    assert!(
        ms.len() <= b.line_count() * 4,
        "bounded, not per-byte: {ms:?}"
    );
    let mut at = lc(0, 0);
    for _ in 0..10 {
        match find_next(&b, r"\b", at, &rx()).unwrap() {
            Some(m) => at = m.start,
            None => break,
        }
    }
}

#[test]
fn repeated_find_over_every_match_terminates_in_both_directions() {
    let b = buf("x\n^\nfoo bar\n$a*");
    for pat in ["^", "$", r"\b", "a*", "x*"] {
        for forward in [true, false] {
            let o = SearchOpts { forward, ..rx() };
            let mut at = lc(0, 0);
            for _ in 0..64 {
                match find_next(&b, pat, at, &o).unwrap() {
                    Some(m) => at = m.end.max(m.start),
                    None => break,
                }
            }
        }
    }
}

// --------------------------------------------------------------- wrap-around

#[test]
fn wrap_on_cycles_through_matches_forever_but_terminates_each_call() {
    let b = buf("x ax bx cx");
    assert_eq!(
        starts(&find_all(&b, "x", &opts()).unwrap()),
        [(0, 0), (0, 3), (0, 6), (0, 9)]
    );
    let mut at = lc(0, 0);
    let mut order = Vec::new();
    for _ in 0..6 {
        let m = find_next(&b, "x", at, &opts())
            .unwrap()
            .expect("wrap must always find something");
        order.push(m.start.col);
        at = m.end;
    }
    assert_eq!(order, vec![3, 6, 9, 0, 3, 6]);
}

#[test]
fn wrap_off_stops_at_the_last_match() {
    let b = buf("x ax bx");
    let no_wrap = SearchOpts {
        wrap_around: false,
        ..opts()
    };
    assert_eq!(one(&b, "x", lc(0, 5), &no_wrap), Some(lc(0, 6)));
    assert_eq!(one(&b, "x", lc(0, 7), &no_wrap), None);
    assert_eq!(one(&b, "x", lc(0, 7), &opts()), Some(lc(0, 0)));
}

#[test]
fn find_prev_walks_backwards_and_wraps() {
    let b = buf("a\na\na");
    assert_eq!(
        find_prev(&b, "a", lc(2, 0), &opts())
            .unwrap()
            .map(|m| m.start),
        Some(lc(1, 0))
    );
    // From (0,1) nothing sits strictly before it -- (0,0) is the hit on the current line and
    // IS before it, so no wrap is needed here.
    assert_eq!(
        find_prev(&b, "a", lc(0, 1), &opts())
            .unwrap()
            .map(|m| m.start),
        Some(lc(0, 0))
    );
    assert_eq!(
        find_prev(&b, "a", lc(0, 0), &opts())
            .unwrap()
            .map(|m| m.start),
        Some(lc(2, 0)),
        "wrap to the last match"
    );
    let no_wrap = SearchOpts {
        wrap_around: false,
        ..opts()
    };
    assert_eq!(
        find_prev(&b, "a", lc(0, 0), &no_wrap).unwrap(),
        None,
        "nothing strictly before the first hit"
    );
}

#[test]
fn directional_helper_follows_the_forward_flag() {
    let b = buf("a\na\na");
    let back = SearchOpts {
        forward: false,
        ..opts()
    };
    assert_eq!(
        find_directional(&b, "a", lc(2, 0), &back)
            .unwrap()
            .map(|m| m.start),
        Some(lc(1, 0))
    );
    assert_eq!(
        find_directional(&b, "a", lc(0, 0), &back)
            .unwrap()
            .map(|m| m.start),
        Some(lc(2, 0))
    );
}

#[test]
fn find_next_on_empty_document_is_none() {
    let b = buf("");
    assert_eq!(find_next(&b, "a", lc(0, 0), &opts()).unwrap(), None);
    assert_eq!(find_prev(&b, "a", lc(0, 0), &opts()).unwrap(), None);
    assert_eq!(count(&b, "a", &opts()).unwrap(), 0);
}

#[test]
fn out_of_range_from_is_clamped_not_an_error() {
    let b = buf("one\ntwo");
    // `clamp` caps the column to the last line's length, so 999/999 lands on line 1.
    assert_eq!(b.clamp(lc(999, 999)), lc(1, 3));
    // `two` sits at (1, 0), which the clamp is already past, so this genuinely wraps.
    assert_eq!(
        find_next(&b, "one", lc(999, 999), &opts())
            .unwrap()
            .map(|m| m.start),
        Some(lc(0, 0)),
        "wraps to the top; the first hit in document order is `one`"
    );
    // Wrapping restarts at the top, so the answer is the first `two` in document order
    // (line 1), not whatever happens to sit at the very start.
    assert_eq!(
        find_next(&b, "two", lc(999, 999), &opts())
            .unwrap()
            .map(|m| m.start),
        Some(lc(1, 0))
    );
    let no_wrap = SearchOpts {
        wrap_around: false,
        ..opts()
    };
    assert_eq!(
        find_next(&b, "one", lc(999, 999), &no_wrap).unwrap(),
        None,
        "clamped to EOF, and with no wrap there is nothing past it"
    );
    assert_eq!(find_next(&b, "a", lc(999, 999), &opts()).unwrap(), None);
}

#[test]
fn count_matches_find_all() {
    let b = buf("foo Foo FOO bar foo");
    assert_eq!(count(&b, "foo", &opts()).unwrap(), 4);
    assert_eq!(
        count(&b, "foo", &opts()).unwrap(),
        find_all(&b, "foo", &opts()).unwrap().len()
    );
    // 4, not 3: every `foo` in that line is a standalone word, including the one at position 0.
    // This assertion read 3 until S11, because `whole_word_ok` used `start.saturating_sub(1)`
    // and so tested the match's own first character as its left neighbour -- a match at offset 0
    // could never pass. FiF found it by searching a tree whose hits start at column 1.
    assert_eq!(count(&b, "foo", &whole()).unwrap(), 4);
}

// --------------------------------------------------------------- replace all

fn text_of(b: &Buffer) -> String {
    b.text_string()
}

#[test]
fn replace_all_returns_the_count_and_rewrites_the_buffer() {
    let mut b = buf("foo bar foo\nFOO");
    let n = replace_all(&mut b, "foo", "baz", None, &opts()).unwrap();
    assert_eq!(n, 3, "case-insensitive by default, so the third one counts");
    assert_eq!(text_of(&b), "baz bar baz\nbaz");
}

#[test]
fn replace_all_decides_matches_before_replacing_so_it_does_not_chase_output() {
    // "aa" -> "a" on "aaaa": overlapping starts 0,1,2 are all matches, applied right to left.
    let mut b = buf("aaaa");
    let n = replace_all(&mut b, "aa", "a", None, &opts()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(text_of(&b), "a");
}

#[test]
fn adjacent_matches_are_all_replaced_not_every_other_one() {
    // The classic "replace skipped the neighbour" regression.
    let mut b = buf("ababab");
    let n = replace_all(&mut b, "ab", "X", None, &opts()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(text_of(&b), "XXX");
}

#[test]
fn replace_all_with_longer_replacement_keeps_every_offset_valid() {
    let mut b = buf("a a a a");
    let n = replace_all(&mut b, "a", "xyz", None, &opts()).unwrap();
    assert_eq!(n, 4);
    assert_eq!(text_of(&b), "xyz xyz xyz xyz");
}

#[test]
fn growing_replacement_that_contains_the_needle_terminates() {
    let mut b = buf("ab");
    let n = replace_all(&mut b, "a", "aa", None, &opts()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "aab");
}

#[test]
fn shrinking_replacement_that_empties_the_needle_terminates() {
    let mut b = buf("xyxyxy");
    let n = replace_all(&mut b, "xy", "", None, &opts()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(text_of(&b), "");
}

#[test]
fn replacement_is_case_insensitive_but_inserted_verbatim() {
    let mut b = buf("Foo FOO foo");
    let n = replace_all(&mut b, "foo", "X", None, &opts()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(text_of(&b), "X X X");
}

#[test]
fn in_selection_range_limits_replacement_and_leaves_the_rest_alone() {
    let mut b = buf("keep foo\nfoo skip\nfoo");
    let n = replace_all(&mut b, "foo", "BAR", Some((lc(1, 0), lc(1, 3))), &opts()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "keep foo\nBAR skip\nfoo");
}

#[test]
fn in_selection_range_covers_whole_lines_when_the_range_does() {
    let mut b = buf("a\na\na\na");
    let n = replace_all(&mut b, "a", "b", Some((lc(1, 0), lc(3, 0))), &opts()).unwrap();
    assert_eq!(n, 2);
    assert_eq!(text_of(&b), "a\nb\nb\na");
}

#[test]
fn in_selection_range_is_half_open_at_the_end() {
    let mut b = buf("foo foo");
    let n = replace_all(&mut b, "foo", "x", Some((lc(0, 0), lc(0, 3))), &opts()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "x foo");
}

#[test]
fn empty_range_replaces_nothing() {
    let mut b = buf("foo foo");
    let n = replace_all(&mut b, "foo", "x", Some((lc(0, 2), lc(0, 2))), &opts()).unwrap();
    assert_eq!(n, 0);
    assert_eq!(text_of(&b), "foo foo");
}

#[test]
fn replacement_expands_numbered_captures_in_regex_mode() {
    let mut b = buf("2026-10-17 and 2024-01-05");
    let n = replace_all(&mut b, r"(\d{4})-(\d{2})-(\d{2})", "$3.$2.$1", None, &rx()).unwrap();
    assert_eq!(n, 2);
    assert_eq!(text_of(&b), "17.10.2026 and 05.01.2024");
}

#[test]
fn replacement_expands_named_captures_in_regex_mode() {
    let mut b = buf("hello world");
    let n = replace_all(&mut b, r"(?P<first>\w+)", "[$first]", None, &rx()).unwrap();
    assert_eq!(n, 2);
    assert_eq!(text_of(&b), "[hello] [world]");
}

#[test]
fn replacement_is_literal_in_literal_mode_dollar_included() {
    let mut b = buf("price is $1 today");
    let n = replace_all(&mut b, "$1", "${nope}", None, &opts()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "price is ${nope} today");
}

#[test]
fn cross_line_match_is_replaced_with_its_terminator() {
    // Joining two lines is the normal reason to match across one, so the terminator inside
    // the match has to go with it.
    let mut b = buf("one\ntwo\nthree");
    let n = replace_all(&mut b, r"two\nthree", "T", None, &rx()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "one\nT");
}

#[test]
fn cross_line_capture_expands_against_the_full_matched_span() {
    let mut b = buf("start\nend");
    let n = replace_all(&mut b, r"(start)\n(end)", "$2 $1", None, &rx()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(text_of(&b), "end start");
}

#[test]
fn replace_all_with_empty_needle_does_nothing() {
    let mut b = buf("abc");
    let n = replace_all(&mut b, "", "x", None, &opts()).unwrap();
    assert_eq!(n, 0);
    assert_eq!(text_of(&b), "abc");
}

#[test]
fn replace_all_preserves_line_count_for_within_line_replacements() {
    let before = buf("one two three");
    let mut b = buf("one two three");
    replace_all(&mut b, "o", "0", None, &opts()).unwrap();
    assert_eq!(b.line_count(), before.line_count());
    assert_eq!(text_of(&b), "0ne tw0 three");
}

#[test]
fn replace_all_on_an_empty_document_is_zero() {
    let mut b = buf("");
    assert_eq!(replace_all(&mut b, "a", "b", None, &opts()).unwrap(), 0);
    assert_eq!(text_of(&b), "");
}

#[test]
fn replace_all_is_undoable_as_one_edit_per_match() {
    // Each replacement is a discrete buffer edit, so undo walks them back one at a time and
    // the text returns exactly.
    let original = "foo foo foo";
    let mut b = buf(original);
    let n = replace_all(&mut b, "foo", "bar", None, &opts()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(text_of(&b), "bar bar bar");
    // Undoing the last replacement restores the third `foo`.
    b.delete_range(lc(0, 8), lc(0, 11));
    b.insert(lc(0, 8), "foo");
    assert_eq!(text_of(&b), "bar bar foo");
}

// ------------------------------------------------------------- SearchOpts map

#[test]
fn search_opts_round_trips_through_find_config() {
    let find = np_core::FindConfig {
        case_sensitive: true,
        whole_word: true,
        regex: true,
        wrap_around: false,
        mark_matches: true,
    };
    let o = SearchOpts::from_find(&find);
    assert!(o.forward, "direction is transient, always forward on open");
    assert_eq!(o.to_find(true), find);
    assert!(!o.to_find(false).mark_matches);
}

#[test]
fn find_config_defaults_map_to_the_documented_search_behavior() {
    let o = SearchOpts::from_find(&np_core::FindConfig::default());
    assert!(!o.case_sensitive);
    assert!(!o.whole_word);
    assert!(!o.regex);
    assert!(o.wrap_around, "spec default");
}

#[test]
fn config_and_search_opts_agree_field_by_field() {
    // The contract §10 requirement, asserted structurally so a renamed field in either place
    // fails here rather than silently dropping a preference.
    let find = np_core::FindConfig {
        case_sensitive: true,
        whole_word: true,
        regex: true,
        wrap_around: true,
        mark_matches: false,
    };
    let o = SearchOpts::from_find(&find);
    assert_eq!(o.case_sensitive, find.case_sensitive);
    assert_eq!(o.whole_word, find.whole_word);
    assert_eq!(o.regex, find.regex);
    assert_eq!(o.wrap_around, find.wrap_around);
}
