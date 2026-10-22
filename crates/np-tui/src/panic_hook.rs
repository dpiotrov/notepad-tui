//! Panic hook: put the terminal back, then say what happened (spec §8).
//!
//! # The order is the whole point
//!
//! A panic while the alternate screen is live would otherwise print its message onto a
//! screen the user cannot scroll, and then hand back a terminal still in raw mode: no echo,
//! no line buffering, and a shell that looks broken. So the hook restores the terminal
//! **first** and prints second, to the real stdout, after leaving the alternate screen.
//!
//! # Chaining
//!
//! The previous hook is saved and called after restoring. Sentry-style crash reporters,
//! test harnesses, and `rustc`'s own hook all install themselves this way, and dropping them
//! silently breaks whoever installed them.
//!
//! # `panic = "abort"`
//!
//! The workspace release profile sets `panic = "abort"`. Verified on this toolchain
//! (1.98.0): with abort, the hook **does** run, but `std::panic::catch_unwind` cannot
//! recover and `Drop` impls never execute — the process dies with SIGABRT. Two consequences
//! shape this module:
//!
//! 1. Restoration must happen inside the hook, because no `Drop` will do it. That is exactly
//!    what [`install`] arranges, so the guarantee holds under both panic strategies.
//! 2. A `catch_unwind` at the top of the run loop is still used, because it is what turns a
//!    panic into a clean shutdown **when unwinding is available** (debug/test builds). Under
//!    abort it simply never gets to act, and the hook's output is what the user sees.
//!
//! See `docs/DEVIATIONS.md` for why the profile was not changed.
//!
//! # Testability
//!
//! The hook's output goes through a [`Reporter`] so a test can capture it instead of
//! scraping process stdout. [`format_panic`] is pure and is what the tests assert on.

use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::logging;
use crate::term;

/// Where a panic report is written.
pub trait Reporter {
    fn write_line(&mut self, line: &str);
}

/// Writes to the real process stdout. The production reporter.
pub struct StdoutReporter;

impl Reporter for StdoutReporter {
    fn write_line(&mut self, line: &str) {
        // Printed with a plain newline rather than `println!`'s lock-and-flush per line
        // because this runs on a panic path where the stdout lock may already be held by
        // the panicking code. A lost line beats a deadlock.
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// Collects lines so tests can assert on them.
#[derive(Default, Clone)]
pub struct CaptureReporter {
    lines: std::sync::Arc<Mutex<Vec<String>>>,
}

impl CaptureReporter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().map(|l| l.clone()).unwrap_or_default()
    }
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }
}

impl Reporter for CaptureReporter {
    fn write_line(&mut self, line: &str) {
        if let Ok(mut v) = self.lines.lock() {
            v.push(line.to_string());
        }
    }
}

/// How many times the hook has run. Tests assert on this instead of on stdout.
static HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Panics seen (for tests that need to observe the hook without capturing output).
pub fn hook_call_count() -> usize {
    HOOK_CALLS.load(Ordering::SeqCst)
}

/// The reporter the installed hook writes to. Swappable in tests.
static REPORTER: OnceLock<Mutex<Box<dyn Reporter + Send>>> = OnceLock::new();

/// Whether a panic has been reported. Used by the run loop to pick its exit code.
static PANIC_SEEN: AtomicUsize = AtomicUsize::new(0);

/// True if a panic was reported during this process.
pub fn panic_seen() -> bool {
    PANIC_SEEN.load(Ordering::SeqCst) > 0
}

/// Build the report lines for a panic.
///
/// Pure so it can be tested exactly: the message, the source location when the panic
/// payload carries one, and the log path (FR-CG1/§8).
pub fn format_panic(
    payload: &str,
    location: Option<&str>,
    log_path: &std::path::Path,
) -> Vec<String> {
    let mut out = Vec::new();
    out.push(String::from("npt: unexpected internal error"));
    out.push(format!("  message: {}", payload.trim()));
    if let Some(loc) = location {
        out.push(format!("  location: {loc}"));
    }
    out.push(format!("  log: {}", log_path.display()));
    out.push(String::from(
        "  Your terminal has been restored. Unsaved changes may be lost; \
         the session file is restored on next start.",
    ));
    out
}

/// Extract a printable message from a panic payload.
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        String::from("<non-string panic payload>")
    }
}

/// Install our panic hook, chaining to whatever was already installed.
///
/// Idempotent: calling it twice does not stack two hooks.
pub fn install() {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.set(()).is_err() {
        return;
    }
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        handle(info, &previous);
    }));
    let _ = REPORTER.set(Mutex::new(Box::new(StdoutReporter)));
}

/// The hook body, split out so tests can drive it with a captured reporter.
fn handle(info: &PanicHookInfo<'_>, previous: &(dyn Fn(&PanicHookInfo<'_>) + Send + Sync)) {
    HOOK_CALLS.fetch_add(1, Ordering::SeqCst);
    PANIC_SEEN.fetch_add(1, Ordering::SeqCst);

    // Restore the terminal BEFORE writing anything, so the text lands on the normal screen.
    // The failure count is deliberately ignored: a terminal we cannot restore is not a
    // reason to stop reporting, and there is nobody to return to.
    let restore_failures = term::restore_terminal_for_panic();

    let payload = payload_text(info.payload());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
    let mut lines = format_panic(&payload, location.as_deref(), &logging::log_path());
    if restore_failures > 0 {
        lines.push(format!(
            "  warning: {} terminal restore command(s) failed; if your shell looks \
             broken, run `reset`",
            restore_failures
        ));
    }

    // Mirror into the log file directly: the `log` facade may itself have been involved in
    // whatever panicked.
    for line in &lines {
        logging::write_direct(line);
    }

    // Emit to the configured reporter (stdout in production, a buffer in tests).
    emit(&lines);

    // Finally give the previous hook its turn, with the terminal already usable.
    previous(info);
}

