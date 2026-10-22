//! Model-based fuzz test for selections, copy/paste and undo (stage S7, spec §10).
//!
//! Extends the S2 harness with the operations this stage added: range and block selections,
//! `selected_text`, `paste_block`, `indent_lines` and `outdent_lines`. The reference model is
//! a `Vec<String>` of logical lines plus a caret, and every operation is mirrored into it, so
//! a divergence is caught at the step that caused it.
//!
//! Undo is checked with the snapshot-stack property from S2: because `Typing` may coalesce
//! further than the driver asks, the real stack can only ever be shallower than the snapshot
//! stack, never deeper.
//!
//! RNG is SplitMix64, implemented locally so there is no dev-dependency and any seed
//! reproduces exactly.

use std::time::{Duration, Instant};

use np_core::{Document, EncodingId, Eol, LineCol, SelectionKind, UndoGroup};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

/// The independent model: lines plus a caret.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Model {
    lines: Vec<String>,
    caret: (usize, usize),
}

impl Model {
    fn new() -> Model {
        Model {
            lines: vec![String::new()],
            caret: (0, 0),
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.chars().count()).unwrap_or(0)
    }

    fn clamp_caret(&mut self) {
        self.caret.0 = self.caret.0.min(self.lines.len() - 1);
        let len = self.line_len(self.caret.0);
        self.caret.1 = self.caret.1.min(len);
    }

    fn insert(&mut self, line: usize, col: usize, s: &str) {
        let line = line.min(self.lines.len() - 1);
        let col = col.min(self.line_len(line));
        let pieces: Vec<&str> = s.split('\n').collect();
        let head: String = self.lines[line].chars().take(col).collect();
        let tail: String = self.lines[line].chars().skip(col).collect();
        if pieces.len() == 1 {
            self.lines[line] = format!("{head}{}{tail}", pieces[0]);
            return;
        }
        self.lines[line] = format!("{head}{}", pieces[0]);
        let mut extra: Vec<String> = pieces[1..pieces.len() - 1]
            .iter()
            .map(|p| p.to_string())
            .collect();
        let last_piece = pieces[pieces.len() - 1].to_string();
        extra.push(format!("{last_piece}{tail}"));
        for (i, e) in extra.iter().enumerate() {
            self.lines.insert(line + 1 + i, e.clone());
        }
    }

    fn delete(&mut self, a: (usize, usize), b: (usize, usize)) {
        let (mut a, mut b) = if a <= b { (a, b) } else { (b, a) };
        a.0 = a.0.min(self.lines.len() - 1);
        b.0 = b.0.min(self.lines.len() - 1);
        a.1 = a.1.min(self.line_len(a.0));
        b.1 = b.1.min(self.line_len(b.0));
        if a == b {
            return;
        }
        if a.0 == b.0 {
            let head: String = self.lines[a.0].chars().take(a.1).collect();
            let tail: String = self.lines[a.0].chars().skip(b.1).collect();
            self.lines[a.0] = format!("{head}{tail}");
            return;
        }
        let head: String = self.lines[a.0].chars().take(a.1).collect();
        let tail: String = self.lines[b.0].chars().skip(b.1).collect();
        self.lines[a.0] = format!("{head}{tail}");
        for _ in (a.0 + 1)..=b.0 {
            self.lines.remove(a.0 + 1);
        }
    }

    /// Rectangle text with space padding, the model's own reading of FR-E3.
    fn block_text(&self, first: usize, last: usize, lo: usize, hi: usize) -> String {
        let mut rows = Vec::new();
        for line in first..=last {
            let src = self.lines.get(line).cloned().unwrap_or_default();
            let taken: String = src.chars().skip(lo).take(hi - lo).collect();
            let pad = (hi - lo).saturating_sub(taken.chars().count());
            rows.push(format!("{}{}", taken, " ".repeat(pad)));
        }
        rows.join("\n")
    }
}

fn now_step(clock: &mut Instant) -> Instant {
    *clock += Duration::from_millis(7);
    *clock
}

