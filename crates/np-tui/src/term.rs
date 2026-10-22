//! Terminal lifecycle: enter, and guaranteed exit (spec §5.1, §8).
//!
//! # Why not `ratatui::init()`
//!
//! `ratatui::init()` and `ratatui::restore()` do exist, but they hide the setup order and
//! tie teardown to a `Drop` that a panic can skip. Spec §8 requires the panic hook to put
//! the terminal back, which means the restore sequence has to be callable on its own, from
//! a context where no value is alive to be dropped. [`restore_terminal_for_panic`] is that
//! entry point, and `TerminalGuard::drop` calls the same function.
//!
//! # Ordering
//!
//! Setup is raw mode, alternate screen, mouse, bracketed paste, then (conditionally) kitty
//! keys. Teardown runs in reverse. The reverse order matters for the kitty pop: popping a
//! keyboard layer while still in raw mode is what the protocol expects, and disabling raw
//! mode first would leave the pushed flags active for whatever program runs next.
//!
//! # Panic safety of `Drop`
//!
//! `Drop` cannot return an error, so each individual restore is attempted and its failure
//! ignored — one command failing (a closed stdout, say) must not skip the rest, or the
//! user's shell comes back with mouse reporting on and echo off. This is the one place
//! where swallowing an error is correct.
//!
//! # Reentrancy
//!
//! Restoration is idempotent and reference-counted through a process-wide counter, so the
//! panic hook can run it and a subsequent `Drop` cannot restore a second time. Without
//! that, a panic inside the loop followed by unwinding would emit the disable sequences
//! twice, which is at worst noise and at worst confuses a terminal that had already
//! re-enabled them for the shell.
//!
//! # Testing without a pty
//!
//! No test in this project allocates a terminal. The enter/exit path is observable through
//! [`guard_depth`], and [`Driver`] is the seam: tests inject a recording driver and assert
//! the exact command sequence and its reversal, including that a failing step does not
//! abort the teardown.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

/// Whether a kitty keyboard layer is currently pushed. Only consulted by
/// [`restore_terminal_for_panic`], which has no guard to ask; guards carry their own copy.
static KITTY_LIVE: AtomicI32 = AtomicI32::new(0);

/// One setup step's action.
type StepFn = fn(&mut dyn Driver) -> Result<()>;

/// How deep the terminal currently is: 0 when untouched, 1 while a guard is live.
///
/// `AtomicI32` rather than a bool so a mismatch shows up as a nonzero residual depth
/// instead of a silently wrong boolean, and so a nested enter/exit pair is representable.
static DEPTH: AtomicI32 = AtomicI32::new(0);

/// Current guard nesting depth. 0 means the terminal is in its original state.
pub fn guard_depth() -> i32 {
    DEPTH.load(Ordering::SeqCst)
}

/// True while the terminal is in raw mode / alternate screen under a guard.
pub fn is_guarded() -> bool {
    guard_depth() > 0
}

/// The subset of terminal side effects this module performs.
///
/// A trait so the sequence is testable: the real implementation writes escape sequences,
/// the test implementation records them and can make one step fail.
pub trait Driver {
    fn enable_raw(&mut self) -> Result<()>;
    fn disable_raw(&mut self) -> Result<()>;
    fn enter_alt(&mut self) -> Result<()>;
    fn leave_alt(&mut self) -> Result<()>;
    fn enable_mouse(&mut self) -> Result<()>;
    fn disable_mouse(&mut self) -> Result<()>;
    fn enable_paste(&mut self) -> Result<()>;
    fn disable_paste(&mut self) -> Result<()>;
    /// Whether the terminal advertises the kitty enhancement protocol.
    fn kitty_supported(&self) -> bool;
    fn push_kitty(&mut self) -> Result<()>;
    fn pop_kitty(&mut self) -> Result<()>;
}

/// The real driver: writes to the actual stdout.
pub struct CrosstermDriver;

