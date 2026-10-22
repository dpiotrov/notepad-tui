//! Find/Replace state and the search commands (spec §6.4 FR-S1..S7).
//!
//! # Why the state is on `App` and not in a widget
//!
//! FR-S1 requires options to survive the dialog closing, and FR-S2 requires `F3` to work when
//! no dialog is in sight. So the needle, the replacement and the flags are application state,
//! and the dialog is a view over them: closing it hides a form, it does not discard a query.
//! That is also what lets S14's generic dialog framework be a rendering swap -- it draws this
//! struct and writes back into it, and owns nothing.
//!
//! # The engine is never run per keystroke
//!
//! Every search entry point materializes the document, and S9 measured a 10 MB rebuild at
//! ~90 ms. Typing in the dialog therefore only **validates** the pattern, which compiles a
//! regex and costs microseconds. The full scan runs on Enter, on `F3`, and on Count.
//! [`FindState::refresh_marks`] is the one call that can be per-edit, and it is gated on
//! "mark matches" being on, which is a user-visible opt-in to exactly that cost.
//!
//! # Errors keep the last good search alive
//!
//! An invalid pattern is reported inline while it is typed and the previous match list is left
//! in place. Clearing matches on each invalid keystroke would flicker the editor and throw
//! away the position the user is trying to navigate back to.
//!
//! # Why commands return an outcome instead of toasting
//!
//! [`FindOutcome`] lets the caller decide the words and the channel, and lets a test assert on
//! what happened without scraping a toast queue. `App::execute` is the only place that turns
//! an outcome into a toast.

use std::time::Instant;

use np_core::{
    expand_replacement, validate, Document, LineCol, Match, Matcher, SearchOpts, SelectionKind,
};

use crate::app::App;

/// Which dialog field would take input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindField {
    #[default]
    Find,
    Replace,
}

/// The result of a search command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindOutcome {
    /// Nothing to do: empty needle, or no document.
    Idle,
    /// The pattern is invalid; the payload is the inline error (FR-S4).
    Invalid(String),
    /// A valid search that found nothing.
    NotFound,
    /// Moved onto a match. `wrapped` when the search passed a document end to get there.
    Found { wrapped: bool },
    /// Replaced N occurrences (FR-S3's count toast).
    Replaced(usize),
    /// N occurrences (FR-S5).
    Counted(usize),
    /// No word under the caret, so there was nothing to search for (FR-S7).
    NoWord,
}

impl FindOutcome {
    /// The toast text, when the outcome warrants one.
    pub fn toast(&self) -> Option<String> {
        match self {
            FindOutcome::Idle | FindOutcome::Found { .. } => None,
            FindOutcome::Invalid(msg) => Some(msg.clone()),
            FindOutcome::NotFound => Some("Search string not found".to_string()),
            FindOutcome::Replaced(n) => Some(format!("Replaced {n} occurrence(s)")),
            FindOutcome::Counted(n) => Some(format!("{n} occurrence(s)")),
            FindOutcome::NoWord => Some("no word under the caret".to_string()),
        }
    }

    /// True when the outcome should be shown in the error color.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            FindOutcome::Invalid(_) | FindOutcome::NotFound | FindOutcome::NoWord
        )
    }
}

/// Everything the search commands and the dialog need, persisting across dialog closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindState {
    /// The needle, exactly as typed.
    pub needle: String,
    /// The replacement, exactly as typed.
    pub replacement: String,
    /// Search options. The four `[find]` fields plus the transient `forward`.
    pub opts: SearchOpts,
    /// "Mark matches", which lives in `[find]` config but is not part of `SearchOpts` because
    /// it changes rendering rather than matching.
    pub mark_matches: bool,
    /// "In selection" (FR-S1). Dialog state, not a preference: it is re-derived from the
    /// current selection each time the dialog opens.
    pub in_selection: bool,
    /// The last validation failure, shown inline. `None` means the needle is usable.
    pub error: Option<String>,
    /// The match the caret is on after a successful find.
    pub current: Option<Match>,
    /// Every match, when marking is on. Document order.
    pub marks: Vec<Match>,
    /// Which field the dialog would take input for.
    pub focus: FindField,
    /// Direction the next `F3` moves. The dialog's ↑/↓ sets this independently of the
    /// persisted flags, which do not store direction.
    pub forward: bool,
    /// Whether the dialog is showing the Replace field and buttons (Ctrl+H rather than Ctrl+F).
    pub replace_mode: bool,
}

