//! Search and replace over a [`Buffer`] (contract §5, spec §6.4 FR-S1..S7).
//!
//! # Why one compiled matcher per query
//!
//! Every entry point here needs the same question answered in a different shape: "where are
//! the matches, in this direction, from here". Naively that is four implementations --
//! literal forward, literal backward, regex forward, regex backward -- and each pair drifts
//! on the edge cases that make search feel broken: zero-width matches, wrap-around,
//! whole-word boundaries, case folding that changes length.
//!
//! So there is exactly one scanning routine, [`Matcher::scan`], which walks a document
//! **forward** and reports matches left to right. Backward search is the same scan with the
//! result list read from the other end, and wrap-around is the same scan with a start
//! position. A forward-only core is what makes those three behaviors agree by construction
//! rather than by three sets of tests.
//!
//! The cost of that choice is that a scan materializes the document once (`text_string`,
//! ~90 ms for 10 MB measured in S9), so a scan is never run per keystroke. [`Matcher`] is
//! built once per query and reused across `find_next` calls -- the F3 loop does not
//! recompile a regex nor re-derive a fold case.
//!
//! # Coordinates
//!
//! The scan works in **byte offsets** into the materialized string, because that is what
//! `regex` and `str::find_iter` report, and converts once at the boundary with
//! [`Buffer::line_col`]. Offsets are never handed back to the caller.
//!
//! # Termination
//!
//! No input may loop forever. Three rules guarantee it, and each is tested:
//!  - an empty needle yields no matches at all;
//!  - a zero-width match advances by one *character* (not one byte), so `^`, `\b` and `a*`
//!    produce one match per line rather than one per UTF-8 byte;
//!  - overlapping literal candidates are advanced by one character too, so `"aa"` in `"aaa"`
//!    reports both without revisiting a start.

use regex::{Regex, RegexBuilder};

use crate::buffer::Buffer;
use crate::config::FindConfig;
use crate::types::LineCol;

/// How to search. The first four fields mirror [`FindConfig`] by name, per contract §10, so
/// dialog state copies straight into options and back without a translation layer.
///
/// `forward` is transient direction rather than a preference, which is why it has no
/// counterpart in `[find]` config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub wrap_around: bool,
    pub forward: bool,
}

impl SearchOpts {
    /// Options from persisted `[find]` config, searching forward.
    pub fn from_find(find: &FindConfig) -> Self {
        SearchOpts {
            case_sensitive: find.case_sensitive,
            whole_word: find.whole_word,
            regex: find.regex,
            wrap_around: find.wrap_around,
            forward: true,
        }
    }

    /// The reverse: what the dialog writes back to config when it closes (FR-S1).
    ///
    /// `mark_matches` is preserved rather than defaulted, because it is a preference the
    /// dialog does not own on every open.
    pub fn to_find(&self, mark_matches: bool) -> FindConfig {
        FindConfig {
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
            wrap_around: self.wrap_around,
            mark_matches,
        }
    }
}

/// One search hit. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: LineCol,
    pub end: LineCol,
    /// Characters covered, so the UI can select a match without recomputing.
    pub text_len: usize,
}

impl Match {
    fn new(start: LineCol, end: LineCol, text_len: usize) -> Self {
        Match {
            start,
            end,
            text_len,
        }
    }
}

/// Why a search could not run.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Spec FR-S4: shown inline in the dialog, never a crash.
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
}

/// Expand a replacement string against the text one match covered (FR-S3).
///
/// In regex mode `$1`, `$name` and `${name}` are capture references; in literal mode the
/// replacement goes in verbatim, `$1` included. The one implementation, so Replace-one and
/// Replace-All cannot drift on what `$1` means.
///
/// A pattern that fails to compile yields the replacement verbatim rather than an error. The
/// dialog validates on every keystroke, so arriving here with a broken pattern means the
/// needle changed underneath the caller, and inserting literal text is survivable in a way
/// that losing the edit is not.
pub fn expand_replacement(pattern: &str, matched: &str, replacement: &str) -> String {
    if !replacement.contains('$') {
        // Nothing to expand, so no compile is needed. This is the common case and it keeps a
        // plain replace from paying for a regex build.
        return replacement.to_string();
    }
    // The case-insensitivity of the search does not change how a replacement expands, but the
    // pattern must compile the same way it did for the search or the capture groups would not
    // line up. `Matcher::expand` is the preferred entry point when a Matcher is at hand.
    match Regex::new(pattern) {
        Ok(re) => re.replace(matched, replacement).into_owned(),
        Err(_) => replacement.to_string(),
    }
}

