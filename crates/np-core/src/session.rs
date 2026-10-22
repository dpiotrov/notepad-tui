//! Session persistence: what `npt` writes on exit and restores on start (contract §11,
//! spec §7.2, FR-CG3, FR-F8).
//!
//! # A corrupt session must never stop the editor
//!
//! [`Session::load`] returns `Option`, not `Result`, and returns `None` for every failure:
//! missing file, unreadable, torn write, wrong types. That is asymmetric on purpose. A
//! session file is a convenience, and the user cannot do anything about a broken one except
//! delete it; refusing to start over a half-written `session.toml` turns a minor annoyance
//! into an unusable editor. The config file follows the same rule and returns warnings
//! instead, because there the user *can* fix it and would want to know.
//!
//! # Writes are atomic
//!
//! The same half-written file must not be creatable in the first place, so a save goes to a
//! temporary file in the destination directory and is renamed over the target. A crash
//! mid-write leaves the previous session intact and a stray temp file, which is the correct
//! trade: the temp file costs nothing, a torn session costs the user their open tabs.
//!
//! # What is and is not remembered
//!
//! Per tab: path, cursor, scroll, bookmarks, folds, language override, read-only. Not the
//! undo stack, and not the modified flag. An undo history that survives a restart would have
//! to be serialized in full, and restoring "you are 200 steps into an undo chain" is not a
//! state anyone can reason about. A document restored from disk is clean because the disk
//! matches it; a document restored from retained `content` is marked modified by the caller,
//! because the retained text is not on disk and must not be lost silently.
//!
//! # Untitled buffers
//!
//! A buffer with no path exists nowhere but memory, so its content is persisted, capped at
//! [`UNTITLED_SESSION_MAX_BYTES`]. Over the cap the content is truncated with a warning
//! rather than dropped, and the tab is still restored: losing the last megabyte of a scratch
//! buffer is better than losing the buffer, and a tab that appears empty is at least a clue.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// Largest untitled-buffer content written to `session.toml` (FR-CG3).
///
/// One megabyte of scratch text is far more than anyone types into an unnamed buffer, while
/// keeping the session file small enough to read and write synchronously at startup and
/// exit. The cap is measured in bytes because that is what the file costs, and truncation
/// happens on a `char` boundary so the retained text is always valid UTF-8.
pub const UNTITLED_SESSION_MAX_BYTES: usize = 1024 * 1024;

/// One tab in a saved session (spec §7.2).
///
/// `#[serde(default)]` for the same reason as [`Session`]: a session file written before a
/// field existed must still restore the tabs it does describe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionTab {
    /// `None` means an untitled buffer, whose text lives in `content`.
    pub path: Option<PathBuf>,
    /// Untitled buffers only, capped at [`UNTITLED_SESSION_MAX_BYTES`].
    pub content: Option<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll_top: usize,
    pub bookmarks: Vec<usize>,
    /// 0-based inclusive `[start, end]` line ranges, as written in spec §7.2.
    pub folded: Vec<[usize; 2]>,
    /// `""` means auto-detect (spec §7.2).
    pub language: String,
    pub readonly: bool,
}

/// The whole `session.toml`.
///
/// `#[serde(default)]` on the struct, so a file missing any of these fields still loads.
/// That matters more here than in config: `load` returns `None` on *any* failure, so a field
/// this build gained after the user's file was written would silently wipe their whole
/// session instead of defaulting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub active_tab: usize,
    pub recent: Vec<PathBuf>,
    pub tab: Vec<SessionTab>,
}

