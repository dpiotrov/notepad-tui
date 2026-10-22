//! Clipboard access with an OSC 52 fallback, and the primary selection (spec §6.3).
//!
//! # Why a provider rather than a direct arboard call
//!
//! `arboard::Clipboard::new()` fails on a bare SSH session with no X11/Wayland/OSX
//! clipboard owner, which is the normal environment for a terminal editor. It also fails
//! slowly. So the provider:
//!  - constructs arboard **lazily**, on the first real clipboard use, never in `new`;
//!  - **caches the failure**, so a headless machine pays the probe cost once per process
//!    rather than once per Ctrl+C;
//!  - falls back to OSC 52, which asks the *terminal* to hold the text and works over SSH
//!    with no server-side display at all;
//!  - falls back to nothing, because a clipboard is not worth failing an edit over.
//!
//! Every method returns nothing fallible for the same reason: `Ctrl+C` must never be the
//! thing that produces an error dialog.
//!
//! # Testing
//!
//! [`osc52_sequence`] is pure and byte-exact, and the provider writes OSC 52 through an
//! injected sink, so the whole fallback chain is testable with no display server and no
//! real clipboard. `App::new_headless` installs a capturing sink for exactly this.

use std::cell::RefCell;
use std::rc::Rc;

use base64::Engine;

/// How the clipboard is reached (`[ui] clipboard`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardMode {
    /// Try the system clipboard, then OSC 52.
    #[default]
    Auto,
    /// Always OSC 52, never touch the system clipboard.
    Osc52,
    /// Disable clipboard access entirely.
    None,
}

impl ClipboardMode {
    /// Parse the config value. Unknown values become `Auto`, matching the config layer's
    /// rule that a bad preference degrades to the default and warns rather than failing.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "osc52" | "osc-52" | "osc" => ClipboardMode::Osc52,
            "none" | "off" | "disabled" | "false" => ClipboardMode::None,
            _ => ClipboardMode::Auto,
        }
    }

    pub fn config_key(self) -> &'static str {
        match self {
            ClipboardMode::Auto => "auto",
            ClipboardMode::Osc52 => "osc52",
            ClipboardMode::None => "none",
        }
    }
}

/// Where a clipboard write actually went. Reported so the UI and the tests can say what
/// happened instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    System,
    Osc52,
    /// `mode = none`, or the system clipboard was unavailable and OSC 52 had nowhere to go.
    Nowhere,
}

/// The OSC 52 payload for `text`, ending with a string terminator.
///
/// Shape: `ESC ] 52 ; c ; <base64(UTF-8)> ESC \`. Clipboard buffer `c` is the *clipboard*
/// (as opposed to `p`, the primary selection).
///
/// Terminated with ST rather than BEL: both are accepted by xterm and derivatives, and ST
/// is the one that cannot be confused with an bell the terminal rings for other reasons.
///
/// No chunking. Some terminals cap the sequence length (xterm's default limit is 8 KiB of
/// OSC payload) and will silently drop an oversized copy; see `docs/DEVIATIONS.md`. Chunking
/// would need a per-terminal notion of the limit, and a truncated-but-visible paste is worse
/// than a documented limit, so the limit is stated instead.
pub fn osc52_sequence(text: &str) -> Vec<u8> {
    osc52_sequence_buffer('c', text)
}

/// OSC 52 sequence targeting an explicit buffer selection (`c` clipboard, `p` primary).
pub fn osc52_sequence_buffer(selection: char, text: &str) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = Vec::with_capacity(6 + b64.len() + 2);
    out.extend_from_slice(b"\x1b]52;");
    let mut sel = [0u8; 4];
    out.extend_from_slice(selection.encode_utf8(&mut sel).as_bytes());
    out.push(b';');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// A sink that appends to a shared buffer, so a caller can read back what would have gone
/// to the terminal. Used by headless `App` values and by tests.
pub fn capture_sink(sink: &Rc<RefCell<Vec<u8>>>) -> OscSink {
    let shared = Rc::clone(sink);
    Box::new(move |bytes: &[u8]| shared.borrow_mut().extend_from_slice(bytes))
}

