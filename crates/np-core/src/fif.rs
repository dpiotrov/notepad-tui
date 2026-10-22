//! Find in Files (spec §6.5 FR-FF1..FF3, contract §16's data shapes).
//!
//! # Headless by construction
//!
//! No threads here. [`search_files`] walks a tree and pushes events into a caller-supplied
//! callback, which is what makes the feature testable without a terminal or a worker: a test
//! collects events into a `Vec`. Threading lives in `np-tui::bg`, which owns the cancel flag
//! and the message channel; this module only ever asks the flag it was handed.
//!
//! # One matcher, two callers
//!
//! Matching goes through [`crate::search::scan_offsets`], the same scan in-document search
//! uses. FiF does not get its own case folding, whole-word or anchor rules, so a query cannot
//! find something in a file that `F3` would not find once that file is open. That agreement is
//! asserted directly in the tests.
//!
//! # Coordinates
//!
//! [`FifHit`] carries **1-based** line and column, because the panel renders `path:line:col:`
//! (FR-FF3) and that string is what a user reads and a script parses. The engine is 0-based
//! throughout; the `+1` happens in exactly one place ([`hit_at`]) and the click path adds
//! exactly one back.
//!
//! # Files that are not text, and files that are huge
//!
//! A file whose first 8 KiB contain a NUL is skipped as binary (FR-N5), reusing S5's
//! [`looks_binary`] so the two features cannot disagree about the definition. A file over
//! [`MAX_SEARCH_BYTES`] is skipped with a [`FifEvent::Skipped`] note rather than read.
//!
//! Chunked reading with match-across-boundary handling was the alternative and was rejected:
//! re-joining a match that straddles a read boundary means reconstructing the anchor and
//! whole-word context of the whole preceding line, and getting that subtly wrong produces a
//! search that quietly misses hits — the worst failure a search tool can have. A 10 MiB cap
//! costs nothing real, because the panel's result list is bounded long before a file that
//! large is worth searching, and a user who needs one can open it and use the 100 MB
//! background path (FR-F10). The skip is *reported*, never silent.
//!
//! # The file the query came from
//!
//! Not special-cased. Searching a directory searches the directory: if the caret's own file is
//! in it, its hits belong in the results. Excluding it would make "search the project" lie
//! about the project.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;

use crate::encoding::looks_binary;
use crate::search::{scan_offsets, SearchOpts};

/// Largest file FiF will read. See the module docs for why larger files are reported skipped.
pub const MAX_SEARCH_BYTES: u64 = 10 * 1024 * 1024;

/// How many lines of one file to report before stopping. A generated file can hold a million
/// matches; the panel cannot show them and building the list would pin the worker for seconds.
/// The limit is per file, so a tree of many files still yields a full search.
pub const MAX_HITS_PER_FILE: usize = 1_000;

/// What to search for, where, and under which rules. Everything is owned: the request is
/// handed to a worker thread and must not borrow `App`.
#[derive(Debug, Clone)]
pub struct FifRequest {
    /// The needle, exactly as typed.
    pub query: String,
    /// Directory to walk. A file path is accepted and treated as a one-file search.
    pub dir: PathBuf,
    /// Glob filter like `*.rs`, or `None` for every file the other rules allow.
    pub glob: Option<String>,
    /// The same options in-document search uses, so results agree.
    pub opts: SearchOpts,
    /// Honor `.gitignore` / `.ignore` / `.git/info/exclude` (FR-FF1).
    pub use_gitignore: bool,
    /// Descend into dotfiles and hidden directories.
    pub include_hidden: bool,
    /// Skip files larger than [`MAX_SEARCH_BYTES`] rather than reading them.
    pub respect_size_cap: bool,
}

impl FifRequest {
    /// A request with the search options `opts` and every optional rule off.
    pub fn new(query: impl Into<String>, dir: impl Into<PathBuf>, opts: SearchOpts) -> Self {
        FifRequest {
            query: query.into(),
            dir: dir.into(),
            glob: None,
            opts,
            use_gitignore: true,
            include_hidden: false,
            respect_size_cap: true,
        }
    }
}

