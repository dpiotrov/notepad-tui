//! The file picker (FR-U5, FR-F2): path field plus a filtered, directories-first listing.
//!
//! # One directory, never a tree
//!
//! `walkdir` is used to read exactly one directory. FR-F2 asks for a *picker*, and a picker
//! that recurses a large tree on every keystroke of the path field is an I/O stall that
//! looks like a hang. Navigation is `..` and "press Enter on a directory", which is what
//! Notepad++ does and what keeps the listing instant. The `ignore` crate's recursive walk
//! stays where it belongs: Find in Files.
//!
//! # The path field is the source of truth for the directory
//!
//! Typing `/tmp/np/src/main.rs` and pressing Enter must open that file, so the field is
//! never treated as a mere filter over the current listing. [`resolve`] decides what an
//! Enter means: an existing directory navigates into it, an existing file opens, anything
//! else is a Save As target or an error. That single function is where all five behaviors
//! are decided, which is what makes "why did it not open" answerable.
//!
//! # Fuzzy filtering
//!
//! The same `SkimMatcherV2` the language picker uses, applied to the *listing's* names as
//! you type in the path field only when the text after the last separator is not itself a
//! path prefix. That distinction matters: `/tmp/rep` should navigate, not filter, because a
//! user typing an absolute path means the path.

use std::path::{Path, PathBuf};

/// One row of the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name as displayed, including `/` for directories and the literal `..`.
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// The `..` row, which is not a real directory entry and must sort first always.
    pub is_parent: bool,
}

/// Read one directory.
///
/// Directories sort before files, and each group sorts case-insensitively, because a picker
/// that interleaves `src/` between `setup.py` and `README.md` is harder to scan than one
/// where all the folders are together. `show_hidden` controls dotfiles; the `..` row is
/// added whenever there is a parent to go to, hidden or not.
///
/// Errors are not returned: a directory that cannot be read yields an empty listing plus a
/// message, because the picker must stay open and keep the typed path rather than collapsing
/// on a permissions problem.
pub fn list_dir(dir: &Path, show_hidden: bool) -> (Vec<Entry>, Option<String>) {
    let mut entries = Vec::new();
    let mut error = None;
    match std::fs::read_dir(dir) {
        Ok(reader) => {
            for item in reader {
                let Ok(item) = item else { continue };
                let path = item.path();
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                if !show_hidden && name.starts_with('.') {
                    continue;
                }
                let is_dir = path.is_dir();
                entries.push(Entry {
                    name: if is_dir {
                        format!("{name}/")
                    } else {
                        name.clone()
                    },
                    path,
                    is_dir,
                    is_parent: false,
                });
            }
        }
        Err(e) => error = Some(format!("cannot read {}: {e}", dir.display())),
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    // `..` first, always, and only when there is somewhere above to go.
    if let Some(parent) = dir.parent() {
        entries.insert(
            0,
            Entry {
                name: "../".to_string(),
                path: parent.to_path_buf(),
                is_dir: true,
                is_parent: true,
            },
        );
    }
    (entries, error)
}

/// Split a typed path into the directory to list and the text to filter by.
///
/// The rule is the one described in the module docs: text containing a separator is a path,
/// text without one is a filter. `/tmp/nope` lists `/tmp` filtered by `nope`, which shows
/// the user the near-misses instead of an empty box, and pressing Enter still navigates or
/// opens if the path turns out to be real.
///
/// The exception is text that names something that already exists. `/tmp/x/alpha.txt` is a
/// complete file name, and filtering the listing by it would hide the entry the user is
/// looking at while leaving the field pointing at a path that [`resolve`] can open -- two
/// answers to the same question. Existing text lists its directory unfiltered.
pub fn split_query(text: &str) -> (PathBuf, String) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (PathBuf::from("."), String::new());
    }
    // A trailing separator means "list this directory", with no filter.
    if trimmed.ends_with('/') || trimmed.ends_with(std::path::MAIN_SEPARATOR) {
        return (PathBuf::from(trimmed), String::new());
    }
    let path = Path::new(trimmed);
    let (parent, name) = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            (parent.to_path_buf(), name.to_string_lossy().into_owned())
        }
        _ => (PathBuf::from("."), trimmed.to_string()),
    };
    // Text that already names something on disk is a path, not a filter. Without this the
    // last segment is *always* read as a filter, which hides the very entry the user typed
    // (`/tmp/x/alpha.txt` lists `/tmp/x` filtered by `alpha.txt` -- fine -- but the moment
    // the listing is re-read after a navigation the entry can vanish, and a partial name
    // that happens to be an exact existing name filters everything else away).
    if path.exists() {
        return (parent, String::new());
    }
    (parent, name)
}

/// Filter `entries` by `query`, best match first, directories still before files.
///
/// An empty query returns the input untouched, so the sorted listing is what the user sees
/// before they have typed anything.
pub fn filter_entries(entries: &[Entry], query: &str) -> Vec<Entry> {
    if query.trim().is_empty() {
        return entries.to_vec();
    }
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            // `..` scores a free pass. SkimMatcherV2 reads it as two dots and a slash and
            // matches no prose query, which would leave the one row every picker needs
            // unreachable the instant the user types a letter.
            if e.is_parent {
                return Some((i64::MAX, i));
            }
            matcher.fuzzy_match(&e.name, query).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .map(|(_, i)| entries[i].clone())
        .collect()
}

/// What the user meant by pressing Enter on a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Open this existing file.
    OpenFile(PathBuf),
    /// Navigate into this existing directory.
    EnterDir(PathBuf),
    /// Write to this path, which does not exist yet (Save As).
    CreateAt(PathBuf),
    /// The path is not usable; here is why.
    Invalid(String),
}

/// Decide what a path field's contents mean.
///
/// `for_save` changes one thing: a nonexistent path is a valid Save As target rather than
/// an error. Everything else is the same question, and sharing the answer is what keeps
/// Open and Save As from developing different ideas of what a path is.
pub fn resolve(text: &str, for_save: bool) -> Resolved {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Resolved::Invalid("no path given".to_string());
    }
    let path = PathBuf::from(trimmed);
    if path.is_dir() {
        return Resolved::EnterDir(path);
    }
    if path.exists() {
        return Resolved::OpenFile(path);
    }
    if for_save {
        // A Save As target's parent must exist, or the write will fail with a worse message
        // than "that folder is not there".
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() && !parent.exists() => {
                return Resolved::Invalid(format!("no such directory: {}", parent.display()));
            }
            _ => {}
        }
        return Resolved::CreateAt(path);
    }
    Resolved::Invalid(format!("no such file: {trimmed}"))
}

/// The directory a picker should start in (FR-F2).
///
/// Preference order: an explicit CLI directory argument, then the last directory used in
/// this session, then the active document's own directory, then the process cwd. Each step
/// exists because the one before it can be absent -- a headless test has no CLI argument, a
/// first run has no history, and an untitled buffer has no directory.
pub fn start_dir(
    cli_dir: Option<&Path>,
    last: Option<&Path>,
    active_doc: Option<&Path>,
) -> PathBuf {
    if let Some(d) = cli_dir.filter(|d| d.is_dir()) {
        return d.to_path_buf();
    }
    if let Some(d) = last.filter(|d| d.is_dir()) {
        return d.to_path_buf();
    }
    if let Some(d) = active_doc.and_then(|p| p.parent()) {
        if d.is_dir() {
            return d.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The text to put in the path field for a directory.
pub fn dir_text(dir: &Path) -> String {
    let mut s = dir.display().to_string();
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}
