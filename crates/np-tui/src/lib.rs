//! `npt`'s terminal-facing half, as a library so it can be tested headless.
//!
//! The binary in `main.rs` is thin on purpose: it wires the real terminal to the `App`
//! state machine that lives here. Integration tests construct `App` through
//! `App::new_headless` and render into ratatui's `TestBackend`, so no TTY, no clipboard,
//! and no files are required (spec §10).

pub mod actions;
pub mod app;
pub mod bg;
pub mod cli;
pub mod clipboard;
pub mod event;
pub mod fif;
pub mod find;
pub mod highlight;
pub mod keymap;
pub mod logging;
pub mod mouse;
pub mod panic_hook;
pub mod session;
pub mod signal;
pub mod style;
pub mod tabs;
pub mod term;
pub mod ui;
pub mod view;

pub use actions::{Action, MenuId};
pub use app::{App, Banner, ContextKind, DialogKind, Mode, PendingConfirm, ToastQueue};
pub use clipboard::{osc52_sequence, ClipboardMode, ClipboardProvider, ClipboardTarget};
pub use find::{FindField, FindOutcome, FindState};
pub use keymap::{format_key, parse_key, Keymap};
pub use tabs::{ClosedTab, ConfirmButton, ConfirmRequest, PendingOp, TabSpan, TAB_MAX_WIDTH};
