//! The engine's only file-access point (spec FR-F1..F4, F6..F10).
//!
//! Nothing else in `np-core` touches the filesystem. That is the point of this module: one
//! place owns load, save, permission handling and the external-change stamp, so a later
//! stage cannot accidentally invent a second save path that skips the atomicity or the
//! permission copy.
//!
//! # Save is atomic (FR-F3)
//!
//! Write to a temp file **in the same directory**, copy the original's permission bits, then
//! `rename`. Same directory is not an optimization: `rename` across filesystems fails, and a
//! fallback that copied instead would be non-atomic, leaving a half-written file if the
//! process died mid-save. The temp name is unique per save so concurrent editors on the same
//! directory cannot collide.
//!
//! # Nothing is lost on failure
//!
//! The document is never cleared before the write succeeds, and the original file is not
//! touched until the rename. A failed save leaves both the original bytes and the in-memory
//! document intact, with `modified` still true, so the user can fix the cause and retry.
//! This is verified by a test that saves into a read-only directory.
//!
//! # Streaming (FR-E1)
//!
//! Bytes are written line by line from the buffer. A whole-file `String` would double the
//! memory of a 100 MB document for no benefit, and the spec's memory target (≤ 4× file size)
//! leaves no room for it.
//!
//! # No `unsafe`, and no `libc`
//!
//! Permissions go through `std::fs::Permissions`, which is enough to copy the mode bits we
//! care about on Unix and is the whole story on other platforms.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::buffer::Buffer;
use crate::encoding::{self, DecodedText};
use crate::eol::{self, EolMode};
use crate::error::{CoreError, CoreResult};
use crate::types::{EncodingId, Eol};

/// What a file looked like on disk when we last read or wrote it (FR-F7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub mtime: SystemTime,
    pub size: u64,
}

impl FileStamp {
    /// Read the stamp from the filesystem.
    pub fn of(path: &Path) -> CoreResult<FileStamp> {
        let meta = fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
        let mtime = meta.modified().map_err(|e| CoreError::io(path, e))?;
        Ok(FileStamp {
            mtime,
            size: meta.len(),
        })
    }

    /// True when `other` describes a different file state.
    ///
    /// Both mtime and size are compared: an editor that only watched mtime would miss a
    /// same-second write, and one that only watched size would miss a same-length edit.
    pub fn differs(&self, other: &FileStamp) -> bool {
        self.mtime != other.mtime || self.size != other.size
    }
}

/// Everything learned about a file by reading it.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    pub path: PathBuf,
    /// Decoded text plus what the decoder concluded.
    pub decoded: DecodedText,
    /// Per-line terminators as they appeared in the file (FR-N4). Index `i` is line `i`'s
    /// ending; a shorter vector means the last line had none.
    pub line_eols: Vec<Eol>,
    /// Dominant ending, for the status bar and as the default for new lines.
    pub dominant_eol: Eol,
    /// NUL in the first 8 KiB (FR-N5). The UI asks "open as text?" rather than deciding here.
    pub binary: bool,
    /// The file was not writable by us.
    pub readonly: bool,
    /// Bytes on disk, for the size/mtime cache and for a binary-safe reload.
    pub size: u64,
    pub mtime: SystemTime,
}

/// Save-time toggles from `[editor]` (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveOptions {
    /// Remove trailing whitespace from every line (FR-E13).
    pub trim_trailing_ws: bool,
    /// Make sure the file ends with a newline (FR-E14).
    pub ensure_final_newline: bool,
    /// Keep a `path~` backup of the previous contents (FR-F3).
    pub backup: bool,
}

impl SaveOptions {
    /// Build from the config field names, so the UI does not restate the mapping.
    pub fn from_config(
        trim_trailing_ws: bool,
        ensure_final_newline: bool,
        backup: bool,
    ) -> SaveOptions {
        SaveOptions {
            trim_trailing_ws,
            ensure_final_newline,
            backup,
        }
    }
}

/// What a save produced, so the caller can refresh its stamp (FR-F3).
#[derive(Debug, Clone)]
pub struct SaveInfo {
    pub path: PathBuf,
    pub bytes_written: u64,
    pub mtime: SystemTime,
    /// Where the backup went, when `SaveOptions::backup` was set.
    pub backup_path: Option<PathBuf>,
}