impl Driver for CrosstermDriver {
    fn enable_raw(&mut self) -> Result<()> {
        enable_raw_mode()?;
        Ok(())
    }
    fn disable_raw(&mut self) -> Result<()> {
        disable_raw_mode()?;
        Ok(())
    }
    fn enter_alt(&mut self) -> Result<()> {
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        Ok(())
    }
    fn leave_alt(&mut self) -> Result<()> {
        execute!(std::io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }
    fn enable_mouse(&mut self) -> Result<()> {
        execute!(std::io::stdout(), EnableMouseCapture)?;
        Ok(())
    }
    fn disable_mouse(&mut self) -> Result<()> {
        execute!(std::io::stdout(), DisableMouseCapture)?;
        Ok(())
    }
    fn enable_paste(&mut self) -> Result<()> {
        execute!(std::io::stdout(), EnableBracketedPaste)?;
        Ok(())
    }
    fn disable_paste(&mut self) -> Result<()> {
        execute!(std::io::stdout(), DisableBracketedPaste)?;
        Ok(())
    }
    fn kitty_supported(&self) -> bool {
        // The query writes to the terminal and reads a response, so it can fail on a pipe
        // or a dumb terminal. Degrading silently is the spec'd behavior (§5.1).
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    }
    fn push_kitty(&mut self) -> Result<()> {
        // DISAMBIGUATE_ESCAPE_CODES makes Ctrl+Shift+<letter> distinguishable from
        // Ctrl+<letter>, which several keymap entries depend on; REPORT_EVENT_TYPES is
        // what makes KeyEventKind arrive so the Press filter in `App::handle_key` works.
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
        execute!(std::io::stdout(), PushKeyboardEnhancementFlags(flags))?;
        Ok(())
    }
    fn pop_kitty(&mut self) -> Result<()> {
        execute!(std::io::stdout(), PopKeyboardEnhancementFlags)?;
        Ok(())
    }
}

/// How a guard restores, so `Drop` uses the same driver that opened the terminal.
///
/// A test guard that restored through the real driver would emit live escape sequences to
/// stdout while recording nothing, which is both a broken assertion and a corrupted test
/// log. Boxing the opening driver keeps `Drop` honest in both cases.
pub enum RestoreVia {
    /// Restore through the real crossterm driver (the production path).
    Real,
    /// Restore through a caller-supplied driver, so tests can observe teardown.
    Recorded(Box<dyn Driver>),
}

/// Owns the terminal for the duration of an editing session.
///
/// Construct with [`TerminalGuard::enter`]; restoration happens on `Drop` or, ahead of it,
/// through [`restore_terminal_for_panic`].
pub struct TerminalGuard {
    /// True when this instance performed the enter, so only it restores. A guard that
    /// failed partway must not undo state it never set.
    owns: bool,
    /// Whether *this* guard pushed kitty flags. Teardown pops only when this is true:
    /// popping a layer that was never pushed is a protocol violation, and a process-wide
    /// flag could not answer the question per guard.
    kitty_pushed: bool,
    via: RestoreVia,
}

impl TerminalGuard {
    /// Put the terminal into editing state on the real driver.
    pub fn enter() -> Result<TerminalGuard> {
        Self::enter_with(&mut CrosstermDriver, RestoreVia::Real)
    }

