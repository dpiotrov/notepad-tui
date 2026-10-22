//! Find in Files on a worker thread (spec §6.5, contract §16).
//!
//! The whole job is to run [`np_core::search_files`] off the UI thread and translate its events
//! into [`BgMessage`]s. No editor state is touched here, and none can be: this module has no
//! access to `App`, and `search_files` takes only owned data.
//!
//! # Why the thread is spawned per search rather than pooled
//!
//! A search is a one-shot, cancellable, long-running job, and the cancel semantics are much
//! simpler that way: the thread owns one `Arc<AtomicBool>` for its whole life, so there is no
//! "which request is this job" state inside the worker. A pool would need that state and would
//! still need the same staleness filter at the drain end, buying nothing. The thread exits when
//! the walk ends, so a cancelled search stops consuming CPU at once rather than idling.
//!
//! # Join handles are deliberately dropped
//!
//! Nobody waits for the worker. Waiting would put a blocking join on the UI thread, which is
//! the freeze this whole stage exists to avoid. The worker's last act is to send a terminal
//! message (`FifDone` or `FifCancelled`), so the UI knows it ended without having to join it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use np_core::{FifEvent, FifRequest};

use super::BgMessage;
use super::CANCEL_CHECK_HITS;

/// Spawn a search and return its join handle and request id.
///
/// `cancel` must come from [`BgWorker::begin_request`](super::BgWorker::begin_request) so a
/// later `cancel()` reaches this thread. The caller keeps `request_id` and drops it at drain
/// time to filter out anything this search sends after the user moved on.
pub fn spawn_fif<F>(
    request_id: usize,
    req: FifRequest,
    cancel: Arc<AtomicBool>,
    send: F,
) -> JoinHandle<()>
where
    F: Fn(BgMessage) + Send + 'static,
{
    std::thread::spawn(move || {
        let mut send = send;
        run_fif(request_id, &req, &cancel, &mut send);
    })
}

