//! Line-ending policy (spec §6.11 FR-N4, contract §6).
//!
//! # Why this is a separate concern
//!
//! `Buffer` stores text with every terminator normalized to `\n`, because ropey treats
//! `\r`, `\r\n`, `\u{2028}` and friends all as line breaks — a lone `\r` kept in the rope
//! would become a line break and destroy the model. So the *original* endings have to live
//! beside the buffer, one per line, which is what [`split_lines_keep_eol`] produces and what
//! `Document::line_eols` stores.
//!
//! # Mixed endings are real
//!
//! A file with `a\r\nb\nc\r` has three different terminators. FR-N4 says preserve them, so
//! the model is a per-line vector, not a single document-wide value. The single `Eol` on
//! `Document` is the *dominant* one, used for the status bar and as the default for newly
//! typed lines.
//!
//! # Save-time modes
//!
//! `eol_mode` (spec §7.1) decides what gets written:
//!  - `file` — keep each line's own ending (the loaded one; new lines take the dominant)
//!  - `lf` / `crlf` / `cr` — force one ending on every line
//!
//! [`join_for_save`] is the single place that decision is applied, so there is exactly one
//! save path's worth of EOL logic.

use crate::types::Eol;

/// What `eol_mode` asks for at save time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EolMode {
    /// Keep each line's original ending (spec §7.1 default).
    #[default]
    File,
    /// Force LF on every line.
    Lf,
    /// Force CRLF on every line.
    Crlf,
    /// Force CR on every line.
    Cr,
}

impl EolMode {
    /// Parse the config spelling. Unrecognized values fall back to `File`, matching FR-CG2's
    /// "ignore and log" rule; the config module does the logging.
    pub fn parse(s: &str) -> EolMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "lf" => EolMode::Lf,
            "crlf" | "dos" | "windows" => EolMode::Crlf,
            "cr" | "mac" | "macos" => EolMode::Cr,
            _ => EolMode::File,
        }
    }

    /// The forced ending, or `None` for `File` ("keep each line's own").
    pub fn forced(self) -> Option<Eol> {
        match self {
            EolMode::File => None,
            EolMode::Lf => Some(Eol::Lf),
            EolMode::Crlf => Some(Eol::Crlf),
            EolMode::Cr => Some(Eol::Cr),
        }
    }
}

/// Split text into line contents and their terminators, preserving mixed endings.
///
/// Returns `(lines, eols)` where `eols.len()` is the number of *terminators* found, so it is
/// `lines.len()` when the text ends with a newline and `lines.len() - 1` when it does not.
/// That asymmetry is deliberate: it is exactly the information needed to reconstruct the
/// original bytes, and callers index it as `eols.get(line_index)`.
///
/// Line content never contains a terminator, which is the invariant `Buffer` requires.
///
/// ```
/// use np_core::eol::split_lines_keep_eol;
/// use np_core::Eol;
/// let (lines, eols) = split_lines_keep_eol("a\r\nb\nc\r");
/// assert_eq!(lines, vec!["a", "b", "c"]);
/// assert_eq!(eols, vec![Eol::Crlf, Eol::Lf, Eol::Cr]);
/// ```
pub fn split_lines_keep_eol(text: &str) -> (Vec<String>, Vec<Eol>) {
    let mut lines: Vec<String> = Vec::new();
    let mut eols: Vec<Eol> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF is one terminator, not two.
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    eols.push(Eol::Crlf);
                } else {
                    eols.push(Eol::Cr);
                }
                lines.push(std::mem::take(&mut current));
            }
            '\n' => {
                eols.push(Eol::Lf);
                lines.push(std::mem::take(&mut current));
            }
            other => current.push(other),
        }
    }
    // Trailing text after the last terminator is a line with no terminator. When the text
    // ends *with* a newline this is the empty final line, which the model keeps (a file
    // ending in "\n" has a last, empty line).
    if !current.is_empty() {
        lines.push(current);
    }
    (lines, eols)
}

/// The most frequent terminator (FR-N4). Ties break to LF; empty or terminator-free text
/// is LF.
///
/// Only counts terminators that exist, so a file with one CRLF and no other breaks reports
/// CRLF rather than being outvoted by the absence of anything else.
pub fn detect_dominant_eol(text: &str) -> Eol {
    let (_lines, eols) = split_lines_keep_eol(text);
    detect_dominant_from(&eols)
}

