//! Fold detection (spec FR-V5).
//!
//! Two strategies, and both run: **indentation** for every language and **brace pairs** for
//! C-like ones. Indentation alone cannot fold a Python `def` body and a brace block in the
//! same file, and braces alone cannot fold Python at all, so the result is the union with
//! duplicates and strictly-nested duplicates removed.
//!
//! # What a fold range means
//!
//! `FoldRange { start, end }` is inclusive on both ends, and `start` is the **header** -- the
//! line that stays on screen. A range therefore has to be at least two lines long, since a
//! one-line range would hide nothing and still draw a fold marker, which reads as a control
//! that does nothing.
//!
//! # Unbalanced braces
//!
//! This is the case that separates a usable folder from a hostile one. A stray `{` with no
//! `}` must not fold the rest of the file, and a stray `}` must not fold backwards. Both are
//! handled by discarding any opener still open when the walk ends, and by ignoring a closer
//! with nothing on the stack. Braces inside strings and comments are handled by a small
//! state scanner rather than a count, because `"{"` in a string is the classic way a naive
//! brace counter produces a fold that swallows half a file.
//!
//! # Purity
//!
//! [`compute_folds`] is a pure function of text plus a language name, and [`merge_folds`] is
//! a pure function of two range lists. That is what lets a caller recompute on every edit and
//! keep the user's open/closed choices without any notification protocol.

use crate::view::FoldRange;

/// Languages whose blocks are delimited by braces.
///
/// Matched against the resolved syntect syntax name, case-insensitively. Deliberately a
/// list rather than "any language with braces in it": the scanner is cheap but not free, and
/// naming the family keeps the behavior explainable.
const BRACE_LANGUAGES: &[&str] = &[
    "c",
    "c++",
    "cpp",
    "c#",
    "csharp",
    "cuda",
    "d",
    "dart",
    "go",
    "groovy",
    "haskell",
    "java",
    "javascript",
    "json",
    "jsx",
    "kotlin",
    "less",
    "objc",
    "objective-c",
    "php",
    "rust",
    "scala",
    "scss",
    "solidity",
    "swift",
    "typescript",
    "tsx",
    "vala",
    "zig",
];

/// Whether `language` uses brace-delimited blocks.
pub fn is_brace_language(language: &str) -> bool {
    let name = language.trim().to_ascii_lowercase();
    // A syntect name can carry a modifier ("Rust", "C++"), so compare both the whole name
    // and its first word.
    if BRACE_LANGUAGES.contains(&name.as_str()) {
        return true;
    }
    let head = name.split([' ', '/', '(']).next().unwrap_or("");
    BRACE_LANGUAGES.contains(&head)
}

/// Every foldable range in `text`, in document order, outermost first.
///
/// `language` is the resolved syntax name (`""` for plain text). Ranges come back with
/// `collapsed: false`; the caller's [`merge_folds`] decides what stays closed.
///
/// Indentation folding applies to every language, including brace ones, so a plain-text
/// outline made of indentation still folds.
pub fn compute_folds(text: &str, language: &str) -> Vec<FoldRange> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut ranges = indent_folds(&lines);
    if is_brace_language(language) {
        ranges.extend(brace_folds(&lines));
    }
    normalize_ranges(ranges, &lines)
}