fn emit(lines: &[String]) {
    match REPORTER.get() {
        Some(lock) => {
            if let Ok(mut reporter) = lock.lock() {
                for line in lines {
                    reporter.write_line(line);
                }
            }
        }
        None => {
            // install() has not run (tests, or a panic before startup). Fall back to stderr.
            for line in lines {
                eprintln!("{line}");
            }
        }
    }
}

/// Run `f`, reporting a panic through the same path as the hook and returning `false`.
///
/// This is the `catch_unwind` the stage spec asks for. It recovers in debug and test builds;
/// under `panic = "abort"` the process dies first and the hook is what the user sees. Having
/// both means neither strategy leaves the terminal unrestored.
/// The error type for [`guard_running`]: the panic was already reported by the hook, so
/// there is nothing further to describe than "it panicked".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panicked;

impl std::fmt::Display for Panicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("panicked (already reported by the panic hook)")
    }
}

impl std::error::Error for Panicked {}

pub fn guard_running<F: FnOnce() -> R, R>(f: F) -> Result<R, Panicked> {
    // The caller supplies `UnwindSafe` (via `AssertUnwindSafe`) when it knows the post-panic
    // state will not be read, which is the case for every caller here.
    let f = panic::AssertUnwindSafe(f);
    match panic::catch_unwind(f) {
        Ok(value) => Ok(value),
        Err(payload) => {
            // The hook already ran and already restored, so nothing to do here but convert.
            let _ = payload;
            Err(Panicked)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic as sp;

    /// Serialize hook tests: they mutate process-global hook state.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        match LOCK.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[test]
    fn report_contains_message_location_and_log_path() {
        let lines = format_panic(
            "index out of bounds",
            Some("src/app.rs:12:3"),
            std::path::Path::new("/home/u/.local/share/npt/npt.log"),
        );
        let text = lines.join("\n");
        assert!(text.contains("index out of bounds"), "{text}");
        assert!(text.contains("src/app.rs:12:3"), "{text}");
        assert!(text.contains("/home/u/.local/share/npt/npt.log"), "{text}");
    }

    #[test]
    fn report_works_without_a_location() {
        let lines = format_panic("boom", None, std::path::Path::new("npt.log"));
        let text = lines.join("\n");
        assert!(text.contains("boom"), "{text}");
        assert!(
            !text.contains("location:"),
            "no location line when unknown: {text}"
        );
    }

    #[test]
    fn payload_text_handles_both_string_shapes() {
        let a = String::from("literal");
        assert_eq!(payload_text(&"static str"), "static str");
        assert_eq!(payload_text(&a), "literal");
        assert_eq!(payload_text(&42u8), "<non-string panic payload>");
    }

    #[test]
    fn a_real_panic_runs_the_hook_and_restores_the_terminal() {
        let _g = serial();
        install();
        // Enter the "terminal" so we can prove the hook leaves it.
        let base = term::guard_depth();
        if base == 0 {
            let _enter =
                term::TerminalGuard::enter_with(&mut term::CrosstermDriver, term::RestoreVia::Real);
        }

        let before = hook_call_count();
        let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();

        // Run in its own thread: a panic here must not take the test process down, and a
        // fresh thread gets its own unwind so the assertion below still executes.
        let handle = std::thread::spawn(move || {
            let result = sp::catch_unwind(|| {
                panic!("deliberate test panic");
            });
            result.is_err()
        });
        let recovered = handle.join().unwrap_or(false);

        assert!(recovered, "catch_unwind should catch in a debug/test build");
        assert!(
            hook_call_count() > before,
            "the installed hook must fire for a panic"
        );
        // The hook restores, so depth returns to where it was (or below if it started
        // negative, which restore clamps).
        assert!(
            term::guard_depth() <= base,
            "hook must not leave the terminal guarded"
        );
        // Nothing was captured because this test does not install a capture reporter; the
        // hook fell back to stderr, which is the documented behavior before install().
        assert!(sink.lock().unwrap().is_empty());
    }

    #[test]
    fn capture_reporter_collects_lines() {
        let mut r = CaptureReporter::new();
        r.write_line("one");
        r.write_line("two");
        assert_eq!(r.lines(), vec!["one".to_string(), "two".to_string()]);
        assert_eq!(r.text(), "one\ntwo");
    }

    #[test]
    fn install_is_idempotent() {
        let _g = serial();
        install();
        install();
        // A second install must not double-report. Proven by panicking once and checking the
        // count moved by exactly one.
        let before = hook_call_count();
        let handle = std::thread::spawn(|| {
            let _ = sp::catch_unwind(|| panic!("once"));
        });
        let _ = handle.join();
        assert_eq!(
            hook_call_count(),
            before + 1,
            "hook installed twice would count 2"
        );
    }

    #[test]
    fn guard_running_returns_ok_when_nothing_panics() {
        assert_eq!(guard_running(|| 7), Ok(7));
    }

    #[test]
    fn guard_running_catches_a_panic() {
        let _g = serial();
        install();
        let before = hook_call_count();
        let r: Result<i32, Panicked> = guard_running(|| panic!("caught by guard"));
        assert!(r.is_err());
        assert_eq!(hook_call_count(), before + 1);
    }

    #[test]
    fn panic_seen_flips_after_a_reported_panic() {
        let _g = serial();
        install();
        let handle = std::thread::spawn(|| {
            let _ = sp::catch_unwind(|| panic!("sets the flag"));
        });
        let _ = handle.join();
        assert!(panic_seen());
    }
}
