//! Background work: the worker plumbing behind Find in Files and large-file loading
//! (spec §5.1, §6.5, FR-F10; contract §16).
//!
//! # The rule this module exists to enforce
//!
//! Spec §5.1: the UI thread owns all editor state. A worker computes and *sends*; it never
//! touches `App`, never touches a `Document`, never touches a terminal. Everything a worker
//! produces arrives as an owned [`BgMessage`] and is applied by
//! [`App::drain_background`](crate::app::App::drain_background) on the UI thread. There is no
//! shared document and no lock around editor state, because there is nothing to lock.
//!
//! That rule is why the types here are shaped the way they are: requests take owned
//! `PathBuf`/`String`/`SearchOpts` rather than references, and the cancel signal is an
//! `Arc<AtomicBool>` rather than a method on `App`.
//!
//! # Why the channel is `Arc<Mutex<..>>` and not a bare `mpsc` pair
//!
//! `App` is `Clone` — tests fork state with it, and S7/S8 rely on that. A raw
//! `mpsc::Sender`/`Receiver` pair is neither `Clone` nor cheaply shareable, so holding one
//! directly would force `App` to give up `Clone` or wrap the whole worker in an option that
//! silently breaks on fork. Sharing each endpoint behind an `Arc<Mutex<..>>` keeps `App`
//! clonable, and the cost is one uncontended lock per drain. A cloned `App` shares the same
//! worker, which is correct: a forked `App` in a test is a second view of the same session,
//! the same reasoning `App::clone` already applies to the clipboard.
//!
//! # Staleness, not just cancellation
//!
//! Every message carries the `request_id` that produced it. A worker that has been cancelled
//! may still have messages sitting in the channel, and a *previous* search can finish after a
//! new one has started. Matching on `request_id` at drain time is what stops an old search
//! from repopulating a panel the user already cleared; the cancel flag alone cannot do it,
//! because the flag is global to the worker while the results belong to a run.
//!
//! # Drain budget
//!
//! A search over a large tree can produce hundreds of thousands of hits. Draining the channel
//! without a bound means one idle tick can spend seconds appending rows while keyboard input
//! queues behind it, which is the "UI frozen" failure §9 forbids. `drain_background` takes an
//! explicit budget and stops there; the next tick continues. Measurement and the chosen number
//! are in `docs/STATUS.md`.

pub mod find_files;
pub mod loader;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use np_core::file::LoadedFile;

/// A unit of background work's output, and the only thing a worker may send.
///
/// Every variant carries the `request_id` that produced it so the UI can drop stale results.
/// `FifProgress` deliberately does not: progress is a monotone counter, not a result set, and
/// a late progress tick on a fresh panel is harmless (it reads as "still working") while
/// dropping it would stall the live count.
#[derive(Debug, Clone)]
pub enum BgMessage {
    /// A large file finished loading (FR-F10).
    FileLoaded {
        request: usize,
        result: Result<LoadedFile, String>,
    },
    /// Live counters for the running search (FR-FF2).
    FifProgress { scanned: usize, matches: usize },
    /// One hit (FR-FF3).
    FifMatch {
        request: usize,
        path: PathBuf,
        line: usize,
        col: usize,
        text: String,
        match_len: usize,
    },
    /// The search finished on its own. `total` is the hit count.
    FifDone { request: usize, total: usize },
    /// The search stopped because the cancel flag was set.
    FifCancelled { request: usize },
    /// A file was skipped, with the reason for the note line.
    FifSkipped {
        request: usize,
        path: PathBuf,
        reason: String,
    },
    /// A per-file failure worth showing. The search continued.
    FifError {
        request: usize,
        path: PathBuf,
        message: String,
    },
}