/// A sink that writes straight to the real terminal's stdout.
///
/// Kept separate from `capture_sink` because the failure behavior differs: a terminal that
/// rejects the sequence must not take the editor down with it, so write errors are dropped
/// rather than propagated (a clipboard is never worth an error dialog).
pub fn stdout_sink() -> OscSink {
    use std::io::Write;
    Box::new(|bytes: &[u8]| {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        // Best effort by design. `let _ =` because the only failure is a terminal that has
        // gone away, in which case there is nowhere to report it either.
        let _ = handle.write_all(bytes);
        let _ = handle.flush();
    })
}

/// The sink that receives OSC 52 bytes. Tests install a capturing closure.
pub type OscSink = Box<dyn FnMut(&[u8])>;

/// System clipboard + OSC 52 fallback + in-process primary selection.
///
/// The system clipboard handle is created on first use and the result, success or failure,
/// is remembered. `new` does no I/O at all.
pub struct ClipboardProvider {
    mode: ClipboardMode,
    /// `None` = not probed yet, `Some(false)` = probed and broken, `Some(true)` = usable.
    system_ok: Option<bool>,
    osc: OscSink,
    /// The primary selection. `arboard` has no primary clipboard on any platform, so this
    /// is an in-process buffer: copy/paste inside one `npt` session works, and pasting into
    /// another terminal's primary does not. Documented in `docs/DEVIATIONS.md`.
    primary: RefCell<String>,
}

impl std::fmt::Debug for ClipboardProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardProvider")
            .field("mode", &self.mode)
            .field("system_ok", &self.system_ok)
            .field("primary_len", &self.primary.borrow().chars().count())
            .finish_non_exhaustive()
    }
}

impl Default for ClipboardProvider {
    fn default() -> Self {
        ClipboardProvider::new(ClipboardMode::Auto)
    }
}

impl ClipboardProvider {
    /// Build a provider. Never blocks, never fails, touches nothing external.
    pub fn new(mode: ClipboardMode) -> Self {
        ClipboardProvider {
            mode,
            system_ok: None,
            // Discards by default: a provider built without an explicit sink has nowhere to
            // put OSC 52, which is the same observable outcome as `None` mode.
            osc: Box::new(|_bytes| {}),
            primary: RefCell::new(String::new()),
        }
    }

    /// Install the writer used for OSC 52 output. The real terminal passes a function that
    /// writes to stdout; tests capture.
    pub fn with_osc_sink(mut self, sink: OscSink) -> Self {
        self.osc = sink;
        self
    }

    pub fn mode(&self) -> ClipboardMode {
        self.mode
    }

    /// True when no backend at all can serve a paste (FR-U1 "Paste grey when clipboard
    /// empty").
    ///
    /// This answers "is there a clipboard", not "is the clipboard non-empty". An OSC 52
    /// clipboard is write-only from our side -- querying it needs a reply channel through
    /// the terminal's input stream, which crossterm does not expose -- so emptiness is not
    /// knowable without pasting. The menu uses this to grey Paste out only when the user
    /// configured `clipboard = "none"`, which is the one case we can state for certain.
    pub fn is_disabled(&self) -> bool {
        self.mode == ClipboardMode::None
    }

    /// The in-process mirror's contents, without consuming anything.
    ///
    /// Used by the menu and the context menu to decide whether Paste has a plausible
    /// payload when no system backend is reachable.
    pub fn has_content(&mut self) -> bool {
        if self.is_disabled() {
            return false;
        }
        if self.mode != ClipboardMode::Osc52 && self.system_available() {
            return true;
        }
        !self.clipboard_mirror().is_empty()
    }

    /// Copy `text` to the clipboard. Returns where it landed, for tests and toasts.
    pub fn copy(&mut self, text: &str) -> ClipboardTarget {
        self.write(text, 'c')
    }