/// Fold ranges from indentation: a line folds everything indented deeper under it.
///
/// The rule is the one editors actually use, and the details are what make it not drive
/// people mad:
///  - a blank line never *opens* a fold -- it has no indentation of its own to be measured;
///  - a fold runs to the **last** line that is still deeper than the header, so the blank
///    lines *between* two sibling blocks stay visible rather than being dragged into the
///    first block;
///  - a header must actually have a deeper line below it, or there is nothing to hide.
fn indent_folds(lines: &[&str]) -> Vec<FoldRange> {
    let mut out = Vec::new();
    let count = lines.len();
    for i in 0..count {
        let header = match indent_of(lines[i]) {
            Some(level) => level,
            // A blank line carries no indentation of its own, so it cannot be a header.
            None => continue,
        };
        // Find where the block ends by walking down while the structure says "still inside".
        //
        // Blank lines are the whole difficulty, and the rule that resolves them is *prospective*
        // rather than positional: a blank line continues the block only when the next non-blank
        // line is deeper than the header. That single test gets all three cases right.
        //
        //  - Blank inside a body (`def f():` / body / blank / deeper body): the next real line is
        //    indented, so the blank is interior and the fold covers it.
        //  - Blank separator between two sibling functions: the next real line is at the same
        //    indent as the header or shallower, so the blank is *not* interior and the fold stops
        //    before it. Ending on a blank is what drags the empty line between two functions into
        //    the fold above, so collapsing one visibly eats its neighbour's whitespace.
        //  - Trailing blanks at end of file: no deeper line follows, so they are never eaten.
        //
        // The blank's own indent is deliberately not consulted -- a blank line's indent is an
        // accident of the editor, not structure.
        let mut last_deeper = None;
        let mut j = i + 1;
        while j < count {
            match indent_of(lines[j]) {
                Some(other) if other > header => {
                    last_deeper = Some(j);
                    j += 1;
                }
                Some(_) => break,
                None => match next_content_indent(lines, j) {
                    Some(deeper) if deeper > header => {
                        last_deeper = Some(j);
                        j += 1;
                    }
                    _ => break,
                },
            }
        }
        if let Some(end) = last_deeper {
            if end > i {
                out.push(FoldRange {
                    start: i,
                    end,
                    collapsed: false,
                });
            }
        }
    }
    out
}

/// The indent of the first non-blank line at or after `from`, or `None` if there is none.
///
/// The blank-line rule in [`indent_folds`] needs "what does the structure do next", and this is
/// that question. It is O(lines) in the worst case and called once per blank line per header,
/// which for real source is a handful of lines: consecutive blanks stop the scan immediately
/// because the second blank's "next content" is the same answer the first one already used to
/// continue.
fn next_content_indent(lines: &[&str], from: usize) -> Option<usize> {
    (from..lines.len()).find_map(|k| indent_of(lines[k]))
}

/// Indent width of `line` in cells, or `None` when the line is blank.
///
/// Tabs advance to their stop rather than counting as one, so a file indented with tabs and
/// one indented with spaces produce the same structure. A tab is treated as a single stop of
/// width 4 here because folding does not know the document's tab width, and any consistent
/// choice yields the same tree for a file that indents consistently.
fn indent_of(line: &str) -> Option<usize> {
    let mut cells = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => cells += 1,
            '\t' => cells += (cells / INDENT_TAB + 1) * INDENT_TAB - cells,
            '\r' => {}
            c if c.is_whitespace() => {}
            _ => return Some(cells),
        }
    }
    None
}

/// The tab width folding assumes when measuring indentation.
const INDENT_TAB: usize = 4;

/// Fold ranges from brace pairs, for C-like languages.
///
/// A range opens at the line containing a `{` and closes at the line containing the matching
/// `}`. The header is the opening line, so `fn f() {` stays visible and the body hides.
///
/// Braces inside strings, character literals and comments are skipped by a per-line state
/// scanner that carries across lines, so a block comment containing an unmatched brace does
/// not fold the file. An opener with no closer is dropped when the document ends, which is
/// the "unbalanced braces must not produce nonsense" requirement: the fallback is *no fold*,
/// never a fold to the end of the file.
fn brace_folds(lines: &[&str]) -> Vec<FoldRange> {
    let mut out = Vec::new();
    // One frame per open brace, innermost last. A frame remembers whether it is the line's
    // *first* brace, because only that one owns a fold header; the rest exist so that a closer
    // pops the brace it actually closes rather than an unrelated one.
    let mut stack: Vec<BraceFrame> = Vec::new();
    // One scanner for the whole file, not one per line: a `/* ... */` legitimately spans line
    // breaks, and restarting the state each line loses that. The scanner clears a `'` literal
    // at end of line on its own, so lifetimes and apostrophes still cannot swallow a file.
    let mut state = ScanState::default();

    for (index, line) in lines.iter().enumerate() {
        // Per-line counters, deliberately reset rather than carried: `openers.saturating_sub(
        // closers)` is zero for a balanced line, and the naive "carry the surplus" version
        // reports a *negative* surplus as a huge one, which then claims every brace below it is
        // already matched and silently drops the real folds of the rest of the file.
        let mut openers = 0usize;
        let mut closers = 0usize;
        scan_line(line, &mut state, &mut openers, &mut closers);

        for offset in 0..openers {
            stack.push(BraceFrame {
                line: index,
                is_header: offset == 0,
            });
        }
        // Closers beyond this line's openers are unmatched *at this line*, which only happens
        // in malformed input. Resolving them against the stack is correct and cheap: a closer
        // that empties the stack is simply ignored, since folding backwards to some unrelated
        // earlier brace is never the right answer.
        let mut pending = closers;
        while pending > 0 {
            match stack.pop() {
                Some(frame) if frame.is_header && index > frame.line => {
                    out.push(FoldRange {
                        start: frame.line,
                        end: index,
                        collapsed: false,
                    });
                }
                Some(_) => {}
                None => break,
            }
            pending -= 1;
        }
    }
    // Anything still open at the end is unbalanced: dropped, per the module docs. A stray `{`
    // must never fold the rest of the file.
    out
}