impl Default for FindState {
    fn default() -> Self {
        FindState {
            needle: String::new(),
            replacement: String::new(),
            opts: SearchOpts::default(),
            mark_matches: true,
            in_selection: false,
            error: None,
            current: None,
            marks: Vec::new(),
            focus: FindField::default(),
            forward: true,
            replace_mode: false,
        }
    }
}

impl FindState {
    /// Seed from `[find]` config (FR-S1: options persist in config).
    pub fn from_config(config: &np_core::Config) -> Self {
        FindState {
            opts: SearchOpts::from_find(&config.find),
            mark_matches: config.find.mark_matches,
            forward: true,
            ..FindState::default()
        }
    }

    /// Write the persisted flags back into a config (FR-S1).
    ///
    /// `forward` and `in_selection` are left out on purpose: direction is a per-search choice,
    /// and "in selection" describes a selection that will not exist next time.
    pub fn persist(&self, config: &mut np_core::Config) {
        config.find = self.opts.to_find(self.mark_matches);
    }

    /// Validate the needle without searching (FR-S4), storing and returning the message.
    ///
    /// A non-regex needle is always valid, metacharacters included: in literal mode
    /// `[unclosed` is four characters of text, not a broken pattern.
    pub fn validate_now(&mut self) -> Option<String> {
        self.error = validate(&self.needle, &self.opts)
            .err()
            .map(|e| e.to_string());
        self.error.clone()
    }

    /// The options with the dialog's current direction applied.
    pub fn effective_opts(&self) -> SearchOpts {
        SearchOpts {
            forward: self.forward,
            ..self.opts.clone()
        }
    }

    /// Re-derive the mark list. Cheap when marking is off, which is the point.
    pub fn refresh_marks(&mut self, doc: &Document) {
        if !self.mark_matches || self.error.is_some() || self.needle.is_empty() {
            self.marks.clear();
            return;
        }
        self.marks = Matcher::new(&doc.buffer, &self.needle, &self.opts)
            .map(|m| m.matches().to_vec())
            .unwrap_or_default();
    }
}

/// The range "In selection" implies, or `None` for the whole document.
pub fn selection_range(app: &App) -> Option<(LineCol, LineCol)> {
    let sel = app.doc()?.selection?;
    if sel.is_empty() {
        return None;
    }
    Some(sel.bounds())
}

/// Open the Find dialog (FR-S1, Ctrl+F).
///
/// "In selection" auto-checks when the selection spans two or more lines, which is FR-S1's
/// stated rule and the reason the box exists: a person who selected a paragraph almost always
/// means "only in here".
pub fn open_find(app: &mut App, replace_mode: bool) {
    app.find.replace_mode = replace_mode;
    app.find.focus = FindField::Find;
    app.find.in_selection = app
        .doc()
        .and_then(|d| d.selection)
        .is_some_and(|s| !s.is_empty() && s.line_span() >= 2);
    app.find.validate_now();
    app.refresh_marks();
    app.push_mode(crate::app::Mode::Dialog(if replace_mode {
        crate::app::DialogKind::Replace
    } else {
        crate::app::DialogKind::Find
    }));
}

/// Close the dialog, persisting options (FR-S1) and leaving the caret in the editor.
pub fn close_find(app: &mut App) {
    app.find.persist(&mut app.config);
    while matches!(
        app.mode(),
        crate::app::Mode::Dialog(crate::app::DialogKind::Find)
            | crate::app::Mode::Dialog(crate::app::DialogKind::Replace)
    ) {
        app.pop_mode();
    }
    app.dirty = true;
}