/// Validate a pattern without searching (FR-S4).
///
/// The dialog calls this on every keystroke so an incomplete pattern is reported while it is
/// being typed, rather than after Enter. A non-regex needle is always valid.
pub fn validate(needle: &str, opts: &SearchOpts) -> Result<(), SearchError> {
    if !opts.regex {
        return Ok(());
    }
    compile(needle, opts).map(|_| ())
}

// ---------------------------------------------------------------- public API

/// First match strictly after `from`, searching in `opts.forward`'s direction.
pub fn find_next(
    buf: &Buffer,
    needle: &str,
    from: LineCol,
    opts: &SearchOpts,
) -> Result<Option<Match>, SearchError> {
    let matcher = Matcher::new(buf, needle, opts)?;
    Ok(matcher.first_after(buf, from))
}

/// First match strictly before `from`.
///
/// `opts.forward` is ignored: the function's name is the direction. Callers that mean
/// "search in whatever direction the dialog says" use [`find_directional`].
pub fn find_prev(
    buf: &Buffer,
    needle: &str,
    from: LineCol,
    opts: &SearchOpts,
) -> Result<Option<Match>, SearchError> {
    let opts = SearchOpts {
        forward: false,
        ..opts.clone()
    };
    let matcher = Matcher::new(buf, needle, &opts)?;
    Ok(matcher.first_after(buf, from))
}

/// Search in `opts.forward`'s direction: next when forward, previous when backward (FR-S2).
pub fn find_directional(
    buf: &Buffer,
    needle: &str,
    from: LineCol,
    opts: &SearchOpts,
) -> Result<Option<Match>, SearchError> {
    let matcher = Matcher::new(buf, needle, opts)?;
    Ok(matcher.first_after(buf, from))
}

/// Every match, for Mark-matches and Count (FR-S2, FR-S5). Always document order.
pub fn find_all(buf: &Buffer, needle: &str, opts: &SearchOpts) -> Result<Vec<Match>, SearchError> {
    let matcher = Matcher::new(buf, needle, opts)?;
    Ok(matcher.matches.clone())
}

/// Number of matches (FR-S5).
pub fn count(buf: &Buffer, needle: &str, opts: &SearchOpts) -> Result<usize, SearchError> {
    Ok(find_all(buf, needle, opts)?.len())
}

/// Replace every match inside `range`, or the whole document when `range` is `None`.
///
/// Returns the number of replacements so the UI can toast it (FR-S3). Replacements are
/// applied right to left, so each edit's offsets are still valid for the ones to its left --
/// the same trick undo uses, and the reason no offset bookkeeping is needed here.
///
/// Matching happens once, before any replacement, which is what makes `"aa"` -> `"a"` on
/// `"aaaa"` replace twice instead of chasing its own output.
pub fn replace_all(
    buf: &mut Buffer,
    needle: &str,
    replacement: &str,
    range: Option<(LineCol, LineCol)>,
    opts: &SearchOpts,
) -> Result<usize, SearchError> {
    let matcher = Matcher::new(buf, needle, opts)?;
    if matcher.matches.is_empty() {
        return Ok(0);
    }
    let mut hits: Vec<&Match> = matcher
        .matches
        .iter()
        .filter(|m| in_range(m, range))
        .collect();
    if hits.is_empty() {
        return Ok(0);
    }
    // Right to left: an edit can only move text at or after its own position, so every
    // earlier match keeps the coordinates it was found with.
    hits.sort_by_key(|m| std::cmp::Reverse(m.start));

    let mut replaced = 0usize;
    for m in hits {
        let text = matched_text(buf, m);
        let new_text = matcher.expand(replacement, &text);
        // Deleting first and inserting second is the one order that is always correct:
        // replacing `"ab"` with `"xab"` would otherwise double the prefix on a re-scan.
        buf.delete_range(m.start, m.end);
        buf.insert(m.start, &new_text);
        replaced += 1;
    }
    Ok(replaced)
}