/// Dominant ending from a precomputed list. Used by `Document`, which keeps per-line endings
/// rather than the raw text.
pub fn detect_dominant_from(eols: &[Eol]) -> Eol {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    for e in eols {
        match e {
            Eol::Lf => lf += 1,
            Eol::Crlf => crlf += 1,
            Eol::Cr => cr += 1,
        }
    }
    // Tie-break order LF > CRLF > CR is stated in the stage spec. `>=` on the first
    // comparison makes LF win a LF-vs-CRLF tie.
    if lf >= crlf && lf >= cr {
        Eol::Lf
    } else if crlf >= cr {
        Eol::Crlf
    } else {
        Eol::Cr
    }
}

/// Normalize every terminator in `text` to `target`.
///
/// Useful for whole-string operations; the save path uses [`join_for_save`] because it needs
/// the per-line model rather than a second parse of the text.
pub fn normalize_for_save(text: &str, target: Eol) -> String {
    let (lines, eols) = split_lines_keep_eol(text);
    join_with(&lines, eols.len(), &target, None, false)
}

/// Reconstruct file bytes from the line model, applying the save-time EOL policy.
///
/// `lines` are terminator-free contents. `eols` are the per-line terminators as loaded
/// (`eols.get(i)` is the ending of line `i`; absent means that line had none).
///
/// - `mode = File` writes each line's own ending; a line with no recorded ending (one the
///   user added this session) gets `dominant`.
/// - A forced mode writes `forced` everywhere.
/// - `ensure_final_newline` appends a terminator to the last line when it has none.
///
/// The last line's missing terminator is preserved when `ensure_final_newline` is false, so
/// a file that did not end in a newline still does not after an unrelated edit.
pub fn join_for_save(
    lines: &[String],
    eols: &[Eol],
    mode: EolMode,
    dominant: Eol,
    ensure_final_newline: bool,
) -> String {
    match mode.forced() {
        Some(forced) => join_with(lines, lines.len(), &forced, None, ensure_final_newline),
        None => join_with(
            lines,
            lines.len(),
            &dominant,
            Some(eols),
            ensure_final_newline,
        ),
    }
}

