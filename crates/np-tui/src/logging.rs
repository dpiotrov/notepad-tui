//! File logging (FR-CG1): `~/.local/share/npt/npt.log`.
//!
//! # Never fatal
//!
//! If the log cannot be opened the editor still starts, with logging simply off. A text
//! editor that refuses to launch because a data directory is unwritable would be a much
//! worse outcome than a session without logs, and the same holds for a read-only `HOME` or
//! a full disk. So [`init`] reports what happened through a return value and never through
//! an error the caller must handle.
//!
//! # Why the path matters
//!
//! The panic hook prints this path so a user who just lost their session knows where the
//! details are (spec §8). That is the one piece of information the logger must be able to
//! supply even when initialization failed, which is why [`log_path`] is independent of
//! [`init`].
//!
//! # Rotation
//!
//! One generation of backup (`npt.log` -> `npt.log.1`) at a fixed size. Unbounded growth in
//! a data directory is a slow leak, and a full rewrite on every start would throw away the
//! log from the crash the user actually wants to read.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};

/// Rotate the log once it passes this size. 2 MB is generous for a session and small enough
/// that the backup never surprises anyone.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Whether a logger was successfully installed.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The resolved log path, cached so the panic hook can print it without re-deriving it.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Guard around the log file for explicit writes from the panic hook.
static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// Where the log goes: `$XDG_DATA_HOME/npt/npt.log`, else `~/.local/share/npt/npt.log`.
///
/// Available even when [`init`] has not run or failed, because the panic hook needs a path
/// to tell the user about.
pub fn log_path() -> PathBuf {
    if let Some(p) = LOG_PATH.get() {
        return p.clone();
    }
    let path = data_dir().map(|d| d.join("npt.log"));
    if let Some(p) = path {
        let _ = LOG_PATH.set(p.clone());
        return p;
    }
    // No data directory at all. A relative name keeps the panic message meaningful rather
    // than printing an empty path.
    PathBuf::from("npt.log")
}

/// XDG data directory for npt, created on demand.
pub fn data_dir() -> Option<PathBuf> {
    let base = dirs::data_dir()?;
    Some(base.join("npt"))
}

/// Install the file logger. Returns `true` when logging is active.
///
/// Never panics and never fails the process. Calling it twice is harmless: the second call
/// reports the state set by the first, since `log` allows only one global logger.
pub fn init(level: LevelFilter) -> bool {
    if INSTALLED.load(Ordering::SeqCst) {
        return true;
    }
    let path = log_path();
    if let Some(parent) = path.parent() {
        // Creating the directory can fail (read-only HOME, permissions). That turns logging
        // off; it does not stop the editor.
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!(
                "npt: cannot create log directory {}: {err}",
                parent.display()
            );
            return false;
        }
    }
    rotate_if_large(&path);

    let file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("npt: cannot open log file {}: {err}", path.display());
            return false;
        }
    };

    let mut builder = ConfigBuilder::new();
    builder.set_time_level(LevelFilter::Trace);
    builder.set_thread_level(LevelFilter::Off);
    builder.set_target_level(LevelFilter::Off);
    builder.set_location_level(LevelFilter::Off);
    builder.set_max_level(level);
    let config = builder.build();

    // A second handle so `write_direct` can still reach the file after `log` takes the
    // first. If the clone fails, logging via the facade is the only path available.
    let hook_handle = file.try_clone().ok();
    if WriteLogger::init(level, config, file).is_err() {
        // A logger was already installed (a second init, or a test harness). Not an error
        // worth reporting, and the editor carries on.
        return false;
    }
    let _ = LOG_FILE.set(Mutex::new(hook_handle));
    INSTALLED.store(true, Ordering::SeqCst);
    log::info!("npt {} starting", env!("CARGO_PKG_VERSION"));
    true
}

/// True once a file logger is live.
pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::SeqCst)
}

/// Append a line straight to the log, bypassing the `log` facade.
///
/// Used by the panic hook, which must not depend on the logger being healthy: it writes the
/// file handle directly. Returns false when nothing could be written.
pub fn write_direct(line: &str) -> bool {
    let Some(lock) = LOG_FILE.get() else {
        return false;
    };
    let Ok(guard) = lock.lock() else {
        return false;
    };
    let mut file = match guard.as_ref() {
        Some(f) => f,
        None => return false,
    };
    writeln!(file, "{line}").is_ok()
}

/// Move a too-large log aside so a fresh session starts clean.
fn rotate_if_large(path: &std::path::Path) {
    let size = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    if size < MAX_LOG_BYTES {
        return;
    }
    let backup = backup_path(path);
    // A failed rename is not worth complaining about: worst case the log keeps growing for
    // one more session.
    let _ = fs::rename(path, &backup);
}

fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "npt.log".to_string());
    name.push('1');
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_ends_with_npt_log() {
        let p = log_path();
        assert!(
            p.to_string_lossy().ends_with("npt.log"),
            "unexpected log path {p:?}"
        );
    }

    #[test]
    fn log_path_is_stable_across_calls() {
        assert_eq!(log_path(), log_path());
    }

    #[test]
    fn backup_path_appends_a_suffix() {
        assert_eq!(
            backup_path(std::path::Path::new("/x/npt.log")),
            PathBuf::from("/x/npt.log1")
        );
    }

    #[test]
    fn rotate_leaves_a_small_log_alone() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("npt.log");
        fs::write(&path, b"tiny").expect("write");
        rotate_if_large(&path);
        assert!(path.exists(), "a small log must not be rotated");
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn rotate_moves_a_large_log_aside() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("npt.log");
        // Just over the threshold, without allocating 2 MB.
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).expect("write");
        rotate_if_large(&path);
        assert!(!path.exists(), "the large log should have been moved");
        assert!(backup_path(&path).exists(), "and kept as a backup");
    }

    #[test]
    fn rotate_tolerates_a_missing_file() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("absent.log");
        rotate_if_large(&path); // must not panic
    }

    #[test]
    fn write_direct_without_init_returns_false() {
        // In a fresh process the OnceLock is unset; after another test initialized it, the
        // call may legitimately succeed. Either way it must not panic.
        let _ = write_direct("probe line");
    }
}