/// Walk a match list from `begin` by `step`, taking the first match `keep` accepts. Indices
/// are signed so a backward walk runs off the front without underflowing.
fn scan(all: &[Match], begin: isize, step: isize, keep: &dyn Fn(&Match) -> bool) -> Option<Match> {
    let len = all.len() as isize;
    let mut i = begin;
    while (0..len).contains(&i) {
        let m = all[i as usize];
        if keep(&m) {
            return Some(m);
        }
        i += step;
    }
    None
}

/// Search in the state's direction and move onto the hit (FR-S2, FR-S7).
///
/// On success the selection spans the match with the caret at its end, so `F3 F3 F3` and then
/// typing replaces successive matches with no extra keystroke.
///
/// # Moving off the match the caret is standing on
///
/// The engine's `first_after` is a strict inequality, which is what terminates the F3 loop,
/// but applied to the caret alone it is wrong in both directions:
///
///  - backward: the caret sits at the previous match's **end**, and that match satisfies
///    `start < from`, so it is re-selected forever and `Shift+F3` looks frozen.
///  - forward: a match that begins exactly where the search starts is real text the user has
///    not seen yet, and a strict `start > from` skips it and wraps onto something later.
///
/// Rather than lean on the inequality, the loop asks the question directly: walk the match
/// list in the direction of travel, skip the one match already selected, and take the first
/// one left. `current` is dropped by identity, which is exactly "move off where you are" --
/// it never drops a distinct match that happens to share a position. When nothing lies ahead
/// in that direction, `wrap_around` restarts from the far end with the same rule.
///
/// Overlapping matches are reported by the scan (`oo` in `fooo` yields 1..3 and 2..4) but F3
/// walks whole selections: once 1..3 is selected the caret is at 3 and the next stop is past
/// it, because re-selecting the overlap would make the loop oscillate.
pub fn find_next(app: &mut App) -> FindOutcome {
    let find = &app.find;
    if find.needle.is_empty() {
        return FindOutcome::Idle;
    }
    let needle = find.needle.clone();
    let opts = find.effective_opts();
    let forward = opts.forward;
    let current = find.current;
    let Some(doc) = app.doc() else {
        return FindOutcome::Idle;
    };
    let matcher = match Matcher::new(&doc.buffer, &needle, &opts) {
        Ok(m) => m,
        Err(e) => {
            app.find.error = Some(e.to_string());
            return FindOutcome::Invalid(e.to_string());
        }
    };
    let all = matcher.matches();
    let caret = doc.cursor.pos;
    // Where the last find or Ctrl+F3 left us, as a range. The caret is at one of its edges
    // after a find, and somewhere inside it after Ctrl+F3 reads a word.
    let here = current.map(|m| (m.start, m.end)).or_else(|| {
        doc.selection
            .filter(|sel| !sel.is_empty())
            .map(|sel| sel.bounds())
    });
    // "Move off where you are": anything touching the current range is not progress, so the
    // next F3 cannot answer with the selection it is standing on -- which is precisely the
    // bug that made Shift+F3 repeat one match forever.
    let advances = |m: &Match| match here {
        Some((from, to)) => m.end > from && m.start < to,
        None => false,
    };
    // A zero-width match selects nothing, so the caret never moves and the overlap test above
    // can never see progress: pattern `^` would sit on line 0 forever. The fallback is to
    // require the position to change, which for these matches is the only meaning "next" has.
    // FR-S4's "`F3` repeated 5x must advance and terminate" is satisfied by the scan walking
    // one line anchor per press and wrapping cleanly.
    let stuck_on_zero_width = current.is_some_and(|m| m.start == m.end);
    let no_progress = |m: &Match| stuck_on_zero_width && m.start == caret;
    // Direction of travel, measured from the caret. Forward counts a match that begins
    // exactly at the caret, so a match at position 0 is reachable from 0 rather than being
    // skipped and wrapped past.
    let ahead = |m: &Match| {
        if forward {
            m.start >= caret
        } else {
            m.start < caret
        }
    };
    let keep = |m: &Match| ahead(m) && !advances(m) && !no_progress(m);
    // Walk in the direction of travel, then -- only when wrap is on -- from the far end. The
    // wrap pass drops `ahead` because after turning a corner every remaining match is ahead.
    let step: isize = if forward { 1 } else { -1 };
    let begin: isize = if forward { 0 } else { all.len() as isize - 1 };
    let hit = scan(all, begin, step, &keep).or_else(|| {
        opts.wrap_around
            .then(|| scan(all, begin, step, &|m| !advances(m) && !no_progress(m)))
            .flatten()
    });
    app.find.error = None;
    let Some(hit) = hit else {
        app.find.current = None;
        app.dirty = true;
        return FindOutcome::NotFound;
    };
    // Wrapping is decided against the document edge: a hit behind the caret going forward, or
    // ahead of it going backward, can only have come from the far end.
    let wrapped = if forward {
        hit.start < caret
    } else {
        hit.start > caret
    };
    if let Some(doc) = app.doc_mut() {
        // Caret at the match end (FR-S2 item 7) so typing overwrites the match.
        doc.begin_selection(hit.start, hit.end, SelectionKind::Range);
    }
    app.find.current = Some(hit);
    reveal_caret(app, hit.start);
    if app.find.mark_matches {
        app.refresh_marks();
    }
    app.dirty = true;
    FindOutcome::Found { wrapped }
}