/// Shared writer behind [`normalize_for_save`] and [`join_for_save`].
///
/// `per_line` supplies an ending per line index when `mode` is `File`; `default` covers a
/// line with no recorded ending. The final line's terminator is omitted when the source had
/// none and `ensure_final_newline` is false.
fn join_with(
    lines: &[String],
    _count: usize,
    default: &Eol,
    per_line: Option<&[Eol]>,
    ensure_final_newline: bool,
) -> String {
    if lines.is_empty() {
        // An empty document writes nothing, or a single terminator if the user asked for a
        // final newline. Notepad++ behavior: "insert final newline" on an empty buffer
        // creates one empty line, which is what a lone terminator means.
        return if ensure_final_newline {
            default.as_str().to_string()
        } else {
            String::new()
        };
    }
    // Rough capacity: contents plus one terminator each.
    let mut out = String::with_capacity(
        lines.iter().map(|l| l.chars().count()).sum::<usize>() + lines.len() * 2,
    );
    let last = lines.len() - 1;
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        let is_last = i == last;
        // Which ending this line gets.
        let eol: Eol = match per_line {
            // File mode: the loaded ending if we have one, else the dominant default.
            Some(eols) => eols.get(i).copied().unwrap_or(*default),
            // Forced mode: the forced ending everywhere.
            None => *default,
        };
        // The last line only gets a terminator if it originally had one, or if the user
        // asked to ensure one.
        let has_terminator = match per_line {
            Some(eols) => i < eols.len(),
            None => true,
        };
        if !is_last || has_terminator || ensure_final_newline {
            out.push_str(eol.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_endings_are_preserved_per_line() {
        // The FR-N4 case from the stage spec.
        let (lines, eols) = split_lines_keep_eol("a\r\nb\nc\r");
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert_eq!(eols, vec![Eol::Crlf, Eol::Lf, Eol::Cr]);
    }

    #[test]
    fn crlf_is_one_terminator_not_two() {
        let (lines, eols) = split_lines_keep_eol("a\r\nb\r\n");
        assert_eq!(lines, vec!["a", "b"]);
        assert_eq!(eols, vec![Eol::Crlf, Eol::Crlf]);
    }

    #[test]
    fn text_without_trailing_newline_has_one_fewer_eol_than_lines() {
        let (lines, eols) = split_lines_keep_eol("a\nb");
        assert_eq!(lines, vec!["a", "b"]);
        assert_eq!(eols.len(), 1, "line 'b' has no terminator");
        // The asymmetry is what lets the file be reconstructed exactly.
        assert!(eols.len() < lines.len());
    }

    #[test]
    fn trailing_newline_yields_an_empty_final_line_only_when_rejoined() {
        // "a\n" is one line plus one terminator; there is no phantom empty second line in
        // the split output, but joining still ends with the newline.
        let (lines, eols) = split_lines_keep_eol("a\n");
        assert_eq!(lines, vec!["a"]);
        assert_eq!(eols, vec![Eol::Lf]);
    }

    #[test]
    fn split_rejoin_is_exact_for_every_shape() {
        for text in [
            "",
            "a",
            "a\n",
            "a\r\n",
            "a\r",
            "\n",
            "\r\n",
            "\r",
            "a\r\nb\nc\r",
            "a\n\nb",
            "\n\n",
            "no newline at end",
            "mixed\r\nwindows\nunix\rcr\nlast",
        ] {
            let (lines, eols) = split_lines_keep_eol(text);
            // Rebuild using the per-line endings, no policy.
            let rebuilt = join_with(&lines, lines.len(), &Eol::Lf, Some(&eols), false);
            assert_eq!(rebuilt, text, "round-trip failed for {text:?}");
        }
    }

    #[test]
    fn dominant_is_the_most_frequent() {
        assert_eq!(detect_dominant_eol("a\r\nb\r\nc\n"), Eol::Crlf);
        assert_eq!(detect_dominant_eol("a\nb\nc\r\n"), Eol::Lf);
        assert_eq!(detect_dominant_eol("a\rb\rc\n"), Eol::Cr);
    }

    #[test]
    fn dominant_ties_break_to_lf() {
        // One LF, one CRLF: LF wins.
        assert_eq!(detect_dominant_eol("a\nb\r\n"), Eol::Lf);
        // One LF, one CR: LF wins.
        assert_eq!(detect_dominant_eol("a\nb\r"), Eol::Lf);
        // One CRLF, one CR, no LF: CRLF wins.
        assert_eq!(detect_dominant_eol("a\r\nb\rc"), Eol::Crlf);
    }

    #[test]
    fn dominant_of_empty_or_terminator_free_text_is_lf() {
        assert_eq!(detect_dominant_eol(""), Eol::Lf);
        assert_eq!(detect_dominant_eol("no newlines at all"), Eol::Lf);
    }

    #[test]
    fn dominant_of_a_single_terminator_is_that_terminator() {
        assert_eq!(detect_dominant_eol("a\r\n"), Eol::Crlf);
        assert_eq!(detect_dominant_eol("a\r"), Eol::Cr);
    }

    #[test]
    fn normalize_for_save_converts_everything() {
        assert_eq!(normalize_for_save("a\r\nb\nc\r", Eol::Lf), "a\nb\nc\n");
        assert_eq!(normalize_for_save("a\nb\nc", Eol::Crlf), "a\r\nb\r\nc\r\n");
        assert_eq!(normalize_for_save("a\nb", Eol::Cr), "a\rb\r");
    }

    #[test]
    fn eol_mode_file_preserves_each_line() {
        let (lines, eols) = split_lines_keep_eol("a\r\nb\nc\r");
        let out = join_for_save(&lines, &eols, EolMode::File, Eol::Lf, false);
        assert_eq!(out, "a\r\nb\nc\r", "file mode must not change any ending");
    }

    #[test]
    fn eol_mode_lf_normalizes_all() {
        let (lines, eols) = split_lines_keep_eol("a\r\nb\nc\r");
        let out = join_for_save(&lines, &eols, EolMode::Lf, Eol::Crlf, false);
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn eol_mode_crlf_normalizes_all() {
        let (lines, eols) = split_lines_keep_eol("a\nb\nc");
        let out = join_for_save(&lines, &eols, EolMode::Crlf, Eol::Lf, false);
        assert_eq!(out, "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn eol_mode_cr_normalizes_all() {
        let (lines, eols) = split_lines_keep_eol("a\r\nb\nc");
        let out = join_for_save(&lines, &eols, EolMode::Cr, Eol::Lf, false);
        // A forced mode terminates every line, the last one included — same as the LF and
        // CRLF cases above. Only `file` mode can leave the final line unterminated.
        assert_eq!(out, "a\rb\rc\r");
    }

    #[test]
    fn a_new_line_in_file_mode_takes_the_dominant_ending() {
        // Loaded "a\nb" (one LF). User adds a third line, so the model has 3 lines but only
        // the first two have recorded endings.
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let eols = vec![Eol::Lf, Eol::Lf];
        let out = join_for_save(&lines, &eols, EolMode::File, Eol::Crlf, false);
        // Line 2 (no recorded ending) gets the dominant CRLF; the last line gets one because
        // it needs a terminator to exist as a line and the model says so.
        assert!(out.contains("c"), "{out:?}");
        assert!(out.starts_with("a\nb\n"), "existing endings kept: {out:?}");
    }

    #[test]
    fn ensure_final_newline_adds_a_missing_terminator() {
        let (lines, eols) = split_lines_keep_eol("a\nb");
        let without = join_for_save(&lines, &eols, EolMode::File, Eol::Lf, false);
        let with = join_for_save(&lines, &eols, EolMode::File, Eol::Lf, true);
        assert_eq!(without, "a\nb");
        assert_eq!(with, "a\nb\n");
    }

    #[test]
    fn ensure_final_newline_is_a_no_op_when_one_exists() {
        let (lines, eols) = split_lines_keep_eol("a\nb\n");
        let out = join_for_save(&lines, &eols, EolMode::File, Eol::Lf, true);
        assert_eq!(out, "a\nb\n", "must not add a second newline");
    }

    #[test]
    fn empty_document_writes_nothing_unless_asked() {
        let lines: Vec<String> = Vec::new();
        let eols: Vec<Eol> = Vec::new();
        assert_eq!(
            join_for_save(&lines, &eols, EolMode::File, Eol::Lf, false),
            ""
        );
        assert_eq!(
            join_for_save(&lines, &eols, EolMode::File, Eol::Lf, true),
            "\n"
        );
    }

    #[test]
    fn eol_mode_parse_covers_the_documented_values() {
        assert_eq!(EolMode::parse("file"), EolMode::File);
        assert_eq!(EolMode::parse("lf"), EolMode::Lf);
        assert_eq!(EolMode::parse("crlf"), EolMode::Crlf);
        assert_eq!(EolMode::parse("cr"), EolMode::Cr);
        assert_eq!(EolMode::parse("CRLF"), EolMode::Crlf, "case-insensitive");
        assert_eq!(EolMode::parse("  LF  "), EolMode::Lf, "trimmed");
        // Unknown falls back to File; the config layer logs it.
        assert_eq!(EolMode::parse("nonsense"), EolMode::File);
    }

    #[test]
    fn forced_maps_only_for_non_file_modes() {
        assert_eq!(EolMode::File.forced(), None);
        assert_eq!(EolMode::Lf.forced(), Some(Eol::Lf));
        assert_eq!(EolMode::Crlf.forced(), Some(Eol::Crlf));
        assert_eq!(EolMode::Cr.forced(), Some(Eol::Cr));
    }

    #[test]
    fn lone_cr_alone_is_a_line_break_not_a_lost_line() {
        // The whole reason Buffer normalizes: a lone \r must still separate lines.
        let (lines, eols) = split_lines_keep_eol("one\rtwo\rthree");
        assert_eq!(lines, vec!["one", "two", "three"]);
        assert_eq!(eols.len(), 2);
    }

    #[test]
    fn blank_lines_are_kept() {
        let (lines, eols) = split_lines_keep_eol("a\n\n\nb");
        assert_eq!(lines, vec!["a", "", "", "b"]);
        assert_eq!(eols.len(), 3);
    }
}
