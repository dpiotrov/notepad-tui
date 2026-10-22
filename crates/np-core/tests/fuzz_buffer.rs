//! Model-based fuzz test for `Buffer` (spec §10, stage S1 mandatory item).
//!
//! Applies a deterministic stream of random inserts/deletes to both a `Buffer` and a
//! reference `Vec<char>`, asserting equality after **every** operation rather than only
//! at the end, so a divergence is caught at the step that caused it.
//!
//! RNG is a small SplitMix64 implemented here on purpose: no dev-dependency, and the
//! seeds are hardcoded so any failure reproduces exactly.

use np_core::{Buffer, LineCol};

/// SplitMix64. Deterministic and dependency-free.
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
}

/// Character alphabet chosen to exercise the tricky paths: ASCII, wide CJK, combining
/// marks, tabs, newlines, and astral-plane emoji.
const ALPHABET: &[char] = &[
    'a',
    'b',
    'c',
    'Z',
    '9',
    '_',
    ' ',
    '=',
    ';',
    '\t',
    '\n',
    'é',
    'ö',
    '漢',
    '字',
    '語',
    '→',
    '\u{0301}', // combining acute
    '\u{1F600}',
    '😀',
    '\u{2028}', // line separator: must normalize, not split
];

/// Reference model: plain text with LF-normalized line endings, exactly the invariant
/// `Buffer` maintains.
#[derive(Debug)]
struct Model {
    text: Vec<char>,
}

impl Model {
    fn new() -> Self {
        Model { text: Vec::new() }
    }

    /// Insert already-normalized text. Callers pass `&[char]`; normalization happens in
    /// `insert_raw`, which is what the fuzz loop uses, so the model and the buffer always
    /// hold the identical LF-only representation and `line_count` cannot disagree.
    fn insert(&mut self, offset: usize, s: &[char]) {
        debug_assert!(
            !s.iter().any(|c| *c == '\r'
                || *c == '\u{0b}'
                || *c == '\u{0c}'
                || *c == '\u{85}'
                || *c == '\u{2028}'
                || *c == '\u{2029}'),
            "model received unnormalized text: {s:?}"
        );
        let at = offset.min(self.text.len());
        self.text.splice(at..at, s.iter().copied());
    }

    /// Normalize then insert, mirroring `Buffer::insert`.
    fn insert_raw(&mut self, offset: usize, s: &[char]) {
        let norm: Vec<char> = normalize(&s.iter().collect::<String>()).chars().collect();
        self.insert(offset, &norm);
    }

    fn remove(&mut self, from: usize, to: usize) {
        let lo = from.min(self.text.len());
        let hi = to.min(self.text.len());
        if lo < hi {
            self.text.drain(lo..hi);
        }
    }

    /// (line, col) for an offset, counting only '\n' as a break.
    fn line_col(&self, offset: usize) -> (usize, usize) {
        let at = offset.min(self.text.len());
        let line = self.text[..at].iter().filter(|c| **c == '\n').count();
        let line_start = self.text[..at]
            .iter()
            .rposition(|c| *c == '\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        (line, at - line_start)
    }

    /// Offset of the start of `line`, clamped.
    fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut seen = 0;
        for (i, c) in self.text.iter().enumerate() {
            if *c == '\n' {
                seen += 1;
                if seen == line {
                    return i + 1;
                }
            }
        }
        self.text.len()
    }

    fn line_count(&self) -> usize {
        self.text.iter().filter(|c| **c == '\n').count() + 1
    }

    fn as_string(&self) -> String {
        // Already LF-only: `insert_raw` normalizes on entry.
        self.text.iter().collect()
    }
}

/// Same normalization the buffer applies: every terminator flavor collapses to '\n'.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push('\n');
            }
            '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            other => out.push(other),
        }
    }
    out
}

/// A position expressed the way a caller would express it: possibly out of range, which
/// `Buffer` must clamp rather than reject.
fn random_linecol(rng: &mut Rng, model: &Model) -> LineCol {
    let line = if rng.chance(10) {
        model.line_count() + rng.below(3) // deliberately out of range
    } else {
        rng.below(model.line_count())
    };
    LineCol::new(line, rng.below(12))
}

impl Model {
    /// Mirror of `Buffer::clamp`: the model must clamp identically or the two diverge
    /// for out-of-range input rather than because of a buffer bug.
    fn clamp(&self, lc: LineCol) -> LineCol {
        let line = lc.line.min(self.line_count() - 1);
        LineCol::new(line, lc.col.min(self.line_len(line)))
    }

    /// Character length of `line` excluding its terminator.
    fn line_len(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }
        let start = self.line_start(line);
        let end = self.text[start..]
            .iter()
            .position(|c| *c == '\n')
            .map(|p| start + p)
            .unwrap_or(self.text.len());
        end - start
    }

    /// Absolute offset of a clamped `LineCol`.
    fn offset_of(&self, lc: LineCol) -> usize {
        let c = self.clamp(lc);
        self.line_start(c.line) + c.col
    }
}