/// Read a file for opening (FR-F1, FR-N1, FR-N5, FR-F9).
///
/// Never panics; every failure is a `CoreError::Io` carrying the path so the UI can name the
/// file. `force` is the CLI `-e` / "Reload as…" override and skips encoding detection.
///
/// A binary file is still returned, with `binary = true`: the decision to open it is the
/// user's (FR-N5 asks for a confirm dialog), so this function reports rather than refuses.
pub fn load(path: &Path, force: Option<EncodingId>) -> CoreResult<LoadedFile> {
    let bytes = fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let meta = fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
    let decoded = encoding::decode(&bytes, force);
    let (_content_lines, line_eols) = eol::split_lines_keep_eol(&decoded.text);
    let dominant_eol = eol::detect_dominant_from(&line_eols);
    Ok(LoadedFile {
        path: path.to_path_buf(),
        // A file with no write bit for us opens read-only (FR-F9).
        readonly: meta.permissions().readonly(),
        binary: encoding::looks_binary(&bytes),
        size: meta.len(),
        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        line_eols,
        dominant_eol,
        decoded,
    })
}

/// Write a buffer to disk atomically (FR-F3).
///
/// `lines`/`line_eols` come from the document so the per-line ending policy can be applied;
/// `buffer` supplies the text. The two are separate arguments because the document owns the
/// EOL model and the buffer owns the text.
///
/// Steps: read the original's permissions → write a unique temp file in the same directory →
/// apply permissions → flush and fsync → optionally back up the original → rename over the
/// target. On any failure before the rename the original is untouched and the temp file is
/// removed.
pub fn save_atomic(
    path: &Path,
    buffer: &Buffer,
    line_eols: &[Eol],
    encoding_id: &EncodingId,
    dominant: Eol,
    mode: EolMode,
    opts: &SaveOptions,
) -> CoreResult<SaveInfo> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Capture the original mode before anything is replaced. A missing file (a new save) gets
    // the process default.
    let original_perms = fs::metadata(path).ok().map(|m| m.permissions());

    let tmp = temp_path(path);

    // UTF-16 and BOM variants cannot be streamed as raw text, because the terminator bytes
    // and the BOM depend on the codec. Those go through an in-memory encode; the UTF-8 path,
    // which is the overwhelming majority and the one with the memory target, streams.
    let plan = WritePlan::new(buffer, line_eols, dominant, mode, opts);
    let result = if needs_whole_encode(encoding_id) {
        save_encoded_whole(
            path,
            dir,
            &tmp,
            buffer,
            encoding_id,
            &plan,
            original_perms.as_ref(),
        )
    } else {
        save_streamed_utf8(
            path,
            dir,
            &tmp,
            buffer,
            encoding_id,
            &plan,
            original_perms.as_ref(),
        )
    };

    match result {
        Ok(info) => Ok(info),
        Err(err) => {
            // Best-effort cleanup. A leftover temp file in the user's directory is untidy but
            // harmless; a failed cleanup must not mask the real error.
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Whether the codec requires buffering the whole document before writing.
fn needs_whole_encode(enc: &EncodingId) -> bool {
    // Plain UTF-8 is written straight from the rope. Everything with a BOM or a multi-byte
    // codec needs the byte-level encode step.
    !matches!(enc, EncodingId::Utf8)
}

/// The number of lines to write, reconciling two different line models.
///
/// `Buffer` is ropey-backed, and ropey counts a trailing terminator as starting a new empty
/// line: `"a\nb\nc\n"` is 4 lines. `split_lines_keep_eol` returns 3 lines with 3
/// terminators, because it does not invent a line after the last break. Writing 4 lines would
/// append a spurious extra terminator, so the eol model wins whenever it agrees with the
/// text: use the buffer's count only when there is genuinely unterminated content after the
/// last recorded terminator.
fn effective_line_count(buffer: &Buffer, line_eols: &[Eol]) -> usize {
    let buffer_lines = buffer.line_count();
    if buffer_lines == 0 {
        return 0;
    }
    // The buffer has one more line than there are terminators exactly when the document ends
    // with a terminator; that trailing empty line is implied by the terminator itself and
    // must not be written as an extra line.
    if buffer_lines == line_eols.len() + 1 {
        // Only treat it as implied when the last buffer line really is empty.
        if buffer.line_string(buffer_lines - 1).is_empty() {
            return line_eols.len().max(1);
        }
    }
    buffer_lines
}

/// The streaming path: UTF-8, line by line, constant memory.
fn save_streamed_utf8(
    path: &Path,
    dir: &Path,
    tmp: &Path,
    buffer: &Buffer,
    encoding_id: &EncodingId,
    plan: &WritePlan<'_>,
    original_perms: Option<&fs::Permissions>,
) -> CoreResult<SaveInfo> {
    // UTF-8 BOM is streamed too: write the three BOM bytes, then the text.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| CoreError::io(tmp, e))?;
    let mut w = BufWriter::new(file);
    if matches!(encoding_id, EncodingId::Utf8Bom) {
        w.write_all(&[0xEF, 0xBB, 0xBF])
            .map_err(|e| CoreError::io(tmp, e))?;
    }

    write_lines(&mut w, buffer, tmp, plan)?;
    // NOTE: the last-line terminator rule here must stay identical to
    // `eol::join_for_save`, which the non-streaming path uses. A forced mode terminates
    // every line including the last; `file` mode ends the last line only if it originally
    // had a terminator (or ensure_final_newline is set).

    w.flush().map_err(|e| CoreError::io(tmp, e))?;
    // fsync before the rename, so a power loss cannot leave a renamed-but-empty file.
    let file = w
        .into_inner()
        .map_err(|e| CoreError::io(tmp, e.into_error()))?;
    file.sync_all().map_err(|e| CoreError::io(tmp, e))?;
    finish_save(path, dir, tmp, file, original_perms, plan.opts)
}

/// Everything the two save paths need to turn a buffer into bytes, assembled once so the
/// streaming and whole-buffer writers cannot drift apart on any of it.
#[derive(Debug, Clone, Copy)]
struct WritePlan<'a> {
    /// Per-line endings; the vector's length is how many lines get a terminator.
    line_eols: &'a [Eol],
    /// The ending a line with no recorded one falls back to.
    dominant: Eol,
    /// LF / CRLF / CR override, or `None` for "keep what each line had".
    mode: EolMode,
    /// Trim trailing whitespace, ensure a final newline.
    opts: &'a SaveOptions,
    /// Lines to write, reconciled between ropey's and the eol vector's line models.
    line_count: usize,
}

impl<'a> WritePlan<'a> {
    fn new(
        buffer: &Buffer,
        line_eols: &'a [Eol],
        dominant: Eol,
        mode: EolMode,
        opts: &'a SaveOptions,
    ) -> WritePlan<'a> {
        WritePlan {
            line_eols,
            dominant,
            mode,
            opts,
            line_count: effective_line_count(buffer, line_eols),
        }
    }
}

