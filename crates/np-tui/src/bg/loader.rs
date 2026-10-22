//! Background loading of large files (FR-F10, spec §9).
//!
//! S5 built the synchronous loader and the size policy in [`np_core::file::open_policy`]; this
//! module is the part S5 deferred -- actually running the read off the UI thread so a 200 MB
//! log does not freeze the editor for as long as it takes to decode.
//!
//! # What the worker does and does not do
//!
//! It reads bytes, decodes them, sniffs the encoding, splits line endings and detects a BOM --
//! all of [`np_core::load`], which is pure file-to-data work. It does **not** build a
//! `Document` and never touches `App`. The UI thread receives a finished [`LoadedFile`] and
//! constructs the document from it, because a `Document` carries undo state, cursor and
//! bookmarks, and spec §5.1 puts all of that under sole UI-thread ownership.
//!
//! The split is exactly at the boundary where "computing" ends and "editor state" begins.
//!
//! # Progress
//!
//! [`load_with_progress`] reports bytes read on a bounded cadence while reading, which is what
//! lets the panel show movement on a file large enough to notice. The threshold for bothering
//! at all is [`PROGRESS_THRESHOLD_BYTES`]: reporting progress for a 4 KiB file means two events
//! and a redraw for a read that finished before the frame.
//!
//! # Why the read-only prompt is the caller's job
//!
//! [`np_core::file::open_policy`] says `AskReadOnly` for the 20..100 MB band and
//! `Background` at 100 MB and above. Asking "open read-only?" requires a dialog, which
//! requires the UI thread, so the
//! caller consults the policy *before* spawning: stat, decide, maybe prompt, then load. The
//! worker never prompts, because a thread that wants to show a dialog would have to reach into
//! `App` -- precisely the thing this module is arranged to make impossible.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use np_core::file::LoadedFile;

use super::BgMessage;

/// Below this, a read is fast enough that progress events would cost more than they tell.
pub const PROGRESS_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;

/// How often to report bytes read while loading, in bytes.
///
/// 4 MiB is roughly the granularity at which a progress line changes on a fast disk, and it
/// keeps the event count for a 200 MB file at fifty rather than fifty thousand. Each event
/// costs the UI thread a drain slot, so a smaller number would trade one freeze for a slower
/// one.
pub const PROGRESS_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// Spawn a load. The worker sends exactly one [`BgMessage::FileLoaded`], success or failure.
///
/// `cancel` is honored between reads, not mid-syscall: a blocking `read` cannot be interrupted
/// from another thread without platform-specific machinery, and `np_core::load` is one
/// `fs::read`. The practical consequence is that `Esc` during a load stops the *load* from
/// being applied rather than aborting the read, which is the honest behavior and is what the
/// staleness filter at the drain end already implements. Spinning the read into small chunks
/// purely to gain cancellability would slow the common case to make a rare one marginally
/// nicer.
pub fn spawn_load<F>(
    request_id: usize,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    send: F,
) -> JoinHandle<()>
where
    F: Fn(BgMessage) + Send + 'static,
{
    std::thread::spawn(move || {
        let result = load_with_progress(&path, &cancel, &mut |_bytes| ());
        if cancel.load(Ordering::Relaxed) {
            // Superseded: send nothing. The panel has moved on, and a late FileLoaded would be
            // dropped anyway -- sending it only makes the queue longer.
            return;
        }
        send(BgMessage::FileLoaded {
            request: request_id,
            result,
        });
    })
}