impl BgMessage {
    /// The `request_id` this message belongs to.
    ///
    /// `FifProgress` has none on purpose: a progress tick is a counter, not a result, and a
    /// late tick on a fresh panel reads as "still working" rather than as stale data. `None`
    /// tells the drain path "never drop this for staleness".
    pub fn request(&self) -> Option<usize> {
        match self {
            BgMessage::FileLoaded { request, .. }
            | BgMessage::FifMatch { request, .. }
            | BgMessage::FifDone { request, .. }
            | BgMessage::FifCancelled { request, .. }
            | BgMessage::FifSkipped { request, .. }
            | BgMessage::FifError { request, .. } => Some(*request),
            BgMessage::FifProgress { .. } => None,
        }
    }

    /// Whether this message ends a run.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BgMessage::FifDone { .. } | BgMessage::FifCancelled { .. }
        )
    }
}

/// The handle `App` owns. Cheap to clone; all clones talk to the same channel.
#[derive(Clone)]
pub struct BgWorker {
    tx: Arc<Mutex<std::sync::mpsc::Sender<BgMessage>>>,
    rx: Arc<Mutex<std::sync::mpsc::Receiver<BgMessage>>>,
    /// The current cancel flag. Replaced by [`BgWorker::reset_cancel`] when a new run starts,
    /// so a worker still holding the old `Arc` cannot be un-cancelled by the new run's reset.
    cancel: Arc<Mutex<Arc<AtomicBool>>>,
    next_request: Arc<AtomicUsize>,
    /// Messages sent and not yet received. See [`BgWorker::len`].
    pending: Arc<AtomicUsize>,
}

impl Default for BgWorker {
    /// Same as [`BgWorker::new`]. Needed because `mpsc::Sender` has no `Default`, so the
    /// derive cannot supply one for a struct holding a channel.
    fn default() -> BgWorker {
        BgWorker::new()
    }
}

impl std::fmt::Debug for BgWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BgWorker")
            .field("pending", &self.len())
            .finish()
    }
}