/// One open brace: the line it opened on, and whether it owns the line's fold marker.
#[derive(Debug, Clone, Copy)]
struct BraceFrame {
    line: usize,
    is_header: bool,
}

/// Where a line scanner is, so braces in prose and string literals are ignored.
///
/// Reset at the start of every line except for `block_comment`, which is the only state a
/// real C-like language carries across a line boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ScanState {
    /// Inside a `/* ... */` block comment.
    block_comment: bool,
    /// Inside a string, with the delimiter that opened it. `"` and `'` only; raw strings and
    /// verbatim escapes are beyond what a fold scanner should attempt, and the failure mode
    /// for mis-scanning one is a missing fold rather than a wrong one.
    string_delim: Option<char>,
}

/// Count the real braces in `line`, advancing `state` across it.
///
/// `//` line comments end brace scanning for the rest of the line. A `/*` opens a block that
/// may close on the same line or many lines later -- `state` is deliberately owned by the
/// caller and not reset per line, because a block comment that spans lines and resets is how a
/// commented-out `{` folds half a file. Escapes with `\\` end a string, which is what keeps
/// `"{"` and `'}'` out of the count.
fn scan_line(line: &str, state: &mut ScanState, openers: &mut usize, closers: &mut usize) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if state.block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                state.block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(delim) = state.string_delim {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == delim {
                state.string_delim = None;
            }
            i += 1;
            continue;
        }
        match c {
            '/' if chars.get(i + 1) == Some(&'/') => break,
            '/' if chars.get(i + 1) == Some(&'*') => {
                state.block_comment = true;
                i += 2;
                continue;
            }
            '"' | '\'' => {
                state.string_delim = Some(c);
            }
            '{' => *openers += 1,
            '}' => *closers += 1,
            _ => {}
        }
        i += 1;
    }
    // A single-quoted literal that never closed is almost certainly Rust or C++ lifetime
    // syntax or an apostrophe in a comment-adjacent string, not a string. Carrying it across
    // a line boundary would swallow real braces on every following line, so it is dropped at
    // end of line. Double quotes are kept: a multi-line string is real in several of the
    // languages on the list.
    if state.string_delim == Some('\'') {
        state.string_delim = None;
    }
}

/// Block-comment state at the start of every line, computed in one pass.
///
/// `states[k]` is true when line `k` begins inside an open `/* */`. Built once per document
/// rather than re-derived per query, because `normalize_ranges` asks about a large fraction of
/// the document's lines and the per-line answer depends on every line above it.
fn block_comment_states(lines: &[&str]) -> Vec<bool> {
    let mut states = Vec::with_capacity(lines.len());
    let mut in_block = false;
    for &line in lines {
        states.push(in_block);
        in_block = scan_block_state(line, in_block);
    }
    states
}