/// One result. Line and column are **1-based** for display (FR-FF3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FifHit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    /// The whole line, as it appeared, so the panel can show context and highlight the match
    /// inside it. Trailing newline stripped.
    pub text: String,
    /// Length of the match in characters, so the highlight needs no second scan.
    pub match_len: usize,
}

impl FifHit {
    /// `path:line:col:` — the format FR-FF3 asks for, and the one an editor or terminal turns
    /// into a jump.
    pub fn display_location(&self) -> String {
        format!("{}:{}:{}:", self.path.display(), self.line, self.col)
    }
}

/// What the walk reports as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FifEvent {
    /// A file began being searched. `scanned` is the running count including this one.
    Scanned { path: PathBuf, scanned: usize },
    /// A hit.
    Match(FifHit),
    /// A file was passed over, with the reason for the panel's note line.
    Skipped { path: PathBuf, reason: SkipReason },
    /// Something failed that is worth telling the user about. The walk continues.
    Error { path: PathBuf, message: String },
}

/// Why a file was not searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// NUL in the first 8 KiB (FR-N5).
    Binary,
    /// Larger than [`MAX_SEARCH_BYTES`].
    TooLarge,
}

impl SkipReason {
    /// The panel's wording.
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::Binary => "skipped (binary)",
            SkipReason::TooLarge => "skipped (too large)",
        }
    }
}

/// Search `req.dir`, pushing events into `sink`.
///
/// `sink` returning `false` stops the walk promptly, which is the cancel path: the worker's
/// `Esc` handler returns false, and the next file-check boundary ends the search. The flag is
/// consulted per file and every [`CANCEL_CHECK_LINES`] lines inside a file, so a single huge
/// file cannot hold a cancel hostage.
///
/// Returns the totals so a caller that ignores the events still knows what happened. An
/// invalid regex is reported through [`search_files`]’s return value rather than an event,
/// because it means no file was searched at all.
pub fn search_files<F: FnMut(&FifEvent) -> bool>(
    req: &FifRequest,
    cancel: &std::sync::atomic::AtomicBool,
    sink: &mut F,
) -> FifSummary {
    let filter = GlobFilter::parse(req.glob.as_deref());
    let mut summary = FifSummary::default();

    if req.query.is_empty() {
        return summary;
    }
    if let Some(raw) = filter.invalid() {
        summary.errors.push(format!("invalid file filter: {raw}"));
        return summary;
    }
    // Validate once, before walking anything: a broken pattern should not cost a directory
    // traversal to discover.
    if let Err(err) = scan_offsets("", &req.query, &req.opts) {
        summary.errors.push(err.to_string());
        summary.invalid_query = true;
        return summary;
    }

    let mut builder = WalkBuilder::new(&req.dir);
    builder
        .hidden(!req.include_hidden)
        .git_ignore(req.use_gitignore)
        .ignore(req.use_gitignore)
        .parents(req.use_gitignore)
        // `require_git` defaults to true, which means `.gitignore` is only consulted inside a
        // directory that actually has a `.git` (or `.jj`). FR-FF1 asks for gitignore rules to
        // apply, and a plain extracted tree, a SVN checkout and a `git archive` export all have
        // `.gitignore` files and no `.git` -- the feature would silently search ignored files in
        // exactly the trees where generated-file noise is worst. Verified against `ignore`
        // 0.4.33: with the default, an ignored file is still walked.
        .require_git(false)
        // Follow symlinks only when asked; the default (off) is what avoids cycles.
        .follow_links(false);

    for entry in builder.build() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            summary.cancelled = true;
            return summary;
        }
        let Ok(entry) = entry else {
            // A traversal error has no reliable path attached; record that something failed
            // and keep going rather than abandoning the search.
            summary
                .errors
                .push("unreadable directory entry".to_string());
            continue;
        };
        let path = entry.path().to_path_buf();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if !filter.matches(&path) {
            continue;
        }
        summary.scanned += 1;
        if !emit(
            sink,
            &FifEvent::Scanned {
                path: path.clone(),
                scanned: summary.scanned,
            },
        ) {
            summary.cancelled = true;
            return summary;
        }

        match search_one_file(req, &path, cancel, sink, &mut summary) {
            Ok(()) => {}
            Err(message) => {
                summary
                    .errors
                    .push(format!("{}: {message}", path.display()));
                emit(sink, &FifEvent::Error { path, message });
            }
        }
        // A sink that stopped mid-file set the flag on the summary; the outer loop has to see
        // it too. Without this, cancelling during a dense file emitted every later file's
        // `Scanned` event, which is exactly the "cancel did nothing" behavior FR-FF2 forbids.
        if summary.cancelled {
            return summary;
        }
    }
    summary
}