/// Write every line's content and terminator into `w`.
fn write_lines<W: Write>(
    w: &mut W,
    buffer: &Buffer,
    tmp: &Path,
    plan: &WritePlan<'_>,
) -> CoreResult<()> {
    let forced = plan.mode.forced();
    let line_count = plan.line_count;
    let last = line_count.saturating_sub(1);

    for i in 0..line_count {
        let mut text = buffer.line_string(i);
        if plan.opts.trim_trailing_ws {
            // Trim only spaces and tabs: a trailing form feed or vertical tab is content.
            let trimmed = text.trim_end_matches([' ', '\t']);
            text.truncate(trimmed.len());
        }
        w.write_all(text.as_bytes())
            .map_err(|e| CoreError::io(tmp, e))?;

        // Which lines get a terminator. Decided from the *buffer* line count and the loaded
        // eol list, never from both at once, or a forced mode double-terminates the last
        // line. Mirrors `eol::join_for_save`:
        //   - any line but the last always has one (otherwise lines would glue together)
        //   - the last line has one if a forced mode is in effect, if it loaded with one, or
        //     if the user asked to ensure one
        let write_eol = if i < last {
            true
        } else {
            forced.is_some() || plan.line_eols.len() > last || plan.opts.ensure_final_newline
        };
        if write_eol {
            let eol =
                forced.unwrap_or_else(|| plan.line_eols.get(i).copied().unwrap_or(plan.dominant));
            w.write_all(eol.as_str().as_bytes())
                .map_err(|e| CoreError::io(tmp, e))?;
        }
    }
    Ok(())
}