    /// Read the clipboard, if any backend can supply it.
    pub fn paste(&mut self) -> Option<String> {
        if self.mode == ClipboardMode::None {
            return None;
        }
        if self.mode != ClipboardMode::Osc52 && self.system_available() {
            if let Some(text) = system_get() {
                return Some(text);
            }
        }
        // OSC 52 *query* (the `?` form) needs a reply channel back through the terminal's
        // own input stream, which crossterm does not expose. So OSC 52 is write-only here,
        // and a paste falls back to the in-process copy. See docs/DEVIATIONS.md.
        Some(self.clipboard_mirror())
    }

    /// Copy to the primary selection (middle-click paste source, spec §6.3).
    pub fn copy_primary(&mut self, text: &str) -> ClipboardTarget {
        *self.primary.borrow_mut() = text.to_string();
        // Mirror into the clipboard buffer too, so a terminal that only wires the clipboard
        // still offers the text.
        let target = self.write(text, 'p');
        if self.mode == ClipboardMode::None {
            ClipboardTarget::Nowhere
        } else {
            target
        }
    }

    /// Read the primary selection. In-process only; see the field docs.
    pub fn paste_primary(&mut self) -> Option<String> {
        if self.mode == ClipboardMode::None {
            return None;
        }
        let stored = self.primary.borrow().clone();
        if stored.is_empty() {
            // Nothing selected in this session yet; fall back to the clipboard mirror so a
            // middle click still does something useful after an external copy.
            return self.paste();
        }
        Some(stored)
    }

    fn write(&mut self, text: &str, selection: char) -> ClipboardTarget {
        if self.mode == ClipboardMode::None || text.is_empty() {
            return ClipboardTarget::Nowhere;
        }
        if self.mode != ClipboardMode::Osc52 && self.system_available() {
            if system_set(text) {
                // Keep the mirror in step so a later paste still works if the system
                // clipboard is cleared or taken over by another process.
                if selection == 'c' {
                    self.set_clipboard_mirror(text);
                }
                return ClipboardTarget::System;
            }
            // The probe said yes and the operation said no: treat the backend as gone from
            // here on rather than re-failing on every keystroke.
            self.system_ok = Some(false);
        }
        let seq = osc52_sequence_buffer(selection, text);
        (self.osc)(&seq);
        if selection == 'c' {
            self.set_clipboard_mirror(text);
        }
        ClipboardTarget::Osc52
    }

    /// Probe the system clipboard once and remember the answer.
    fn system_available(&mut self) -> bool {
        match self.system_ok {
            Some(ok) => ok,
            None => {
                let ok = system_probe();
                self.system_ok = Some(ok);
                ok
            }
        }
    }

    // The clipboard mirror keeps `paste()` working after an OSC 52 copy, which the terminal
    // owns and we cannot read back. A one-entry cache is enough: every copy overwrites it,
    // and a paste of older text is a better outcome than a paste of nothing.
    fn clipboard_mirror(&self) -> String {
        CLIPBOARD_MIRROR.with(|m| m.borrow().clone())
    }

    fn set_clipboard_mirror(&self, text: &str) {
        CLIPBOARD_MIRROR.with(|m| *m.borrow_mut() = text.to_string());
    }
}

thread_local! {
    /// See `ClipboardProvider::clipboard_mirror`. Thread-local rather than a field because
    /// the mirror has to survive across provider values in tests that build a fresh
    /// provider per case.
    static CLIPBOARD_MIRROR: RefCell<String> = const { RefCell::new(String::new()) };
}

// ------------------------------------------------------- system clipboard
//
// Isolated behind three functions so the arboard dependency, and every failure mode it
// brings, has exactly one boundary. `np-tui` tests never reach these: they run with
// `ClipboardMode::Osc52` or `None`.

fn system_probe() -> bool {
    arboard::Clipboard::new().is_ok()
}

fn system_set(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).is_ok(),
        Err(_) => false,
    }
}

fn system_get() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}