/// Run a search to completion, sending messages as it goes.
///
/// Split from [`spawn_fif`] so a test can drive the exact same code path on the current thread
/// and assert the message sequence without a real thread or a sleep.
pub fn run_fif<F>(request_id: usize, req: &FifRequest, cancel: &AtomicBool, send: &mut F)
where
    F: FnMut(BgMessage),
{
    let mut hits = 0usize;
    let mut since_send = 0usize;
    // The last file count the walk reported. Progress is sent on a cadence rather than per hit,
    // so a dense file needs the figure from the last `Scanned` event instead of a rescan.
    let mut scanned = 0usize;
    let summary = np_core::search_files(req, cancel, &mut |event| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match event {
            FifEvent::Match(hit) => {
                hits += 1;
                since_send += 1;
                send(BgMessage::FifMatch {
                    request: request_id,
                    path: hit.path.clone(),
                    line: hit.line,
                    col: hit.col,
                    text: hit.text.clone(),
                    match_len: hit.match_len,
                });
                // Progress rides along with hits rather than on a timer, which keeps the live
                // count exact and avoids a second thread. Sent on a cadence so a file with a
                // hit on every line does not emit a progress message per hit -- the panel
                // redraws per drained message, so that is a freeze by a different name.
                if since_send >= CANCEL_CHECK_HITS {
                    since_send = 0;
                    send(BgMessage::FifProgress {
                        scanned,
                        matches: hits,
                    });
                }
            }
            FifEvent::Scanned { scanned: n, .. } => {
                scanned = *n;
                since_send = 0;
                send(BgMessage::FifProgress {
                    scanned: *n,
                    matches: hits,
                });
            }
            FifEvent::Skipped { path, reason } => send(BgMessage::FifSkipped {
                request: request_id,
                path: path.clone(),
                reason: reason.label().to_string(),
            }),
            FifEvent::Error { path, message } => send(BgMessage::FifError {
                request: request_id,
                path: path.clone(),
                message: message.clone(),
            }),
        }
        true
    });

    if summary.cancelled || cancel.load(Ordering::Relaxed) {
        send(BgMessage::FifCancelled {
            request: request_id,
        });
        return;
    }
    if summary.invalid_query {
        // A broken pattern searched nothing. Report it as a zero-hit completion plus the
        // message, so the panel shows the error and returns to idle rather than hanging in
        // "searching".
        for message in &summary.errors {
            send(BgMessage::FifError {
                request: request_id,
                path: PathBuf::new(),
                message: message.clone(),
            });
        }
    }
    send(BgMessage::FifDone {
        request: request_id,
        total: summary.matches,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use np_core::SearchOpts;
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Dir {
            // Exclusive create, retried: a counter restarts each run, so leftovers from an
            // aborted run would collide and pollute the results.
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let base = std::env::temp_dir();
            for _ in 0..1_000 {
                let n = SEQ.fetch_add(1, Ordering::Relaxed);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let p = base.join(format!("npt-bg-{name}-{}-{n}-{nanos}", std::process::id()));
                if fs::create_dir(&p).is_ok() {
                    return Dir(p);
                }
            }
            panic!("could not create a unique tempdir for {name}");
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn req(dir: &PathBuf) -> FifRequest {
        FifRequest::new("needle", dir, SearchOpts::default())
    }

    #[test]
    fn run_fif_emits_progress_matches_and_a_terminal_done() {
        let d = Dir::new("run");
        fs::write(d.0.join("a.txt"), "needle here\n").unwrap();
        fs::write(d.0.join("b.txt"), "two needles\nneedle\n").unwrap();
        let mut got: Vec<BgMessage> = Vec::new();
        let cancel = AtomicBool::new(false);
        run_fif(7, &req(&d.0), &cancel, &mut |m| got.push(m));

        let matches: Vec<&BgMessage> = got
            .iter()
            .filter(|m| matches!(m, BgMessage::FifMatch { .. }))
            .collect();
        assert_eq!(matches.len(), 3, "got {got:?}");
        assert!(got
            .iter()
            .any(|m| matches!(m, BgMessage::FifProgress { .. })));
        match got.last() {
            Some(BgMessage::FifDone { request, total }) => {
                assert_eq!((*request, *total), (7, 3), "FifDone is last");
            }
            other => panic!("expected FifDone last, got {other:?}"),
        }
        for m in &got {
            if let BgMessage::FifMatch { request, .. } = m {
                assert_eq!(*request, 7, "every hit carries the request id");
            }
        }
    }

    #[test]
    fn a_cancelled_run_ends_with_cancelled_not_done() {
        let d = Dir::new("cancel");
        for i in 0..200 {
            fs::write(d.0.join(format!("f{i}.txt")), "needle\n").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut got: Vec<BgMessage> = Vec::new();
        // Cancel after the first hit by flipping the flag from inside the sink.
        let summary = np_core::search_files(&req(&d.0), &cancel, &mut |ev| {
            if matches!(ev, FifEvent::Match(_)) {
                cancel.store(true, Ordering::Relaxed);
            }
            true
        });
        assert!(summary.cancelled);
        // Now the message-level contract.
        let cancel2 = AtomicBool::new(true);
        run_fif(9, &req(&d.0), &cancel2, &mut |m| got.push(m));
        match got.last() {
            Some(BgMessage::FifCancelled { request }) => assert_eq!(*request, 9),
            other => panic!("expected FifCancelled last, got {other:?}"),
        }
        assert!(
            !got.iter().any(|m| matches!(m, BgMessage::FifDone { .. })),
            "a cancelled run must not also claim success"
        );
    }

    #[test]
    fn an_invalid_query_reports_the_error_and_still_terminates() {
        let d = Dir::new("badq");
        fs::write(d.0.join("a.txt"), "x\n").unwrap();
        let bad = FifRequest {
            opts: SearchOpts {
                regex: true,
                ..SearchOpts::default()
            },
            ..FifRequest::new("[bad", &d.0, SearchOpts::default())
        };
        let mut got: Vec<BgMessage> = Vec::new();
        run_fif(3, &bad, &AtomicBool::new(false), &mut |m| got.push(m));
        assert!(got.iter().any(|m| matches!(m, BgMessage::FifError { .. })));
        match got.last() {
            Some(BgMessage::FifDone { request, total }) => {
                assert_eq!((*request, *total), (3, 0));
            }
            other => panic!("expected FifDone last, got {other:?}"),
        }
    }

    #[test]
    fn a_spawned_search_delivers_its_results() {
        let d = Dir::new("spawn");
        fs::write(d.0.join("a.txt"), "needle\n").unwrap();
        let worker = super::super::BgWorker::new();
        let id = worker.next_request_id();
        let cancel = worker.begin_request();
        let tx = worker.clone();
        let handle = spawn_fif(id, req(&d.0), cancel, move |m| {
            tx.send(m);
        });
        let mut got = Vec::new();
        // Join first, then drain: the worker can send between the last poll and the thread
        // actually exiting, so polling alone can miss the terminal message. Joining makes the
        // wait exact and removes the timing dependence entirely.
        let _ = handle.join();
        while let Some(m) = worker.try_recv() {
            got.push(m);
        }
        assert!(
            got.iter().any(|m| matches!(m, BgMessage::FifMatch { .. })),
            "got {got:?}"
        );
        assert!(got.iter().any(|m| matches!(m, BgMessage::FifDone { .. })));
    }

    #[test]
    fn skipped_files_become_messages_with_a_reason() {
        let d = Dir::new("skip");
        fs::write(d.0.join("big.txt"), "").unwrap();
        let f = fs::OpenOptions::new()
            .write(true)
            .open(d.0.join("big.txt"))
            .unwrap();
        f.set_len(np_core::MAX_SEARCH_BYTES + 1).unwrap();
        drop(f);
        let mut got: Vec<BgMessage> = Vec::new();
        run_fif(1, &req(&d.0), &AtomicBool::new(false), &mut |m| got.push(m));
        assert!(got.iter().any(|m| matches!(
            m,
            BgMessage::FifSkipped { reason, .. } if reason.contains("too large")
        )));
    }
}