/// The non-streaming path for BOM/UTF-16 codecs, which need a byte-level encode.
///
/// This holds the document in memory twice, which is why it is limited to the codecs that
/// genuinely require it. The UTF-8 path streams and never does.
fn save_encoded_whole(
    path: &Path,
    dir: &Path,
    tmp: &Path,
    buffer: &Buffer,
    encoding_id: &EncodingId,
    plan: &WritePlan<'_>,
    original_perms: Option<&fs::Permissions>,
) -> CoreResult<SaveInfo> {
    let lines: Vec<String> = (0..plan.line_count)
        .map(|i| buffer.line_string(i))
        .collect();
    let text = eol::join_for_save(
        &lines,
        plan.line_eols,
        plan.mode,
        plan.dominant,
        plan.opts.ensure_final_newline,
    );
    let text = if plan.opts.trim_trailing_ws {
        trim_trailing(&text)
    } else {
        text
    };
    let bytes = encoding::encode(&text, encoding_id.clone());

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| CoreError::io(tmp, e))?;
    let mut w = BufWriter::new(file);
    w.write_all(&bytes).map_err(|e| CoreError::io(tmp, e))?;
    w.flush().map_err(|e| CoreError::io(tmp, e))?;
    let file = w
        .into_inner()
        .map_err(|e| CoreError::io(tmp, e.into_error()))?;
    file.sync_all().map_err(|e| CoreError::io(tmp, e))?;
    finish_save(path, dir, tmp, file, original_perms, plan.opts)
}

/// Trim trailing spaces and tabs from every line of `text`.
fn trim_trailing(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive(['\n', '\r']) {
        // Keep the terminator, trim the content before it.
        let content_end = line.find(['\n', '\r']).unwrap_or(line.len());
        let (content, terminator) = line.split_at(content_end);
        out.push_str(content.trim_end_matches([' ', '\t']));
        out.push_str(terminator);
    }
    out
}

/// Permission-copy, optional backup, and the atomic rename.
fn finish_save(
    path: &Path,
    _dir: &Path,
    tmp: &Path,
    file: File,
    original_perms: Option<&fs::Permissions>,
    opts: &SaveOptions,
) -> CoreResult<SaveInfo> {
    drop(file);
    // Apply the original mode before the rename, so the file is never briefly world-readable
    // on its way to being 0600.
    if let Some(perm) = original_perms {
        fs::set_permissions(tmp, perm.clone()).map_err(|e| CoreError::io(tmp, e))?;
    }

    let backup_path = if opts.backup && path.exists() {
        let bp = backup_path_for(path);
        // Copy the current file aside. If this fails the save still proceeds: losing a backup
        // is better than losing the save, and the original is still intact at this point.
        if fs::copy(path, &bp).is_ok() {
            Some(bp)
        } else {
            None
        }
    } else {
        None
    };

    fs::rename(tmp, path).map_err(|e| CoreError::io(path, e))?;

    let meta = fs::metadata(path).map_err(|e| CoreError::io(path, e))?;
    Ok(SaveInfo {
        path: path.to_path_buf(),
        bytes_written: meta.len(),
        mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        backup_path,
    })
}

/// A unique temp path in the target's directory.
///
/// Uniqueness matters: two `npt` instances saving the same file, or a save racing with itself
/// after a retry, must not write through one another's temp file. `create_new` in the open
/// call is what actually guarantees exclusivity; the counter and PID make the name unlikely
/// to collide in the first place.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "npt".to_string());
    let pid = std::process::id();
    // A nanosecond timestamp gives per-attempt uniqueness without a global counter.
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut tmp_name = format!(".{name}.npt-tmp-{pid}-{nanos}");
    // Keep the name bounded so a very long file name cannot blow the filesystem's limit.
    if tmp_name.len() > 200 {
        tmp_name = format!(".npt-tmp-{pid}-{nanos}");
    }
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

/// The backup file name for `path` (a `~` suffix, as editors conventionally do).
fn backup_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "npt".to_string());
    name.push('~');
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// How big a file may be before opening it needs special handling (FR-F10, spec §9).
pub const ASK_READONLY_BYTES: u64 = 20 * 1024 * 1024;
/// Above this, the file must load in the background with progress.
pub const BACKGROUND_BYTES: u64 = 100 * 1024 * 1024;

/// What to do about a file of a given size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenDecision {
    /// Open directly, read-write.
    Open,
    /// Ask the user whether to open read-only (20 MB..100 MB).
    AskReadOnly,
    /// Load in the background with progress (>= 100 MB).
    Background,
}

/// The size policy, as a pure function so the boundaries are exactly testable.
///
/// Background loading itself is S11, which owns the worker plumbing; until then the caller
/// loads synchronously regardless of the decision. Keeping the decision separate means S11
/// only has to act on it, not redefine it.
pub fn open_policy(size: u64) -> OpenDecision {
    if size >= BACKGROUND_BYTES {
        OpenDecision::Background
    } else if size >= ASK_READONLY_BYTES {
        OpenDecision::AskReadOnly
    } else {
        OpenDecision::Open
    }
}