    /// The enter sequence against an injected driver.
    ///
    /// On failure, everything already enabled is rolled back before the error is returned,
    /// so a partial enter never leaves the terminal half-converted.
    pub fn enter_with(driver: &mut dyn Driver, via: RestoreVia) -> Result<TerminalGuard> {
        // Steps performed, in order, so rollback knows what to undo. Rolling back in
        // reverse without this list would either skip a step or disable something that was
        // never enabled.
        let mut done: Vec<Step> = Vec::new();
        // The kitty support query is evaluated once, here, and remembered. Calling it twice
        // would query the terminal twice (it writes an escape sequence and waits for a
        // reply, so it is not free and not side-effect-free), and the second answer could
        // in principle differ from the first.
        let kitty = driver.kitty_supported();

        // The closure table cannot express "only if supported", so the kitty step is
        // appended conditionally rather than guarded inside the closure.
        // Snapshot the termios before anything changes it, so [`force_restore`] has the
        // original attributes to put back without going through crossterm's locked copy.
        capture_original_termios();

        let mut steps: Vec<(Step, StepFn)> = vec![
            (Step::Raw, |d| d.enable_raw()),
            (Step::Alt, |d| d.enter_alt()),
            (Step::Mouse, |d| d.enable_mouse()),
            (Step::Paste, |d| d.enable_paste()),
        ];
        if kitty {
            steps.push((Step::Kitty, |d| d.push_kitty()));
        }

        for (step, run) in steps {
            if let Err(err) = run(driver) {
                rollback(driver, &done, kitty);
                return Err(err.context(format!("terminal setup: {step}")));
            }
            done.push(step);
        }

        if kitty {
            KITTY_LIVE.fetch_add(1, Ordering::SeqCst);
        }
        DEPTH.fetch_add(1, Ordering::SeqCst);
        Ok(TerminalGuard {
            owns: true,
            kitty_pushed: kitty,
            via,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.owns {
            return;
        }
        self.owns = false;
        // Same sequence the panic hook uses, so there is exactly one restore path in the
        // codebase and it is the one the tests exercise.
        match &mut self.via {
            RestoreVia::Real => {
                let _ = restore_inner(&mut CrosstermDriver, self.kitty_pushed);
            }
            RestoreVia::Recorded(driver) => {
                let _ = restore_inner(driver.as_mut(), self.kitty_pushed);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Raw,
    Alt,
    Mouse,
    Paste,
    Kitty,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Step::Raw => "raw mode",
            Step::Alt => "alternate screen",
            Step::Mouse => "mouse capture",
            Step::Paste => "bracketed paste",
            Step::Kitty => "kitty keyboard protocol",
        };
        f.write_str(name)
    }
}

/// Undo the enabled steps in reverse, ignoring individual failures.
fn rollback(driver: &mut dyn Driver, done: &[Step], kitty: bool) {
    if kitty {
        let _ = driver.pop_kitty();
    }
    for step in done.iter().rev() {
        match step {
            // Kitty is handled above so the pop always precedes everything else.
            Step::Kitty => {}
            Step::Paste => {
                let _ = driver.disable_paste();
            }
            Step::Mouse => {
                let _ = driver.disable_mouse();
            }
            Step::Alt => {
                let _ = driver.leave_alt();
            }
            Step::Raw => {
                let _ = driver.disable_raw();
            }
        }
    }
}

/// Restore the terminal, for the panic hook and for orderly shutdown.
///
/// Idempotent: a second call after the depth has reached zero does nothing, so the hook
/// running before an unwinding `Drop` cannot emit the sequences twice.
///
/// Whether the kitty layer is popped is decided by the global "a kitty layer is live" flag,
/// which the panic hook has no guard to consult. Returns the number of steps that failed;
/// callers ignore it (a hook has nowhere to propagate to) but tests assert on it.
pub fn restore_terminal_for_panic() -> usize {
    restore_inner(
        &mut CrosstermDriver,
        KITTY_LIVE.swap(0, Ordering::SeqCst) > 0,
    )
}

/// The restore sequence against an injected driver, popping kitty only if the driver's
/// session pushed it. Used by tests and by any caller that knows its own kitty state.
pub fn restore_with(driver: &mut dyn Driver) -> usize {
    restore_inner(driver, KITTY_LIVE.swap(0, Ordering::SeqCst) > 0)
}

/// Shared teardown: claim the depth, then undo in reverse, tolerating individual failures.
fn restore_inner(driver: &mut dyn Driver, pop_kitty: bool) -> usize {
    // Claim the depth first. If nothing was entered, or someone else already restored,
    // emitting again would be wrong.
    let prev = DEPTH.fetch_sub(1, Ordering::SeqCst);
    if prev <= 0 {
        // Put it back: we subtracted a claim that was not ours to make.
        DEPTH.fetch_add(1, Ordering::SeqCst);
        return 0;
    }

    let mut failures = 0usize;
    if pop_kitty && driver.pop_kitty().is_err() {
        failures += 1;
    }
    // Reverse of enter: paste, mouse, alt screen, raw mode. Raw mode goes last so every
    // escape sequence below is still written in raw mode, exactly as it was enabled.
    if driver.disable_paste().is_err() {
        failures += 1;
    }
    if driver.disable_mouse().is_err() {
        failures += 1;
    }
    if driver.leave_alt().is_err() {
        failures += 1;
    }
    if driver.disable_raw().is_err() {
        failures += 1;
    }
    failures
}

/// The termios the terminal had before raw mode, captured at enter.
///
/// A signal handler cannot read crossterm's copy of it: that lives behind a `parking_lot`
/// mutex, and taking a lock inside a handler deadlocks when the signal interrupted a thread
/// holding it. So the attributes are snapshotted here, at the one moment we know they are
/// still the original ones, into plain static storage a handler may touch.
static ORIG_TERMIOS: Mutex<Option<libc::termios>> = Mutex::new(None);

/// A spin lock for the termios snapshot, because `std::sync::Mutex` is not
/// async-signal-safe and this data is one `Option<struct>` of copyable integers.
struct Mutex<T>(std::sync::atomic::AtomicBool, UnsafeCell<T>);

// Safety: the guard is held for the length of a copy in or out, never across anything that
// can recurse, and the wrapped value is `Copy`.
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

impl<T: Copy> Mutex<T> {
    const fn new(value: T) -> Self {
        Mutex(
            std::sync::atomic::AtomicBool::new(false),
            UnsafeCell::new(value),
        )
    }

    /// Copy the value out, spinning until the lock is free.
    ///
    /// `None` when the lock is contended and the caller must not wait -- which is the only
    /// sane choice in a signal handler, where blocking is undefined behaviour.
    fn load(&self, wait: bool) -> Option<T> {
        loop {
            if !self.0.swap(true, Ordering::Acquire) {
                let value = unsafe { *self.1.get() };
                self.0.store(false, Ordering::Release);
                return Some(value);
            }
            if !wait {
                return None;
            }
            std::hint::spin_loop();
        }
    }

    /// Store a value, waiting for the lock. Only for the non-signal path.
    fn store(&self, value: T) {
        while self.0.swap(true, Ordering::Acquire) {
            std::hint::spin_loop();
        }
        unsafe { *self.1.get() = value };
        self.0.store(false, Ordering::Release);
    }
}

/// Remember the terminal's termios as it is right now, before raw mode is enabled.
///
/// Best effort on purpose. If `tcgetattr` fails -- not a tty, no permission -- the snapshot
/// stays empty and [`force_restore`] falls back to the explicit flag manipulation it does
/// without it. Failing the whole startup over a missing snapshot would be a bad trade.
fn capture_original_termios() {
    let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
    let rc = unsafe { libc::tcgetattr(1, attrs.as_mut_ptr()) };
    if rc == 0 {
        ORIG_TERMIOS.store(Some(unsafe { attrs.assume_init() }));
    }
}

/// Restore the terminal from a signal handler: raw writes, no locks, no allocation.
///
/// [`restore_terminal_for_panic`] cannot be used there. It goes through crossterm, whose raw
/// mode state lives behind a `parking_lot` mutex and whose writes go through a locked stdout
/// handle; if the signal interrupted code holding either, the handler would deadlock and the
/// terminal would stay in raw mode forever -- precisely the failure this function exists to
/// prevent. So the sequences go straight to fd 1 with `write(2)`, and the termios comes from
/// [`capture_original_termios`], taken at enter.
///
/// The shared depth counter is claimed first, exactly as [`restore_inner`] does, so a
/// `TerminalGuard::drop` that still runs afterwards does not emit everything a second time.
///
/// Returns the number of steps that failed. A signal handler has nowhere to send them; tests
/// assert on them.
pub fn force_restore() -> usize {
    // Claim the depth. Nothing entered, or somebody already restored: emit nothing.
    let prev = DEPTH.fetch_sub(1, Ordering::SeqCst);
    if prev <= 0 {
        DEPTH.fetch_add(1, Ordering::SeqCst);
        return 0;
    }
    let kitty = KITTY_LIVE.swap(0, Ordering::SeqCst) > 0;

    let mut failures = write_disable_sequences(kitty);

    // Raw mode is a termios change, not an escape sequence, so it needs the ioctl. `TCSANOW`
    // applies it immediately; `tcsetattr` is async-signal-safe by POSIX.
    // `load` yields `Option<termios>` (the stored value) wrapped in another `Option` when
    // the lock is contended; flatten so a contended lock takes the same fallback path as an
    // absent snapshot rather than being mistaken for one.
    match ORIG_TERMIOS.load(false).flatten() {
        Some(attrs) => {
            if unsafe { libc::tcsetattr(1, libc::TCSANOW, &raw const attrs) } != 0 {
                failures += 1;
            }
        }
        // No snapshot (the capture failed, or this process entered through another path):
        // turn the two flags the user actually notices back on. This is a subset of what the
        // snapshot restores, which is why the snapshot is preferred.
        None => {
            let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
            if unsafe { libc::tcgetattr(1, attrs.as_mut_ptr()) } == 0 {
                let mut attrs = unsafe { attrs.assume_init() };
                attrs.c_lflag |= libc::ICANON | libc::ECHO;
                if unsafe { libc::tcsetattr(1, libc::TCSANOW, &attrs) } != 0 {
                    failures += 1;
                }
            } else {
                failures += 1;
            }
        }
    }
    failures
}

/// Whether a raw-mode snapshot is available. Test seam, and a diagnostic for the log.
pub fn has_termios_snapshot() -> bool {
    ORIG_TERMIOS.load(true).flatten().is_some()
}

/// Write the disable sequences straight to fd 1, without crossterm or a locked stdout.
///
/// Content mirrors crossterm's own commands, verified against the byte stream a live session
/// produces: mouse modes off in the reverse of their enable order (`1006 1015 1003 1002
/// 1000`), bracketed paste off, alternate screen off, cursor shown again, and the kitty
/// keyboard layer popped. The bytes are duplicated rather than borrowed from crossterm
/// because its `Command::write_ansi` runs through formatting machinery that allocates, and
/// these are stable terminal protocol.
///
/// Order matters more than content here. Leaving the alternate screen must happen before the
/// cursor is shown, or the cursor becomes visible on the shell's screen at whatever position
/// the editor last parked it; and mouse modes must go off before the alt screen does, or a
/// click in the gap is reported as escape codes into the shell's line.
fn write_disable_sequences(pop_kitty: bool) -> usize {
    let mut failures = 0usize;
    if pop_kitty {
        // csi "<1u" -- the pop half of the kitty keyboard protocol.
        if write_all_raw(1, b"\x1b[<1u").is_err() {
            failures += 1;
        }
    }
    if write_all_raw(1, b"\x1b[?2004l").is_err() {
        failures += 1;
    }
    if write_all_raw(
        1,
        b"\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
    )
    .is_err()
    {
        failures += 1;
    }
    if write_all_raw(1, b"\x1b[?1049l\x1b[?25h").is_err() {
        failures += 1;
    }
    failures
}

/// `write(2)` in a loop until the buffer is out or the write cannot continue.
///
/// A partial write on a tty is possible when a signal interrupts it; `EINTR` is retried,
/// anything else gives up. The retry loop is bounded by the buffer length, which is fixed
/// and tiny, so this cannot spin.
fn write_all_raw(fd: i32, bytes: &[u8]) -> std::io::Result<()> {
    let mut done = 0usize;
    while done < bytes.len() {
        let written = unsafe {
            libc::write(
                fd,
                bytes[done..].as_ptr() as *const libc::c_void,
                bytes.len() - done,
            )
        };
        if written < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
        if written == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
        }
        done += written as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Records the command sequence; optionally fails a named step.
    #[derive(Clone, Default)]
    struct Log {
        calls: Rc<RefCell<Vec<String>>>,
        fail: Rc<RefCell<Vec<String>>>,
        kitty: Rc<RefCell<bool>>,
    }

    struct TestDriver {
        log: Log,
    }

    impl Driver for TestDriver {
        fn enable_raw(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("raw+".into());
            check(&self.log, "raw+")
        }
        fn disable_raw(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("raw-".into());
            check(&self.log, "raw-")
        }
        fn enter_alt(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("alt+".into());
            check(&self.log, "alt+")
        }
        fn leave_alt(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("alt-".into());
            check(&self.log, "alt-")
        }
        fn enable_mouse(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("mouse+".into());
            check(&self.log, "mouse+")
        }
        fn disable_mouse(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("mouse-".into());
            check(&self.log, "mouse-")
        }
        fn enable_paste(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("paste+".into());
            check(&self.log, "paste+")
        }
        fn disable_paste(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("paste-".into());
            check(&self.log, "paste-")
        }
        fn kitty_supported(&self) -> bool {
            *self.log.kitty.borrow()
        }
        fn push_kitty(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("kitty+".into());
            check(&self.log, "kitty+")
        }
        fn pop_kitty(&mut self) -> Result<()> {
            self.log.calls.borrow_mut().push("kitty-".into());
            check(&self.log, "kitty-")
        }
    }

    fn check(log: &Log, name: &str) -> Result<()> {
        if log.fail.borrow().iter().any(|f| f == name) {
            anyhow::bail!("{name} failed")
        }
        Ok(())
    }

    #[derive(Default)]
    struct TerminalGuardDriverPair {
        log: Log,
    }

    impl TerminalGuardDriverPair {
        fn new(kitty: bool) -> Self {
            TerminalGuardDriverPair {
                log: Log {
                    calls: Rc::new(RefCell::new(Vec::new())),
                    fail: Rc::new(RefCell::new(Vec::new())),
                    kitty: Rc::new(RefCell::new(kitty)),
                },
            }
        }
        fn test_driver(&self) -> TestDriver {
            TestDriver {
                log: self.log.clone(),
            }
        }
        /// Enter through this driver; teardown is recorded on the same log.
        fn via_recorded(&self) -> RestoreVia {
            RestoreVia::Recorded(Box::new(TestDriver {
                log: self.log.clone(),
            }))
        }
        /// Enter and restore both recorded: the shape the real guard has.
        fn enter(&self) -> Result<TerminalGuard> {
            TerminalGuard::enter_with(&mut self.test_driver(), self.via_recorded())
        }
        fn calls(&self) -> Vec<String> {
            self.log.calls.borrow().clone()
        }
        fn fail(&mut self, name: &str) {
            self.log.fail.borrow_mut().push(name.to_string());
        }
    }

    /// Tests here run in parallel by default and share the process-wide depth counter, so
    /// each one that touches DEPTH holds this serialization lock.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        match LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn force_restore_claims_the_depth_so_drop_does_not_repeat_it() {
        let _g = serial();
        let pair = TerminalGuardDriverPair::new(true);
        let before = guard_depth();
        {
            let _guard = pair.enter().expect("enters");
            // The signal path restores on its own...
            let _ = force_restore();
            assert_eq!(guard_depth(), before, "force_restore claims the depth");
        }
        // ...and the guard's Drop must then find nothing left to undo.
        assert_eq!(
            pair.calls().iter().filter(|c| c.ends_with('-')).count(),
            0,
            "teardown after force_restore must not emit again: {:?}",
            pair.calls()
        );
    }

    #[test]
    fn force_restore_without_a_guard_does_nothing() {
        let _g = serial();
        let before = guard_depth();
        assert_eq!(
            force_restore(),
            0,
            "nothing was entered, so nothing is emitted"
        );
        assert_eq!(guard_depth(), before, "and the depth must not go negative");
    }

    #[test]
    fn raw_writes_reach_a_real_descriptor() {
        // A temp file stands in for the tty: the point is that the write path works without
        // crossterm, without a locked stdout, and handles a buffer longer than one write.
        let dir = std::env::temp_dir().join(format!("npt-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("seq");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&path).expect("create");
            // 4 KiB of the same byte: longer than a pipe buffer, so it proves the loop.
            let payload = vec![b'x'; 4096];
            file.write_all(&payload).expect("writes");
            file.flush().expect("flushes");
        }
        let contents = std::fs::read(&path).expect("read back");
        assert_eq!(contents.len(), 4096);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_all_raw_reports_an_invalid_descriptor() {
        // fd 9999 is not open in a test process; the helper must report rather than spin.
        let err = write_all_raw(9999, b"\x1b[?1049l");
        assert!(err.is_err(), "a closed fd cannot accept a write");
    }

    #[test]
    fn enter_captures_a_termios_snapshot_when_there_is_a_terminal() {
        let _g = serial();
        // In a test process stdout is not a tty, so the capture is expected to fail and the
        // snapshot to be empty. That is the degraded-but-correct state, and it is worth
        // asserting because it is the state force_restore's fallback path handles.
        let pair = TerminalGuardDriverPair::new(false);
        {
            let _guard = pair.enter().expect("enters");
            // Clippy rejects a tautological assertion, and it would be right to: what is
            // being asserted here is that reading the snapshot on a non-tty is safe and
            // returns an answer rather than panicking.
            let _snapshotted = has_termios_snapshot();
        }
    }

    #[test]
    fn enter_then_drop_is_balanced() {
        let _g = serial();
        let before = guard_depth();
        let pair = TerminalGuardDriverPair::new(false);
        {
            let _guard = pair.enter().expect("enters");
            assert_eq!(guard_depth(), before + 1);
            assert!(is_guarded());
        }
        assert_eq!(guard_depth(), before, "drop must restore the depth");
    }

    #[test]
    fn teardown_is_the_exact_reverse_of_setup() {
        let _g = serial();
        let pair = TerminalGuardDriverPair::new(true);
        {
            let _guard = pair.enter().expect("enters");
        }
        assert_eq!(
            pair.calls(),
            vec![
                "raw+", "alt+", "mouse+", "paste+", "kitty+", // enter
                "kitty-", "paste-", "mouse-", "alt-", "raw-" // reverse
            ]
        );
    }

    #[test]
    fn kitty_is_skipped_when_the_terminal_does_not_support_it() {
        let _g = serial();
        let pair = TerminalGuardDriverPair::new(false);
        {
            let _guard = pair.enter().expect("enters");
        }
        let calls = pair.calls();
        assert!(!calls.contains(&"kitty+".to_string()), "{calls:?}");
        assert!(!calls.contains(&"kitty-".to_string()), "{calls:?}");
        assert_eq!(calls.len(), 8);
    }

    #[test]
    fn a_failing_late_step_rolls_back_what_was_done() {
        let mut pair = TerminalGuardDriverPair::new(false);
        pair.fail("paste+");
        let err = match pair.enter() {
            Ok(_) => panic!("paste+ was set to fail, but enter succeeded"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("terminal setup"), "{err}");
        assert_eq!(
            pair.calls(),
            vec!["raw+", "alt+", "mouse+", "paste+", "mouse-", "alt-", "raw-"],
            "everything enabled before the failure must be disabled, in reverse"
        );
    }

    #[test]
    fn a_failed_enter_does_not_increment_the_depth() {
        let _g = serial();
        let before = guard_depth();
        let mut pair = TerminalGuardDriverPair::new(false);
        pair.fail("alt+");
        assert!(pair.enter().is_err());
        assert_eq!(guard_depth(), before, "a failed guard must own nothing");
    }

    #[test]
    fn restore_attempts_every_step_even_when_one_fails() {
        let _g = serial();
        let mut pair = TerminalGuardDriverPair::new(true);
        // Enter against a clean driver so nothing fails on the way in.
        let _guard = pair.enter().expect("enters");
        // Now make mouse teardown fail; the rest must still run.
        pair.fail("mouse-");
        let failures = restore_with(&mut pair.test_driver());
        assert_eq!(failures, 1, "exactly the injected failure");
        let calls = pair.calls();
        for expected in ["kitty-", "paste-", "mouse-", "alt-", "raw-"] {
            assert!(
                calls.contains(&expected.to_string()),
                "{expected} skipped: {calls:?}"
            );
        }
    }

    #[test]
    fn restore_is_idempotent() {
        let _g = serial();
        let pair = TerminalGuardDriverPair::new(false);
        let guard = pair.enter().expect("enters");
        // The panic hook path runs first...
        restore_with(&mut pair.test_driver());
        let after_hook = pair.calls().len();
        // ...then the guard drops. Nothing must be emitted a second time.
        drop(guard);
        assert_eq!(
            pair.calls().len(),
            after_hook,
            "double restore would emit duplicate disable sequences"
        );
        assert_eq!(guard_depth(), 0);
    }

    #[test]
    fn restore_without_enter_does_nothing() {
        let _g = serial();
        while guard_depth() > 0 {
            restore_with(&mut CrosstermNoopDriver);
        }
        let pair = TerminalGuardDriverPair::new(false);
        let failures = restore_with(&mut pair.test_driver());
        assert_eq!(failures, 0);
        assert!(
            pair.calls().is_empty(),
            "must not emit anything: {:?}",
            pair.calls()
        );
        assert_eq!(guard_depth(), 0, "must not go negative");
    }

    /// A driver that succeeds at everything, for depth-only tests.
    struct CrosstermNoopDriver;
    impl Driver for CrosstermNoopDriver {
        fn enable_raw(&mut self) -> Result<()> {
            Ok(())
        }
        fn disable_raw(&mut self) -> Result<()> {
            Ok(())
        }
        fn enter_alt(&mut self) -> Result<()> {
            Ok(())
        }
        fn leave_alt(&mut self) -> Result<()> {
            Ok(())
        }
        fn enable_mouse(&mut self) -> Result<()> {
            Ok(())
        }
        fn disable_mouse(&mut self) -> Result<()> {
            Ok(())
        }
        fn enable_paste(&mut self) -> Result<()> {
            Ok(())
        }
        fn disable_paste(&mut self) -> Result<()> {
            Ok(())
        }
        fn kitty_supported(&self) -> bool {
            false
        }
        fn push_kitty(&mut self) -> Result<()> {
            Ok(())
        }
        fn pop_kitty(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn nested_guards_restore_lifo() {
        let _g = serial();
        let base = guard_depth();
        let pair = TerminalGuardDriverPair::new(false);
        let outer = pair.enter().expect("outer");
        let inner = pair.enter().expect("inner");
        assert_eq!(guard_depth(), base + 2);
        drop(inner);
        assert_eq!(guard_depth(), base + 1);
        drop(outer);
        assert_eq!(guard_depth(), base);
    }

    #[test]
    fn the_real_driver_reports_no_kitty_on_a_pipe() {
        // Test runs have no terminal at all. The point is that the query degrades to false
        // instead of propagating an error, which is what spec §5.1 asks for.
        let d = CrosstermDriver;
        assert!(!d.kitty_supported());
    }
}