/// The word under the caret, or `None` when the caret is not on one.
pub fn word_under_caret(app: &App) -> Option<String> {
    let doc = app.doc()?;
    let (start, end) = doc.buffer.word_range_at(doc.cursor.pos);
    if start == end {
        return None;
    }
    let text: String = doc
        .buffer
        .line_string(start.line)
        .chars()
        .skip(start.col)
        .take(end.col - start.col)
        .collect();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `Ctrl+F3` / `Ctrl+Shift+F3`: adopt the word under the caret and search for it (FR-S7).
///
/// Whole word is forced on. The user asked for *this word*, not for a fragment of longer
/// identifiers, and the needle came out of a word boundary to begin with.
///
/// # Selects the word under the caret first
///
/// The occurrence the pointer is on becomes the selection, and the *next* Ctrl+F3 moves to the
/// following one. Two reasons: the word is highlighted immediately, which is the feedback the
/// shortcut is asking for, and it matches Notepad++ -- which means a test can assert the word
/// was picked up without depending on how far a search happened to travel.
///
/// This is done by selecting the word range directly rather than by seeding `current` and
/// searching. Seeding relies on the word range being one of the pattern's matches, and that is
/// not guaranteed: in `"foobar foo foobar"` with the caret at 7, whole-word `foo` matches only
/// 7..10, while `word_range_at` legitimately reports 6..10 because `_` and letters are the same
/// identifier class to `Buffer::word_*`. The seed then never matched, and the command reported
/// nothing.
///
/// With no word under the caret this reports [`FindOutcome::NoWord`] rather than searching for
/// the empty string.
pub fn find_word(app: &mut App, forward: bool) -> FindOutcome {
    let Some(word) = word_under_caret(app) else {
        return FindOutcome::NoWord;
    };
    let anchor = app
        .doc()
        .map(|d| d.buffer.word_range_at(d.cursor.pos))
        .filter(|(start, end)| start != end);
    app.find.needle = word;
    app.find.forward = forward;
    app.find.opts.forward = forward;
    app.find.opts.whole_word = true;
    app.find.validate_now();
    let Some((start, end)) = anchor else {
        // Should not happen -- `word_under_caret` already found a word -- but a search is
        // still the honest fallback.
        return find_next(app);
    };
    let matched = Match {
        start,
        end,
        text_len: end.col.saturating_sub(start.col),
    };
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(start, end, SelectionKind::Range);
    }
    app.find.current = Some(matched);
    reveal_caret(app, start);
    app.refresh_marks();
    app.dirty = true;
    FindOutcome::Found { wrapped: false }
}