/// True when `stamp` no longer describes the file on disk (FR-F7).
///
/// A missing file counts as changed: the UI then offers Save As rather than overwriting a
/// deletion (spec §8).
pub fn changed_on_disk(stamp: &FileStamp, path: &Path) -> bool {
    match FileStamp::of(path) {
        Ok(now) => stamp.differs(&now),
        Err(_) => true,
    }
}

/// True when the file exists at all. Used to distinguish "changed" from "deleted".
pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// Whether the current user can write `path`.
///
/// For an existing file this asks the file; for a new one it asks the directory, because a
/// new file's writability is decided by its parent.
pub fn is_writable(path: &Path) -> bool {
    if path.exists() {
        !path
            .metadata()
            .map(|m| m.permissions().readonly())
            .unwrap_or(true)
    } else {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        dir.metadata()
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn buffer_of(text: &str) -> Buffer {
        Buffer::from_str_lossy(text)
    }

    fn eols_of(text: &str) -> Vec<Eol> {
        eol::split_lines_keep_eol(text).1
    }

    fn opts() -> SaveOptions {
        SaveOptions::default()
    }

    // ------------------------------------------------------------ open_policy

    #[test]
    fn open_policy_boundaries_are_exact() {
        // FR-F10 thresholds, tested at and around each boundary.
        let mb = 1024 * 1024;
        assert_eq!(open_policy(0), OpenDecision::Open);
        assert_eq!(open_policy(19 * mb), OpenDecision::Open);
        assert_eq!(
            open_policy(20 * mb - 1),
            OpenDecision::Open,
            "19.9 MB opens"
        );
        assert_eq!(
            open_policy(20 * mb),
            OpenDecision::AskReadOnly,
            "20 MB asks"
        );
        assert_eq!(open_policy(20 * mb + 1), OpenDecision::AskReadOnly);
        assert_eq!(open_policy(50 * mb), OpenDecision::AskReadOnly);
        assert_eq!(
            open_policy(100 * mb - 1),
            OpenDecision::AskReadOnly,
            "99 MB still asks"
        );
        assert_eq!(
            open_policy(100 * mb),
            OpenDecision::Background,
            "100 MB backgrounds"
        );
        assert_eq!(open_policy(100 * mb + 1), OpenDecision::Background);
        assert_eq!(open_policy(u64::MAX), OpenDecision::Background);
    }

    // ------------------------------------------------------------------ load

    #[test]
    fn load_reads_text_and_detects_eol() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("a.txt");
        fs::write(&path, "one\r\ntwo\r\nthree\r\n").expect("write");
        let loaded = load(&path, None).expect("loads");
        assert_eq!(loaded.decoded.text, "one\r\ntwo\r\nthree\r\n");
        assert_eq!(loaded.dominant_eol, Eol::Crlf);
        assert!(!loaded.binary);
        assert_eq!(loaded.size, 17, "one\\r\\ntwo\\r\\nthree\\r\\n is 17 bytes");
    }

    #[test]
    fn load_preserves_mixed_eols_per_line() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("mixed.txt");
        fs::write(&path, "a\r\nb\nc\r").expect("write");
        let loaded = load(&path, None).expect("loads");
        assert_eq!(loaded.line_eols, vec![Eol::Crlf, Eol::Lf, Eol::Cr]);
    }

    #[test]
    fn load_detects_bom_encodings() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("u16.txt");
        fs::write(&path, encoding::encode("héllo", EncodingId::Utf16Le)).expect("write");
        let loaded = load(&path, None).expect("loads");
        assert_eq!(loaded.decoded.encoding, EncodingId::Utf16Le);
        assert!(loaded.decoded.had_bom);
        assert_eq!(loaded.decoded.text, "héllo");
    }

    #[test]
    fn load_reports_binary_without_refusing() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bin.dat");
        fs::write(&path, [0u8, 1, 2, 3, b'x']).expect("write");
        let loaded = load(&path, None).expect("a binary file still loads");
        assert!(loaded.binary, "the flag lets the UI ask (FR-N5)");
    }

    #[test]
    fn load_missing_file_is_an_io_error_with_the_path() {
        let err = load(Path::new("/no/such/dir/npt-file.txt"), None).expect_err("must fail");
        match &err {
            CoreError::Io { path, .. } => assert_eq!(path, Path::new("/no/such/dir/npt-file.txt")),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn load_sets_readonly_from_permissions() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("ro.txt");
        fs::write(&path, "x").expect("write");
        let mut perm = fs::metadata(&path).expect("meta").permissions();
        perm.set_readonly(true);
        fs::set_permissions(&path, perm).expect("chmod");
        let loaded = load(&path, None).expect("loads");
        assert!(loaded.readonly, "FR-F9: a read-only file opens read-only");
    }

    #[test]
    fn load_honors_forced_encoding() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("cp.txt");
        fs::write(
            &path,
            encoding::encode("Привет", EncodingId::Label("windows-1251")),
        )
        .expect("write");
        let loaded = load(&path, Some(EncodingId::Label("windows-1251"))).expect("loads");
        assert_eq!(loaded.decoded.text, "Привет");
    }

    // ------------------------------------------------------------------- save

    #[test]
    fn save_writes_expected_bytes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("out.txt");
        let buf = buffer_of("hello\nworld");
        let info = save_atomic(
            &path,
            &buf,
            &eols_of("hello\nworld"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&path).expect("read"), "hello\nworld");
        assert_eq!(info.bytes_written, 11);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("out.txt");
        let buf = buffer_of("content");
        save_atomic(
            &path,
            &buf,
            &eols_of("content"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        // Nothing but the target may remain.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path() != path)
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn save_preserves_permissions() {
        // The FR-F3 requirement, tested exactly as the spec describes: chmod 0640, save,
        // assert still 0640.
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("perm.txt");
        fs::write(&path, "original").expect("write");
        set_mode(&path, 0o640);

        let buf = buffer_of("replacement");
        save_atomic(
            &path,
            &buf,
            &eols_of("replacement"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");

        assert_eq!(fs::read_to_string(&path).expect("read"), "replacement");
        assert_eq!(mode_of(&path), 0o640, "save must preserve the mode");
    }

    #[test]
    fn save_preserves_a_restricted_mode() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("secret.txt");
        fs::write(&path, "x").expect("write");
        set_mode(&path, 0o600);
        let buf = buffer_of("y");
        save_atomic(
            &path,
            &buf,
            &eols_of("y"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn save_into_a_readonly_directory_fails_without_touching_the_original() {
        // The "no path exists by which a failed save loses document state" acceptance item.
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("locked.txt");
        fs::write(&path, "original bytes").expect("write");
        set_mode(&path, 0o644);
        // Make the directory unwritable so the temp file cannot be created.
        set_mode(dir.path(), 0o500);

        let buf = buffer_of("new bytes");
        let result = save_atomic(
            &path,
            &buf,
            &eols_of("new bytes"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        );
        // Restore so the tempdir can be cleaned up and so the assertion below can read.
        set_mode(dir.path(), 0o700);

        assert!(result.is_err(), "saving into a read-only dir must fail");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "original bytes",
            "a failed save must leave the original file intact"
        );
    }

    #[test]
    fn save_into_a_vanished_directory_is_an_error() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("gone").join("file.txt");
        let buf = buffer_of("x");
        let result = save_atomic(
            &path,
            &buf,
            &[],
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        );
        assert!(
            result.is_err(),
            "a missing directory must be reported, not swallowed"
        );
    }

    #[test]
    fn save_replaces_the_file_not_appends() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("r.txt");
        fs::write(&path, "a very long original piece of content").expect("write");
        let buf = buffer_of("short");
        save_atomic(
            &path,
            &buf,
            &eols_of("short"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&path).expect("read"), "short");
    }

    // ------------------------------------------------------- EOL and options

    #[test]
    fn save_applies_eol_mode_lf() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("lf.txt");
        let buf = buffer_of("a\r\nb\nc\r");
        save_atomic(
            &path,
            &buf,
            &eols_of("a\r\nb\nc\r"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::Lf,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc\n");
    }

    #[test]
    fn save_applies_eol_mode_crlf() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("crlf.txt");
        let buf = buffer_of("a\nb\nc");
        save_atomic(
            &path,
            &buf,
            &eols_of("a\nb\nc"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::Crlf,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read(&path).expect("read"), b"a\r\nb\r\nc\r\n");
    }

    #[test]
    fn save_in_file_mode_preserves_mixed_endings() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("keep.txt");
        let buf = buffer_of("a\r\nb\nc\r");
        save_atomic(
            &path,
            &buf,
            &eols_of("a\r\nb\nc\r"),
            &EncodingId::Utf8,
            Eol::Crlf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read(&path).expect("read"), b"a\r\nb\nc\r");
    }

    #[test]
    fn trim_trailing_ws_only_when_configured() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let buf = buffer_of("a   \nb\t\t\nc");

        let off = dir.path().join("off.txt");
        save_atomic(
            &off,
            &buf,
            &eols_of("a   \nb\t\t\nc"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&off).expect("read"), "a   \nb\t\t\nc");

        let on = dir.path().join("on.txt");
        let trim = SaveOptions {
            trim_trailing_ws: true,
            ..Default::default()
        };
        save_atomic(
            &on,
            &buf,
            &eols_of("a   \nb\t\t\nc"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &trim,
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&on).expect("read"), "a\nb\nc");
    }

    #[test]
    fn ensure_final_newline_only_when_configured() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let buf = buffer_of("a\nb");

        let off = dir.path().join("off.txt");
        save_atomic(
            &off,
            &buf,
            &eols_of("a\nb"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&off).expect("read"), "a\nb");

        let on = dir.path().join("on.txt");
        let eo = SaveOptions {
            ensure_final_newline: true,
            ..Default::default()
        };
        save_atomic(
            &on,
            &buf,
            &eols_of("a\nb"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &eo,
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&on).expect("read"), "a\nb\n");
    }

    #[test]
    fn backup_keeps_the_previous_contents() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("b.txt");
        fs::write(&path, "first").expect("write");
        let buf = buffer_of("second");
        let bo = SaveOptions {
            backup: true,
            ..Default::default()
        };
        let info = save_atomic(
            &path,
            &buf,
            &eols_of("second"),
            &EncodingId::Utf8,
            Eol::Lf,
            EolMode::File,
            &bo,
        )
        .expect("saves");
        assert_eq!(fs::read_to_string(&path).expect("read"), "second");
        let bp = info.backup_path.expect("backup path reported");
        assert_eq!(fs::read_to_string(&bp).expect("backup readable"), "first");
    }

    // ----------------------------------------------------------- encodings

    #[test]
    fn save_and_reload_round_trips_unicode_encodings() {
        // Unicode codecs hold any scalar, so the accented sample survives all four.
        let dir = tempfile::tempdir().expect("tmpdir");
        for enc in [
            EncodingId::Utf8,
            EncodingId::Utf8Bom,
            EncodingId::Utf16Le,
            EncodingId::Utf16Be,
        ] {
            let path = dir
                .path()
                .join(format!("{}.txt", enc.label().replace('-', "_")));
            let buf = buffer_of("héllo wörld");
            save_atomic(
                &path,
                &buf,
                &eols_of("héllo wörld"),
                &enc,
                Eol::Lf,
                EolMode::File,
                &opts(),
            )
            .expect("saves");
            let loaded = load(&path, None).expect("loads");
            assert_eq!(
                loaded.decoded.text,
                "héllo wörld",
                "{} round-trip",
                enc.label()
            );
            assert!(!loaded.decoded.lossy, "{} reported lossy", enc.label());
        }
    }

    #[test]
    fn save_and_reload_round_trips_single_byte_codecs_in_their_own_scripts() {
        // Each single-byte codec round-trips its own repertoire: ASCII is safe everywhere,
        // and the codec-specific script is safe only for that codec. Saving text outside a
        // codec's repertoire is genuinely lossy and is asserted in encoding.rs.
        let cases = [
            (EncodingId::Label("windows-1252"), "héllo wörld"),
            (EncodingId::Label("ISO-8859-1"), "café naïve"),
            (EncodingId::Label("windows-1251"), "Привет мир"),
        ];
        let dir = tempfile::tempdir().expect("tmpdir");
        for (i, (enc, text)) in cases.iter().enumerate() {
            let path = dir.path().join(format!("sb{i}.txt"));
            let buf = buffer_of(text);
            save_atomic(
                &path,
                &buf,
                &eols_of(text),
                enc,
                Eol::Lf,
                EolMode::File,
                &opts(),
            )
            .expect("saves");
            let loaded = load(&path, Some(enc.clone())).expect("loads");
            assert_eq!(&loaded.decoded.text, text, "{} round-trip", enc.label());
            assert!(!loaded.decoded.lossy, "{} reported lossy", enc.label());
        }
    }

    #[test]
    fn save_utf16le_writes_a_bom_and_two_byte_units() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("u16.txt");
        let buf = buffer_of("hi");
        save_atomic(
            &path,
            &buf,
            &eols_of("hi"),
            &EncodingId::Utf16Le,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        let bytes = fs::read(&path).expect("read");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(&bytes[2..4], &[b'h', 0]);
    }

    #[test]
    fn save_utf8_bom_writes_the_bom() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("bom.txt");
        let buf = buffer_of("x");
        save_atomic(
            &path,
            &buf,
            &eols_of("x"),
            &EncodingId::Utf8Bom,
            Eol::Lf,
            EolMode::File,
            &opts(),
        )
        .expect("saves");
        let bytes = fs::read(&path).expect("read");
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
    }

    // -------------------------------------------------------------- stamps

    #[test]
    fn stamp_detects_mtime_and_size_changes() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("s.txt");
        fs::write(&path, "one").expect("write");
        let before = FileStamp::of(&path).expect("stamp");
        assert!(
            !changed_on_disk(&before, &path),
            "unchanged file is not changed"
        );

        fs::write(&path, "much longer content").expect("write");
        assert!(changed_on_disk(&before, &path), "a size change is detected");
    }

    #[test]
    fn stamp_detects_a_deleted_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("d.txt");
        fs::write(&path, "x").expect("write");
        let stamp = FileStamp::of(&path).expect("stamp");
        fs::remove_file(&path).expect("remove");
        assert!(
            changed_on_disk(&stamp, &path),
            "a deletion counts as changed (spec §8)"
        );
        assert!(!exists(&path));
    }

    #[test]
    fn stamp_differs_compares_both_fields() {
        let a = FileStamp {
            mtime: SystemTime::UNIX_EPOCH,
            size: 1,
        };
        let b = FileStamp {
            mtime: SystemTime::UNIX_EPOCH,
            size: 2,
        };
        assert!(a.differs(&b), "size difference counts");
        let c = FileStamp {
            mtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1),
            size: 1,
        };
        assert!(a.differs(&c), "mtime difference counts");
        assert!(!a.differs(&a));
    }

    // -------------------------------------------------------------- helpers

    #[test]
    fn temp_path_stays_in_the_same_directory() {
        // Cross-filesystem temp would turn rename into a non-atomic copy.
        let p = temp_path(Path::new("/some/dir/file.txt"));
        assert_eq!(p.parent(), Some(Path::new("/some/dir")));
        let name = p.file_name().expect("name").to_string_lossy().into_owned();
        assert!(name.starts_with('.'), "temp file is hidden: {name}");
        assert!(name.len() <= 200);
    }

    #[test]
    fn temp_paths_are_unique_across_calls() {
        // Two saves must not share a temp name, or they would write through each other.
        let a = temp_path(Path::new("/d/f.txt"));
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = temp_path(Path::new("/d/f.txt"));
        assert_ne!(a, b, "temp names must differ between attempts");
    }

    #[test]
    fn backup_path_appends_tilde() {
        assert_eq!(
            backup_path_for(Path::new("/x/notes.txt")),
            PathBuf::from("/x/notes.txt~")
        );
    }

    #[test]
    fn is_writable_reflects_permissions() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("w.txt");
        fs::write(&path, "x").expect("write");
        assert!(is_writable(&path));
        set_mode(&path, 0o444);
        assert!(!is_writable(&path));
    }

    #[test]
    fn is_writable_for_a_new_file_asks_the_directory() {
        let dir = tempfile::tempdir().expect("tmpdir");
        assert!(is_writable(&dir.path().join("new.txt")));
        set_mode(dir.path(), 0o500);
        assert!(!is_writable(&dir.path().join("new.txt")));
    }

    #[test]
    fn trim_trailing_leaves_terminators_alone() {
        assert_eq!(trim_trailing("a  \nb\t\n"), "a\nb\n");
        assert_eq!(trim_trailing("a  \r\nb\t\r\n"), "a\r\nb\r\n");
        assert_eq!(trim_trailing("no trim"), "no trim");
    }

    // Unix permission helpers. cfg-gated so the module stays portable even though the tests
    // only run where modes exist.
    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("meta").permissions().mode() & 0o777
    }

    #[cfg(not(unix))]
    fn set_mode(_path: &Path, _mode: u32) {}

    #[cfg(not(unix))]
    fn mode_of(_path: &Path) -> u32 {
        0o644
    }
}