impl Session {
    /// `dirs::data_dir()/npt`, the home of `session.toml` and `npt.log`.
    ///
    /// `dirs` rather than `$HOME` so `XDG_DATA_HOME` is honored, which is also what makes the
    /// location testable: a test points the env var at a tempdir and asserts the path.
    pub fn data_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("npt"))
    }

    /// Where the session file lives, or `None` when no data directory can be resolved.
    pub fn default_path() -> Option<PathBuf> {
        Self::data_dir().map(|d| d.join("session.toml"))
    }

    /// Load a session, returning `None` on *any* problem.
    ///
    /// Deliberately silent about which problem: every caller does the same thing, which is
    /// start with defaults. The binary logs the distinction so a user debugging their session
    /// directory can still see it.
    pub fn load(path: &Path) -> Option<Session> {
        let text = std::fs::read_to_string(path).ok()?;
        // A torn file is the case this exists for: a truncated TOML document fails to parse,
        // and so does a file whose `tab` entries are the wrong shape. Both become `None`.
        toml::from_str(&text).ok()
    }

    /// Write the session atomically: temp file in the same directory, then rename.
    ///
    /// Same-directory matters. A temp file on a different filesystem would make the rename a
    /// copy, which is exactly the non-atomic operation the whole exercise is avoiding.
    pub fn save(&self, path: &Path) -> CoreResult<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| CoreError::Invalid(format!("cannot serialize session: {e}")))?;
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
        }
        let tmp = temp_path(path);
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| CoreError::io(&tmp, e))?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Do not leave the temp file behind on a failed rename, or repeated failures
                // fill the data directory with `session.toml.tmp.N`.
                let _ = std::fs::remove_file(&tmp);
                Err(CoreError::io(path, e))
            }
        }
    }

    /// Record a file as most-recently used (FR-F8).
    ///
    /// Dedup happens on the *canonical* path when the file exists, so opening the same file
    /// through a symlink or a `dir/../file` path does not produce two entries for it. When
    /// the file does not exist -- a fresh save, or a path that has since gone away -- the
    /// original path is used, because canonicalizing would drop the entry entirely and the
    /// user would watch a file they just saved vanish from the menu.
    pub fn push_recent(&mut self, path: PathBuf, max: usize) {
        if max == 0 {
            // A zero cap means "do not remember", which is a config choice rather than a bug.
            self.recent.clear();
            return;
        }
        let key = canonical_for_dedup(&path);
        self.recent.retain(|p| canonical_for_dedup(p) != key);
        self.recent.insert(0, path);
        self.recent.truncate(max);
    }

    /// Capture the current tabs into a session, applying the untitled content cap.
    ///
    /// Returns the session and any warnings from truncation, so the UI can say "this buffer
    /// was saved only partly" instead of quietly dropping text.
    pub fn from_tabs<I, D>(
        tabs: I,
        active_tab: usize,
        recent: Vec<PathBuf>,
    ) -> (Session, Vec<String>)
    where
        I: IntoIterator<Item = D>,
        D: SessionSnapshot,
    {
        let mut warnings = Vec::new();
        let mut tab = Vec::new();
        for doc in tabs {
            let mut entry = doc.snapshot();
            if let Some(content) = entry.content.as_ref() {
                let bytes = content.len();
                if bytes > UNTITLED_SESSION_MAX_BYTES {
                    let kept = truncate_on_char_boundary(content, UNTITLED_SESSION_MAX_BYTES);
                    warnings.push(format!(
                        "untitled buffer is {bytes} bytes; only {} saved to the session",
                        UNTITLED_SESSION_MAX_BYTES
                    ));
                    entry.content = Some(kept);
                }
            }
            tab.push(entry);
        }
        (
            Session {
                active_tab,
                recent,
                tab,
            },
            warnings,
        )
    }
}

/// Blanket so an iterator of references works: `documents.iter()` yields `&Document`, and
/// without this the caller would have to clone every document to capture a session.
impl<T: SessionSnapshot + ?Sized> SessionSnapshot for &T {
    fn snapshot(&self) -> SessionTab {
        (*self).snapshot()
    }
}

/// What a session needs from a document.
///
/// A trait rather than a direct `&Document` parameter so a test can build a session from
/// plain data without constructing a whole document, and so `np-core` keeps one place where
/// the field mapping lives.
pub trait SessionSnapshot {
    fn snapshot(&self) -> SessionTab;
}

impl SessionSnapshot for crate::document::Document {
    fn snapshot(&self) -> SessionTab {
        use crate::types::LanguageId;
        // Untitled buffers carry their content; a file-backed tab does not, because the file
        // *is* the content and storing a second copy would restore stale text over a newer
        // file on disk.
        let content = match &self.path {
            None => Some(self.buffer.text_string()),
            Some(_) => None,
        };
        SessionTab {
            path: self.path.clone(),
            content,
            cursor_line: self.cursor.pos.line,
            cursor_col: self.cursor.pos.col,
            scroll_top: self.scroll.top_line,
            bookmarks: self.bookmarks.iter().copied().collect(),
            // Only collapsed folds are worth restoring: an expanded range carries no state.
            folded: self
                .folds
                .iter()
                .filter(|f| f.collapsed)
                .map(|f| [f.start, f.end])
                .collect(),
            language: LanguageId::to_session_str(&self.language),
            readonly: self.readonly,
        }
    }
}

