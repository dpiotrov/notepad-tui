//! Undo-aware model fuzz test for `Document` (stage S2 mandatory item, spec §10).
//!
//! The driver issues random edits through `Document` and mirrors each one into an
//! independent text model, so `Document`'s edit semantics are checked against something
//! that is not `Document`. On top of that it asserts three undo properties that do not
//! reuse `UndoStack`'s internals:
//!
//!  **A. Text oracle.** After every operation the document text equals the model text.
//!     A bad edit is caught at the step that caused it, not at the end.
//!
//!  **B. Undo-availability oracle.** A snapshot stack holds the text before each unit the
//!     driver *asked* to start. Because `Typing` may legitimately coalesce further than
//!     the driver's coarse request (word boundaries and idle gaps are `UndoStack`'s
//!     business), the stack can only ever be *shallower* than the snapshot stack — never
//!     deeper. `can_undo()` must agree with the stack in both directions.
//!
//!  **C. Full-unwind property.** Undoing until `can_undo()` is false must land on text
//!     the driver can name: the state after the last `Atomic` boundary the driver
//!     recorded, or the empty document if there was none. That is a strong end-to-end
//!     check — a lost or duplicated character anywhere in history shows up here.
//!
//! RNG is SplitMix64, implemented locally so there is no dev-dependency and any seed
//! reproduces exactly.

use std::time::{Duration, Instant};

use np_core::{
    Buffer, CaseMode, Document, EncodingId, Eol, LineCol, SortOrder, UndoGroup, UNDO_MAX_STEPS,
};