/// Replace the current match, then advance (FR-S3).
pub fn replace_current(app: &mut App) -> FindOutcome {
    let find = &app.find;
    if find.needle.is_empty() {
        return FindOutcome::Idle;
    }
    if let Some(msg) = find.error.clone() {
        return FindOutcome::Invalid(msg);
    }
    // Only the text is needed: expansion keys off the needle, and `expand_replacement`
    // compiles it the same way the search did.
    let (needle, replacement) = (find.needle.clone(), find.replacement.clone());
    let Some(current) = app.find.current else {
        // Not on a match yet: find one first, so `Replace` right after `Ctrl+H` does something
        // instead of nothing.
        return find_next(app);
    };
    let Some(doc) = app.doc() else {
        return FindOutcome::Idle;
    };
    let matched = matched_text(doc, &current);
    let new_text = expand_replacement(&needle, &matched, &replacement);
    let now = app.clock;
    let result = app
        .doc_mut()
        .map(|d| d.replace_range(current.start, current.end, &new_text, now));
    match result {
        Some(Ok(_)) => {}
        Some(Err(e)) => return FindOutcome::Invalid(e.user_message()),
        None => return FindOutcome::Idle,
    }
    app.find.current = None;
    app.mark_session_dirty();
    app.refresh_marks();
    app.dirty = true;
    // Advance so repeated Enter walks the file, which is how Replace feels in Notepad++.
    match find_next(app) {
        FindOutcome::Found { .. } | FindOutcome::NotFound => FindOutcome::Replaced(1),
        other => other,
    }
}

/// Replace every match, limited to the selection when "In selection" is on (FR-S3).
pub fn replace_all(app: &mut App) -> FindOutcome {
    let find = &app.find;
    if find.needle.is_empty() {
        return FindOutcome::Idle;
    }
    if let Some(msg) = find.error.clone() {
        return FindOutcome::Invalid(msg);
    }
    let (needle, replacement) = (find.needle.clone(), find.replacement.clone());
    let range = if find.in_selection {
        selection_range(app)
    } else {
        None
    };
    let Some(doc) = app.doc() else {
        return FindOutcome::Idle;
    };
    if doc.readonly {
        return FindOutcome::Invalid("document is read-only".to_string());
    }
    // Replace All is a forward operation regardless of the dialog's arrow: the count and the
    // result are direction-independent, and a fixed direction keeps the engine's right-to-left
    // edit order well defined.
    let matcher = match Matcher::new(&doc.buffer, &needle, &find.opts) {
        Ok(m) => m,
        Err(e) => {
            app.find.error = Some(e.to_string());
            return FindOutcome::Invalid(e.to_string());
        }
    };
    // Decide every replacement BEFORE touching the buffer. Re-scanning after each edit is what
    // makes `"aa"` -> `"a"` on `"aaaa"` chase its own output.
    let spans: Vec<(LineCol, LineCol, String)> = matcher
        .matches()
        .iter()
        .filter(|m| match range {
            Some((start, end)) => m.start >= start && m.start < end,
            None => true,
        })
        .map(|m| {
            let matched = matched_text(doc, m);
            (m.start, m.end, matcher.expand(&replacement, &matched))
        })
        .collect();
    let now = app.clock;
    let Some(doc) = app.doc_mut() else {
        return FindOutcome::Idle;
    };
    match doc.replace_ranges(&spans, now) {
        Ok(n) => {
            app.find.current = None;
            if n > 0 {
                app.mark_session_dirty();
            }
            app.refresh_marks();
            app.dirty = true;
            FindOutcome::Replaced(n)
        }
        Err(e) => FindOutcome::Invalid(e.user_message()),
    }
}