/// Advance the "inside a /* */" flag across one line.
///
/// Strings are skipped so a `/*` inside a string literal does not open a comment, and a quote
/// inside a comment does not open a string. The line-local `in_string` deliberately dies at the
/// line end, matching `scan_line`'s rule for `'`: an apostrophe must not comment out the rest
/// of the file.
fn scan_block_state(line: &str, mut in_block: bool) -> bool {
    let trimmed = line.trim_start();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut in_string: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        if let Some(delim) = in_string {
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if chars[i] == delim {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if in_block {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block = true;
            i += 2;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            in_string = Some(chars[i]);
        }
        i += 1;
    }
    in_block
}

/// Sort, drop degenerate ranges, and drop ranges a user could not operate.
///
/// Both strategies report the same block constantly -- a brace body is also an indented body --
/// so exact duplicates go first. The two rules that follow exist because the gutter has room
/// for exactly one marker per line, and a range that owns no marker is a range that hides lines
/// nobody can unhide.
///
/// # One range per header line
///
/// Two ranges with the same `start` cannot both be offered: the marker on that line toggles one
/// of them and the other sits there invisibly, hiding lines the user can neither see nor open.
/// A stray brace produces exactly this -- `fn a() {` with a `}` buried in a literal closes
/// early, and the real closer then looks like a second block -- so this is not tidiness, it is
/// what keeps a malformed file from swallowing half of itself. The **widest** range wins: it is
/// the one the scanner says reaches a real closer, and collapsing slightly too wide is one more
/// click to undo while collapsing too narrow is not.
///
/// # Drop a range fully hidden inside another collapsed range
///
/// An indented line inside a block comment is a perfectly ordinary indent fold, and a brace
/// block wraps the same lines: `fn a() {` / `    /* {` / `       }` reports a fold at line 1
/// whose entire body is comment. Nothing in the row layout knows the two came from different
/// detectors, so that inner fold is real, gets no marker of its own (the outer one owns those
/// lines' headers), and hides its lines behind a control that cannot reach them.
///
/// A strictly-nested range whose header line is *itself* inside another range is therefore
/// dropped. Nesting where the inner header is a real, visible line is kept -- that is what a
/// nested `if` inside a function needs, and it is the common case.
fn normalize_ranges(ranges: Vec<FoldRange>, lines: &[&str]) -> Vec<FoldRange> {
    let mut cleaned: Vec<FoldRange> = Vec::with_capacity(ranges.len());
    for mut range in ranges {
        range.collapsed = false;
        range.end = range.end.max(range.start);
        if range.end <= range.start {
            continue;
        }
        cleaned.push(range);
    }
    // Widest first within a header, so the survivor is the one the dedup below keeps.
    cleaned.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));

    // Two derived facts, each computed once for the whole document instead of once per
    // candidate range. Both were recomputed per range before S15, which is what made opening a
    // 10 MB file take 15 s.
    //
    //  * `started_headers` replaces a linear scan of the accepted list for "have I already
    //    emitted a fold at this header". The list is sorted by start, so the previous accepted
    //    start is the only candidate.
    //  * `comment_body` answers "is every line in this range's body blank or comment" from a
    //    prefix sum, rather than by re-deriving block-comment state from line 0 per line.
    let comment_only_line = comment_only_flags(lines);
    // Prefix count of body lines that are NOT blank-and-not-comment. A range's body is all
    // comment when that count is zero over the body span.
    let mut not_comment_prefix: Vec<usize> = Vec::with_capacity(lines.len() + 1);
    not_comment_prefix.push(0usize);
    for &is_comment in &comment_only_line {
        let blank = lines[not_comment_prefix.len() - 1].trim().is_empty();
        let counts_as_content = !(blank || is_comment);
        not_comment_prefix
            .push(not_comment_prefix[not_comment_prefix.len() - 1] + counts_as_content as usize);
    }

    let mut out: Vec<FoldRange> = Vec::with_capacity(cleaned.len());
    let mut last_accepted_start: Option<usize> = None;
    for range in cleaned {
        // Sorted by start, so a duplicate header is always adjacent to the one that won.
        if last_accepted_start == Some(range.start) {
            continue;
        }
        // A fold whose entire body is comment hides nothing a user can reach: the enclosing
        // block's header owns the gutter marker on these lines. See [`comment_only_flags`].
        let body_start = (range.start + 1).min(lines.len());
        let body_end = (range.end + 1).min(lines.len());
        let body_all_comment = body_start >= body_end
            || not_comment_prefix[body_end] == not_comment_prefix[body_start];
        if body_all_comment {
            continue;
        }
        last_accepted_start = Some(range.start);
        out.push(range);
    }
    out
}

/// Whether each line says nothing except comment, in document order.
///
/// One pass, carrying the block-comment state forward. The per-line rule is unchanged from
/// [`comment_line_is_non_code`]; the difference is that the block-comment state is
/// carried forward once instead of re-derived per line.
fn comment_only_flags(lines: &[&str]) -> Vec<bool> {
    let states = block_comment_states(lines);
    let mut out = Vec::with_capacity(lines.len());
    for (index, &line) in lines.iter().enumerate() {
        out.push(comment_line_is_non_code(line, states[index]));
    }
    out
}