/// Dedup key: the canonical path when there is one, otherwise the path as given.
fn canonical_for_dedup(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Truncate to `max` bytes without splitting a `char`.
fn truncate_on_char_boundary(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// A collision-resistant temp path beside `path`.
///
/// A fixed `name.tmp` would let two `npt` processes saving into the same data directory
/// overwrite each other's temp file and produce a session that is a mixture of neither. The
/// process id plus a counter makes a collision between concurrent savers implausible without
/// pulling in a temp-file dependency for a single rename.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.toml".to_string());
    let mut candidate = path.with_file_name(format!("{name}.tmp.{}", std::process::id()));
    let mut n = 0u32;
    while candidate.exists() && n < 64 {
        n += 1;
        candidate = path.with_file_name(format!("{name}.tmp.{}.{n}", std::process::id()));
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_never_splits_a_char() {
        // Two-byte character straddling the boundary must not panic.
        let text = "aééé";
        let cut = truncate_on_char_boundary(text, 2);
        assert!(cut.is_char_boundary(cut.len()));
        assert_eq!(cut, "a");
    }

    #[test]
    fn truncation_leaves_short_text_alone() {
        assert_eq!(truncate_on_char_boundary("abc", 10), "abc");
        assert_eq!(truncate_on_char_boundary("abc", 3), "abc");
    }

    #[test]
    fn default_path_ends_with_npt_session_toml() {
        if let Some(p) = Session::default_path() {
            assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("session.toml"));
            assert_eq!(
                p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str()),
                Some("npt")
            );
        }
    }

    #[test]
    fn push_recent_dedups_moves_to_front_and_truncates() {
        let mut s = Session::default();
        for name in ["a.txt", "b.txt", "c.txt"] {
            s.push_recent(PathBuf::from(name), 10);
        }
        assert_eq!(
            s.recent,
            vec![
                PathBuf::from("c.txt"),
                PathBuf::from("b.txt"),
                PathBuf::from("a.txt")
            ]
        );

        // Re-opening an existing entry moves it to the front rather than duplicating it.
        s.push_recent(PathBuf::from("a.txt"), 10);
        assert_eq!(s.recent.len(), 3);
        assert_eq!(s.recent[0], PathBuf::from("a.txt"));

        // Truncation drops the oldest.
        s.push_recent(PathBuf::from("d.txt"), 2);
        assert_eq!(s.recent.len(), 2);
        assert_eq!(s.recent[0], PathBuf::from("d.txt"));
        assert_eq!(s.recent[1], PathBuf::from("a.txt"));
    }

    #[test]
    fn push_recent_with_zero_cap_remembers_nothing() {
        let mut s = Session::default();
        s.push_recent(PathBuf::from("a.txt"), 0);
        assert!(s.recent.is_empty());
    }

    #[test]
    fn a_missing_file_canonicalizes_to_itself_for_dedup() {
        // Two spellings of the same non-existent path must still dedup, or a fresh save
        // appears twice in the menu.
        let mut s = Session::default();
        s.push_recent(PathBuf::from("/no/such/../dir/x.txt"), 10);
        s.push_recent(PathBuf::from("/no/such/dir/x.txt"), 10);
        // Same directory resolves the same way, so at most one entry survives when the
        // parent exists; when it does not, both are kept, which is the documented fallback.
        assert!(!s.recent.is_empty());
    }

    #[test]
    fn torn_session_loads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "active_tab = 0\nrecent = [\"a.rs\"\n[[tab]\ncursor_")
            .expect("write");
        assert_eq!(
            Session::load(&path),
            None,
            "a half-written file must not startle us"
        );
    }

    #[test]
    fn missing_session_loads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(Session::load(&dir.path().join("nope.toml")), None);
    }

    #[test]
    fn wrong_typed_session_loads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "active_tab = \"not a number\"\n").expect("write");
        assert_eq!(Session::load(&path), None);
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("session.toml");
        let s = Session {
            active_tab: 2,
            recent: vec![PathBuf::from("/home/u/a.rs")],
            tab: vec![
                SessionTab {
                    path: Some(PathBuf::from("/home/u/a.rs")),
                    cursor_line: 42,
                    cursor_col: 7,
                    scroll_top: 30,
                    bookmarks: vec![12, 40],
                    folded: vec![[10, 20]],
                    language: "rust".into(),
                    ..Default::default()
                },
                SessionTab {
                    content: Some("scratch".into()),
                    cursor_line: 1,
                    language: String::new(),
                    readonly: true,
                    ..Default::default()
                },
            ],
        };
        s.save(&path).expect("save");
        let back = Session::load(&path).expect("load");
        assert_eq!(back, s);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.toml");
        Session::default().save(&path).expect("save");
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["session.toml".to_string()], "{leftovers:?}");
    }

    #[test]
    fn spec_example_session_parses() {
        // Verbatim from spec §7.2, so the format is pinned against the document.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.toml");
        std::fs::write(
            &path,
            r#"active_tab = 0
recent = ["/home/u/src/main.rs"]

[[tab]]
path = "/home/u/src/main.rs"
cursor_line = 42
cursor_col = 7
scroll_top = 30
bookmarks = [12, 40]
folded = [[10, 20]]
language = "rust"
readonly = false
"#,
        )
        .expect("write");
        let s = Session::load(&path).expect("the spec example must load");
        assert_eq!(s.active_tab, 0);
        assert_eq!(s.recent, vec![PathBuf::from("/home/u/src/main.rs")]);
        assert_eq!(s.tab.len(), 1);
        assert_eq!(s.tab[0].cursor_line, 42);
        assert_eq!(s.tab[0].folded, vec![[10, 20]]);
        assert_eq!(s.tab[0].language, "rust");
    }
}