fn run_seed(seed: u64, ops: usize) {
    let mut rng = Rng::new(seed);
    let mut buf = Buffer::new();
    let mut model = Model::new();

    for step in 0..ops {
        // Randomly interleave pure reads so invariant checks fire even without edits.
        match rng.below(10) {
            0..=4 => {
                // Insert 1..4 characters at a random position.
                let n = 1 + rng.below(4);
                let mut chunk: Vec<char> = Vec::with_capacity(n);
                for _ in 0..n {
                    chunk.push(ALPHABET[rng.below(ALPHABET.len())]);
                }
                let lc = random_linecol(&mut rng, &model);
                let want = normalize(&chunk.iter().collect::<String>());

                buf.insert(lc, &chunk.iter().collect::<String>());
                model.insert_raw(model.offset_of(lc), &chunk);

                // Normalization is checked directly: inserting the raw chunk must leave
                // exactly the LF-normalized text in the document (verified by the
                // whole-document equality assertion below, which compares against a
                // normalized model).
                assert!(
                    buf.text_string().contains(&want) || want.is_empty(),
                    "seed {seed} step {step}: normalized insert {want:?} not present"
                );
            }
            5..=8 => {
                // Delete a random range, endpoints in either order.
                let a = random_linecol(&mut rng, &model);
                let b = random_linecol(&mut rng, &model);
                let mut oa = model.offset_of(a);
                let mut ob = model.offset_of(b);
                if oa > ob {
                    std::mem::swap(&mut oa, &mut ob);
                }
                buf.delete_range(a, b);
                model.remove(oa, ob);
            }
            _ => {
                // Invariant probe: line_count must agree.
                assert_eq!(
                    buf.line_count(),
                    model.line_count(),
                    "seed {seed} step {step}: line_count differs"
                );
            }
        }

        // ---- full equality after every single operation ----
        let got = buf.text_string();
        let want = model.as_string();
        assert_eq!(got, want, "seed {seed} diverged after step {step}");

        // ---- coordinate round trip over the whole document ----
        assert_eq!(
            buf.line_count(),
            model.line_count(),
            "seed {seed} step {step}: line_count"
        );
        for offset in 0..=buf.char_count() {
            let pos = buf.line_col(offset);
            assert_eq!(
                buf.char_offset(pos),
                Some(offset),
                "seed {seed} step {step}: round trip at offset {offset} -> {pos:?}"
            );
            let (mline, mcol) = model.line_col(offset);
            assert_eq!(
                (pos.line, pos.col),
                (mline, mcol),
                "seed {seed} step {step}: line_col at offset {offset}"
            );
        }

        // ---- clamp is total and yields addressable positions ----
        for line in [0usize, 1, 5, usize::MAX] {
            for col in [0usize, 3, usize::MAX] {
                let c = buf.clamp(LineCol::new(line, col));
                assert!(
                    c.line < buf.line_count(),
                    "seed {seed} step {step}: bad line"
                );
                assert!(
                    c.col <= buf.line_len_chars(c.line),
                    "seed {seed} step {step}: bad col"
                );
            }
        }
    }
}

#[test]
fn fuzz_seed_1() {
    run_seed(0x5DEECE66D, 1_000);
}

#[test]
fn fuzz_seed_2() {
    run_seed(0xDEADBEEFCAFEBABE, 1_000);
}

#[test]
fn fuzz_seed_3() {
    run_seed(1, 1_000);
}

#[test]
fn fuzz_seed_4_high_width_content() {
    // A seed biased toward wide/combining characters via a different alphabet walk.
    let mut rng = Rng::new(0xC0FFEE);
    let mut buf = Buffer::new();
    let mut model = Model::new();
    for step in 0..1_000 {
        let ch = if rng.chance(60) {
            ['漢', '字', '\u{0301}', '😀', '\u{1F468}', '\t'][rng.below(6)]
        } else {
            ['a', 'b', '\n', ' '][rng.below(4)]
        };
        let lc = random_linecol(&mut rng, &model);
        buf.insert(lc, &ch.to_string());
        model.insert_raw(model.offset_of(lc), &[ch]);
        assert_eq!(
            buf.text_string(),
            model.as_string(),
            "seed 0xC0FFEE diverged at step {step}"
        );
    }
}

#[test]
fn fuzz_delete_heavy() {
    let mut rng = Rng::new(0xFEEDFACE);
    let mut buf = Buffer::from_str_lossy("alpha\nbeta\ngamma\ndelta\n");
    let mut model = Model {
        text: "alpha\nbeta\ngamma\ndelta\n".chars().collect(),
    };
    for step in 0..1_000 {
        let a = random_linecol(&mut rng, &model);
        let b = random_linecol(&mut rng, &model);
        let mut oa = model.offset_of(a);
        let mut ob = model.offset_of(b);
        if oa > ob {
            std::mem::swap(&mut oa, &mut ob);
        }
        buf.delete_range(a, b);
        model.remove(oa, ob);
        assert_eq!(
            buf.text_string(),
            model.as_string(),
            "delete-heavy diverged at step {step}"
        );
        assert!(buf.line_count() >= 1, "line count must never reach 0");
        // Occasionally re-seed content so the document does not stay empty forever.
        if buf.char_count() < 4 {
            let lc = random_linecol(&mut rng, &model);
            buf.insert(lc, "xyz\n");
            model.insert_raw(model.offset_of(lc), &['x', 'y', 'z', '\n']);
        }
    }
}
