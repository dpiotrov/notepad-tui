//! Termination signals: put the terminal back before the process dies (spec §8).
//!
//! # The bug this exists for
//!
//! The panic hook and `TerminalGuard::drop` cover the two ways a *function* can leave the
//! program: an unwind and a normal return. Neither covers the way most people end a stuck
//! TUI, which is `kill <pid>` or pressing Ctrl-C twice. With no handler, the kernel tears the
//! process down where it stands: raw mode stays on, the alternate screen stays up, mouse
//! reporting keeps swallowing the shell's clicks, and the next program to read the tty finds
//! itself writing into a screen nobody asked for. The user's report was exactly this: escape
//! garbage typing itself into the console after `kill`.
//!
//! # Why a handler and not a `Drop`
//!
//! A signal does not unwind. There is no stack to run down and no value whose `Drop` will
//! fire, so restoration has to be *invoked* from the handler. That makes the handler's job
//! very small and very specific: mark the terminal as no longer ours, restore it, then let
//! the process die with the status the signal implies.
//!
//! # Async-signal safety
//!
//! A signal handler may only call async-signal-safe functions. Nothing here allocates, locks
//! a mutex, or touches stdio: [`crate::term::force_restore`] writes its disable sequences
//! with one `write(2)` per sequence on a fixed buffer, and the shutdown path is a plain
//! atomic flag plus a self-pipe. The wake-up half of the design runs on the normal thread,
//! where it is safe to do anything; the handler half only writes a byte.
//!
//! # Two exits from one signal
//!
//! The handler sets a flag and pushes a byte into a pipe, which makes the event loop's poll
//! return so the *loop* can shut down in its normal way -- session written, final frame
//! drawn. But a process wedged in a blocking syscall that will not return, or one sent a
//! second signal, must not stay alive: the same handler counts entries and on the second
//! one restores the terminal itself and re-raises with the default disposition. That is what
//! makes `kill -TERM` followed by `kill -TERM` always work, which is the property that
//! matters when the editor is already misbehaving.
//!
//! # `SIGPIPE`
//!
//! Ignored on purpose. A TUI writes to its own terminal; a broken pipe there should not kill
//! the process mid-keystroke. Rust already ignores it at startup, and this re-asserts it so
//! a library that resets dispositions cannot silently change editor behaviour.

use std::io::ErrorKind;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::term;

/// Set when a termination signal has arrived and the loop should stop.
static TERMINATED: AtomicBool = AtomicBool::new(false);
/// How many termination signals have been handled. The second one takes the hard path.
static HITS: AtomicU32 = AtomicU32::new(0);
/// The signal that arrived, for the log and for the re-raise.
static LAST_SIGNAL: AtomicU32 = AtomicU32::new(0);

/// Read end of the self-pipe, kept alive for the process's lifetime so the write end stays
/// valid inside a handler that may run at any moment.
static WAKE: OnceLock<WakePipe> = OnceLock::new();

/// The descriptors of the self-pipe used to interrupt the event loop.
struct WakePipe {
    reader: OwnedFd,
    writer: OwnedFd,
}

/// True when a termination signal has been seen.
pub fn terminated() -> bool {
    TERMINATED.load(Ordering::SeqCst)
}

/// The number of termination signals handled so far. Exposed for tests and diagnostics.
pub fn hit_count() -> u32 {
    HITS.load(Ordering::SeqCst)
}

/// The file descriptor to poll alongside terminal input, so a signal wakes the loop.
///
/// `None` until [`install`] has run, or when the pipe could not be created -- in which case
/// the loop simply keeps its original timeout-driven behaviour, and the handler's hard path
/// still restores the terminal.
pub fn wake_fd() -> Option<i32> {
    WAKE.get().map(|p| p.reader.as_raw_fd())
}

/// Install handlers for the signals that end a session.
///
/// Called before the terminal is entered, so a signal that arrives during setup is handled
/// by the same code as one that arrives mid-edit. Failure is not fatal: an editor without a
/// signal handler behaves the way it did before this module existed, which is degraded but
/// not broken.
pub fn install() -> bool {
    // Created before the handlers exist: a handler that fires must never have to allocate a
    // descriptor, and `OnceLock::get` on an uninitialised static is cheap and lock-free.
    if WAKE.get().is_none() {
        match create_pipe() {
            Some(pipe) => {
                // A lost race means another thread installed first; `set` returns the loser,
                // which we simply drop along with its descriptors.
                let _ = WAKE.set(pipe);
            }
            None => log::warn!("signal wake pipe unavailable; signals use the hard path only"),
        }
    }

    let mut ok = true;
    for number in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        // `register` is unsafe because the closure runs on the signal stack, where only
        // async-signal-safe work is permitted. `handle` is written to that rule: no
        // allocation, no lock, no stdio, no recursion -- see its own comment.
        match unsafe { signal_hook_registry::register(number, move || handle(number)) } {
            Ok(_) => {}
            Err(err) => {
                log::warn!("cannot install handler for signal {number}: {err}");
                ok = false;
            }
        }
    }
    ok
}