impl BgWorker {
    /// A fresh worker with an empty channel.
    pub fn new() -> BgWorker {
        let (tx, rx) = std::sync::mpsc::channel();
        BgWorker {
            tx: Arc::new(Mutex::new(tx)),
            rx: Arc::new(Mutex::new(rx)),
            cancel: Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))),
            next_request: Arc::new(AtomicUsize::new(1)),
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Allocate a request id. Ids are unique for the life of the worker and never zero.
    pub fn next_request_id(&self) -> usize {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    /// The cancel flag a new worker should be handed, in its *unset* state.
    ///
    /// Called at the start of a run. A worker still running from the previous request keeps
    /// the `Arc` it was given, so this cannot accidentally revive a cancelled search, and it
    /// cannot be told to stop by the new run either -- it will finish and have its messages
    /// dropped as stale.
    pub fn begin_request(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut guard) = self.cancel.lock() {
            *guard = Arc::clone(&flag);
        }
        flag
    }

    /// Ask whatever is running to stop (FR-FF2's `Esc`).
    pub fn cancel(&self) {
        if let Ok(guard) = self.cancel.lock() {
            guard.store(true, Ordering::Relaxed);
        }
    }

    /// Whether a cancel has been asked for.
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .lock()
            .map(|g| g.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Send a message. Fails only if the receiving end is gone, which means the `App` was
    /// dropped and the worker should stop.
    pub fn send(&self, msg: BgMessage) -> bool {
        match self.tx.lock() {
            Ok(tx) => {
                if tx.send(msg).is_ok() {
                    self.pending.fetch_add(1, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Take one message if one is waiting, without blocking.
    pub fn try_recv(&self) -> Option<BgMessage> {
        let got = match self.rx.lock() {
            Ok(rx) => rx.try_recv().ok(),
            Err(_) => None,
        };
        if got.is_some() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
        got
    }

    /// How many messages are queued.
    ///
    /// `std::sync::mpsc::Receiver` has no `len` on this toolchain, so the count is maintained
    /// here: `send` increments and `try_recv` decrements. It is an `Arc` counter rather than a
    /// field computed from the queue, so a clone sees the same number, and it is only exact for
    /// messages sent through this handle -- which is every message, since the raw sender is
    /// private.
    pub fn len(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// True when nothing is queued. Kept because `len` exists; clippy asks.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Discard everything queued. Used when a run is superseded so the panel starts clean.
    pub fn clear(&self) {
        let mut drained = 0usize;
        if let Ok(rx) = self.rx.lock() {
            while rx.try_recv().is_ok() {
                drained += 1;
            }
        }
        if drained > 0 {
            self.pending.fetch_sub(drained, Ordering::Relaxed);
        }
    }
}

/// The cancel-check cadence inside one file, in lines.
///
/// Checked every N *hits*, not every line: a file with no hits does not loop per line here, and
/// a file with a hit on every line would make the check free but the loop noisier. 1024 keeps
/// the worst-case cancel latency well under a frame for any realistic hit density while making
/// the atomic load invisible in a profile.
pub const CANCEL_CHECK_HITS: usize = 1_024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_start_at_one_and_never_repeat() {
        let w = BgWorker::new();
        assert_eq!(w.next_request_id(), 1);
        assert_eq!(w.next_request_id(), 2);
        assert_ne!(w.next_request_id(), w.next_request_id());
    }

    #[test]
    fn messages_round_trip_and_len_tracks_the_queue() {
        let w = BgWorker::new();
        assert!(w.is_empty());
        assert!(w.send(BgMessage::FifProgress {
            scanned: 3,
            matches: 1
        }));
        assert_eq!(w.len(), 1);
        assert!(!w.is_empty());
        assert!(w.try_recv().is_some());
        assert!(w.try_recv().is_none(), "drained");
    }

    #[test]
    fn clear_drops_everything_queued() {
        let w = BgWorker::new();
        for i in 0..5 {
            w.send(BgMessage::FifProgress {
                scanned: i,
                matches: 0,
            });
        }
        w.clear();
        assert!(w.is_empty());
    }

    #[test]
    fn cancel_reaches_the_flag_a_worker_was_given() {
        let w = BgWorker::new();
        let flag = w.begin_request();
        assert!(!flag.load(Ordering::Relaxed));
        w.cancel();
        assert!(flag.load(Ordering::Relaxed), "the worker's own Arc sees it");
        assert!(w.is_cancelled());
    }

    #[test]
    fn a_new_request_does_not_revive_an_old_workers_flag() {
        // The staleness rule that matters: cancel run A, start run B, and A's worker must stay
        // cancelled rather than being reset to "keep going" by B.
        let w = BgWorker::new();
        let a = w.begin_request();
        w.cancel();
        assert!(a.load(Ordering::Relaxed));
        let b = w.begin_request();
        assert!(!b.load(Ordering::Relaxed), "B runs");
        assert!(a.load(Ordering::Relaxed), "A stays cancelled");
        assert!(!w.is_cancelled(), "the current request is not cancelled");
    }

    #[test]
    fn a_clone_shares_the_channel_and_the_cancel_state() {
        let w = BgWorker::new();
        let twin = w.clone();
        twin.send(BgMessage::FifProgress {
            scanned: 1,
            matches: 1,
        });
        assert_eq!(w.len(), 1, "same queue");
        let flag = w.begin_request();
        twin.cancel();
        assert!(flag.load(Ordering::Relaxed), "same cancel target");
    }

    #[test]
    fn a_clone_shares_the_request_id_counter() {
        let w = BgWorker::new();
        let twin = w.clone();
        assert_eq!(w.next_request_id(), 1);
        assert_eq!(twin.next_request_id(), 2, "no id collision across clones");
    }

    #[test]
    fn worker_is_debug_and_default() {
        let w = BgWorker::default();
        let text = format!("{w:?}");
        assert!(text.contains("BgWorker"));
        assert!(w.is_empty());
    }
}