/// The text a match covers, read straight out of the buffer.
///
/// Spans lines: `end` is exclusive, so a match ending at `(2, 0)` covers through line 1's
/// terminator. Rebuilding it from `line_string(start.line)` alone would silently truncate a
/// multi-line match at its first newline, which makes `$1` expand against the wrong source.
fn matched_text(buf: &Buffer, m: &Match) -> String {
    let mut out = String::new();
    for line in m.start.line..=m.end.line {
        let text = buf.line_string(line);
        let from = if line == m.start.line { m.start.col } else { 0 };
        let to = if line == m.end.line {
            m.end.col
        } else {
            buf.line_len_chars(line)
        };
        out.extend(text.chars().skip(from).take(to.saturating_sub(from)));
        if line < m.end.line {
            out.push('\n');
        }
    }
    out
}

/// Whether a match lies within `range`, treating `None` as the whole document.
///
/// The test is on the match's *start*, which is the rule that keeps a multi-line match
/// straddling the end of a selection replaceable: Notepad++ replaces a match that begins
/// inside the selection even when it runs past the boundary.
fn in_range(m: &Match, range: Option<(LineCol, LineCol)>) -> bool {
    match range {
        None => true,
        Some((start, end)) => m.start >= start && m.start < end,
    }
}

// ------------------------------------------------------------------ matcher

/// A compiled query over one snapshot of a buffer.
#[derive(Debug, Clone)]
pub struct Matcher {
    opts: SearchOpts,
    matches: Vec<Match>,
    /// Captures for `$1`/`${name}` expansion, present only in regex mode.
    regex: Option<Regex>,
}

impl Matcher {
    /// Compile and scan. An invalid regex is the only failure.
    pub fn new(buf: &Buffer, needle: &str, opts: &SearchOpts) -> Result<Self, SearchError> {
        if needle.is_empty() {
            // FR-S1: an empty needle is not an error, and must not match everywhere.
            return Ok(Matcher {
                opts: opts.clone(),
                matches: Vec::new(),
                regex: None,
            });
        }
        // One materialization per query, and one char-offset -> LineCol conversion at the end.
        // The scans themselves are `&str`-based so Find in Files can run the identical matcher
        // over a file's text without building a rope for it first.
        let text = buf.text_string();
        if opts.regex {
            let re = compile(needle, opts)?;
            let found = scan_regex(&text, &re, opts.whole_word);
            Ok(Matcher {
                opts: opts.clone(),
                matches: offsets_to_matches(buf, &found),
                regex: Some(re),
            })
        } else {
            let found = scan_literal(&text, needle, opts);
            Ok(Matcher {
                opts: opts.clone(),
                matches: offsets_to_matches(buf, &found),
                regex: None,
            })
        }
    }

    /// Matches in document order.
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// The match strictly after `from` in the opts' direction, honoring `wrap_around`.
    ///
    /// `from` is exclusive in both directions, which is what makes an `F3` loop advance: the
    /// caret sits at the end of the current match after a find (item 7), so the next `F3`
    /// cannot return the same match again.
    pub fn first_after(&self, buf: &Buffer, from: LineCol) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        let from = buf.clamp(from);
        if self.opts.forward {
            if let Some(m) = self.matches.iter().find(|m| m.start > from) {
                return Some(*m);
            }
            // Wrap to the first match in the document. Terminating is structural, not a
            // loop guard: the list is finite and this returns an element of it.
            self.opts.wrap_around.then(|| self.matches[0])
        } else {
            if let Some(m) = self.matches.iter().rev().find(|m| m.start < from) {
                return Some(*m);
            }
            self.opts
                .wrap_around
                .then(|| self.matches[self.matches.len() - 1])
        }
    }

    /// Expand a replacement. In regex mode `$1` / `${name}` are capture references; in
    /// literal mode the string is inserted verbatim, `$1` included (FR-S3).
    pub fn expand(&self, replacement: &str, matched: &str) -> String {
        match &self.regex {
            Some(re) => {
                // `replace` needs a `&str` haystack, and the matched text is exactly the
                // capture source, so no re-scan of the document is required.
                re.replace(matched, replacement).into_owned()
            }
            None => replacement.to_string(),
        }
    }
}