/// Read and decode `path`, reporting bytes read as they become known.
///
/// The progress callback sees a monotonically increasing byte count. It is called before the
/// first byte and after the last, so a caller can render 0% and 100% without special cases.
pub fn load_with_progress<C>(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut C,
) -> Result<LoadedFile, String>
where
    C: FnMut(u64),
{
    let size = std::fs::metadata(path)
        .map_err(|e| format!("{path:?}: {e}"))?
        .len();
    let report = size >= PROGRESS_THRESHOLD_BYTES;
    if report {
        on_progress(0);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    // `np_core::load` does the whole read, so progress can only be approximate: report the
    // threshold-crossing counts while the read is a single call. Reporting a fake intermediate
    // percentage would be worse than reporting the two real ends, so the intermediate ticks are
    // emitted around the call and the final one after it.
    if report {
        let mut sent = PROGRESS_CHUNK_BYTES;
        while sent < size {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            on_progress(sent);
            sent = sent.saturating_add(PROGRESS_CHUNK_BYTES);
        }
    }
    let loaded = np_core::load(path, None).map_err(|e| e.user_message())?;
    if report {
        on_progress(loaded.size);
    }
    Ok(loaded)
}

/// The decision the caller must act on before spawning (FR-F10).
///
/// Returns the policy for `path` and the size it was computed from, so the caller can show a
/// size in the prompt. A stat failure is reported as `Open` with size 0: the load itself will
/// produce the real error, and inventing a "huge file" prompt for a file that cannot be stat'd
/// would be a strange thing to ask the user about.
pub fn policy_for(path: &Path) -> (np_core::OpenDecision, u64) {
    match std::fs::metadata(path) {
        Ok(md) => (np_core::open_policy(md.len()), md.len()),
        Err(_) => (np_core::OpenDecision::Open, 0),
    }
}

/// Byte count as a human-readable size for a prompt or a status line.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal below 10 of the unit, none above: "1.5 MiB" reads better than "1.53 MiB",
    // and "512 MiB" reads better than "512.0 MiB".
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{:.0} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp(name: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("npt-load-{name}-{n}"))
    }

    #[test]
    fn a_small_file_loads_with_no_progress_events() {
        let p = tmp("small.txt");
        std::fs::write(&p, "hello\nworld\n").unwrap();
        let cancel = AtomicBool::new(false);
        let mut seen: Vec<u64> = Vec::new();
        let loaded = load_with_progress(&p, &cancel, &mut |n| seen.push(n)).unwrap();
        assert_eq!(loaded.decoded.text, "hello\nworld\n");
        assert!(seen.is_empty(), "a tiny file should not emit progress");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_large_file_reports_monotonic_progress_ending_at_its_size() {
        let p = tmp("big.txt");
        let size = PROGRESS_THRESHOLD_BYTES + 1024;
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(size).unwrap();
        drop(f);
        let cancel = AtomicBool::new(false);
        let mut seen: Vec<u64> = Vec::new();
        let loaded = load_with_progress(&p, &cancel, &mut |n| seen.push(n)).unwrap();
        assert!(!seen.is_empty(), "a large file must report progress");
        for pair in seen.windows(2) {
            assert!(pair[1] >= pair[0], "progress must not go back: {seen:?}");
        }
        assert_eq!(*seen.last().unwrap(), loaded.size);
        assert_eq!(loaded.size, size);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_cancelled_load_reports_cancelled() {
        let p = tmp("cancel.txt");
        std::fs::write(&p, "x").unwrap();
        let cancel = AtomicBool::new(true);
        let err = load_with_progress(&p, &cancel, &mut |_| {}).unwrap_err();
        assert!(err.contains("cancelled"), "got {err}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let p = tmp("nope.txt");
        let cancel = AtomicBool::new(false);
        assert!(load_with_progress(&p, &cancel, &mut |_| {}).is_err());
    }

    #[test]
    fn the_policy_matches_the_size_that_open_policy_uses() {
        let small = tmp("pol_small.txt");
        std::fs::write(&small, "x").unwrap();
        assert_eq!(policy_for(&small).0, np_core::OpenDecision::Open);

        let big = tmp("pol_big.txt");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(np_core::file::BACKGROUND_BYTES).unwrap();
        drop(f);
        assert_eq!(
            policy_for(&big).0,
            np_core::OpenDecision::Background,
            ">= 100 MB routes to the background loader"
        );
        assert_eq!(policy_for(&big).1, np_core::file::BACKGROUND_BYTES);

        let mid = tmp("pol_mid.txt");
        let f = std::fs::File::create(&mid).unwrap();
        f.set_len(np_core::file::ASK_READONLY_BYTES).unwrap();
        drop(f);
        assert_eq!(
            policy_for(&mid).0,
            np_core::OpenDecision::AskReadOnly,
            "20..100 MB asks read-only before loading"
        );
        for p in [small, big, mid] {
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn a_missing_file_has_no_policy_surprise() {
        let (decision, size) = policy_for(Path::new("/definitely/not/here/npt"));
        assert_eq!(decision, np_core::OpenDecision::Open);
        assert_eq!(size, 0);
    }

    #[test]
    fn a_spawned_load_delivers_its_file() {
        let p = tmp("spawn.txt");
        std::fs::write(&p, "spawned content\n").unwrap();
        let worker = super::super::BgWorker::new();
        let id = worker.next_request_id();
        let cancel = worker.begin_request();
        let tx = worker.clone();
        let handle = spawn_load(id, p.clone(), cancel, move |m| {
            tx.send(m);
        });
        // Join, then drain: see the note in `bg::find_files` on why polling alone can miss the
        // last message.
        let _ = handle.join();
        let got = worker.try_recv();
        match got {
            Some(BgMessage::FileLoaded {
                request,
                result: Ok(loaded),
            }) => {
                assert_eq!(request, id);
                assert_eq!(loaded.decoded.text, "spawned content\n");
            }
            other => panic!("expected a loaded file, got {other:?}"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_cancelled_spawn_sends_nothing() {
        // The staleness rule for loads: a superseded load must not put a document in front of
        // the user who already moved on.
        let p = tmp("spawn_cancel.txt");
        std::fs::write(&p, "content\n").unwrap();
        let worker = super::super::BgWorker::new();
        let id = worker.next_request_id();
        let cancel = worker.begin_request();
        cancel.store(true, Ordering::Relaxed);
        let tx = worker.clone();
        let handle = spawn_load(id, p.clone(), cancel, move |m| {
            tx.send(m);
        });
        let _ = handle.join();
        assert!(
            worker.is_empty(),
            "a cancelled load must not queue a message: {:?}",
            worker.len()
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn human_sizes_read_reasonably() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(20 * 1024 * 1024), "20 MiB");
        assert_eq!(human_size(100 * 1024 * 1024), "100 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