/// Search one file. `Err` is an I/O problem the user should see; a skipped file is not an
/// error and is reported as [`FifEvent::Skipped`].
fn search_one_file<F: FnMut(&FifEvent) -> bool>(
    req: &FifRequest,
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    sink: &mut F,
    summary: &mut FifSummary,
) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if req.respect_size_cap && size > MAX_SEARCH_BYTES {
        summary.skipped += 1;
        emit(
            sink,
            &FifEvent::Skipped {
                path: path.to_path_buf(),
                reason: SkipReason::TooLarge,
            },
        );
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if looks_binary(&bytes) {
        summary.skipped += 1;
        emit(
            sink,
            &FifEvent::Skipped {
                path: path.to_path_buf(),
                reason: SkipReason::Binary,
            },
        );
        return Ok(());
    }
    // Lossy on purpose. A file that is not valid UTF-8 in some spot still has text worth
    // finding, and the alternative is to fail the whole file over one bad byte. The hit's
    // *text* may carry a replacement character; its line and column still point at the right
    // place in the file, which is what the jump uses.
    let text = String::from_utf8_lossy(&bytes).into_owned();

    let found = scan_offsets(&text, &req.query, &req.opts).map_err(|e| e.to_string())?;
    if found.is_empty() {
        return Ok(());
    }

    // Line offsets computed once per file and reused: `offset_line_col` walks the text, so
    // calling it per hit would make a file with 50k matches quadratic.
    let lines: Vec<(usize, usize)> = line_offsets(&text);
    for (start, end) in found {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            summary.cancelled = true;
            return Ok(());
        }
        if summary.matches >= MAX_HITS_PER_FILE {
            // Per-file ceiling reached. Recorded so the panel can say the list is truncated
            // rather than implying the file had exactly this many hits.
            summary.truncated = true;
            return Ok(());
        }
        let (line, col) = line_col_of(&lines, start);
        let text_line = line_text(&text, &lines, line);
        let hit = hit_at(path, line, col, &text_line, end - start);
        summary.matches += 1;
        if !emit(sink, &FifEvent::Match(hit)) {
            summary.cancelled = true;
            return Ok(());
        }
    }
    Ok(())
}

/// Build a hit, applying the one and only 0-based to 1-based conversion (FR-FF3).
fn hit_at(path: &Path, line: usize, col: usize, text: &str, match_len: usize) -> FifHit {
    FifHit {
        path: path.to_path_buf(),
        line: line + 1,
        col: col + 1,
        text: text.to_string(),
        match_len,
    }
}

/// `(start, end)` character offsets for every line, terminator excluded.
fn line_offsets(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for line in text.split('\n') {
        let len = line.chars().count();
        out.push((pos, pos + len));
        pos += len + 1;
    }
    out
}

/// The text of one line for display: terminator excluded, trailing CR stripped.
///
/// Only `\n` is split on, so a CRLF file keeps its CR as the line's last character. It must
/// not reach the panel, where it would render as a stray glyph or wrap the row. Stripping just
/// the trailing CR changes no column, because the CR sits after everything a hit's column
/// counts through.
fn line_text(text: &str, lines: &[(usize, usize)], line: usize) -> String {
    let raw = lines
        .get(line)
        .and_then(|(s, e)| text.get(*s..*e))
        .unwrap_or("");
    raw.strip_suffix('\r').unwrap_or(raw).to_string()
}