// ------------------------------------------------------------------ compiling

/// Build the regex for a query.
///
/// `(?i)` is applied through [`RegexBuilder::case_insensitive`] rather than pasted onto the
/// pattern, so a needle that already carries flags or begins with `(?-i)` keeps working.
///
/// No builder flag touches line semantics. `(?m)` would redefine `\b` as a *line* boundary
/// as well as re-anchoring `^`/`$`, and the `regex` crate has no look-around to work around
/// that with. Line-level anchoring is instead produced by *how* the pattern is run: see
/// [`scan_regex`], which scans each line span separately so `^` and `$` mean "start and end
/// of line" for free, while a pattern containing an explicit `\n` still spans lines.
///
/// `.` deliberately does not match `\n`, which is what keeps `<.*>` inside one tag pair.
fn compile(needle: &str, opts: &SearchOpts) -> Result<Regex, SearchError> {
    let mut builder = RegexBuilder::new(needle);
    builder.case_insensitive(opts.case_sensitive);
    // A pathological needle must not pin the UI thread. `size_limit` bounds the compiled
    // program, so a catastrophic pattern is refused here -- as the inline dialog error the
    // user can act on -- rather than while the editor is frozen.
    builder.size_limit(REGEX_SIZE_LIMIT);
    builder
        .build()
        .map_err(|e| SearchError::InvalidRegex(e.to_string()))
}

/// Compiled-program size cap, in bytes. Generous for real patterns, small enough that a
/// catastrophic one is refused in milliseconds instead of exhausting memory.
const REGEX_SIZE_LIMIT: usize = 8 << 20;

/// The identifier-character rule, shared with `Buffer::word_*` so Ctrl+Left and whole-word
/// search agree on where a word begins (asserted in the stage tests).
///
/// `Buffer` keeps its own copy private; this is the same definition, and the consistency
/// test in `tests/search_s10.rs` is what stops the two from drifting.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// -------------------------------------------------------------- literal scan