/// Whether `line` is blank or comment, given whether it starts inside a block comment.
///
/// A line is non-code when it begins in an open
/// block comment, or when scanning it finds no code character.
fn comment_line_is_non_code(line: &str, starts_in_block: bool) -> bool {
    let trimmed = line.trim_start();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut in_block = starts_in_block;
    let mut in_string: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        if let Some(delim) = in_string {
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if chars[i] == delim {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if in_block {
            if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                in_block = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            return true;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            in_block = true;
            i += 2;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            in_string = Some(chars[i]);
            i += 1;
            continue;
        }
        // A code character: the line has content.
        return false;
    }
    // Reached the end without hitting code: comment, blank, or a line consumed by an open
    // block comment.
    true
}

/// Apply `computed` ranges while keeping the user's collapsed choices.
///
/// A range that still appears in `kept` with the same `(start, end)` inherits its
/// `collapsed` flag. A range whose span moved is *not* matched: guessing that a shifted
/// range is "the same fold" is how an editor ends up with a collapsed region that hides the
/// wrong lines, and an unexpectedly open fold is merely annoying.
///
/// Ranges in `kept` that no longer exist are dropped -- the block they described is gone.
///
/// # Collapsed state is inherited through containment
///
/// Exact-match alone loses the user's state on the edit that matters most: **typing inside a
/// collapsed block**. The block grows by a line, so `(start, end)` no longer matches, the fold
/// re-opens, and every keystroke inside a folded function pops it open. So a computed range
/// also inherits `collapsed` when a kept range shares its header and the two ranges still
/// *overlap* -- which is the signature of the same block getting longer or shorter, not of a
/// different block.
///
/// Containment alone would be too loose, and the case that proves it is a brace block closing
/// early. `fn a() {` ... `}` with a stray `}` in the middle yields computed ranges `(0,2)` and
/// `(0,4)`; a user who folded only the inner `(0,2)` must not have that state carried onto
/// `(0,4)`, or half the file hides behind a fold they never asked for. Requiring overlap fixes
/// that: `(0,4)` shares header 0 with the kept `(0,2)` but starts past its end, so nothing is
/// inherited. A fold whose header is gone is gone too -- no header, no fold.
pub fn merge_folds(computed: &[FoldRange], kept: &[FoldRange]) -> Vec<FoldRange> {
    computed
        .iter()
        .map(|range| FoldRange {
            start: range.start,
            end: range.end,
            collapsed: kept.iter().any(|k| {
                k.collapsed
                    && k.start == range.start
                    && range.start <= k.end
                    && range.start <= range.end
                    && !(range.start > k.end || k.start > range.end)
            }),
        })
        .collect()
}

/// Like [`merge_folds`], but a collapsed range in `kept` survives even when detection no
/// longer proposes it -- as long as the span still exists in the document.
///
/// This is the session-restore rule, and it exists because the two sources of fold state are
/// not equally authoritative. A computed range is a *suggestion*: detection is heuristic, and
/// a heuristic that misses a block merely fails to offer a marker. A collapsed range in a
/// session is a **user action** that was recorded, and dropping it because the current
/// heuristics would not have proposed that exact span silently loses state the user set. So:
///
///  - every computed range is offered, inheriting `collapsed` per [`merge_folds`];
///  - every kept *collapsed* range whose span still fits in the document is preserved, even if
///    nothing computed matches it, and even if the text changed underneath it -- the user
///    closed it, so it stays closed until they open it or the lines run out;
///  - a kept *expanded* range that detection no longer proposes is dropped. An open fold is
///    visually identical to no fold, so keeping it only adds a marker the text no longer
///    justifies.
///
/// The result is sorted and de-duplicated on `(start, end)`, and a computed range that shares a
/// span with a preserved collapsed one yields to it, since the preserved one carries the flag.
pub fn merge_folds_preserving(
    computed: &[FoldRange],
    kept: &[FoldRange],
    line_count: usize,
) -> Vec<FoldRange> {
    let mut out = merge_folds(computed, kept);
    for keep in kept.iter().filter(|k| k.collapsed) {
        if keep.end >= line_count || keep.end <= keep.start {
            // The lines it described are gone. `Document::clamp_folds` would drop it anyway;
            // dropping it here keeps the de-duplication below from having to reason about it.
            continue;
        }
        match out
            .iter_mut()
            .find(|f| f.start == keep.start && f.end == keep.end)
        {
            Some(existing) => existing.collapsed = true,
            None => out.push(*keep),
        }
    }
    out.sort_by_key(|f| (f.start, f.end));
    out
}

/// Set every range's `collapsed` flag. Used by Fold All and Unfold All (FR-V5).
pub fn set_all_collapsed(folds: &mut [FoldRange], collapsed: bool) {
    for fold in folds {
        fold.collapsed = collapsed;
    }
}

/// Toggle the fold whose **header** is `line`, returning true when something changed.
///
/// Only a range that starts at `line` is touched, which is what makes the gutter marker
/// unambiguous: the glyph on a line controls the block that begins there, never one that
/// merely contains it. When several ranges share a header (a brace block whose body is also
/// an indent block ending one line earlier), the outermost one wins and everything sharing
/// that header moves with it, so a single click never leaves a half-collapsed block.
pub fn toggle_fold_at(folds: &mut [FoldRange], line: usize) -> bool {
    let headers: Vec<FoldRange> = folds.iter().filter(|f| f.start == line).copied().collect();
    if headers.is_empty() {
        return false;
    }
    // The state of the widest range is the one the user sees, so it decides the direction.
    let widest = headers.iter().max_by_key(|f| f.end).copied();
    let target = match widest {
        Some(f) => !f.collapsed,
        None => return false,
    };
    let mut changed = false;
    for fold in folds.iter_mut() {
        if fold.start == line {
            fold.collapsed = target;
            changed = true;
        }
    }
    changed
}

/// Unfold every collapsed range containing `line`, including `line` itself.
///
/// Returns true when anything opened. Used when the caret or an edit lands inside a collapsed
/// range (FR-V5: "editing inside a folded range unfolds it"), and when navigating to a
/// bookmark that is currently hidden.
///
/// `line == range.start` counts as inside. A header is visible, but a user acting on a header
/// is about to see the block's contents move, and opening it is the predictable response; a
/// bookmark on a header line should never appear to do nothing.
pub fn unfold_containing(folds: &mut [FoldRange], line: usize) -> bool {
    let mut changed = false;
    for fold in folds.iter_mut() {
        if fold.collapsed && line >= fold.start && line <= fold.end {
            fold.collapsed = false;
            changed = true;
        }
    }
    changed
}

/// Shift fold ranges for an insertion of `added` lines at `at_line`.
///
/// Ranges entirely below the insertion move down. Ranges spanning it grow. A range that
/// started exactly at `at_line` keeps its header, which is right for "type at the start of a
/// function body": the header line is unchanged and the block got longer.
pub fn shift_folds_insert(folds: &mut [FoldRange], at_line: usize, added: usize) {
    if added == 0 {
        return;
    }
    for fold in folds.iter_mut() {
        if fold.start > at_line {
            fold.start += added;
            fold.end += added;
        } else if fold.end >= at_line {
            fold.end += added;
        }
    }
}

/// Shift fold ranges for the removal of lines `[first, last]` inclusive.
///
/// A range entirely inside the removed block is dropped. A range that partially overlaps
/// shrinks. Anything below moves up. Ranges left shorter than two lines are dropped: a fold
/// that hides nothing must not keep a marker.
pub fn shift_folds_remove(folds: &mut Vec<FoldRange>, first: usize, last: usize) {
    if last < first {
        return;
    }
    let span = last - first + 1;
    for fold in folds.iter_mut() {
        if fold.end < first {
            // Entirely above the hole: untouched.
            continue;
        }
        if fold.start > last {
            // Entirely below it: everything it covers moved up by the removed span.
            fold.start -= span;
            fold.end -= span;
            continue;
        }
        // Overlapping. `first` is where the hole starts, so the last surviving line above it is
        // `first - 1`; a clamped end below that leaves a range shorter than two lines, which the
        // retain below drops.
        if fold.end > last {
            fold.end -= span;
        } else {
            fold.end = first.saturating_sub(1);
        }
        if fold.start >= first {
            fold.start = first.saturating_sub(1);
        }
    }
    folds.retain(|f| f.end > f.start);
}