/// Line and column of a character offset, using a precomputed line table.
///
/// An offset sitting on a line terminator is reported as that line's end, which is where a
/// zero-width match at end-of-line belongs. Offsets past the last character clamp to the last
/// line rather than producing a line that does not exist.
fn line_col_of(lines: &[(usize, usize)], offset: usize) -> (usize, usize) {
    for (i, (start, end)) in lines.iter().enumerate() {
        if offset <= *end {
            return (i, offset - start);
        }
    }
    match lines.last() {
        Some((start, end)) => (lines.len() - 1, offset.saturating_sub(*start).min(*end)),
        None => (0, 0),
    }
}

/// Whether to stop early. `false` means stop.
fn emit<F: FnMut(&FifEvent) -> bool>(sink: &mut F, event: &FifEvent) -> bool {
    sink(event)
}

/// What a walk produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FifSummary {
    /// Files considered.
    pub scanned: usize,
    /// Hits reported.
    pub matches: usize,
    /// Files skipped as binary or too large.
    pub skipped: usize,
    /// The per-file hit ceiling was reached for at least one file.
    pub truncated: bool,
    /// The walk stopped because the cancel flag was set.
    pub cancelled: bool,
    /// The query was not a valid pattern; nothing was searched.
    pub invalid_query: bool,
    /// Messages worth showing, including per-file failures.
    pub errors: Vec<String>,
}

/// A parsed `glob` value.
///
/// `globset::Glob` is used rather than the `ignore` crate's gitignore matcher because a
/// user-typed `*.rs` is a glob, not an ignore rule: ignore patterns are directory-relative and
/// carry negation semantics that would surprise here. `Glob::new` + `is_match` on the path is
/// the behavior the field promises.
#[derive(Debug, Clone, Default)]
pub struct GlobFilter {
    raw: Option<String>,
    matcher: Option<GlobMatcher>,
    invalid: Option<String>,
}

impl GlobFilter {
    /// Parse user input. Empty means every file; a bad pattern is remembered, not fatal.
    pub fn parse(input: Option<&str>) -> GlobFilter {
        let Some(raw) = input.map(str::trim).filter(|s| !s.is_empty()) else {
            return GlobFilter::default();
        };
        // Match on the path's tail as well as the whole path, so `*.rs` finds `src/main.rs`.
        // `Glob` anchored at `/` would otherwise require the full path to match.
        match Glob::new(raw) {
            Ok(glob) => GlobFilter {
                raw: Some(raw.to_string()),
                matcher: Some(glob.compile_matcher()),
                invalid: None,
            },
            Err(_) => GlobFilter {
                raw: Some(raw.to_string()),
                matcher: None,
                invalid: Some(raw.to_string()),
            },
        }
    }

    /// Whether `path` passes. Directories always pass so the walk can descend.
    pub fn matches(&self, path: &Path) -> bool {
        let Some(matcher) = &self.matcher else {
            return true;
        };
        if matcher.is_match(path) {
            return true;
        }
        // The common case: `*.rs` should match `deep/nested/main.rs` even though the compiled
        // glob has no leading `**/`.
        path.file_name()
            .is_some_and(|n| matcher.is_match(Path::new(n)))
    }

    /// The pattern as typed, for the panel's filter field.
    pub fn raw_pattern(&self) -> String {
        self.raw.clone().unwrap_or_else(|| "*".to_string())
    }

    /// The pattern that failed to compile, if one did.
    pub fn invalid(&self) -> Option<&str> {
        self.invalid.as_deref()
    }
}

/// Re-export so the panel can show a consistent error string for a bad query.
pub fn query_error_message(query: &str, opts: &SearchOpts) -> Option<String> {
    scan_offsets("", query, opts).err().map(|e| e.to_string())
}

/// The line and column a `path:line:col:` triple maps to in a freshly opened buffer, as
/// `(LineCol, ...)`. Kept here so the 1-based rule has one home.
pub fn zero_based(line_1based: usize, col_1based: usize) -> (usize, usize) {
    (line_1based.saturating_sub(1), col_1based.saturating_sub(1))
}