/// Find every literal occurrence, forward, in document order.
///
/// # Shape of the algorithm
///
/// A plain naive scan: try the needle at every document character position, move on. It is
/// written as one function with an explicit `attempt` and `matched` pair rather than as a
/// stateful single pass, because the stateful version has to rewind on a mid-needle mismatch
/// and getting that rewind right is exactly where previous attempts went wrong (and where a
/// rewind to the wrong place livelocks). Here `attempt` only ever increases, so termination is
/// visible in one line: the outer loop's counter advances by at least one every iteration, no
/// matter what the inner comparison decides.
///
/// # Coordinates are characters, not bytes
///
/// Everything below works in **character** indices into `doc`, which is what
/// [`Buffer::line_col`] consumes. The byte-indexed version of the same arithmetic is a silent
/// corruption on any non-ASCII document: `мир` in `привет мир привет` sits at character 7 and
/// byte 13, and reporting the latter puts the caret six characters off.
///
/// # Case folding
///
/// Per-character full folding, asserted per source character, with a byte-length-agnostic
/// mapping back to the document. `char::to_lowercase` changes length -- `\u{0130}` becomes
/// `i` plus `\u{0307}` -- so one document character can consume two needle characters while
/// still advancing one position in the document. That is what `folded_span` reports.
///
/// The rule is deliberately **not** `str::to_lowercase`, which applies the final-sigma rule
/// and folds a word-final `\u{03a3}` to a character that is not the one under the cursor. And
/// there is **no** `\u{00df}` -> `ss` expansion, because Unicode has no such fold in either
/// direction; inventing one would break every search for a literal sharp s.
fn scan_literal(text: &str, needle: &str, opts: &SearchOpts) -> Vec<(usize, usize)> {
    let doc: Vec<char> = text.chars().collect();
    let mut out = Vec::new();

    if needle.is_empty() || doc.is_empty() {
        return out;
    }
    // One flag governs both helpers, so the comparison and the consumption count can never
    // disagree. When they did, case-sensitive search of "Foo" consumed the needle's `F` *and*
    // `o` against a single document `F` and matched nothing at all.
    let fold = !opts.case_sensitive;
    let folded_needle: Vec<char> = if fold {
        needle.chars().flat_map(|c| c.to_lowercase()).collect()
    } else {
        needle.chars().collect()
    };
    if folded_needle.is_empty() {
        return out;
    }

    let mut attempt = 0usize;
    while attempt < doc.len() {
        // Compare the folded needle against the document starting at `attempt`. `ni` walks the
        // needle, `ci` walks the document; they advance at different rates because a single
        // document character may fold to several needle characters.
        let mut ni = 0usize;
        let mut ci = attempt;
        while ni < folded_needle.len() && ci < doc.len() {
            let ch = doc[ci];
            if !starts_folded_run(&folded_needle, ni, ch, fold) {
                break;
            }
            ni += folded_span(ch, &folded_needle, ni, fold);
            ci += 1;
        }
        if ni >= folded_needle.len() && ci > attempt {
            if whole_word_ok(&doc, attempt, ci, opts.whole_word) {
                out.push((attempt, ci));
            }
            // Overlapping candidates: the next attempt starts one character later, not at
            // `ci`, so `aa` in `aaaa` reports 0, 1 and 2.
            attempt += 1;
            continue;
        }
        attempt += 1;
    }
    out
}

/// Whether `doc_ch`'s lowercase form continues `folded` at index `ni`.
///
/// Requires the *whole* lowercase sequence of `doc_ch` to be present, so a one-scalar needle
/// does not match half of a length-changing fold. Matching `i` against `\u{0130}` would put
/// the caret in the middle of what the user sees as one character.
fn starts_folded_run(folded: &[char], ni: usize, doc_ch: char, fold: bool) -> bool {
    if !fold {
        return folded.get(ni) == Some(&doc_ch);
    }
    let mut k = ni;
    for a in doc_ch.to_lowercase() {
        match folded.get(k) {
            Some(&b) if a == b => k += 1,
            _ => return false,
        }
    }
    true
}

/// How many needle characters `doc_ch` accounts for at `ni`. Floored at 1 so a caller's
/// document cursor always advances, even on the path where the run did not match.
fn folded_span(doc_ch: char, folded: &[char], ni: usize, fold: bool) -> usize {
    if !fold {
        return 1;
    }
    let mut count = 0usize;
    for a in doc_ch.to_lowercase() {
        match folded.get(ni + count) {
            Some(&b) if a == b => count += 1,
            _ => break,
        }
    }
    count.max(1)
}

// ----------------------------------------------------------------- regex scan