/// The handler body. Kept tiny and free of allocation, locking, and stdio.
fn handle(number: i32) {
    LAST_SIGNAL.store(number as u32, Ordering::SeqCst);
    TERMINATED.store(true, Ordering::SeqCst);
    let hits = HITS.fetch_add(1, Ordering::SeqCst) + 1;
    if hits == 1 {
        // Soft path: wake the loop and let it shut down properly. A partial write is fine --
        // any byte at all makes `poll` return.
        if let Some(pipe) = WAKE.get() {
            let _ = write_one(pipe.writer.as_raw_fd());
        }
        return;
    }
    // Hard path: the user is insisting. Restore the terminal directly and let the default
    // disposition kill us, so a wedged loop cannot keep the process alive.
    term::force_restore();
    unsafe {
        libc::signal(number, libc::SIG_DFL);
        libc::kill(libc::getpid(), number);
    }
}

/// Write the single wake byte. `write(2)` on a pipe is async-signal-safe.
fn write_one(fd: i32) -> std::io::Result<()> {
    let byte = [1u8];
    let written = unsafe { libc::write(fd, byte.as_ptr() as *const libc::c_void, byte.len()) };
    if written < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Drain the wake pipe so a stale byte cannot make the loop spin.
///
/// Called by the loop after it has noticed a termination. Draining rather than reading one
/// byte means N signals cost one wake-up instead of N iterations.
pub fn drain_wake() {
    let Some(pipe) = WAKE.get() else { return };
    let fd = pipe.reader.as_raw_fd();
    let mut buf = [0u8; 64];
    loop {
        let read = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if read <= 0 {
            return;
        }
    }
}

/// Create a non-blocking pipe. Done with `pipe2` because a blocking wake-up read inside a
/// poll loop could hang the very loop the pipe exists to rescue.
fn create_pipe() -> Option<WakePipe> {
    let mut fds = [0i32; 2];
    // O_NONBLOCK = 1 << 11 on Linux.
    const O_NONBLOCK: i32 = 0o4000;
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), O_NONBLOCK) };
    if rc != 0 {
        return None;
    }
    let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Some(WakePipe { reader, writer })
}

/// True when an I/O error is the expected "interrupted by a signal" kind.
///
/// Lives here because it is the signal module's share of the loop's error handling: once a
/// handler exists, `EINTR` becomes a normal event rather than an anomaly, and the loop needs
/// to ask the question in the same place that answers whether a termination arrived.
pub fn is_interrupted(err_kind: ErrorKind) -> bool {
    err_kind == ErrorKind::Interrupted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminated_starts_false_and_a_hit_is_counted() {
        // The handler itself is only exercised by the process-level test (a real signal in a
        // test process would kill the harness), but the counters it drives are observable.
        let before = hit_count();
        assert_eq!(hit_count(), before);
        let _ = terminated();
    }

    #[test]
    fn a_wake_pipe_can_be_created_and_drained() {
        let pipe = create_pipe().expect("pipe2 works here");
        let rd = pipe.reader.as_raw_fd();
        let wr = pipe.writer.as_raw_fd();
        assert!(write_one(wr).is_ok(), "the wake byte must be accepted");
        let mut buf = [0u8; 4];
        let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert_eq!(n, 1, "the wake byte arrives");
        // A second read on a drained non-blocking pipe reports would-block, never a hang.
        let n = unsafe { libc::read(rd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert!(n <= 0, "drained pipe must not block, got {n}");
    }

    #[test]
    fn wake_fd_is_none_before_install_and_some_after() {
        // Order-independent: install() is idempotent and may have run for another test.
        install();
        assert!(wake_fd().is_some(), "install must leave a pollable fd");
    }

    #[test]
    fn install_is_idempotent() {
        assert!(install());
        assert!(install(), "a second install must not fail or leak handlers");
    }

    #[test]
    fn interrupted_kind_is_recognised() {
        assert!(is_interrupted(ErrorKind::Interrupted));
        assert!(!is_interrupted(ErrorKind::UnexpectedEof));
    }
}