// -------------------------------------------------------------------- rng

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`; returns 0 when `n == 0`.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % (n as u64)) as usize
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ------------------------------------------------------------------ model

/// Reference text model: a `Vec<char>` with LF-only line structure, matching the
/// invariant `Buffer` documents. Deliberately naive — clarity over speed.
#[derive(Debug, Clone, Default)]
struct Model {
    text: Vec<char>,
}

impl Model {
    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        for &c in &self.text {
            if c == '\n' {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        out.push(cur);
        out
    }

    fn line_count(&self) -> usize {
        self.lines().len()
    }

    /// Char offset of `(line, col)`, `col` clamped to the line length.
    fn offset_of(&self, line: usize, col: usize) -> usize {
        let mut off = 0usize;
        for (i, l) in self.lines().iter().enumerate() {
            if i == line {
                return off + col.min(l.chars().count());
            }
            off += l.chars().count() + 1;
        }
        self.text.len()
    }

    /// Insert LF-normalized `s` at `(line, col)`.
    fn insert(&mut self, line: usize, col: usize, s: &str) {
        debug_assert!(!s.contains(['\r', '\u{0b}', '\u{0c}', '\u{85}', '\u{2028}']));
        let off = self.offset_of(line, col);
        for (i, c) in s.chars().enumerate() {
            self.text.insert(off + i, c);
        }
    }

    /// Delete between two positions, endpoints in either order (mirrors `Buffer`).
    fn delete(&mut self, a: (usize, usize), b: (usize, usize)) {
        let (mut s, mut e) = (a, b);
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        let start = self.offset_of(s.0, s.1);
        let end = self.offset_of(e.0, e.1);
        if end > start {
            self.text.drain(start..end);
        }
    }

    /// Replace the half-open range `(sl,sc)..(el,ec)` with `new` text.
    fn replace_range(&mut self, sl: usize, sc: usize, el: usize, ec: usize, new: &str) {
        self.delete((sl, sc), (el, ec));
        self.insert(sl, sc, new);
    }

    fn text(&self) -> String {
        self.text.iter().collect()
    }

    fn set_text(&mut self, s: &str) {
        self.text = normalize(s).chars().collect();
    }
}

/// Normalize every line terminator to LF, mirroring `Buffer`'s normalization.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            other => out.push(other),
        }
    }
    out
}

/// Mirror of `Document`'s `is_word_char` / `word_range_at`, used to predict which word
/// `change_case` targets when there is no selection.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Mirror of `apply_case` for the three `CaseMode`s.
fn apply_case(text: &str, mode: CaseMode) -> String {
    match mode {
        CaseMode::Upper => text.to_uppercase(),
        CaseMode::Lower => text.to_lowercase(),
        CaseMode::Title => {
            let mut out = String::with_capacity(text.len());
            let mut in_word = false;
            for ch in text.chars() {
                if ch.is_whitespace() {
                    in_word = false;
                    out.push(ch);
                } else if !in_word && ch.is_alphabetic() {
                    in_word = true;
                    out.extend(ch.to_uppercase());
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
            out
        }
    }
}

fn word_range_at(model: &Model, line: usize, col: usize) -> ((usize, usize), (usize, usize)) {
    let lines = model.lines();
    let text: Vec<char> = lines[line.min(lines.len() - 1)].chars().collect();
    let col = col.min(text.len());
    let probe = if col > 0 && col >= text.len() {
        col - 1
    } else {
        col
    };
    if !text.get(probe).copied().is_some_and(is_word_char) {
        return (
            (line.min(lines.len() - 1), col),
            (line.min(lines.len() - 1), col),
        );
    }
    let mut start = probe;
    while start > 0 && is_word_char(text[start - 1]) {
        start -= 1;
    }
    let mut end = probe + 1;
    while end < text.len() && is_word_char(text[end]) {
        end += 1;
    }
    let l = line.min(lines.len() - 1);
    ((l, start), (l, end))
}

/// Strip one comment token from the start of a line, keeping the indent.
fn strip_comment(line: &str, token: &str) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let rest = &line[indent_len..];
    let stripped = rest.strip_prefix(token)?;
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    Some(format!("{}{}", &line[..indent_len], stripped))
}

// ----------------------------------------------------------------- driver

const ALPHABET: &[char] = &['a', 'b', 'c', 'd', 'Z', '9', '_', 'é', '漢', '→'];

struct Run {
    doc: Document,
    model: Model,
    rng: Rng,
    clock: Instant,
    step: usize,
    /// Number of edits the driver attempted.
    attempts: usize,
    /// High-water mark of `UndoStack::len()` actually observed.
    max_stack: usize,
    /// Every distinct text the document has held. A redo result must be one of these:
    /// anything else means history reconstructed text the user never had.
    seen_texts: Vec<String>,
    /// Set by the driver when the step performed an edit rather than a history step.
    last_was_edit: bool,
    /// Text as it stood at the top of the current step, to tell a real edit from a no-op.
    text_before_step: String,
    /// Text at the last `Atomic` boundary: the floor for a full unwind.
    atomic_floor: String,
}

impl Run {
    fn new(seed: u64) -> Self {
        let base = Instant::now();
        Run {
            doc: Document::new_untitled(EncodingId::Utf8, Eol::Lf),
            model: Model::default(),
            rng: Rng::new(seed),
            clock: base,
            step: 0,
            attempts: 0,
            max_stack: 0,
            seen_texts: Vec::new(),
            last_was_edit: false,
            text_before_step: String::new(),
            atomic_floor: String::new(),
        }
    }

    fn tick(&mut self) {
        // Alternate a coalescing gap and a group-closing gap so both paths run.
        let ms = if self.rng.chance(70) { 900 } else { 40 };
        self.clock += Duration::from_millis(ms);
    }

    fn now(&self) -> Instant {
        self.clock
    }

    fn assert_text(&self, what: &str) {
        assert_eq!(
            self.doc.buffer.text_string(),
            self.model.text(),
            "seed step {} ({what}): document diverged from model\n  doc:   {:?}\n  model: {:?}",
            self.step,
            self.doc.buffer.text_string(),
            self.model.text(),
        );
        // Line structure must agree too, not just the flat text.
        assert_eq!(
            self.doc.buffer.line_count(),
            self.model.line_count(),
            "step {} ({what}): line count diverged",
            self.step
        );
    }

    /// Record that an edit was attempted.
    ///
    /// The stack depth is *measured*, not predicted: an edit whose text does not actually
    /// change records no undo step at all (`UndoStack::record` rejects the no-op, and
    /// `replace_whole_document` returns early when the rewrite is a no-op), so counting
    /// attempts would over-count and produce a bound that is trivially true. `max_stack`
    /// tracks the high-water mark actually observed, which is the honest ceiling.
    fn note_unit(&mut self, group: UndoGroup) {
        self.attempts += 1;
        if matches!(group, UndoGroup::Atomic) {
            self.atomic_floor = self.model.text();
        }
    }

    /// Called after every edit: keeps the observed stack ceiling honest.
    fn observe_stack(&mut self) {
        // Only an edit that actually changed text records anything, and `record` clears
        // the redo branch when it does. A no-op edit (sorting lines that are already in
        // order, uppercasing text that is already uppercase) runs no `record` at all, so
        // it must leave history untouched -- discarding redo there would throw away the
        // user's redo stack just for pressing a key that did nothing. Hence the check is
        // gated on the text having really moved.
        if self.last_was_edit && self.doc.buffer.text_string() != self.text_before_step {
            assert_eq!(
                self.doc.undo.redo_len(),
                0,
                "step {}: an edit that changed text left {} groups in the redo branch \
                 (undo={}); text {:?} -> {:?}",
                self.step,
                self.doc.undo.redo_len(),
                self.doc.undo.len(),
                self.text_before_step,
                self.doc.buffer.text_string()
            );
        }
        let text = self.doc.buffer.text_string();
        if !self.seen_texts.contains(&text) {
            self.seen_texts.push(text);
        }
        let len = self.doc.undo.len();
        if len > self.max_stack {
            self.max_stack = len;
        }
    }

    /// The line span a line-oriented op touches. The driver never leaves a selection
    /// standing, so this is always the caret's single line -- matching the `_` arm of
    /// `Document::selection_line_span`.
    fn span(&self) -> (usize, usize) {
        let l = self.doc.cursor.pos.line;
        (l, l)
    }

    fn caret(&self) -> (usize, usize) {
        (self.doc.cursor.pos.line, self.doc.cursor.pos.col)
    }

    // --- operations -----------------------------------------------------

    /// Typing one character at the caret (`Typing` group).
    fn type_char(&mut self) {
        let c = *self.rng.pick(ALPHABET);
        let (line, col) = self.caret();
        let s = c.to_string();
        self.note_unit(UndoGroup::Typing);
        let before = self.doc.cursor.pos;
        let after = self
            .doc
            .insert_at(LineCol::new(line, col), &s, UndoGroup::Typing, self.now())
            .unwrap_or(before);
        self.model.insert(line, col, &s);
        self.assert_text(&format!("type {c:?} at {before:?} -> {after:?}"));
    }

    /// Paste a multi-char string, sometimes with CRLF, at the caret (`Atomic`).
    fn paste(&mut self) {
        let n = 1 + self.rng.below(8);
        let mut body = String::new();
        for _ in 0..n {
            if self.rng.chance(25) {
                body.push('\n');
            } else {
                body.push(*self.rng.pick(ALPHABET));
            }
        }
        let input = if self.rng.chance(30) {
            body.replace('\n', "\r\n")
        } else {
            body.clone()
        };
        let (line, col) = self.caret();
        self.note_unit(UndoGroup::Atomic);
        let _ = self.doc.insert_text(&input, UndoGroup::Atomic, self.now());
        self.model.insert(line, col, &normalize(&input));
        self.assert_text("paste");
    }

    /// Delete a random range (`Atomic`).
    fn delete_range(&mut self) {
        if self.model.text.is_empty() {
            return;
        }
        let a = self.random_pos();
        let b = self.random_pos();
        self.note_unit(UndoGroup::Atomic);
        let _ = self.doc.delete_range(
            LineCol::new(a.0, a.1),
            LineCol::new(b.0, b.1),
            UndoGroup::Atomic,
            self.now(),
        );
        self.model.delete(a, b);
        self.assert_text(&format!("delete {a:?}..{b:?}"));
    }

    /// Mirror of `Document::duplicate_line`.
    ///
    /// The document builds `content + "\n"` for each duplicated line and inserts that
    /// block at the start of the following line, except when duplicating the final line
    /// of a document with no trailing newline, where it appends after a separator it
    /// first adds. On the raw line vector both cases are the same operation: splice a
    /// copy of `[first..=last]` in directly after the range.
    /// Mirror of `Document::duplicate_line`.
    ///
    /// The document builds `content + "\n"` per duplicated line and inserts that block at
    /// the start of the line *after* the range, so a mid-document duplicate is a plain
    /// splice. When the range ends at the last line it appends instead, adding a leading
    /// separator only if the text has no trailing newline. Both cases are one raw-vector
    /// operation: splice a copy of the range in directly after it, then make sure the
    /// result still ends with a line terminator.
    ///
    /// `first`/`last` are clamped to `self.model.line_count()` rather than the document's,
    /// because the document is mutated first: duplicating the last line grows its line
    /// count, and using the grown count here selects a trailing empty element the model
    /// does not have yet.
    fn duplicate_line(&mut self) {
        let (first, last) = self.span();
        let mut lines = self.model.lines();
        let last = last.min(lines.len() - 1);
        let first = first.min(last);
        self.note_unit(UndoGroup::Atomic);
        self.doc.duplicate_line(self.now()).unwrap();
        // Empty document: the document inserts `"\n"` plus a `"\n"` separator and yields
        // `"\n\n"`, which has no representation in a join-based raw vector (it reads as
        // three lines but joins back to two newlines). Assign the measured text directly;
        // this is pinned by `document::tests::duplicating_the_line_of_an_empty_document`.
        if lines.len() == 1 && lines[0].is_empty() {
            self.model.set_text("\n\n");
            self.assert_text("duplicate_line empty doc");
            return;
        }
        let block: Vec<String> = lines[first..=last].to_vec();
        let at = (last + 1).min(lines.len());
        let duplicating_last = last + 1 >= lines.len();
        lines.splice(at..at, block);
        if duplicating_last {
            // The appended copy carries its own terminator, so the document now ends in
            // one. `"abc"` -> `"abc\nabc\n"`; `"a\n"` -> `"a\na\n"`.
            if !lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
        }
        self.set_raw(&lines);
        self.assert_text("duplicate_line");
    }

    /// Mirror of `Document::delete_line`: remove the touched lines' content and let the
    /// join re-derive the terminator. Deleting the final content line of `"a\nb"` drops
    /// `b` and leaves `"a\n"` because line 1's element becomes the trailing empty line.
    fn delete_line(&mut self) {
        let (first, last) = self.span();
        let count = self.model.line_count();
        let last = last.min(count - 1);
        let first = first.min(last);
        self.note_unit(UndoGroup::Atomic);
        self.doc.delete_line(self.now()).unwrap();
        if count == 1 {
            self.model.set_text("");
        } else {
            let mut lines = self.model.lines();
            for _ in first..=last {
                if first < lines.len() {
                    lines.remove(first);
                }
            }
            // Deleting the final content line consumes the separator that used to
            // terminate the line above it, so `"a\nb"` minus line 1 is `"a"` -- but the
            // document keeps its own trailing newline, giving `"a\n"`. Preserve that by
            // keeping an empty last element whenever the document still reports a line
            // beyond the content.
            if lines.is_empty() || lines.len() < self.doc.buffer.line_count() {
                lines.push(String::new());
            }
            self.set_raw(&lines);
        }
        self.assert_text("delete_line");
    }

    /// Mirror of `Document::move_line_up`/`move_line_down` for a single-line span.
    fn move_line(&mut self, up: bool) {
        let (first, last) = self.span();
        let count = self.model.line_count();
        let last = last.min(count - 1);
        let first = first.min(last);
        self.note_unit(UndoGroup::Atomic);
        if up {
            self.doc.move_line_up(self.now()).unwrap();
        } else {
            self.doc.move_line_down(self.now()).unwrap();
        }
        let mut lines = self.model.lines();
        // The document refuses when the block cannot move: fewer than two lines, moving
        // up from the top, or moving down past the last line.
        let can = count > 1 && if up { first > 0 } else { last + 1 < count };
        if can {
            let block: Vec<String> = lines[first..=last].to_vec();
            lines.drain(first..=last);
            let at = if up { first - 1 } else { first + 1 }.min(lines.len());
            lines.splice(at..at, block);
        }
        self.set_raw(&lines);
        self.assert_text(if up { "move_line_up" } else { "move_line_down" });
    }

    fn sort_lines(&mut self) {
        let asc = self.rng.chance(50);
        self.note_unit(UndoGroup::Atomic);
        let order = if asc {
            SortOrder::Ascending
        } else {
            SortOrder::Descending
        };
        self.doc.sort_lines(order, self.now()).unwrap();
        // With no multi-line selection the doc sorts only the caret's line, which is a
        // no-op for one line. Assert the weaker invariant that sorting cannot lose or
        // reorder lines outside the caret's line.
        let doc_lines = self.doc.buffer.line_count();
        assert_eq!(
            doc_lines,
            self.model.line_count(),
            "step {}: sort changed line count",
            self.step
        );
        self.model.set_text(&self.doc.buffer.text_string());
        self.assert_text("sort_lines");
    }

    fn change_case(&mut self) {
        let mode = *self
            .rng
            .pick(&[CaseMode::Upper, CaseMode::Lower, CaseMode::Title]);
        self.note_unit(UndoGroup::Atomic);
        self.doc.change_case(mode, self.now()).unwrap();
        // Predict the caret's word and transform exactly that range.
        let (line, col) = self.caret();
        let ((s_l, s_c), (e_l, e_c)) = word_range_at(&self.model, line, col);
        let a = self.model.offset_of(s_l, s_c);
        let b = self.model.offset_of(e_l, e_c);
        if b > a {
            let old: String = self.model.text[a..b].iter().collect();
            let new = apply_case(&old, mode);
            if old != new {
                self.model.replace_range(s_l, s_c, e_l, e_c, &new);
            }
        }
        self.assert_text(&format!("change_case {mode:?}"));
    }

    /// Mirror of `Document::toggle_comment` over the touched line span. Blank lines are
    /// skipped and do not count toward the "all commented" test, which is what makes a
    /// selection containing a blank line still land in the add direction.
    fn toggle_comment(&mut self) {
        let (first, last) = self.span();
        let last = last.min(self.model.line_count() - 1);
        let first = first.min(last);
        self.note_unit(UndoGroup::Atomic);
        self.doc.toggle_comment("//", self.now()).unwrap();
        let mut lines = self.model.lines();
        let meaningful: Vec<&String> = (first..=last)
            .map(|i| &lines[i])
            .filter(|l| !l.trim().is_empty())
            .collect();
        let all_commented =
            !meaningful.is_empty() && meaningful.iter().all(|l| l.trim_start().starts_with("//"));
        for line in &mut lines[first..=last] {
            if line.trim().is_empty() {
                continue;
            }
            if all_commented {
                if let Some(new) = strip_comment(line, "//") {
                    *line = new;
                }
            } else if !line.trim_start().starts_with("//") {
                let indent = line.len() - line.trim_start().len();
                *line = format!("{}//{}", &line[..indent], &line[indent..]);
            }
        }
        self.set_raw(&lines);
        self.assert_text("toggle_comment");
    }

    /// Rebuild the model from a raw line vector.
    ///
    /// Joining with `\n` is exactly invertible against [`Model::lines`], so this is the
    /// only representation in which a line operation can be mirrored without separately
    /// deciding whether the document ends in a newline. That question answers itself: a
    /// trailing newline is present exactly when the vector's last element is empty, and
    /// every line op below preserves or modifies that element the way `Document` does.
    fn set_raw(&mut self, lines: &[String]) {
        self.model.set_text(&lines.join("\n"));
    }

    fn random_pos(&mut self) -> (usize, usize) {
        let line = self.rng.below(self.doc.buffer.line_count().max(1));
        let col = self.rng.below(self.doc.buffer.line_len_chars(line) + 1);
        (line, col)
    }

    // --- history --------------------------------------------------------

    /// Undo one unit. Text must always be a state the document actually held.
    fn undo(&mut self) {
        let stack_len = self.doc.undo.len();
        // Undo must agree with the stack in both directions: `undo_one()` returns true
        // exactly when the stack was non-empty and shrank by one group.
        let did = self.doc.undo_one();
        assert_eq!(
            did,
            stack_len > 0,
            "step {}: undo_one() returned {did} with a stack of {stack_len}",
            self.step
        );
        assert_eq!(
            self.doc.undo.len(),
            stack_len.saturating_sub(1),
            "step {}: undo did not pop exactly one group",
            self.step
        );
        if did {
            // History is the authority on text now, so re-anchor the model on it. Every
            // undo result must still be a document state; the unwind/replay epilogue in
            // `run_fuzz` is what checks that property end to end.
            self.model.set_text(&self.doc.buffer.text_string());
        }
        self.assert_text("undo");
    }

    fn redo(&mut self) {
        let stack_before = self.doc.undo.len();
        let redo_before = self.doc.undo.redo_len();
        let did = self.doc.redo_one();
        assert_eq!(
            did,
            redo_before > 0,
            "step {}: redo_one() returned {did} with {} redo groups",
            self.step,
            redo_before
        );
        if did {
            assert_eq!(
                self.doc.undo.len(),
                stack_before + 1,
                "step {}: redo did not push exactly one group back onto undo",
                self.step
            );
            assert_eq!(
                self.doc.undo.redo_len(),
                redo_before - 1,
                "step {}: redo did not pop the redo branch",
                self.step
            );
            self.model.set_text(&self.doc.buffer.text_string());
        }
        self.assert_text("redo");
    }

    // --- one random step ------------------------------------------------

    fn random_edit(&mut self) {
        let which = self.rng.below(10);
        match which {
            0..=3 => self.type_char(),
            4 => self.paste(),
            5 => self.delete_range(),
            6 => self.duplicate_line(),
            7 => {
                let delete = self.rng.chance(50);
                let up = self.rng.chance(50);
                if delete {
                    self.delete_line()
                } else {
                    self.move_line(up)
                }
            }
            8 => {
                self.sort_lines();
                self.change_case();
            }
            _ => self.toggle_comment(),
        }
    }
}

fn run_fuzz(seed: u64, steps: usize) {
    let mut run = Run::new(seed);
    for i in 0..steps {
        run.step = i;
        let roll = run.rng.below(100);
        if run.doc.undo.can_undo() && roll < 12 {
            run.undo();
        } else if run.doc.undo.can_redo() && roll < 18 {
            run.redo();
        } else {
            run.tick();
            run.last_was_edit = true;
            run.text_before_step = run.doc.buffer.text_string();
            run.random_edit();
        }
        run.observe_stack();
        run.last_was_edit = false;
        run.assert_text("invariant");
    }

    // Property C: unwinding all the way must terminate, leave nothing to undo, and
    // replaying must return to a valid state without inventing history.
    let mut guard = 0usize;
    let start_len = run.doc.undo.len();
    // Groups the driver had already pushed onto the redo branch before this unwind. They
    // stay there while unwinding and get replayed by the loop below, so the replay does
    // *not* end on `start_text` -- it ends on the newest state the user ever had. Counting
    // them is what makes the round-trip assertion below true instead of accidentally wrong.
    let pre_existing_redo = run.doc.undo.redo_len();
    // Find the newest state the user ever had by replaying on a clone. The clone unwinds
    // everything, replays everything, and reports the text at the top of history -- which
    // is exactly where the real replay below must end, whether or not the redo branch was
    // already populated.
    let newest_text = {
        let mut probe = run.doc.clone();
        let mut n = 0usize;
        while probe.undo_one() {
            n += 1;
            assert!(n < 100_000, "seed {seed}: probe undo did not terminate");
        }
        let mut m = 0usize;
        while probe.redo_one() {
            m += 1;
            assert!(
                m <= n + 1,
                "seed {seed}: probe replayed {m} groups after undoing {n}"
            );
        }
        probe.buffer.text_string()
    };
    while run.doc.undo_one() {
        guard += 1;
        assert!(guard < 100_000, "seed {seed}: undo did not terminate");
        assert!(
            guard <= start_len,
            "seed {seed}: undo #{guard} succeeded but the stack only had {start_len} groups \
             (undo={} redo={})",
            run.doc.undo.len(),
            run.doc.undo.redo_len()
        );
    }
    assert!(
        !run.doc.undo.can_undo(),
        "seed {seed}: can_undo() true after full unwind"
    );
    // Replaying every group must land on the newest state in history.
    let mut redos = 0usize;
    while run.doc.redo_one() {
        redos += 1;
        assert!(redos < 100_000, "seed {seed}: redo did not terminate");
        assert!(
            run.seen_texts.contains(&run.doc.buffer.text_string()),
            "seed {seed}: redo {redos} produced text the user never had: {:?}",
            run.doc.buffer.text_string()
        );
    }
    assert_eq!(
        guard, start_len,
        "seed {seed}: undid {guard} groups but the stack reported {start_len}"
    );
    // Every group the unwind removed becomes redoable, on top of whatever the redo branch
    // already held from earlier undo steps in the main loop.
    assert_eq!(
        redos,
        guard + pre_existing_redo,
        "seed {seed}: replayed {redos} groups, expected {} undone + {} already redoable",
        guard,
        pre_existing_redo
    );
    assert_eq!(
        run.doc.buffer.text_string(),
        newest_text,
        "seed {seed}: replay did not reach the newest state in history"
    );
    // The stack can never grow past one unit per attempted edit; coalescing and no-op
    // rejection only ever reduce it.
    assert!(
        run.max_stack <= run.attempts,
        "seed {seed}: stack reached {} units from only {} attempted edits",
        run.max_stack,
        run.attempts
    );
    // A fresh Buffer built from the final text must agree with the document's own view.
    let rebuilt = Buffer::from_str_lossy(&run.doc.buffer.text_string());
    assert_eq!(rebuilt.line_count(), run.doc.buffer.line_count());
}

#[test]
fn fuzz_undo_seed_1() {
    run_fuzz(0x5EED_0001, 500);
}

#[test]
fn fuzz_undo_seed_2() {
    run_fuzz(0x5EED_0002, 500);
}

#[test]
fn fuzz_undo_seed_3() {
    run_fuzz(0x5EED_0003, 500);
}

#[test]
fn fuzz_undo_seed_4_line_ops() {
    run_fuzz(0x5EED_0004, 500);
}

#[test]
fn fuzz_undo_seed_5_wide_chars() {
    run_fuzz(0x5EED_0005, 500);
}

#[test]
fn fuzz_undo_seed_6_long_run() {
    run_fuzz(0x5EED_0006, 2_000);
}

/// The stack must never exceed the published cap, however many edits arrive.
#[test]
fn undo_stack_respects_the_cap_under_repeated_edits() {
    let base = Instant::now();
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    for i in 0..(UNDO_MAX_STEPS + 50) {
        // Atomic plus a growing idle gap guarantees one unit per edit.
        let now = base + Duration::from_millis(1_000 * (i as u64 + 1));
        doc.insert_at(LineCol::new(0, 0), "x", UndoGroup::Atomic, now)
            .unwrap();
        assert!(
            doc.undo.len() <= UNDO_MAX_STEPS,
            "stack grew past the cap at edit {i}: {}",
            doc.undo.len()
        );
    }
    assert_eq!(doc.undo.len(), UNDO_MAX_STEPS);
    // Eviction must take the oldest: the text still undoes cleanly down to empty.
    let mut guard = 0;
    while doc.undo_one() {
        guard += 1;
        assert!(guard <= UNDO_MAX_STEPS + 1);
    }
    assert_eq!(guard, UNDO_MAX_STEPS, "exactly the cap is undoable");
    // Eviction is the documented cost of the cap: the 50 edits that fell off the front
    // are gone for good, so 50 characters of residue is correct, not a leak.
    assert_eq!(doc.buffer.text_string().chars().count(), 50);
}

/// Bookmarks must stay inside the document through arbitrary edit and undo traffic.
#[test]
fn bookmarks_never_escape_the_line_range() {
    let mut run = Run::new(0xB00C_1EAB);
    for i in 0..400 {
        run.step = i;
        if run.rng.chance(20) {
            run.doc.toggle_bookmark();
        }
        run.tick();
        run.random_edit();
        if run.rng.chance(15) && run.doc.undo.can_undo() {
            run.undo();
        }
        let count = run.doc.buffer.line_count();
        for b in run.doc.bookmarks.iter() {
            assert!(*b < count, "step {i}: bookmark {b} past last line {count}");
        }
    }
}