/// Count occurrences without moving anything (FR-S5).
pub fn count_matches(app: &mut App) -> FindOutcome {
    let find = &app.find;
    if find.needle.is_empty() {
        return FindOutcome::Idle;
    }
    if let Some(msg) = find.error.clone() {
        return FindOutcome::Invalid(msg);
    }
    let (needle, opts) = (find.needle.clone(), find.opts.clone());
    let range = if app.find.in_selection {
        selection_range(app)
    } else {
        None
    };
    let Some(doc) = app.doc() else {
        return FindOutcome::Idle;
    };
    match Matcher::new(&doc.buffer, &needle, &opts) {
        Ok(m) => {
            let n = range
                .map(|(s, e)| {
                    m.matches()
                        .iter()
                        .filter(|mm| mm.start >= s && mm.start < e)
                        .count()
                })
                .unwrap_or_else(|| m.matches().len());
            FindOutcome::Counted(n)
        }
        Err(e) => {
            app.find.error = Some(e.to_string());
            FindOutcome::Invalid(e.to_string())
        }
    }
}

/// Go To Line (FR-S6): accept `42` or `42:7`, clamp to the document, reject junk inline.
///
/// Clamping rather than refusing is the spec's call. A mistyped large number or a session
/// recorded against a longer file should land on the last line, not fail.
pub fn go_to(app: &mut App, input: &str) -> FindOutcome {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return FindOutcome::Invalid("enter a line number".to_string());
    }
    let (line_text, col_text) = match trimmed.split_once(':') {
        Some((l, c)) => (l, Some(c)),
        None => (trimmed, None),
    };
    let Some(line) = parse_usize(line_text) else {
        return FindOutcome::Invalid(format!("not a line number: {trimmed}"));
    };
    if line == 0 {
        return FindOutcome::Invalid("lines start at 1".to_string());
    }
    let col = match col_text {
        Some(c) => match parse_usize(c) {
            Some(n) => n.saturating_sub(1),
            None => return FindOutcome::Invalid(format!("not a column: {c}")),
        },
        None => 0,
    };
    let Some(doc) = app.doc() else {
        return FindOutcome::Idle;
    };
    let target = doc.buffer.clamp(LineCol::new(line - 1, col));
    if let Some(doc) = app.doc_mut() {
        doc.go_to_line(target.line);
        doc.set_cursor(LineCol::new(target.line, target.col), false);
    }
    reveal_caret(app, target);
    app.dirty = true;
    FindOutcome::Found { wrapped: false }
}

/// Parse a positive decimal integer, rejecting anything else.
///
/// Not `str::parse::<usize>()`, which accepts a leading `+`. A field that took `+5` while
/// rejecting `5.0` would be a strange rule to explain.
fn parse_usize(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse::<usize>().ok()
}

/// The text a match covers, across line breaks.
pub fn matched_text(doc: &Document, m: &Match) -> String {
    let mut out = String::new();
    for line in m.start.line..=m.end.line {
        let text = doc.buffer.line_string(line);
        let from = if line == m.start.line { m.start.col } else { 0 };
        let to = if line == m.end.line {
            m.end.col
        } else {
            doc.buffer.line_len_chars(line)
        };
        out.extend(text.chars().skip(from).take(to.saturating_sub(from)));
        if line < m.end.line {
            out.push('\n');
        }
    }
    out
}

/// Scroll so a match is on screen, leaving horizontal scroll alone.
///
/// A find that yanked the file sideways would be disorienting; the vertical move is the one
/// that actually loses the caret. Delegates to `Document::scroll_to_show`, which counts
/// **visual rows**: comparing a match's line index against `top_line` is wrong the moment wrap
/// is on, because a line above the match may occupy several rows and the match can be off
/// screen while its line index still looks in range (S13, FR-V1).
fn reveal_caret(app: &mut App, at: LineCol) {
    let geom = app.geometry;
    let Some(doc) = app.doc_mut() else {
        return;
    };
    doc.scroll_to_show(&geom, at);
}

/// Re-mark matches after a document edit. Called from the edit paths so marks never point at
/// stale text.
pub fn marks_after_edit(app: &mut App, now: Instant) {
    let _ = now;
    app.refresh_marks();
}