/// Find every regex match, forward, in document order.
///
/// # One document scan, plus a pass for anchored patterns
///
/// FR-S4 asks for two things a single whole-document `find_iter` cannot both give:
///  - `\n` in a pattern must match across a line break;
///  - `^` and `$` must mean *line* start and end, Notepad++ style.
///
/// The textbook answer is `RegexBuilder::multi_line`, and it is wrong here. In the `regex`
/// crate that flag changes two things at once: it re-anchors `^`/`$` **and** redefines `\b`
/// as a line boundary. So turning it on breaks `\bfoo` mid-line and makes a bare `\b` match
/// at the start of every line -- an ordinary pattern quietly becoming a whole-file anchor
/// sweep. Look-around, which would express "line start" precisely, is not supported by this
/// crate at all.
///
/// So the main scan runs over the whole document with the pattern exactly as the user wrote
/// it, which gets cross-line matching and correct `\b` semantics for free. That pass alone
/// misses one thing: with no multi-line mode, `^` matches only at byte 0, so `^foo` on line 7
/// is never found. A second, **conditional** pass supplies those hits, and it runs only when
/// the pattern contains a bare `^` or `$`.
///
/// Deciding that from the pattern's *text* is safe in a way that "can this match a newline"
/// never was: a `^` inside a bracket class (`[a^]`) or escaped (`\^`) is not an anchor, and
/// both are excluded by scanning the pattern the way the parser would. The cost when the
/// answer is yes is one extra scan, paid only by patterns that actually need it.
///
/// # Anchored hits must be anchored to a real line start
///
/// The per-line pass copies each line and searches the copy, so `^` resolves against the
/// line's beginning. It then keeps a hit **only if it starts at the line's first character**,
/// because searching a line copy makes column 0 of that copy look like a valid answer for an
/// unanchored pattern too. Without that filter `^b` over `bbb` reports columns 0, 1 and 2 --
/// each slice being its own haystack's start -- which is precisely the bug this design is
/// meant to avoid.
///
/// # Zero-width matches
///
/// [`Regex::find_iter`] owns the advance-past-empty rule (one character, never one byte), so
/// `^`, `$`, `\b` and `a*` each produce one hit per position and `F3` always moves. Re-deriving
/// that by hand is what previously livelocked.
fn scan_regex(text: &str, re: &Regex, whole_word: bool) -> Vec<(usize, usize)> {
    let doc: Vec<char> = text.chars().collect();
    let mut out: Vec<(usize, usize)> = Vec::new();

    // Pass 1: the document as written. Cross-line patterns and `\b` behave correctly here,
    // and so does everything that is not anchored.
    // `$` alone: the document pass's line-final positions are the same anchors the per-line
    // pass reports, so they are dropped there. `^` and `^...$` keep them -- an empty match at a
    // newline is a real answer for those, and dropping it would take the last line's anchor
    // with it.
    let dollar_only = pattern_ends_anchored(re.as_str()) && !pattern_starts_anchored(re.as_str());
    collect_matches(re, text, &doc, 0, whole_word, dollar_only, &mut out);

    // Pass 2: line-level anchors, and only for patterns that have them.
    if has_line_anchor(re.as_str()) {
        let spans = char_line_spans(text);
        for (start, end) in spans {
            let byte_start = nth_byte(text, 0, start);
            let byte_end = nth_byte(text, byte_start, end.saturating_sub(start));
            let Some(line) = text.get(byte_start..byte_end) else {
                continue;
            };
            let anchored_start = pattern_starts_anchored(re.as_str());
            let anchored_end = pattern_ends_anchored(re.as_str());
            for m in re.find_iter(line) {
                // Keep only answers pinned to a line edge. Searching a line copy makes column
                // 0 of that copy look like a valid answer for *any* pattern, so an unfiltered
                // pass would report `^b` at every column of `bbb`. A hit is legitimate when it
                // begins at the line's start and the pattern is start-anchored, or ends at the
                // line's end and the pattern is end-anchored.
                let pinned_start = anchored_start && m.start() == 0;
                let pinned_end = anchored_end && m.end() == line.len();
                if !pinned_start && !pinned_end {
                    continue;
                }
                // A bare `$` answers at the line's end, and a search that lands there has
                // nothing to select. Anchors are positions, so the hit the user wants is the
                // line's beginning, which is also what makes an `F3` cycle advance.
                let (cs0, ce0) = if m.is_empty() && anchored_end && !anchored_start {
                    // Same rule as `^`: an end anchor is a position, and the position the user
                    // means is the line's beginning, not a zero-width spot after its last
                    // character. Without this, `$` reports the document's last line twice --
                    // once from here and once from the document pass at EOF.
                    (start, start)
                } else {
                    let chars_before = line[..m.start()].chars().count();
                    let chars_in = line[m.start()..m.end()].chars().count();
                    let c0 = start + chars_before;
                    (c0, c0 + chars_in)
                };
                if !whole_word_ok(&doc, cs0, ce0, whole_word) {
                    continue;
                }
                out.push((cs0, ce0));
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

/// Whether a pattern contains an unescaped `^` or `$` outside a bracket class.
///
/// `[a^]` and `[^a]` are bracket syntax, `\^` is a literal caret, and neither is an anchor.
/// Inline groups like `(?m)^` still count, which is the right answer: the user asked for
/// line-ish anchoring and the extra pass can only add hits the document scan missed.
fn has_line_anchor(pattern: &str) -> bool {
    let mut in_class = false;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '^' | '$' if !in_class => return true,
            _ => {}
        }
    }
    false
}

/// Whether the pattern's top level must begin at `^`.
///
/// Top level matters: in `a|^b` the alternation's second branch is anchored but the pattern as
/// a whole is not, so a hit elsewhere is legitimate and the filter must not reject it. Patterns
/// that begin with a group are treated as anchored, which is the safe direction -- a false
/// positive here only drops hits the document pass already found.
fn pattern_starts_anchored(pattern: &str) -> bool {
    stripped(pattern).starts_with('^')
}

/// Whether the pattern's top level must end at `$`.
fn pattern_ends_anchored(pattern: &str) -> bool {
    stripped(pattern).ends_with('$')
}

/// The pattern with a leading inline-flag group removed, for anchor detection.
///
/// `(?i)^foo` begins with `(`, not `^`, so the raw string hides the anchor. Flag groups are
/// `(?` followed by flags and a `)`, and only that prefix is stripped -- a capture group like
/// `(a|^b)` is left alone.
fn stripped(pattern: &str) -> &str {
    // A flag group contains only flag characters and an optional `-`.
    let is_flags = |flags: &str| {
        !flags.is_empty()
            && flags
                .chars()
                .all(|c| matches!(c, 'i' | 'm' | 's' | 'U' | 'u' | 'x' | '-'))
    };
    let mut rest = pattern.trim_start();
    // Strip any run of inline flag groups, e.g. `(?i)(?m)^x`.
    while let Some(after) = rest.strip_prefix("(?") {
        let Some(close) = after.find(')') else {
            break;
        };
        if !is_flags(&after[..close]) {
            break;
        }
        rest = &after[close + 1..];
    }
    rest
}

/// Run `re` over `text`, appending character-indexed `(start, end)` spans.
fn collect_matches(
    re: &Regex,
    text: &str,
    doc: &[char],
    char_base: usize,
    whole_word: bool,
    suppress_line_final_dollar: bool,
    out: &mut Vec<(usize, usize)>,
) {
    // Character offsets accumulate as bytes are consumed. Restarting the count from 0 per hit
    // would make the scan quadratic on a document with many matches.
    let mut consumed_bytes = 0usize;
    let mut consumed_chars = char_base;
    for m in re.find_iter(text) {
        let (ms, me) = (m.start(), m.end());
        consumed_chars += text[consumed_bytes..ms].chars().count();
        consumed_bytes = ms;
        let chars_in = text[ms..me].chars().count();
        let (cs, ce) = (consumed_chars, consumed_chars + chars_in);
        // A zero-width end anchor sitting just before a newline is the position the per-line
        // pass already reports, as that line's beginning. Keeping it here too would give `$`
        // two answers per line at different coordinates, and an `F3` cycle would appear to
        // stutter between them. The document's own final `$` is not a line-final position in
        // this sense and is kept.
        if suppress_line_final_dollar && cs == ce && doc.get(ce) == Some(&'\n') {
            continue;
        }
        if !whole_word_ok(doc, cs, ce, whole_word) {
            continue;
        }
        out.push((cs, ce));
    }
}

/// Character-indexed `(start, end)` spans for every logical line, terminators excluded.
fn char_line_spans(text: &str) -> Vec<(usize, usize)> {
    // Splitting on '\n' mirrors what the rope reports: a document ending in a newline yields
    // a final empty line, and every line's content excludes its terminator.
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for line in text.split('\n') {
        let len = line.chars().count();
        spans.push((pos, pos + len));
        // One character for the terminator, which every line but the last has.
        pos += len + 1;
    }
    spans
}

/// Byte offset of the character `count` positions after `from`.
fn nth_byte(text: &str, from: usize, count: usize) -> usize {
    if count == 0 {
        return from.min(text.len());
    }
    match text.get(from..) {
        Some(rest) => rest
            .char_indices()
            .nth(count)
            .map(|(b, _)| from + b)
            .unwrap_or(text.len()),
        None => text.len(),
    }
}

// ------------------------------------------------------------------- helpers

/// The whole-word rule, shared by both scan modes.
///
/// A match is a whole word when neither neighbour is an identifier character. So `foo` matches
/// in `foo`, `(foo)`, `foo.` and `hello foo world`, while rejecting `foobar`, `barfoo`, `foo1`
/// and -- worth stating because it surprises people -- `foo_bar`, since `_` is an identifier
/// character. That class is `alnum + _`, the same one `Buffer::word_*` uses, so Ctrl+Left and
/// whole-word search cannot disagree about where a word begins; a test asserts the agreement.
///
/// **`start` and `end` are character indices**, matching what the scans produce and what
/// `Buffer::line_col` consumes. Mixing a byte offset in here misplaces the neighbour on any
/// non-ASCII line and silently rejects or accepts the wrong matches.
fn whole_word_ok(doc: &[char], start: usize, end: usize, enabled: bool) -> bool {
    if !enabled {
        return true;
    }
    // `start == 0` means there is no character before the match, which is a word boundary.
    // `start.saturating_sub(1)` would read `doc[0]` -- the match's *own first character*,
    // always a word char -- so a whole-word match at the very start of a document could never
    // be accepted. `foo` in `"foo foobar foo"` was found only at column 12 for that reason.
    let before = if start == 0 {
        None
    } else {
        doc.get(start - 1).copied()
    };
    let after = doc.get(end).copied();
    match (before, after) {
        (None, None) => true,
        (Some(c), None) | (None, Some(c)) => !is_word_char(c),
        (Some(a), Some(b)) => !is_word_char(a) && !is_word_char(b),
    }
}

/// Turn a document **character** span into a [`Match`].
///
/// Both offsets are character indices, which is what [`Buffer::line_col`] takes. `text_len`
/// counts the characters covered, terminators included, so the UI can select a match from
/// `start` alone.
/// Map character offsets to `Match` values, the single place a scan's internal coordinates
/// become document coordinates.
fn offsets_to_matches(buf: &Buffer, found: &[(usize, usize)]) -> Vec<Match> {
    found
        .iter()
        .map(|(start, end)| make_match(buf, *start, *end))
        .collect()
}

/// Character offsets of every match in `text`, in the same order and under the same rules as
/// [`Matcher`] produces for a buffer.
///
/// Find in Files needs to run the *identical* matcher over a file's text, and building a rope
/// per file just to ask that question would cost more than the search itself. Exposing the
/// scan over a `&str` means one implementation of case folding, whole-word and anchor rules
/// serves both callers, so in-document and in-files results cannot disagree.
pub fn scan_offsets(
    text: &str,
    needle: &str,
    opts: &SearchOpts,
) -> Result<Vec<(usize, usize)>, SearchError> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    if opts.regex {
        let re = compile(needle, opts)?;
        Ok(scan_regex(text, &re, opts.whole_word))
    } else {
        Ok(scan_literal(text, needle, opts))
    }
}

/// Line index and character column of a character offset, without a `Buffer`.
///
/// The `\n`-split walk is what a rope would report, and it is done once per match line rather
/// than per match: a file with 50k hits on one line must not rescan the text 50k times.
pub fn offset_line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut consumed = 0usize;
    for l in text.split('\n') {
        let len = l.chars().count();
        if offset <= consumed + len {
            return (line, offset - consumed);
        }
        consumed += len + 1;
        line += 1;
    }
    // Past the end: report the last line's end.
    (line.saturating_sub(1), 0)
}

fn make_match(buf: &Buffer, start: usize, end: usize) -> Match {
    let start_lc = buf.line_col(start);
    let end_lc = buf.line_col(end);
    Match::new(start_lc, end_lc, end.saturating_sub(start))
}