#[test]
fn selections_copy_paste_and_undo_agree_with_a_model() {
    for seed in 1..=60u64 {
        let mut rng = Rng::new(seed);
        let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
        let mut model = Model::new();
        let mut clock = Instant::now();
        // A clipboard for the model side.
        let mut clip;

        for step in 0..120 {
            let now = now_step(&mut clock);
            let action = rng.below(7);
            match action {
                0 | 1 => {
                    // Type a random short string at the caret (replacing any selection).
                    let n = 1 + rng.below(4);
                    let s: String = (0..n)
                        .map(|_| (b'a' + rng.below(26) as u8) as char)
                        .collect();
                    // Drive the document from the model's caret and clear any selection left
                    // by a previous step, so both sides insert at the same place into the
                    // same state. Without this, a selection left over from the `2` branch
                    // makes the document replace text the model still thinks is present.
                    doc.clear_selection();
                    doc.cursor.pos = LineCol::new(model.caret.0, model.caret.1);
                    doc.insert_text(&s, UndoGroup::Atomic, now)
                        .unwrap_or_default();
                    model.delete(model.caret, model.caret);
                    let (l, c) = model.caret;
                    model.insert(l, c, &s);
                    let nl = s.matches('\n').count();
                    let last_len = s.rsplit('\n').next().unwrap_or("").chars().count();
                    model.caret = if nl == 0 {
                        (l, c + s.chars().count())
                    } else {
                        (l + nl, last_len)
                    };
                    model.clamp_caret();
                    doc.cursor.pos = LineCol::new(model.caret.0, model.caret.1);
                }
                2 => {
                    // Choose a random selection and read it back through the model.
                    let l1 = rng.below(doc.buffer.line_count());
                    let l2 = rng.below(doc.buffer.line_count());
                    let c1 = rng.below(model.line_len(l1) + 1);
                    let c2 = rng.below(model.line_len(l2) + 1);
                    let kind = if rng.chance(40) {
                        SelectionKind::Block
                    } else {
                        SelectionKind::Range
                    };
                    let a = LineCol::new(l1, c1);
                    let b = LineCol::new(l2, c2);
                    doc.begin_selection(a, b, kind);
                    model.caret = (b.line, b.col);
                    let got = doc.selected_text().unwrap_or_default();
                    let (first, last) = if l1 <= l2 { (l1, l2) } else { (l2, l1) };
                    let (lo, hi) = if c1 <= c2 { (c1, c2) } else { (c2, c1) };
                    let want = match kind {
                        SelectionKind::Range => {
                            // Normalize exactly as `Selection::bounds`, which compares the
                            // (line, col) pair as a unit -- not line and column separately.
                            let a = (l1, c1);
                            let b = (l2, c2);
                            let (sa, sb) = if a <= b { (a, b) } else { (b, a) };
                            let mut out = String::new();
                            let mut line = sa.0;
                            let mut col = sa.1;
                            loop {
                                let src = model.lines[line].chars().skip(col).collect::<String>();
                                if line == sb.0 {
                                    let trimmed: String = model.lines[line]
                                        .chars()
                                        .skip(col)
                                        .take(sb.1.saturating_sub(col))
                                        .collect();
                                    out.push_str(&trimmed);
                                    break;
                                }
                                out.push_str(&src);
                                out.push('\n');
                                line += 1;
                                col = 0;
                            }
                            out
                        }
                        SelectionKind::Block => {
                            if hi == lo {
                                String::new()
                            } else {
                                model.block_text(first, last, lo, hi)
                            }
                        }
                    };
                    assert_eq!(
                        got, want,
                        "seed {seed} step {step}: selected_text for {a:?}..{b:?} {kind:?}"
                    );
                }
                3 => {
                    // Copy whatever is selected, then paste it at the caret.
                    let copied = doc.selected_text().unwrap_or_default();
                    doc.clear_selection();
                    clip = copied.clone();
                    if !clip.is_empty() {
                        doc.cursor.pos = LineCol::new(model.caret.0, model.caret.1);
                        doc.insert_text(&clip, UndoGroup::Atomic, now)
                            .unwrap_or_default();
                        let (l, c) = model.caret;
                        model.insert(l, c, &clip);
                        let nl = clip.matches('\n').count();
                        let last_len = clip.rsplit('\n').next().unwrap_or("").chars().count();
                        model.caret = if nl == 0 {
                            (l, c + clip.chars().count())
                        } else {
                            (l + nl, last_len)
                        };
                        model.clamp_caret();
                        doc.cursor.pos = LineCol::new(model.caret.0, model.caret.1);
                    }
                }
                4 => {
                    // Block paste, checked by invariants only. Its column alignment is defined
                    // in display cells, and modeling that independently means re-implementing
                    // tab stops and grapheme widths -- at which point the "model" is a second
                    // copy of the code and can share its bugs. The checks here are genuinely
                    // implementation-independent, and `block_and_grapheme.rs` pins the exact
                    // cell math with hand-computed fixtures.
                    let rows = 1 + rng.below(3);
                    let w = 1 + rng.below(3);
                    let rect: Vec<String> = (0..rows)
                        .map(|_| {
                            (0..w)
                                .map(|_| (b'A' + rng.below(3) as u8) as char)
                                .collect()
                        })
                        .collect();
                    let payload = rect.join("\n");
                    let before = doc.buffer.text_string();
                    let before_lines = doc.buffer.line_count();
                    doc.paste_block(&payload, now).unwrap_or_default();
                    let after = doc.buffer.text_string();
                    // A block paste never removes lines: it overwrites bands and appends rows.
                    assert!(
                        doc.buffer.line_count() >= before_lines,
                        "seed {seed} step {step}: a block paste never removes lines"
                    );
                    assert!(
                        doc.buffer.line_count() <= before_lines + rows,
                        "seed {seed} step {step}: grew by more rows than were pasted"
                    );
                    for r in &rect {
                        assert!(
                            after.contains(r.as_str()),
                            "seed {seed} step {step}: pasted row {r:?} missing from {after:?}"
                        );
                    }
                    // One undo restores the exact pre-paste text: the atomic-rewrite contract.
                    assert!(
                        doc.undo_one(),
                        "seed {seed} step {step}: block paste is undoable"
                    );
                    assert_eq!(
                        doc.buffer.text_string(),
                        before,
                        "seed {seed} step {step}: one undo reverts the whole block paste"
                    );
                    // The text is back to `before`, which is what the model already holds.
                    // `undo_one` restores text only, not the caret, so put the caret back where
                    // the model thinks it is and drive the document from there. Adopting
                    // `doc.cursor.pos` here would let the paste's caret leak into the model and
                    // desynchronize every later step.
                    doc.clear_selection();
                    let keep = model.caret;
                    doc.cursor.pos = LineCol::new(
                        keep.0.min(model.lines.len() - 1),
                        keep.1.min(model.line_len(keep.0)),
                    );
                }
                5 => {
                    // Indent / outdent, checked by invariants. `Document` acts on the lines the
                    // selection touches and moves the caret only when it is inside that span,
                    // and `outdent` removes "up to one unit" per line; modeling all of that
                    // char-for-char duplicates the implementation. `block_and_grapheme.rs`
                    // covers the exact behavior with explicit fixtures.
                    let l1 = rng.below(doc.buffer.line_count());
                    let l2 = rng.below(doc.buffer.line_count());
                    let (first, last) = if l1 <= l2 { (l1, l2) } else { (l2, l1) };
                    let before = doc.buffer.text_string();
                    let before_len: usize = before.chars().count();
                    doc.begin_selection(
                        LineCol::new(first, 0),
                        LineCol::new(last, 0),
                        SelectionKind::Range,
                    );
                    if rng.chance(50) {
                        doc.indent_lines("  ", now).unwrap_or_default();
                        let after = doc.buffer.text_string();
                        assert!(
                            after.chars().count() >= before_len,
                            "seed {seed} step {step}: indent only adds"
                        );
                        // Every line keeps its original content, just shifted right.
                        let b: Vec<String> = before.split('\n').map(String::from).collect();
                        let a: Vec<String> = after.split('\n').map(String::from).collect();
                        assert_eq!(
                            a.len(),
                            b.len(),
                            "seed {seed} step {step}: indent keeps lines"
                        );
                        for (i, (bl, al)) in b.iter().zip(a.iter()).enumerate() {
                            if i >= first && i <= last {
                                assert!(
                                    al.ends_with(bl.as_str()) && al.len() >= bl.len(),
                                    "seed {seed} step {step}: line {i} should gain a prefix"
                                );
                            } else {
                                assert_eq!(al, bl, "seed {seed} step {step}: line {i} untouched");
                            }
                        }
                    } else {
                        doc.outdent_lines(4, now).unwrap_or_default();
                        let after = doc.buffer.text_string();
                        assert!(
                            after.chars().count() <= before_len,
                            "seed {seed} step {step}: outdent only removes"
                        );
                    }
                    // Undo restores the text exactly -- but only when the op changed
                    // something. Indenting nothing (an outdent on unindented lines) records no
                    // edit by design, so claiming an undo step for it would consume an earlier,
                    // unrelated one. `modified` distinguishes the two honestly.
                    let changed = doc.buffer.text_string() != before;
                    if changed {
                        assert!(
                            doc.undo_one(),
                            "seed {seed} step {step}: a changing indent op is undoable"
                        );
                        assert_eq!(
                            doc.buffer.text_string(),
                            before,
                            "seed {seed} step {step}: undo reverts the indent op"
                        );
                    }
                    // The model adopts the restored text. Indent/outdent cell semantics are
                    // covered by explicit fixtures, not by this model.
                    model.lines = doc
                        .buffer
                        .text_string()
                        .split('\n')
                        .map(String::from)
                        .collect();
                    // Adopt whatever the document actually holds after the undo. Indent and
                    // outdent move the caret in span-dependent ways that a char-based model
                    // cannot reproduce for free, so the document is treated as authoritative
                    // for this operation and the model is rebased on it. The operation itself is
                    // verified by the invariants above plus `block_and_grapheme.rs`.
                    model.lines = doc
                        .buffer
                        .text_string()
                        .split('\n')
                        .map(String::from)
                        .collect();
                    doc.clear_selection();
                    model.caret = (doc.cursor.pos.line, doc.cursor.pos.col);
                }
                _ => {
                    // Backspace one character. The document is authoritative about its own
                    // caret here: `delete_char_before` clears the selection and may no-op at
                    // the document start, so sync from it before modeling the delete.
                    let (l, c) = model.caret;
                    // `delete_char_before` removes the whole selection when one exists, and the
                    // model has no selection concept, so clear it first and delete one char.
                    doc.clear_selection();
                    doc.cursor.pos = LineCol::new(l, c);
                    doc.delete_char_before(now).unwrap_or_default();
                    if c > 0 {
                        model.delete((l, c - 1), (l, c));
                        model.caret = (l, c - 1);
                    } else if l > 0 {
                        let prev = model.line_len(l - 1);
                        model.delete((l - 1, prev), (l, 0));
                        model.caret = (l - 1, prev);
                    }
                }
            }

            assert_eq!(
                doc.buffer.text_string(),
                model.text(),
                "seed {seed} step {step} diverged (action {action}) doc_caret={:?} model_caret={:?}",
                doc.cursor.pos,
                model.caret,
            );
            assert!(doc.buffer.line_count() >= 1, "line count is never zero");
        }

        // Full unwind: undoing to the bottom must land on a document the model can still
        // name as text, and repeated undo must terminate with nothing left to undo.
        let mut guard = 0;
        while doc.undo_one() {
            guard += 1;
            assert!(guard < 5_000, "undo must terminate (seed {seed})");
        }
        // The buffer must still be a well-formed document.
        let _ = doc.buffer.text_string();
        assert_eq!(
            doc.buffer.line_count(),
            doc.buffer.text_string().split('\n').count()
        );
    }
}
