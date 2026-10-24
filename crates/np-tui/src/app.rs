//! UI state and the single command entry point (contract §13, spec §5.1-§5.3).
//!
//! # One way in
//!
//! Every command reaches the editor through [`App::execute`]. Menus, the command palette,
//! the keymap and later the mouse all produce an `Action` and nothing else, which is why
//! behavior never depends on how something was triggered. The exception the contract
//! allows is continuous view state (drag-select, scrollbar drag), which S6 changes
//! directly because routing a 200-pixel drag through a command enum would lose the
//! intermediate states.
//!
//! # Mode is a stack, not a value
//!
//! Dialogs stack: a "discard changes?" confirm opens on top of Save As, and `Esc` must
//! close only the confirm (spec §5.2). A single `Mode` field cannot express that, so
//! `mode_stack` is a `Vec<Mode>` and `Esc` pops exactly one entry. Input routes by mode
//! first and only then consults the keymap, which is what keeps typing `s` in a text field
//! from triggering Save.
//!
//! # Headless by construction
//!
//! [`App::new_headless`] builds a fully usable `App` with no terminal, no clipboard and no
//! config file. That is not a convenience: every integration test in later stages drives
//! this constructor, and anything that reached for the real terminal here would make the
//! suite unrunnable in CI. The real terminal belongs to `main.rs` and the S4 lifecycle.
//!
//! # Clock
//!
//! `Document` methods require an injected `Instant` (see `docs/STATUS.md`). `App` holds one
//! `clock` field and advances it explicitly: production reads the real clock once per
//! event batch in `begin_batch`, tests call `advance_clock` to control undo grouping.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::{Config, Document, LineCol, RectCore, SortOrder, ViewGeometry};

use crate::actions::{Action, MenuId, SubmenuId};
use crate::clipboard::{capture_sink, ClipboardMode, ClipboardProvider};
use crate::keymap::Keymap;
use crate::tabs::{self, ClosedTab, ConfirmButton, ConfirmRequest, PendingOp};

/// Which dialog is on top of the mode stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Open,
    SaveAs,
    Find,
    Replace,
    GoTo,
    Preferences,
    About,
    Confirm,
    LanguagePicker,
    ThemePicker,
    DocumentList,
    BinaryConfirm,
    CloseAllConfirm,
    /// The Encoding dropdown opened from the status bar or the menu (FR-N3).
    EncodingPicker,
    /// The EOL conversion menu (FR-N4).
    EolMenu,
    /// The Recent Files list (FR-F8).
    RecentFiles,
}

/// Which context menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    Editor,
    Tab,
}

/// A UI input mode (spec §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Editing,
    /// A dropdown in the menu bar.
    ///
    /// `parent` is `None` for one of the nine top-level menus and `Some(submenu)` for a submenu
    /// opened from the menu above it. A submenu shares its parent's `MenuId`, so `Encoding`
    /// open inside `Encoding` used to be indistinguishable from `Encoding` open at all: the
    /// renderer resolved the parent's item list for both levels and painted the same dropdown
    /// twice on the same rows, which is why a row with a submenu arrow looked dead.
    Menu {
        open: MenuId,
        item: usize,
        parent: Option<SubmenuId>,
    },
    Dialog(DialogKind),
    ContextMenu(ContextKind),
    Palette,
    HelpOverlay,
    /// The Find in Files panel owns the keyboard (S11, FR-FF2).
    ///
    /// A mode rather than a flag because the panel is focusable and takes over input the
    /// editor would otherwise get: typing goes to the query field, and `Esc` cancels a running
    /// search before it closes the panel. It is not a `DialogKind` because the panel is docked
    /// rather than modal -- results stay visible while the editor is still usable.
    FindPanel,
}

/// A transient toast (FR-U6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub message: String,
    /// `true` for errors, which the renderer shows in the error color.
    pub error: bool,
}

/// Bounded queue of notifications.
///
/// The cap exists because a burst of failures (a find-in-files run over an unreadable tree)
/// would otherwise grow without limit and push the editor off screen. Oldest are dropped.
#[derive(Debug, Clone, Default)]
pub struct ToastQueue {
    items: Vec<Toast>,
    pub capacity: usize,
}

impl ToastQueue {
    pub fn new() -> Self {
        ToastQueue {
            items: Vec::new(),
            capacity: 5,
        }
    }

    pub fn push(&mut self, message: impl Into<String>) {
        self.push_inner(Toast {
            message: message.into(),
            error: false,
        });
    }

    pub fn push_error(&mut self, message: impl Into<String>) {
        self.push_inner(Toast {
            message: message.into(),
            error: true,
        });
    }

    fn push_inner(&mut self, toast: Toast) {
        if self.items.len() >= self.capacity.max(1) {
            self.items.remove(0);
        }
        self.items.push(toast);
    }

    pub fn pop(&mut self) -> Option<Toast> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.items.remove(0))
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// The disk-change notice (FR-F7): a persistent banner rather than a toast, because the
/// user must acknowledge it before their save overwrites somebody else's edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub message: String,
}

/// A document the editor holds open.
pub type Tab = Document;

/// The whole mutable UI state, owned by the UI thread.
#[derive(Debug)]
pub struct App {
    pub documents: Vec<Tab>,
    pub active: usize,
    pub mode_stack: Vec<Mode>,
    pub config: Config,
    pub keymap: Keymap,
    /// Set when anything visible changed; the render loop clears it after drawing.
    pub dirty: bool,
    pub quit_requested: bool,
    pub toasts: ToastQueue,
    pub banner: Option<Banner>,
    /// Injected clock, threaded into every `Document` call.
    pub clock: Instant,
    /// Overtype for the *active* tab. Mirrored onto `Document::overtype` so per-tab state
    /// travels with the tab; this field is the value new tabs inherit.
    pub overtype: bool,
    /// Computed editor geometry, kept so hit-testing (S6) does not redo the layout.
    pub geometry: ViewGeometry,
    /// The four screen regions from the last layout pass, for hit-testing and the
    /// renderer's own bookkeeping.
    pub regions: Regions,
    /// Number of untitled documents created, for "Untitled-N" naming (FR-F1).
    pub untitled_counter: usize,
    /// Whether the menu bar currently owns the keyboard (F10 / Alt).
    pub menu_bar_active: bool,
    /// Index of the document waiting for a Save-As path (FR-F3).
    ///
    /// An untitled document has no path, so `Save` cannot complete; it records which tab
    /// needs one and leaves it here for the Save As dialog (S14) to consume. Setting a flag
    /// rather than showing a dialog is what keeps this stage free of UI widgets.
    pub pending_save_as: Option<usize>,
    /// Document awaiting a "discard changes?" answer, set by Close/Exit on a modified tab.
    pub pending_confirm: Option<PendingConfirm>,
    /// The save-changes prompt currently owed to the user, and what it is standing in
    /// front of (FR-F5).
    ///
    /// `pending_confirm` above is the S3 shape: an index and nothing else, which was
    /// enough for a yes/no banner but cannot express `[Save] [Don't save] [Cancel]` or
    /// remember that the prompt was raised by Close All rather than Close. `confirm` is
    /// the real state and every new path writes here; `pending_confirm` stays as a
    /// derived view so the S3/S6 tests and S14's dialog keep compiling while both exist.
    pub confirm: Option<ConfirmRequest>,
    /// Recently closed documents, for Reopen Closed Tab (FR-U2).
    ///
    /// Ordered oldest first, so a push appends and a trim drops from the front. Holds the
    /// unsaved text of any document that had it: closing a tab is a one-keystroke
    /// operation, and losing work to it would make `Ctrl+W` a command to fear.
    pub closed_tabs: Vec<ClosedTab>,
    /// Recently used file paths, most recent first (FR-F8). S14's menu reads
    /// [`App::recent_files`].
    pub recent: Vec<std::path::PathBuf>,
    /// Where `session.toml` is written, or `None` when session support is off.
    ///
    /// Held on `App` rather than re-derived from `dirs` each save, so a test can point the
    /// whole session at a tempdir and count writes.
    pub session_path: Option<std::path::PathBuf>,
    /// Where `config.toml` is written, or `None` when preference persistence is off.
    ///
    /// The sibling of `session_path`, and for the same reason: FR-CG5 says a preference edit
    /// persists immediately, and the only way to prove that in a test is to point it at a
    /// tempdir and read the file back. `main.rs` sets it from `--config` or the default
    /// location; a headless `App` leaves it `None` so a test that only exercises behavior
    /// never writes to the real user config.
    pub config_path: Option<std::path::PathBuf>,
    /// A session save is owed. Set by anything that changes restorable state, cleared by a
    /// flush; this is what turns a thousand keystrokes into one write.
    pub session_dirty: bool,
    /// How many session writes this process has performed. A debounce assertion counts this
    /// rather than watching mtimes, which a fast filesystem can make identical.
    pub session_writes: usize,
    /// When autosave last ran, so the interval is measured from an actual save.
    pub last_autosave: Option<std::time::Instant>,
    /// A Close Others / Close All batch paused on a save prompt, so answering can resume it.
    ///
    /// Storing the operation rather than a list of surviving indices means the resume
    /// recomputes the set from the current document list. A snapshot list would go stale the
    /// moment one tab closed, and closing the wrong tab is worse than closing none.
    pub pending_batch: Option<PendingOp>,
    /// Selected row in the Document List dialog (FR-U4).
    ///
    /// Lives on `App` rather than in the dialog module because the dialog is stateless
    /// rendering over this index, the same way the menu bar reads `menu_bar_active`.
    pub document_list_selected: usize,
    /// Background work handle: Find in Files and large-file loads (S11).
    ///
    /// Cheap to clone and shared with any clone of `App`, so a forked `App` in a test sees the
    /// same queue. Holds no editor state; see [`crate::bg`].
    pub bg: crate::bg::BgWorker,
    /// Find in Files panel state (S11, FR-FF1..FF3).
    pub fif: crate::fif::FifState,
    /// The `request_id` of a pending large-file load (FR-F10), or `None`.
    ///
    /// A load that was superseded must not push a document in front of the user, so the id is
    /// checked at the drain exactly like a search's.
    pub pending_load_request: Option<usize>,
    /// Run Find in Files on the calling thread instead of spawning a worker.
    ///
    /// Set by `new_headless`. Without it a test cannot assert on a search's results without
    /// either joining a thread it does not hold or racing the queue, and a sleep-based test is
    /// one slow machine away from a false failure. Production leaves it false, and the worker
    /// path has its own test that uses a real thread.
    pub sync_search: bool,

    /// Syntax highlighting engine (S12, FR-H1).
    ///
    /// Held directly rather than behind an `Arc`, because FR-H3 requires switching the theme
    /// at runtime and `set_theme` needs `&mut`. The `App::clone` cost of reloading both sets
    /// is about 1.2 ms, which is fine: clones happen in tests, not in the frame path.
    pub highlight: np_core::HighlightEngine,
    /// Per-document highlight caches (S12, FR-H4).
    ///
    /// Deliberately not a field on `Document`: contract §7 forbids persisting parse state, and
    /// `Document` is cloned by undo and serialized by session. See `crate::highlight`.
    pub hl_store: crate::highlight::HighlightStore,
    /// Text being typed into the Language picker (S12, FR-H2).
    pub lang_query: String,
    /// Row cursor within the filtered Language picker.
    pub lang_selected: usize,
    /// Scroll offset of the filtered Language picker.
    pub lang_scroll: usize,
    /// Text being typed into the Theme picker (S12, FR-H3).
    pub theme_query: String,
    /// Row cursor within the filtered Theme picker.
    pub theme_selected: usize,
    /// Scroll offset of the filtered Theme picker.
    pub theme_scroll: usize,

    /// The encoding the Reload-As dialog should preselect (FR-N3). S14 reads it.
    pub pending_reload_encoding: Option<np_core::EncodingId>,
    /// Multi-click synthesis state (spec §6.9 rows 4-5).
    ///
    /// crossterm reports no double- or triple-click event, only `Down(Left)`, so the count
    /// has to be accumulated here. Kept on `App` rather than in `mouse.rs` so `route()` stays
    /// a pure function: tests pass a `now` and an explicit `ClickState` and get a
    /// deterministic answer with no clock and no hidden state.
    pub click: ClickState,
    /// The cell the pointer last hovered, for menu/tab highlighting (row 20).
    ///
    /// Stored rather than redrawn on the spot: `route` is pure, so the hover change is an
    /// intent that `App::handle_mouse` applies, and the render pass reads it from here.
    pub hover: Option<HoverTarget>,
    /// The selection anchor captured when a drag began, so extending during the drag keeps
    /// the original anchor instead of jumping to the newest cell.
    pub drag_anchor: Option<LineCol>,
    /// The line a gutter drag started on (§6.9 row 11), `None` when no gutter drag is active.
    ///
    /// Separate from `drag_anchor` because the two must not interfere: a text drag anchors on a
    /// caret position and a gutter drag anchors on a line, and a stale gutter anchor leaking into
    /// a text drag would select from an unrelated line. It is cleared by any press that is not a
    /// gutter press, the same way `drag_anchor` is cleared by a plain click.
    pub gutter_drag_anchor: Option<usize>,
    /// Which scrollbar is being dragged and how far into the thumb the grab landed (§6.9 row 14).
    ///
    /// A thumb grab has to remember *where in the thumb* the pointer took hold of it. Without
    /// that, a drag event maps the pointer's cell to the offset whose thumb *starts* there, so
    /// the content slides under the pointer by the grab depth on the first move -- the thumb
    /// jumps out from under the mouse and the further you drag the worse the error. `offset` is
    /// the document offset at the moment of the grab and `cell` the cell it was grabbed at, so a
    /// later drag adds the cells moved to the offset the grab started from.
    ///
    /// It is a move delta rather than a re-resolved absolute offset because the thumb's own
    /// position changes as the document scrolls: resolving the pointer against the *new*
    /// geometry each event feeds the movement back into itself, and a slow drag creeps at half
    /// the rate the pointer moves.
    pub scrollbar_drag: Option<ScrollbarDrag>,
    /// A left press the open dialog is holding, as the child it landed on and -- when that child
    /// is a list row -- the absolute row it meant.
    ///
    /// The press and the release are two events, and a click is defined only by their agreement.
    /// Without recording the press there is no way to tell a click from a drag that started on a
    /// row and slid away, and so no way to let a press select while a release decides.
    ///
    /// The row is stored alongside the child index because the index cannot be re-read at release
    /// time. Selecting a row moves the list's window, so the window the release resolves against
    /// is not the window the press resolved against, and the same index in it names a different
    /// row. The row number is the fact; the index is a position in a view of it.
    pub dialog_press: Option<DialogPress>,
    /// Rows the Find-in-Files panel occupies at the bottom of the editor (S11 builds it).
    ///
    /// The height rather than a rectangle, because it is all the mouse layer needs: the panel
    /// spans the editor's x/width and sits `height` rows above the editor bottom. Zero means
    /// no panel, which is every state until S11 exists.
    pub find_panel_height: u16,
    /// Find/Replace state, persisting across dialog closes (S10, FR-S1).
    pub find: crate::find::FindState,
    /// The Go To dialog's field text (S10, FR-S6). Lives on `App` rather than in the widget so
    /// the dialog survives a resize and a test can drive it without a terminal.
    pub goto_input: String,
    /// The Go To dialog's inline error, if the current text is not a position.
    pub goto_error: Option<String>,
    /// The document position a context menu was opened at, for S14's menu items.
    ///
    /// Kept separate from the caret because row 18 forbids moving the caret to show a menu:
    /// right-clicking to copy must not relocate the cursor the user was about to type at.
    pub context_menu_target: Option<LineCol>,
    /// Clipboard access, including the in-process primary selection (spec §6.3).
    ///
    /// Owned by `App` rather than being a global so a headless test gets its own provider
    /// with a capturing OSC sink, and so two `App` values in one process cannot see each
    /// other's copies.
    pub clipboard: ClipboardProvider,
    /// Where the last OSC 52 write went, captured for tests and for a later status readout.
    ///
    /// `App` owns the provider, so the sink has to write somewhere reachable; a `RefCell`
    /// keeps it cheap and single-threaded like the rest of `App`.
    pub osc_out: Rc<RefCell<Vec<u8>>>,

    // ------------------------------------------------------------ S14 chrome
    //
    // One block rather than fields scattered through the struct, because S14 added a dozen
    // pieces of dialog state and a reader trying to find "the rest of the dialog fields"
    // should not have to hunt.
    /// Which child of the current dialog has focus (FR-U4).
    ///
    /// An index into the dialog's declared `FocusItem` list rather than a widget handle:
    /// dialogs are data rendered on demand, and `App` holds no widget tree. Reset to the
    /// dialog's default item on open, so a dialog that opens over another does not inherit
    /// an index that means something else in the new child list.
    pub dialog_focus: usize,
    /// The row cursor of the currently open list dialog, by dialog kind.
    ///
    /// Every list dialog already had a row cursor -- `file_selected`, `lang_selected`,
    /// `theme_selected`, `document_list_selected`, or `dialog_focus` for the choice lists -- and
    /// they were five separate fields with the same job. That is what forced `focused_id` into
    /// a three-way reconciliation and made `dialog_focus` mean two things at once: an index into
    /// the Tab order *and* a row number. This is the single place a row cursor lives while a
    /// dialog is open, so the framework can move it without knowing which dialog it is.
    ///
    /// The per-dialog fields stay authoritative when a dialog is *not* open, and are synced on
    /// open and close, so session persistence and the S8/S12 code that reads them directly are
    /// unaffected.
    pub dialog_row: usize,
    /// The first visible row of the open dialog's list.
    ///
    /// A scroll offset that belongs to the *view*, not to the selection. It used to not exist:
    /// the first visible row was derived from the cursor through `framework::list_window`, which
    /// meant the only way to scroll a dialog was to move its selection, and the only way to keep
    /// that from re-selecting something was to park the cursor on the window's last row. The
    /// renderer draws the selection bar *at* the cursor, so a parked cursor painted the highlight
    /// at the bottom of the box, and because `list_window` answers 0 for every cursor below
    /// `visible` the derivation is not invertible -- repeated notches computed a window the cursor
    /// already produced and the list stopped moving. Reported as "selection jumps to the bottom
    /// item regardless of wheel direction" and as a wheel that scrolls once and then dies.
    ///
    /// Reset on dialog open and whenever the listing is re-read, because a new listing has no
    /// history to resume.
    pub dialog_scroll: usize,
    /// The first visible row of the command palette's entry list.
    ///
    /// The palette is a `Mode::Palette`, not a `Mode::Dialog`, so it keeps its own copy of the
    /// same two pieces of state for the same reason.
    pub palette_scroll: usize,
    /// The row the pointer most recently chose, if that choice still stands (FR-U5).
    ///
    /// Cleared whenever the listing is re-read, whenever focus lands on something that is not
    /// a row, and on every dialog open. It exists because the alternative -- reading a click
    /// back out of `dialog_focus` later -- reads an index into a window of rows, and the
    /// window moves. See `App::focused_id`.
    pub pointer_row: Option<usize>,
    /// The Open / Save As path field (FR-F2, FR-U5).
    pub file_input: crate::ui::text_input::TextInput,
    /// Which file-picker dialog is open, so the same field serves both.
    pub file_purpose: FilePurpose,
    /// The directory the picker last showed, so the next Open starts where the user was
    /// (FR-F2 "remember last directory").
    pub last_dir: Option<std::path::PathBuf>,
    /// Show dotfiles in the file picker (FR-F2 hidden-files toggle).
    pub show_hidden: bool,
    /// Row cursor in the file picker's list.
    pub file_selected: usize,
    /// Whether the user actually chose a listing row, as opposed to the picker having opened
    /// with its cursor resting on row 0 (FR-F2, FR-U5).
    ///
    /// # Why existence is not evidence
    ///
    /// `file_selected` always holds a valid row: the picker opens with it at 0 and every listing
    /// re-read clamps it. So "is a row selected" is always true and answers the wrong question.
    /// `[Open]`/`[Save]`/Enter need to know whether the user *chose* one, because the two answers
    /// are genuinely different interactions:
    ///
    /// - Click `hotel.txt`, press `[Open]` -- open `hotel.txt`. The highlight is the choice.
    /// - Type `/tmp/notes.txt` into the field, press Enter -- open `notes.txt`. The typed path is
    ///   the choice, and the untouched highlight on `../` must not hijack it. This case is not
    ///   hypothetical; it is how the picker has always worked, and it is covered by tests.
    ///
    /// # What sets and clears it
    ///
    /// Set by a click on a row and by a keyboard row move. Cleared when the listing is re-read
    /// (a new directory has new rows, so an old row number means nothing) and when the path field
    /// is edited (typing supersedes pointing). Cleared on open.
    pub file_row_chosen: bool,
    /// The file picker's entries, resolved when it opens or the directory changes.
    pub file_entries: Vec<crate::ui::file_picker::Entry>,
    /// Inline error for the file picker, shown under the field.
    pub file_error: Option<String>,
    /// The row a dialog's wheel left *unselected*: the choice the user made before scrolling.
    ///
    /// `pointer_row` already means "the row the pointer chose", but it is cleared by every
    /// keyboard row move and by every focus landing on a non-row, and its documented contract is
    /// that it does not survive a re-read of the listing. The wheel needs a choice that survives
    /// precisely that, because the wheel is the gesture that moves the listing. Storing it in
    /// `pointer_row` anyway would overload a field whose clearing rules belong to the keyboard,
    /// and storing it in `dialog_focus` would be worse: the choice lists read that field directly
    /// as a *window index* when they draw (`ui::dialogs::draw_choice`), and a window index cannot
    /// hold a row that is off-screen -- which is the whole case here.
    ///
    /// So this is one field with one job: hold the user's chosen row across wheel scrolling, and
    /// nothing else. Cleared when the keyboard takes over row movement, when a click chooses a new
    /// row, and on every dialog open.
    pub wheel_choice: Option<usize>,
    /// Row cursor in the command palette (FR-U8).
    pub palette_selected: usize,
    /// The palette's filter text.
    pub palette_query: String,
    /// Scroll offset of the help overlay (FR-U9). Its own field because the overlay is
    /// scrollable and `dialog_focus` is already spending itself on child focus.
    pub help_scroll: usize,
    /// The cell the pointer was at during the most recent press (§6.9 row 18).
    ///
    /// `route` is pure and receives only the `App`, so an intent cannot carry the pointer's
    /// coordinates into `handle_mouse`. Recording them at the top of `handle_mouse` is the
    /// smallest change that lets the context menu open under the cursor.
    pub last_pointer: (u16, u16),
    /// The cell a context menu was opened at, for clamping its box to the pointer (§6.9 row
    /// 18). Separate from `context_menu_target`, which holds the *document* position; this
    /// holds the *screen* position, and conflating them would put the menu at a column
    /// number that happens to match a character offset.
    pub context_menu_cell: (u16, u16),
    /// Which entry of a stacked submenu is highlighted, indexed from its parent item.
    ///
    /// `-1` means no submenu is open. Encoding's two levels are the only nesting in the bar,
    /// and a second `Mode::Menu` on the stack is how the submenu is actually displayed, so
    /// this only records the parent's intent for the arrow highlight.
    pub submenu_open: bool,
    /// The Preferences dialog's row cursor (FR-CG5).
    pub prefs_selected: usize,
    /// A Preferences field being edited inline, with the row it belongs to.
    pub prefs_edit: Option<(usize, crate::ui::text_input::TextInput)>,
    /// The codec a Convert To / Reload As menu item asked for (FR-N3).
    ///
    /// `Action::ConvertEncoding` carries no parameter by contract, and the codec list is not
    /// keyboard-addressable, so the menu records the choice here and `execute` reads it.
    pub pending_encoding: Option<np_core::EncodingId>,
    /// Toasts with the clock reading that queued them (FR-U6).
    ///
    /// Kept alongside `toasts` rather than replacing it: `toasts` is the queue every error
    /// path in S3..S13 already writes to, and `push_toast` mirrors an entry into this list
    /// with a timestamp. Expiry trims this list and rebuilds `toasts` from what survives, so
    /// there is exactly one place time is consulted.
    pub timed_toasts: Vec<crate::ui::toast::TimedToast>,
    /// The banner strip's button hover/selection state (FR-F7, FR-U10).
    ///
    /// Two buttons, `[Reload]` and `[Ignore]`; `0` is Reload. Enter on a banner runs the
    /// selected one, which is what makes the notice operable without a mouse.
    pub banner_button: usize,
}

/// Why the file picker is open, so one widget serves Open and Save As (FR-U5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePurpose {
    /// Choosing a file to open.
    Open,
    /// Choosing a path to write the active document to.
    SaveAs,
}

impl Clone for App {
    /// Copy the editor state, giving the clone a **fresh** clipboard with a capturing sink.
    ///
    /// Two things force a hand-written impl. The provider holds a boxed closure, which is not
    /// `Clone`. And sharing clipboard state between two `App` values would be wrong anyway:
    /// the provider is session state tied to a terminal, and a cloned `App` is a separate
    /// editing session (tests use `clone` to fork state), not a second view onto the same
    /// clipboard.
    fn clone(&self) -> App {
        let osc_out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let mode = self.clipboard.mode();
        App {
            documents: self.documents.clone(),
            active: self.active,
            mode_stack: self.mode_stack.clone(),
            config: self.config.clone(),
            keymap: self.keymap.clone(),
            dirty: self.dirty,
            quit_requested: self.quit_requested,
            toasts: self.toasts.clone(),
            banner: self.banner.clone(),
            clock: self.clock,
            overtype: self.overtype,
            geometry: self.geometry,
            regions: self.regions,
            untitled_counter: self.untitled_counter,
            menu_bar_active: self.menu_bar_active,
            pending_save_as: self.pending_save_as,
            pending_confirm: self.pending_confirm,
            confirm: self.confirm.clone(),
            closed_tabs: self.closed_tabs.clone(),
            recent: self.recent.clone(),
            session_path: self.session_path.clone(),
            config_path: self.config_path.clone(),
            session_dirty: self.session_dirty,
            session_writes: self.session_writes,
            last_autosave: None,
            pending_batch: self.pending_batch,
            document_list_selected: self.document_list_selected,
            pending_reload_encoding: self.pending_reload_encoding.clone(),
            bg: self.bg.clone(),
            fif: self.fif.clone(),
            pending_load_request: self.pending_load_request,
            sync_search: self.sync_search,
            highlight: np_core::HighlightEngine::load(),
            // Caches are derived state. A clone starts cold rather than sharing one `mut`
            // cache between two apps, which would make a test's draws interfere.
            hl_store: crate::highlight::HighlightStore::new(),
            lang_query: self.lang_query.clone(),
            lang_selected: self.lang_selected,
            lang_scroll: self.lang_scroll,
            theme_query: self.theme_query.clone(),
            theme_selected: self.theme_selected,
            theme_scroll: self.theme_scroll,
            click: self.click,
            hover: self.hover,
            drag_anchor: self.drag_anchor,
            gutter_drag_anchor: self.gutter_drag_anchor,
            scrollbar_drag: self.scrollbar_drag,
            dialog_press: self.dialog_press,
            find_panel_height: self.find_panel_height,
            find: self.find.clone(),
            goto_input: self.goto_input.clone(),
            goto_error: self.goto_error.clone(),
            context_menu_target: self.context_menu_target,
            clipboard: ClipboardProvider::new(mode).with_osc_sink(capture_sink(&osc_out)),
            osc_out,
            dialog_focus: self.dialog_focus,
            dialog_row: self.dialog_row,
            dialog_scroll: self.dialog_scroll,
            palette_scroll: self.palette_scroll,
            pointer_row: self.pointer_row,
            wheel_choice: self.wheel_choice,
            file_input: self.file_input.clone(),
            file_purpose: self.file_purpose,
            last_dir: self.last_dir.clone(),
            show_hidden: self.show_hidden,
            file_selected: self.file_selected,
            file_row_chosen: self.file_row_chosen,
            file_entries: self.file_entries.clone(),
            file_error: self.file_error.clone(),
            palette_selected: self.palette_selected,
            palette_query: self.palette_query.clone(),
            help_scroll: self.help_scroll,
            context_menu_cell: self.context_menu_cell,
            last_pointer: self.last_pointer,
            submenu_open: self.submenu_open,
            prefs_selected: self.prefs_selected,
            prefs_edit: self.prefs_edit.clone(),
            pending_encoding: self.pending_encoding.clone(),
            timed_toasts: self.timed_toasts.clone(),
            banner_button: self.banner_button,
        }
    }
}

/// Where the pointer is hovering, for highlight purposes only (spec §6.9 row 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverTarget {
    Tab(usize),
    Menu(usize),
    /// Hovering the text area. Nothing is highlighted today, but tracking it means S14's
    /// menu can dim the editor and a later stage can add hover tooltips without re-plumbing.
    Text,
}

/// An in-progress scrollbar thumb drag.
///
/// Only two pieces of state survive between the press and a following drag event: which cell the
/// button went down on, and which bar. That is deliberately minimal. The drag's offset is a
/// function of the pointer's *current* cell alone -- see `ScrollScope::offset_at_cell` -- so
/// nothing else needs to be remembered, and in particular the view's offset at grab time is *not*
/// added to the pointer's travel. That addition is what capped a drag's reach at the length of the
/// trough, which made the ends of a long file effectively unreachable by mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarDrag {
    /// Which bar. A drag on the vertical bar must not move the horizontal scroll, and vice
    /// versa, even though both are "dragging a scrollbar".
    pub kind: crate::mouse::VerticalOrHorizontal,
    /// Whether the button went down on the thumb or on bare trough.
    ///
    /// The two drags are different gestures and cannot share one rule:
    ///
    ///  - **A thumb grab moves by travel.** The anchor is where the view already was, so the
    ///    document follows the pointer's displacement and never teleports on contact. This is the
    ///    fix for "on click it jumps upside and moves with an increased offset upper than the
    ///    mouse pointer".
    ///  - **A trough press keeps aiming.** The pointer's absolute cell stands for a position, so a
    ///    press in the trough's last cell reaches the end of a long file in one gesture.
    ///
    /// Worth being precise about how much this flag actually changes: for the *drag*, nothing. A
    /// trough press records the pressed cell's own aimed offset as its anchor, so the relative
    /// formula reproduces the absolute one exactly -- cells 10, 14, 16 and 18 give 990, 1386, 1584
    /// and 1782 either way, verified numerically. The distinction lives entirely in the press,
    /// where a thumb refuses to aim. The flag is kept because it states which anchor a grab
    /// carries and because the sticky-end rule applies to thumb drags only; it is not, as it first
    /// looked, two dragging algorithms.
    pub on_thumb: bool,
    /// The track cell the button went down on, which a thumb drag measures its travel from.
    ///
    /// Deliberately the *press* cell, never the pointer's current one. Rewriting it on each drag
    /// event leaves every event exactly one cell from the last, so travel never accumulates and
    /// the document stops moving however far the mouse goes -- measured as an offset pinned at 901
    /// while the pointer crossed ten cells. The live position arrives on each event and needs no
    /// home here, which is why there is no companion `cell` field: it had no reader once travel
    /// was measured correctly, and keeping two cells that must not be confused is how the bug
    /// came back.
    pub start_cell: u16,
    /// The offset the bar was at when the button went down.
    ///
    /// The second half of the anchor, and frozen for the same reason. Threading the current
    /// offset through instead makes the anchor chase the view, so each event's travel is measured
    /// from where the last one landed -- measured as 1000, 1049, 999, 949, ... with the view stuck
    /// near where it started.
    pub start_offset: usize,
}

/// A left button held down inside the open dialog.
///
/// `child` is the focus index the press landed on, used to decide whether the release is a click
/// on a control or a drag that slid away. `row` is the absolute data row behind it, carried
/// because a list's visible window moves when the press selects a row, and an index into a window
/// that has moved no longer names the row that was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogPress {
    /// Which focusable child of the dialog the button went down on.
    pub child: usize,
    /// The absolute list row that child stood for, or `None` for a field, button, or checkbox.
    pub row: Option<usize>,
}

/// Accumulated click count at one cell, for double/triple click (rows 4-5).
///
/// Counted by terminal cell. Since S22 this covers list rows as well as buffer text, and it stays
/// cell-keyed for a reason: selecting a row re-centres the list's window, so a row-index key would
/// treat a deliberate double-click near a window edge as two clicks on two different entries. See
/// `mouse::next_click_state`. Which entry an activation opens is carried by the intent, resolved
/// from the press, so the count never has to identify a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickState {
    /// What the last click was on. `None` when the last click was not on anything countable.
    pub cell: Option<(u16, u16)>,
    /// How many clicks in a row at that target, 1 for a plain click.
    pub count: u32,
    /// When the most recent contributing click happened.
    pub at: Instant,
}

impl Default for ClickState {
    fn default() -> Self {
        ClickState {
            cell: None,
            count: 0,
            at: Instant::now(),
        }
    }
}

impl ClickState {
    /// The click count for a `Down` at `cell` happening at `now`, and the state to store.
    ///
    /// Same cell within `CLICK_MULTI_WINDOW` increments; anything else restarts at 1. The
    /// window is measured from the previous click rather than the first of the sequence, so
    /// a slow triple click still works, which is what users expect from a text editor.
    pub fn register(&self, cell: (u16, u16), now: Instant) -> (u32, ClickState) {
        let same_cell = self.cell == Some(cell);
        let within_window = now.saturating_duration_since(self.at) <= CLICK_MULTI_WINDOW;
        let count = if same_cell && within_window {
            self.count.saturating_add(1)
        } else {
            1
        };
        (
            count,
            ClickState {
                cell: Some(cell),
                count,
                at: now,
            },
        )
    }
}

/// Largest payload treated as a block (rectangular) paste (spec FR-E3, FR-E13).
///
/// Generous for a real rectangle -- a 200x80 block is a few KB -- and far below anything a
/// user pastes from a file, which is the case that must stay fast.
pub const MAX_BLOCK_PASTE_BYTES: usize = 256 * 1024;

/// How close together two clicks at the same cell must be to count as one multi-click.
///
/// Named rather than inlined because both the synthesis and its tests depend on the exact
/// value, and 250 ms is the conventional X11 double-click interval.
pub const CLICK_MULTI_WINDOW: Duration = Duration::from_millis(250);

/// A confirmation the UI owes the user, and what to do about the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingConfirm {
    /// Discard the changes in this document and close it.
    CloseTab(usize),
    /// Discard everything and quit.
    Quit,
}

/// The four rendered regions in the spec §5.4 stacking order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regions {
    pub menu: RectCore,
    pub tabs: RectCore,
    /// Where text is laid out and drawn: the editor band minus any docked panel.
    ///
    /// `editor` shrank when S19 reserved rows for Find in Files, and every consumer that used to
    /// read it still wants "where the text is" -- the painter, the gutter, the scrollbars, the
    /// caret, hit-testing. That is this field.
    pub editor: RectCore,
    pub status: RectCore,
    /// The whole editor band: `editor` plus whatever a docked panel took from its bottom.
    ///
    /// The docked Find in Files panel is positioned against this, not against `editor`, because
    /// `editor` is already reduced by the panel. Reading the panel's height out of `editor` and
    /// then shrinking `editor` again is a circular definition, and `fif::editor_room` would end up
    /// compounding it every time the panel resized.
    pub band: RectCore,
}

/// The fewest text rows a docked Find in Files panel may leave behind.
///
/// Shared by `recompute_geometry`, which takes the panel's rows out of the editor, and
/// `fif::editor_room`, which decides how tall the panel may be. Two numbers for one limit is how
/// a panel ends up taller than the room the text can spare.
pub const FIF_MIN_EDITOR_ROWS: u16 = 3;

impl Regions {
    /// The whole screen the regions collectively own: menu top through status bottom.
    ///
    /// Hit-testing asks this first so a pointer outside every region is `Outside` rather than
    /// falling through to whichever region happens to be checked last.
    pub fn bounds(&self) -> RectCore {
        let top = self.menu.y;
        let bottom = self
            .menu
            .bottom()
            .max(self.tabs.bottom())
            .max(self.band.bottom())
            .max(self.editor.bottom())
            .max(self.status.bottom());
        let width = self
            .menu
            .w
            .max(self.tabs.w)
            .max(self.editor.w)
            .max(self.status.w);
        RectCore::new(0, top, width, bottom.saturating_sub(top))
    }

    /// True when `(x, y)` is inside any region.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.menu.contains(x, y)
            || self.tabs.contains(x, y)
            || self.editor.contains(x, y)
            || self.status.contains(x, y)
    }
}

impl App {
    /// A working `App` with **no terminal, no clipboard and no files** (contract §13).
    ///
    /// Opens a single empty untitled document so the editor is never in the "no tabs"
    /// state, which would give every later test a special case to handle.
    pub fn new_headless(config: Config, area: (u16, u16)) -> App {
        let keymap = Keymap::from_config_overrides(&config.keymap);
        // Headless means "no real terminal", so OSC 52 output is captured rather than
        // written. `main.rs` builds its `App` with a sink that writes to the terminal.
        let osc_out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let clipboard_mode = ClipboardMode::parse(&config.ui.clipboard);
        // Read before `config` moves into the struct: find state seeds from `[find]`.
        let find = crate::find::FindState::from_config(&config);
        // Read before `config` moves in: the configured theme has to be applied to the engine,
        // and an unknown name has to be reported.
        //
        // This is a fix, not a convenience. `config.ui.theme` is written to config.toml, shown in
        // the About panel, and editable in Preferences -- and was never read at startup. Every
        // session began on np-core's `DEFAULT_THEME`, so a user who set `theme = "Dracula"` by
        // hand got base16-ocean until Preferences happened to push the value. A theme list nobody
        // can select from config would make shipping themes pointless.
        //
        // An unknown name is reported rather than ignored. `set_theme` keeps the previous theme on
        // rejection, so a typo costs a color choice and not the session, but the user is told
        // which name was wrong instead of wondering why the editor ignored their config.
        // An empty name means "not set", which is not a name that failed -- so it is not
        // reported. `set_theme` returns false only for a name it does not recognise.
        let mut highlight = np_core::HighlightEngine::load();
        let unknown_theme = if config.ui.theme.is_empty() {
            None
        } else {
            (!highlight.set_theme(&config.ui.theme)).then(|| config.ui.theme.clone())
        };
        let mut app = App {
            documents: Vec::new(),
            active: 0,
            mode_stack: vec![Mode::Editing],
            config,
            keymap,
            dirty: true,
            quit_requested: false,
            toasts: ToastQueue::new(),
            banner: None,
            clock: Instant::now(),
            overtype: false,
            geometry: ViewGeometry {
                gutter_width: 0,
                text_area: RectCore {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
                tab_width: 4,
                word_wrap: false,
                bars: np_core::BarOccupancy::default(),
            },
            regions: Regions::default(),
            untitled_counter: 0,
            menu_bar_active: false,
            pending_save_as: None,
            pending_confirm: None,
            confirm: None,
            closed_tabs: Vec::new(),
            recent: Vec::new(),
            session_path: None,
            config_path: None,
            session_dirty: false,
            session_writes: 0,
            last_autosave: None,
            pending_batch: None,
            document_list_selected: 0,
            pending_reload_encoding: None,
            bg: crate::bg::BgWorker::new(),
            fif: crate::fif::FifState::default(),
            pending_load_request: None,
            sync_search: true,
            highlight,
            hl_store: crate::highlight::HighlightStore::new(),
            lang_query: String::new(),
            lang_selected: 0,
            lang_scroll: 0,
            theme_query: String::new(),
            theme_selected: 0,
            theme_scroll: 0,
            click: ClickState::default(),
            hover: None,
            drag_anchor: None,
            gutter_drag_anchor: None,
            scrollbar_drag: None,
            dialog_press: None,
            find_panel_height: 0,
            find,
            goto_input: String::new(),
            goto_error: None,
            context_menu_target: None,
            osc_out: osc_out.clone(),
            clipboard: ClipboardProvider::new(clipboard_mode).with_osc_sink(capture_sink(&osc_out)),
            dialog_focus: 0,
            dialog_row: 0,
            dialog_scroll: 0,
            palette_scroll: 0,
            pointer_row: None,
            file_input: crate::ui::text_input::TextInput::empty(),
            file_purpose: FilePurpose::Open,
            last_dir: None,
            show_hidden: false,
            file_selected: 0,
            file_row_chosen: false,
            file_entries: Vec::new(),
            file_error: None,
            wheel_choice: None,
            palette_selected: 0,
            palette_query: String::new(),
            help_scroll: 0,
            context_menu_cell: (0, 0),
            last_pointer: (0, 0),
            submenu_open: false,
            prefs_selected: 0,
            prefs_edit: None,
            pending_encoding: None,
            timed_toasts: Vec::new(),
            banner_button: 0,
        };
        let enc = app.config.default_encoding();
        let eol = app.config.default_eol();
        let (doc, counter) = tabs::new_document(enc, eol, app.untitled_counter);
        app.untitled_counter = counter;
        app.open_document(doc);
        // The startup tab is not a change. `open_document` queues a session write because a
        // tab the user opened has to reach disk, and building an `App` is not that: without
        // clearing the flag, a read-only look at the editor would create a session file on
        // the first idle tick.
        app.session_dirty = false;
        app.recompute_geometry(area.0, area.1);
        // A theme name the engine does not know is a config mistake, and the failure is
        // invisible without this: the editor simply opens in the default theme and looks
        // undamaged. Sticky, because the user is about to keep working under a theme they did
        // not ask for.
        if let Some(name) = unknown_theme {
            app.push_toast(
                format!(
                    "unknown theme '{name}', using {}",
                    app.highlight.theme_name()
                ),
                true,
            );
        }
        app
    }

    /// Read the real clock once per event batch. Called by the S4 event loop; tests drive
    /// [`App::advance_clock`] instead so undo grouping is deterministic.
    pub fn begin_batch(&mut self) {
        self.clock = Instant::now();
    }

    /// Move the injected clock forward, for tests that need idle-gap behavior.
    pub fn advance_clock(&mut self, delta: Duration) {
        self.clock += delta;
    }

    /// Queue a notification, stamped with the current clock (FR-U6).
    ///
    /// This is the entry point every error path should use. It writes both representations:
    /// `toasts`, the plain queue S3..S13 already push into and which tests assert on, and
    /// `timed_toasts`, which is what the renderer draws and what expiry trims. Keeping the
    /// two in step here means no call site has to know the second one exists.
    /// Every notification goes through here. Writing `toasts` or `timed_toasts` directly is
    /// a bug: the renderer only draws `timed_toasts`, and [`App::expire_toasts`] rebuilds
    /// `toasts` from it, so a direct push is invisible on screen and disappears from the
    /// queue on the next tick. S15 found roughly thirty call sites in that state, which is
    /// why `every_notification_reaches_the_screen` guards the invariant.
    /// Write `config` to disk, reporting a failure instead of swallowing it.
    ///
    /// Any runtime change to a preference must go through here. Updating `app.config` alone is
    /// the trap: the change is visible for the rest of the session, so it looks like it worked,
    /// and the only evidence of the bug appears on the next start. The Theme picker fell into
    /// exactly that -- it set `config.ui.theme`, repainted the editor, and the setting was gone
    /// after a restart, while Preferences (which does write) persisted the very same field. Two
    /// entry points to one setting, one of them a lie.
    ///
    /// A write failure is a toast, not a rollback, matching `ui::prefs::apply`: the running
    /// editor already honors what the user asked for, and what they lost is only persistence.
    pub fn persist_config(&mut self) {
        let Some(path) = self.config_path.clone() else {
            // No path means session-level config is switched off (a bare `--config`-less test
            // run), not that a write failed. Saying nothing is right; there is nothing lost.
            return;
        };
        if let Err(e) = self.config.save(&path) {
            // `user_message` names the path; the raw Display impl buries it in a Debug chain.
            self.push_toast(format!("could not save config: {}", e.user_message()), true);
        }
    }

    pub fn push_toast(&mut self, message: impl Into<String>, error: bool) {
        let message = message.into();
        if error {
            self.toasts.push_error(&message);
            self.timed_toasts
                .push(crate::ui::toast::TimedToast::error(message, self.clock));
        } else {
            self.toasts.push(&message);
            self.timed_toasts
                .push(crate::ui::toast::TimedToast::info(message, self.clock));
        }
        self.dirty = true;
    }

    /// Drop toasts that have lived past their TTL, keeping every error (FR-U6).
    ///
    /// Called once per event-loop iteration, next to the background drain. Rebuilding
    /// `toasts` from what survives is what keeps the two lists honest; the alternative,
    /// expiring both independently, is how a toast ends up drawn but not in the queue a test
    /// inspects, or vice versa.
    pub fn expire_toasts(&mut self) {
        let removed = crate::ui::toast::expire(&mut self.timed_toasts, self.clock);
        if removed == 0 {
            return;
        }
        let keep: Vec<(String, bool)> = self
            .timed_toasts
            .iter()
            .map(|t| (t.message.clone(), t.error))
            .collect();
        self.toasts.clear();
        for (message, error) in keep {
            if error {
                self.toasts.push_error(message);
            } else {
                self.toasts.push(message);
            }
        }
        self.dirty = true;
    }

    /// Dismiss every sticky error toast, which is what `Esc` or a click on a toast does.
    pub fn dismiss_toasts(&mut self) {
        if self.timed_toasts.is_empty() && self.toasts.is_empty() {
            return;
        }
        self.timed_toasts.clear();
        self.toasts.clear();
        self.dirty = true;
    }

    // -------------------------------------------------------------- tabs

    /// Append a document and make it active.
    /// The caller owns the incoming document's `overtype`, so this does not touch it:
    /// `push_untitled` inherits the current mode, `open` uses the file's own state. Only
    /// the chrome is refreshed here.
    pub fn open_document(&mut self, doc: Tab) {
        self.documents.push(doc);
        self.active = self.documents.len() - 1;
        self.document_list_selected = self.active;
        self.dirty = true;
        // A tab that appears must reach the session just as a tab that disappears must,
        // or the two directions disagree about what the tab list is.
        self.mark_session_dirty();
        let (w, h) = self.frame_size();
        self.recompute_geometry(w, h);
        // FR-V5: a file you just opened already has folds, without waiting for an edit to
        // trigger a re-detect. Geometry first, because fold row heights are measured with it.
        crate::view::refresh_folds(self);
    }

    /// Every interface color currently in force, derived from the active theme.
    ///
    /// One accessor for the whole UI. Chrome, dialogs, lists, fields and the dimmed backdrop all
    /// read their colors here, which is what makes "the theme controls every pixel" true by
    /// construction: there is nowhere else a widget could be getting a color from.
    ///
    /// Recomputed per call rather than cached. It is a handful of arithmetic ops over the theme's
    /// seven colors and callers take it once per painted region, so a cache would trade a real
    /// staleness bug for a saving nobody can measure.
    pub fn palette(&self) -> np_core::ThemePalette {
        self.highlight.palette()
    }

    /// The active document, if any. `App` normally always has one.
    pub fn doc(&self) -> Option<&Tab> {
        self.documents.get(self.active)
    }

    pub fn doc_mut(&mut self) -> Option<&mut Tab> {
        self.documents.get_mut(self.active)
    }

    /// The active document's cursor position, or the origin when there is no document.
    pub fn cursor_pos(&self) -> LineCol {
        self.doc()
            .map(|d| d.cursor.pos)
            .unwrap_or(LineCol::new(0, 0))
    }

    /// Close the active tab. Returns `false` when a confirmation was raised instead.
    fn close_active_tab(&mut self) -> bool {
        self.request_close(self.active, PendingOp::CloseTab(self.active))
    }

    /// Raise the close flow for tab `idx`, deferring `op` behind a save prompt if needed.
    ///
    /// Returns `false` when the user now owes an answer. Every close path in the app goes
    /// through here, so "modified means asked" cannot be forgotten in one path while
    /// working in another -- the classic way an editor silently eats an edit.
    fn request_close(&mut self, idx: usize, op: PendingOp) -> bool {
        let i = match idx.checked_sub(self.documents.len()) {
            // Stale index: the tab is already gone, so there is nothing to ask about.
            Some(_) => return true,
            None => idx,
        };
        // For a single close, the tab in question decides whether to ask. For a batch, the
        // *batch* decides: `CloseOthers` on a clean active tab still has other documents to
        // check, and running the batch is exactly what finds them.
        let asks = match op {
            PendingOp::CloseTab(_) => self
                .documents
                .get(i)
                .map(|doc| doc.modified)
                .unwrap_or(false),
            PendingOp::CloseOthers(_) | PendingOp::CloseAll | PendingOp::Exit => {
                !self.modified_indices().is_empty()
            }
            PendingOp::Reload | PendingOp::Discard => self
                .documents
                .get(i)
                .map(|doc| doc.modified)
                .unwrap_or(false),
        };
        if asks {
            // Name the first modified document, which is the one at risk if the operation
            // proceeds unanswered.
            let target = match op {
                PendingOp::CloseTab(_) => i,
                _ => self.modified_indices().first().copied().unwrap_or(i),
            };
            let name = self
                .documents
                .get(target)
                .map(|d| d.display_name())
                .unwrap_or_else(|| "a document".to_string());
            if matches!(op, PendingOp::CloseAll | PendingOp::Exit) {
                self.pending_batch = Some(op);
            }
            self.raise_confirm(&name, op, Some(target));
            return false;
        }
        self.perform(op);
        true
    }

    /// Put the save-changes prompt on screen and remember what it is blocking.
    fn raise_confirm(&mut self, name: &str, op: PendingOp, target: Option<usize>) {
        self.confirm = Some(ConfirmRequest::save_changes(name, op, target));
        // Keep the legacy field in step: S3-era tests and S14's dialog read it, and a
        // prompt whose two representations disagree would answer the wrong question.
        self.pending_confirm = match op {
            PendingOp::CloseTab(i) => Some(PendingConfirm::CloseTab(i)),
            PendingOp::Exit => Some(PendingConfirm::Quit),
            _ => self.pending_confirm,
        };
        self.banner = Some(Banner {
            message: format!("{name} has unsaved changes"),
        });
        self.push_mode(Mode::Dialog(DialogKind::Confirm));
    }

    /// Answer the open save prompt (FR-F5).
    ///
    /// Cancel must change nothing at all, including the prompt's own target index: the
    /// document the user was about to close is still there, still modified, still active.
    pub fn resolve_confirm(&mut self, button: ConfirmButton) {
        let Some(request) = self.confirm.take() else {
            // Nothing pending: a stray click or a stale key press is ignored rather than
            // running `op` a second time.
            self.clear_confirm_mode();
            return;
        };
        self.pending_confirm = None;
        self.banner = None;
        self.clear_confirm_mode();
        match button {
            ConfirmButton::Cancel => {}
            ConfirmButton::Save => {
                // Save first, and only continue if the save worked. A failed save that
                // closed the tab anyway would destroy the document, since the buffer is
                // the only copy of the edits.
                if self.save_index(request.target.unwrap_or(self.active)) {
                    self.resume(request.op, request.target);
                } else {
                    // The save failed (read-only, disk error). Put the prompt back so the
                    // user can choose Don't save or Cancel rather than having the close
                    // silently dropped.
                    self.raise_confirm_after_failed_save(request);
                }
            }
            ConfirmButton::DontSave => self.resume(request.op, request.target),
            // A yes/no prompt (FR-N5 binary, FR-F10 read-only) resolves the same way the
            // save prompt's two non-cancel answers do: Yes proceeds, No abandons.
            ConfirmButton::Yes => self.resume(request.op, request.target),
            ConfirmButton::No => {}
        }
    }

    /// Re-raise a prompt whose Save attempt failed.
    fn raise_confirm_after_failed_save(&mut self, request: ConfirmRequest) {
        let name = self
            .documents
            .get(request.target.unwrap_or(self.active))
            .map(|d| d.display_name())
            .unwrap_or_else(|| "the document".to_string());
        self.confirm = Some(ConfirmRequest::save_changes(
            &name,
            request.op,
            request.target,
        ));
        self.pending_confirm = match request.op {
            PendingOp::CloseTab(i) => Some(PendingConfirm::CloseTab(i)),
            PendingOp::Exit => Some(PendingConfirm::Quit),
            _ => self.pending_confirm,
        };
        self.push_mode(Mode::Dialog(DialogKind::Confirm));
    }

    /// Leave the confirm mode if that is what is on top, without popping anything else.
    ///
    /// `pop_mode` would remove whatever happened to be topmost, which during a mouse
    /// driven close could be an unrelated menu. This pops only a confirm dialog.
    fn clear_confirm_mode(&mut self) {
        if self.mode() == Mode::Dialog(DialogKind::Confirm) {
            self.pop_mode();
        }
        self.dirty = true;
    }

    /// Save one document by index, reporting whether the buffer is now on disk.
    ///
    /// An untitled document has no path to write to, which counts as a failure: the text
    /// exists nowhere else, so the close must not proceed. S14's Save As dialog takes
    /// over from `pending_save_as`.
    fn save_index(&mut self, idx: usize) -> bool {
        let opts = self.save_options();
        let mode = self.eol_mode();
        let Some(doc) = self.documents.get_mut(idx) else {
            return false;
        };
        if doc.path.is_none() {
            self.pending_save_as = Some(idx);
            self.toasts
                .push_error("cannot close: the document has never been saved");
            return false;
        }
        match doc.save(mode, &opts) {
            Ok(_) => {
                self.dirty = true;
                true
            }
            Err(err) => {
                self.push_toast(err.user_message(), true);
                false
            }
        }
    }

    /// Answer-driven continuation: run `op`, then finish a paused batch or exit if one was
    /// waiting behind it.
    ///
    /// A batch prompt stores the batch op itself, so `op` and `pending_batch` are usually
    /// the same value; running it once and skipping the duplicate keeps Close All from
    /// closing a tab twice when the prompt was raised mid-batch.
    fn resume(&mut self, op: PendingOp, target: Option<usize>) {
        let batch = self.pending_batch.take();
        // The document the prompt was about. Falling back to `active` only when the request
        // named none, because the prompt named a specific file and answering it acts on
        // that file, not on whatever happens to be focused.
        let request_target = target.unwrap_or(self.active);
        match (op, batch) {
            // Exit is its own chain: close the document just answered, then either quit or
            // ask about the next modified one. `perform(Exit)` would call `request_exit`,
            // which does not close the answered tab, so the prompt would repeat forever on
            // a document the user already said good-bye to.
            (PendingOp::Exit, _) => {
                self.close_index(request_target);
                if self.modified_indices().is_empty() {
                    self.quit_requested = true;
                } else {
                    self.request_exit();
                }
            }
            (_, Some(PendingOp::Exit)) => {
                self.perform(op);
                if self.modified_indices().is_empty() {
                    self.quit_requested = true;
                } else {
                    self.request_exit();
                }
            }
            // The batch case, and the reason the answered tab has to be closed *here*:
            // `perform(op)` cannot do it.
            //
            // A batch prompt stores the batch op itself -- `op` and `pending_batch` are the same
            // value. So `perform(op)` re-enters `close_many`, which walks the target set from the
            // top, finds the first modified document, and that is still the document the user just
            // answered. It raises the prompt again and returns without closing anything. Answering
            // `Don't save` therefore looped forever with the tab list unchanged -- not "closes the
            // answered tab and then re-asks", but closes nothing at all, on every round. `Save`
            // escaped it only because clearing the modified bit makes the next pass skip that tab,
            // which is why every pre-existing test passed: `tabs_s8` answers `Save` and nothing
            // else.
            //
            // The Exit arm above already had the right shape, and this copies it: act on the
            // document the prompt was about, then let the batch continue over what is left. The
            // answered tab is removed by index before anything is re-derived, so the batch cannot
            // mistake it for a document that still owes an answer.
            (PendingOp::CloseTab(_) | PendingOp::CloseAll | PendingOp::CloseOthers(_), _) => {
                self.close_index(request_target);
                // The batch op still points at the list as it stood before that removal, so its
                // own indices have to be shifted onto the list as it now is. `CloseOthers(keep)`
                // is the one that matters: `keep` was recorded against the old list, and after a
                // removal a literal index names the wrong tab -- with `[f0, f1, f2]` all modified
                // and `f1` chosen to stay, answering for `f0` and then re-running
                // `CloseOthers(1)` means "keep f2", and the tab the user chose to keep is closed.
                let batch = batch.map(|op| Self::shift_batch_indices(op, request_target));
                if let Some(batch) = batch {
                    self.perform(batch);
                }
            }
            // Anything else (a reload) has no answered tab to remove: the prompt was a gate on the
            // operation, not a decision about one document's survival.
            _ => {
                self.perform(op);
                if let Some(batch) = batch {
                    self.perform(batch);
                }
            }
        }
    }

    /// Re-base a batch operation's recorded index onto the list after a tab has been removed.
    ///
    /// Only `CloseOthers` carries an index that outlives the moment it was recorded, so only it
    /// needs re-basing. `CloseTab(i)` is consumed the moment it is answered and `CloseAll` names
    /// no tab, so both pass through untouched.
    ///
    /// The `None` arm -- the kept tab is the one that just closed -- is not reachable through the
    /// UI today: `request_close_others` excludes the kept tab from the documents it will prompt
    /// about, so a prompt's target is never the tab being kept. It is written out because
    /// `shift_after_removal` returns `Option` and the alternative is an unmakable assumption baked
    /// into the call. Should a future path ever prompt about the kept tab, closing everything is
    /// what `Close Others` means once nothing is left to keep.
    fn shift_batch_indices(op: PendingOp, removed: usize) -> PendingOp {
        match op {
            PendingOp::CloseOthers(keep) => match tabs::shift_after_removal(keep, removed) {
                Some(keep) => PendingOp::CloseOthers(keep),
                None => PendingOp::CloseAll,
            },
            other => other,
        }
    }

    /// Carry out a deferred operation now that any prompt around it has been answered.
    fn perform(&mut self, op: PendingOp) {
        match op {
            PendingOp::CloseTab(i) => self.close_index(i),
            PendingOp::CloseOthers(keep) => self.close_others(keep),
            PendingOp::CloseAll => self.close_all(),
            PendingOp::Reload => self.action_reload(None),
            PendingOp::Exit => self.perform_exit(),
            PendingOp::Discard => {}
        }
    }

    /// Remove tab `i` and activate a neighbour, always capturing it into history first.
    fn close_index(&mut self, i: usize) {
        if i >= self.documents.len() {
            return;
        }
        let doc = self.documents.remove(i);
        self.push_history(&doc);
        if self.documents.is_empty() {
            // The zero-document rule: a fresh untitled buffer rather than an editor with
            // nothing to edit.
            self.push_untitled();
            self.active = 0;
        } else {
            let (next, _) = tabs::neighbor_after_close(i, self.documents.len() + 1);
            self.active = next.min(self.documents.len() - 1);
        }
        // Closing a tab changes which tabs the next session should restore, and nothing else
        // in this path raises the flag: a clean tab sets no `modified` bit, so without this
        // the exit flush sees a clean flag, writes nothing, and the closed tab is back on the
        // next launch.
        self.mark_session_dirty();
        self.load_active_view();
    }

    /// Close every tab but `keep`, asking once per modified document.
    ///
    /// Asking per document rather than one blanket prompt is the honest reading of FR-F5:
    /// Close All over three unsaved files that silently discards two of them is data loss
    /// with a menu item attached. Cancelling on the first prompt leaves the rest open.
    fn close_others(&mut self, keep: usize) {
        let keep = keep.min(self.documents.len().saturating_sub(1));
        self.close_many(PendingOp::CloseOthers(keep), keep);
    }

    /// Close every document, one prompt per modified one.
    fn close_all(&mut self) {
        self.close_many(PendingOp::CloseAll, 0);
    }

    /// Close the set implied by `op`, high index first so nothing shifts under the list.
    ///
    /// Hitting a modified document pauses the batch on a prompt and stores `op` in
    /// `pending_batch`. Answering with Save or Don't save re-runs the same operation, which
    /// recomputes the target set from what is left: idempotent, and immune to the index
    /// drift a snapshotted index list would suffer.
    fn close_many(&mut self, op: PendingOp, preferred: usize) {
        let keep = match op {
            PendingOp::CloseOthers(keep) => Some(keep.min(self.documents.len().saturating_sub(1))),
            _ => None,
        };
        let mut targets: Vec<usize> = (0..self.documents.len())
            .filter(|&i| keep != Some(i))
            .collect();
        targets.sort_unstable_by(|a, b| b.cmp(a));
        for i in targets {
            let Some(doc) = self.documents.get(i) else {
                continue;
            };
            if !doc.modified {
                let doc = self.documents.remove(i);
                self.push_history(&doc);
                continue;
            }
            // A modified document stops the batch and asks. Tabs already closed stay
            // closed, which matches what the user just watched happen.
            let name = doc.display_name();
            self.active = self.active.min(self.documents.len() - 1);
            self.pending_batch = Some(op);
            self.raise_confirm(&name, op, Some(i));
            self.load_active_view();
            return;
        }
        self.pending_batch = None;
        if self.documents.is_empty() {
            self.push_untitled();
            self.active = 0;
        } else {
            self.active = preferred.min(self.documents.len() - 1);
        }
        // Same reason as in `close_index`: this batch removes tabs without going through it.
        self.mark_session_dirty();
        self.load_active_view();
    }

    /// Exit (spec section 6.10 final decision): quit immediately when nothing is modified,
    /// otherwise confirm, and on Cancel leave every document open.
    fn request_exit(&mut self) {
        let modified = self.modified_indices();
        if let Some(first) = modified.first() {
            let name = self
                .documents
                .get(*first)
                .map(|d| d.display_name())
                .unwrap_or_else(|| "a document".to_string());
            // The batch is Close All: answering Save / Don't save for each modified
            // document in turn empties the editor, and `perform_exit` then quits.
            self.pending_batch = Some(PendingOp::CloseAll);
            self.raise_confirm(&name, PendingOp::Exit, Some(*first));
            return;
        }
        self.quit_requested = true;
    }

    /// Finish an exit once every document has been saved or discarded.
    ///
    /// Closing the last tab creates a fresh untitled one, so "all closed" is never literally
    /// true. The real test is that nothing is modified: a brand-new untitled buffer carries
    /// no work, so quitting over it loses nothing.
    fn perform_exit(&mut self) {
        if self.modified_indices().is_empty() {
            self.quit_requested = true;
            return;
        }
        // Still something unsaved: ask about the next one rather than quitting.
        self.request_exit();
    }

    /// Record a closed document so it can be reopened (FR-U2).
    fn push_history(&mut self, doc: &Document) {
        self.closed_tabs.push(ClosedTab::capture(doc));
        let cap = self.config.ui.max_recent_files;
        tabs::trim_history(&mut self.closed_tabs, cap);
    }

    /// Reopen the most recently closed document (FR-U2).
    ///
    /// A clean document re-reads its file, because the file *is* its content and re-reading
    /// picks up any change made on disk meanwhile. A modified one restores the retained
    /// text and stays modified: that text exists nowhere else, and reading the file over it
    /// would throw away exactly what the capture was there to protect.
    pub fn reopen_closed_tab(&mut self) {
        let Some(closed) = self.closed_tabs.pop() else {
            self.push_toast("no closed tab to reopen", false);
            return;
        };
        let mut doc = match (&closed.path, &closed.unsaved_text) {
            (Some(path), None) => match np_core::load(path, Some(closed.encoding.clone())) {
                Ok(loaded) => Document::from_loaded(&loaded),
                Err(err) => {
                    // The file is gone or unreadable. Reopen it as an empty buffer under
                    // the same name rather than losing the tab silently.
                    self.push_toast(err.user_message(), true);
                    let mut doc = Document::new_untitled(closed.encoding.clone(), closed.eol);
                    doc.path = Some(path.clone());
                    doc
                }
            },
            (path, Some(text)) => {
                let mut doc = Document::new_untitled(closed.encoding.clone(), closed.eol);
                doc.buffer = np_core::Buffer::from_str_lossy(text);
                doc.path = path.clone();
                doc.modified = true;
                doc
            }
            (None, None) => Document::new_untitled(closed.encoding.clone(), closed.eol),
        };
        doc.readonly = closed.readonly;
        doc.overtype = self.overtype;
        self.open_document(doc);
    }

    /// Close every tab but the active one, asking about each modified one (FR-F5).
    fn request_close_others(&mut self) {
        let keep = self.active;
        // Only the *other* documents matter: a modified tab the user chose to keep must not
        // trigger a prompt for a tab that is not being closed.
        let at_risk: Vec<usize> = self
            .modified_indices()
            .into_iter()
            .filter(|&i| i != keep)
            .collect();
        if at_risk.is_empty() {
            self.close_others(keep);
            return;
        }
        let target = at_risk[0];
        let name = self
            .documents
            .get(target)
            .map(|d| d.display_name())
            .unwrap_or_else(|| "a document".to_string());
        self.pending_batch = Some(PendingOp::CloseOthers(keep));
        self.raise_confirm(&name, PendingOp::CloseOthers(keep), Some(target));
    }

    /// Close every document, asking about each modified one (FR-F5).
    fn request_close_all(&mut self) {
        if self.modified_indices().is_empty() {
            self.close_all();
            return;
        }
        self.pending_batch = Some(PendingOp::CloseAll);
        let modified = self.modified_indices();
        // Ask about the active one first when it is modified, so the prompt matches the tab
        // under the user's attention rather than jumping to an arbitrary other file.
        let target = if self.doc().map(|d| d.modified).unwrap_or(false) {
            self.active
        } else {
            modified[0]
        };
        let name = self
            .documents
            .get(target)
            .map(|d| d.display_name())
            .unwrap_or_else(|| "a document".to_string());
        self.raise_confirm(&name, PendingOp::CloseAll, Some(target));
    }

    /// Open the Document List dialog (FR-U4).
    /// Open the Language picker (FR-H2).
    ///
    /// The cursor starts on the entry matching the document's current language, so opening
    /// the picker on a Rust file shows Rust highlighted rather than the top of an alphabetic
    /// list.
    fn open_language_picker(&mut self) {
        let names = self.highlight.language_names();
        let current = self.doc().map(|d| {
            crate::highlight::resolve_language(
                &self.highlight,
                &d.language,
                d.path.as_deref(),
                &d.buffer.line_string(0),
            )
        });
        let start = current
            .as_deref()
            .and_then(|c| names.iter().position(|n| n == c))
            .unwrap_or(0);
        self.lang_query.clear();
        self.lang_scroll = 0;
        self.lang_selected = start;
        self.open_list_dialog(DialogKind::LanguagePicker, start);
    }

    /// Open the Theme picker (FR-H3).
    fn open_theme_picker(&mut self) {
        let names = self.highlight.theme_names();
        let current = self.highlight.theme_name().to_string();
        let start = names.iter().position(|n| n == &current).unwrap_or(0);
        self.theme_query.clear();
        self.theme_scroll = 0;
        self.theme_selected = start;
        self.open_list_dialog(DialogKind::ThemePicker, start);
    }

    /// Open a list dialog with the highlight, the focus and the accept path all starting on `start`.
    ///
    /// # Why this exists
    ///
    /// Three of the list dialogs used to push their mode directly after writing only their own
    /// cursor field, skipping `reset_dialog_focus()` -- which every other dialog opener calls. The
    /// shared row cursor `dialog_row` was therefore never seeded, and since `focused_id` answers a
    /// row-pointing index out of `dialog_row` while the *renderer* highlights the per-dialog field,
    /// the two disagreed from the first frame: the picker opened showing the current language
    /// highlighted while Enter, `[accept]` and the mouse all aimed at row 0. Reported on the
    /// Language dialog as the selection jumping when you click, and on the Document List as the
    /// highlight sitting on the first tab instead of the active one.
    ///
    /// A second consequence showed up on reopen. `reset_dialog_focus` copies the per-dialog field
    /// into the shared cursor, so a picker left at row 40 reopened with the cursor at 40 while its
    /// own field said 9 -- the current language was no longer where the highlight was.
    ///
    /// # Order matters
    ///
    /// The mode must be pushed before `reset_dialog_focus` runs, because that is what decides which
    /// field is the dialog's cursor. The field is written first at each call site for the mirror
    /// reason below, and `set_list_cursor` then writes both copies, so no caller has to remember to
    /// keep the two in step.
    fn open_list_dialog(&mut self, kind: DialogKind, start: usize) {
        self.push_mode(Mode::Dialog(kind));
        self.reset_dialog_focus();
        self.set_list_cursor(start);
    }

    /// The picker's filtered candidate list, in display order.
    pub fn picker_candidates(&self, which: DialogKind) -> Vec<String> {
        let all = match which {
            DialogKind::LanguagePicker => self.highlight.language_names(),
            DialogKind::ThemePicker => self.highlight.theme_names(),
            _ => return Vec::new(),
        };
        let query = match which {
            DialogKind::LanguagePicker => &self.lang_query,
            DialogKind::ThemePicker => &self.theme_query,
            _ => return Vec::new(),
        };
        crate::highlight::fuzzy_filter(&all, query)
            .into_iter()
            .map(|i| all[i].clone())
            .collect()
    }

    /// Move the picker cursor, clamped to the filtered list.
    pub(crate) fn picker_move(&mut self, which: DialogKind, delta: i32) {
        let count = self.picker_candidates(which).len();
        if count == 0 {
            return;
        }
        let cur = match which {
            DialogKind::LanguagePicker => &mut self.lang_selected,
            _ => &mut self.theme_selected,
        };
        let next = if delta < 0 {
            cur.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            cur.saturating_add(delta as usize).min(count - 1)
        };
        *cur = next;
    }

    /// Accept the picker's selection (FR-H2, FR-H3).
    /// The row a picker's Enter should apply.
    ///
    /// One place decides this so the key handler and the accept path cannot pick different rows.
    /// Precedence: a row the wheel is holding (a deliberate choice the window has scrolled past),
    /// then a dialog field that names a row nobody has moved the shared cursor to, then the row
    /// under the focus ring, then the cursor.
    pub(crate) fn picker_accept_row_row(&self, which: DialogKind) -> usize {
        if let Some(held) = self.wheel_choice {
            return held;
        }
        let field = match which {
            DialogKind::LanguagePicker => self.lang_selected,
            DialogKind::ThemePicker => self.theme_selected,
            _ => return self.list_cursor(),
        };
        if field != self.list_cursor() {
            return field;
        }
        if let Some(crate::ui::framework::FocusId::ListRow(row)) = self.focused_id() {
            return row;
        }
        self.list_cursor()
    }

    pub(crate) fn picker_accept(&mut self, which: DialogKind) {
        let candidates = self.picker_candidates(which);
        // The accept row, which the caller has already resolved and written into the cursor. After
        // the wheel scrolls a chosen row out of the window the cursor parks on the near edge, so
        // reading the cursor alone here would apply the wrong language.
        let sel = self
            .picker_accept_row_row(which)
            .min(candidates.len().saturating_sub(1));
        let Some(name) = candidates.get(sel).cloned() else {
            self.pop_mode();
            return;
        };
        match which {
            DialogKind::LanguagePicker => {
                if let Some(doc) = self.doc_mut() {
                    // Always store an explicit choice: picking "Rust" on a file auto-detected
                    // as Rust is still an override, and FR-H2 persists it per document.
                    doc.language = np_core::LanguageId::Named(name.clone());
                }
                // Fold detection is language-dependent -- braces for C-like, indentation for
                // Python -- so picking a language is one of the moments the answer changes.
                self.session_dirty = true;
                crate::view::refresh_folds(self);
            }
            DialogKind::ThemePicker => {
                if self.highlight.set_theme(&name) {
                    // Styles, not just parse state, are theme-derived (FR-H3).
                    self.hl_store.clear();
                    self.config.ui.theme = name;
                    // The theme is a preference, so it survives the session. Picking it in the
                    // dialog updated memory and repainted the editor, which made the omission
                    // invisible until a restart.
                    self.persist_config();
                    self.session_dirty = true;
                } else {
                    self.push_toast(format!("unknown theme: {name}"), true);
                    return;
                }
            }
            _ => {}
        }
        self.pop_mode();
    }

    /// Keystrokes for a picker dialog: filtering, navigation, accept, cancel.
    ///
    /// Returns false when the key is not a picker key, so the caller can fall through.
    fn handle_picker_key(&mut self, which: DialogKind, ev: KeyEvent) -> bool {
        use crossterm::event::KeyCode as K;
        match ev.code {
            K::Esc => {
                self.pop_mode();
                true
            }
            K::Enter => {
                self.picker_accept(which);
                true
            }
            K::Up => {
                self.picker_move(which, -1);
                true
            }
            K::Down => {
                self.picker_move(which, 1);
                true
            }
            K::PageUp => {
                self.picker_move(which, -10);
                true
            }
            K::PageDown => {
                self.picker_move(which, 10);
                true
            }
            K::Backspace => {
                let q = match which {
                    DialogKind::LanguagePicker => &mut self.lang_query,
                    _ => &mut self.theme_query,
                };
                q.pop();
                self.picker_reset_cursor(which);
                true
            }
            K::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                let q = match which {
                    DialogKind::LanguagePicker => &mut self.lang_query,
                    _ => &mut self.theme_query,
                };
                q.push(c);
                self.picker_reset_cursor(which);
                true
            }
            _ => false,
        }
    }

    /// After the filter text changes, keep the cursor on the first result.
    fn picker_reset_cursor(&mut self, which: DialogKind) {
        let cur = match which {
            DialogKind::LanguagePicker => &mut self.lang_selected,
            _ => &mut self.theme_selected,
        };
        *cur = 0;
    }

    fn open_document_list(&mut self) {
        self.document_list_selected = self.active;
        self.open_list_dialog(DialogKind::DocumentList, self.active);
    }

    /// Move the Document List selection, clamped rather than wrapped.
    ///
    /// A list is not a tab bar: wrapping past the top makes it impossible to tell where the
    /// end is without counting, and the list is short enough that reaching either end is one
    /// held key.
    pub fn document_list_move(&mut self, delta: i32) {
        let count = self.documents.len();
        if count == 0 {
            return;
        }
        let current = self.document_list_selected.min(count - 1) as i32;
        let next = (current + delta).clamp(0, count as i32 - 1) as usize;
        if next != self.document_list_selected {
            self.document_list_selected = next;
            self.dirty = true;
        }
    }

    /// Activate the selected document and close the dialog.
    pub fn document_list_activate(&mut self) {
        let i = self.document_list_selected;
        self.pop_mode();
        self.activate_tab(i);
    }

    /// Adopt the active document's per-document view state.
    ///
    /// Overtype is the state this exists for. The rule that keeps it coherent: **a document
    /// owns its overtype, and `App::overtype` is a cache of the active document's value.**
    /// A toggle writes both; a tab switch reads only, never writes back. Storing the
    /// outgoing value was the wrong shape -- it meant a toggle made just before `New File`
    /// stamped itself onto the freshly created tab, which is exactly the bug this rule
    /// prevents.
    ///
    /// Every path that changes `self.active` calls this afterwards.
    ///
    /// The find overlay is re-derived here too. `find.marks` and `find.current` are line and
    /// column pairs with no document id attached, so a list computed against tab A describes
    /// positions in a text tab B does not contain -- painting it over the new tab highlights
    /// arbitrary characters that match nothing. Recomputing rather than clearing keeps the
    /// part of the behaviour the user expects: the needle survives the switch, so a query in
    /// progress keeps finding in the new file instead of silently going blank.
    pub fn load_active_view(&mut self) {
        if let Some(doc) = self.doc() {
            let overtype = doc.overtype;
            self.overtype = overtype;
        }
        self.document_list_selected = self.active;
        self.find.current = None;
        self.refresh_marks();
        self.dirty = true;
        let (w, h) = self.frame_size();
        self.recompute_geometry(w, h);
    }

    // -------------------------------------------------------------- modes

    /// The mode on top of the stack, which is where input routing looks.
    pub fn mode(&self) -> Mode {
        self.mode_stack.last().copied().unwrap_or(Mode::Editing)
    }

    pub fn push_mode(&mut self, mode: Mode) {
        self.mode_stack.push(mode);
        self.dirty = true;
    }

    /// Pop exactly one level (spec §5.2). The bottom `Editing` entry is never removed, so
    /// the stack always has a mode.
    pub fn pop_mode(&mut self) {
        if self.mode_stack.len() > 1 {
            self.mode_stack.pop();
        }
        self.dirty = true;
    }

    /// `Esc` semantics: pop a level, or in plain editing clear the selection (spec §6.10).
    ///
    /// The Find and Replace dialogs close through `find::close_find` rather than a bare
    /// `pop_mode`, because closing them is also the moment their options are written back to
    /// `[find]` config (FR-S1). Popping the mode alone would silently lose that.
    fn handle_escape(&mut self) {
        if matches!(
            self.mode(),
            Mode::Dialog(DialogKind::Find) | Mode::Dialog(DialogKind::Replace)
        ) {
            crate::find::close_find(self);
            return;
        }
        if self.mode() == Mode::FindPanel {
            // FR-FF2: `Esc` cancels a running search and closes an idle one. Two presses, two
            // distinct effects, and the order matters -- cancelling must not also close, because
            // the user then cannot read the partial results they asked to keep.
            if self.fif.phase == crate::fif::FifPhase::Running {
                crate::fif::cancel(self);
            } else {
                crate::fif::close(self);
                self.pop_mode();
            }
            return;
        }
        match self.mode() {
            Mode::Editing => {
                // Order matters. A sticky error toast outranks clearing a selection: the
                // toast is the thing the user pressed Esc for (FR-U6 "error toasts sticky
                // until dismissed"), and clearing the selection first would eat the press.
                if self.timed_toasts.iter().any(|toast| toast.error) {
                    self.dismiss_toasts();
                    return;
                }
                if self.menu_bar_active {
                    self.menu_bar_active = false;
                } else if let Some(doc) = self.doc_mut() {
                    doc.clear_selection();
                }
                self.dirty = true;
            }
            Mode::Menu { .. } => {
                self.menu_bar_active = false;
                self.pop_mode();
            }
            _ => self.pop_mode(),
        }
    }

    // ------------------------------------------------------------ editing

    /// Say plainly why a write did not happen (FR-F9).
    ///
    /// `Document` refuses every mutation on a read-only buffer and returns an error, and the
    /// editing helpers above drop that error because they are called one keystroke at a time
    /// and threading a `CoreResult` through each would only repeat the same message. This is
    /// where the message actually gets said. Sticky rather than a 3 s blip: the user is about
    /// to keep typing into a buffer that is not accepting it, and a notice that disappears
    /// mid-frustration is not a notice.
    fn report_readonly(&mut self) {
        let name = self
            .doc()
            .map(|d| d.display_name())
            .unwrap_or_else(|| "this document".to_string());
        // One toast per burst, not one per keystroke.
        let already_queued = self
            .timed_toasts
            .iter()
            .any(|t| t.error && t.message.starts_with("read-only:"));
        if !already_queued {
            self.push_toast(format!("read-only: {name} is not writable"), true);
        }
        self.dirty = true;
    }

    /// Whether the active buffer refuses writes right now.
    fn is_readonly(&self) -> bool {
        self.doc().is_some_and(|d| d.readonly)
    }

    /// Insert one character: overtype, auto-pairing, then the insert itself.
    ///
    /// Overtype (FR-E12) deletes the **whole grapheme cluster** under the caret, not one
    /// scalar value. `e` + U+0301 is one visible character made of two `char`s, and
    /// replacing only the first would leave the accent glued to whatever was typed next,
    /// producing text that looks right in the buffer and wrong on screen.
    fn insert_char(&mut self, c: char) {
        // Read the clock before borrowing the document: `Document` methods take `now`, and
        // `self.doc_mut()` already holds a mutable borrow of `self`.
        let now = self.clock;
        // FR-F9: a read-only buffer refuses the insert inside `Document`, and the return
        // value is dropped by every insert below. Without this gate the keystroke would
        // vanish with no feedback at all -- the cursor would just refuse to move and nothing
        // would say why. One check here covers typing, paste, Tab and Enter, because they all
        // arrive through a path that ends in a `Document` write.
        if self.doc().is_some_and(|d| d.readonly) {
            self.report_readonly();
            return;
        }
        let auto_pair = self.config.editor.auto_pair;
        let closing = closing_for(c);
        let Some(doc) = self.doc_mut() else {
            return;
        };

        // FR-E11: typing an opener with an open selection wraps the selection.
        if auto_pair {
            if let (Some(close), Some(sel)) = (closing, doc.selection) {
                if !sel.is_empty() {
                    let (start, end) = sel.bounds();
                    let inner = doc.selected_text().unwrap_or_default();
                    let wrapped = format!("{c}{inner}{close}");
                    let _ = doc.insert_text(&wrapped, np_core::UndoGroup::Typing, now);
                    // Park the caret after the inserted opener, inside the pair, with the
                    // original text still selected between the pair characters.
                    doc.set_cursor(LineCol::new(start.line, start.col + 1), false);
                    doc.begin_selection(
                        LineCol::new(start.line, start.col + 1),
                        LineCol::new(end.line, end.col + 1),
                        sel.kind,
                    );
                    self.dirty = true;
                    return;
                }
            }
        }

        let overtype = doc.overtype;
        if overtype && doc.selection.is_none() {
            let pos = doc.cursor.pos;
            if let Some((_, end_col)) =
                np_core::buffer::grapheme_range(&doc.buffer, pos.line, pos.col)
            {
                let to = LineCol::new(pos.line, end_col);
                let _ = doc.delete_range(pos, to, np_core::UndoGroup::Typing, now);
            }
        }

        let _ = doc.insert_text(&c.to_string(), np_core::UndoGroup::Typing, now);

        // FR-E11: typing an opener on an empty selection inserts its partner and steps back
        // between the two, so typing continues inside the pair.
        if auto_pair {
            if let Some(close) = closing {
                if let Some(doc) = self.doc_mut() {
                    let pos = doc.cursor.pos;
                    let _ = doc.insert_text(&close.to_string(), np_core::UndoGroup::Typing, now);
                    doc.set_cursor(LineCol::new(pos.line, pos.col), false);
                }
            }
        }
        self.dirty = true;
    }

    /// `Tab` indents when there is a selection and inserts otherwise; `Shift+Tab` outdents
    /// (spec §6.10).
    fn press_tab(&mut self, shift: bool) {
        let has_selection = self
            .doc()
            .and_then(|d| d.selection)
            .is_some_and(|s| !s.is_empty());
        if shift {
            self.action_outdent();
        } else if has_selection {
            self.execute(Action::Indent);
        } else {
            let width = self
                .doc()
                .map(|d| d.tab_width())
                .unwrap_or(self.config.editor.tab_width)
                .max(1);
            let text = if self.config.editor.insert_spaces {
                " ".repeat(width)
            } else {
                "\t".to_string()
            };
            let now = self.clock;
            if let Some(doc) = self.doc_mut() {
                let _ = doc.insert_text(&text, np_core::UndoGroup::Typing, now);
            }
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ execute

    /// Run one command. The single entry point for every input path.
    /// Actions after which the viewport must **not** be pulled back onto the caret.
    ///
    /// Two distinct reasons, both about respecting an explicit user intent:
    ///
    ///  * Tab switches restore the view that tab was left in, scroll included. Reeling the
    ///    viewport onto the caret would discard a scroll position the user set deliberately --
    ///    wheeling down to read something and then peeking at another tab is ordinary.
    ///  * Page scrolling *is* moving the viewport away from where it was. Scrolling to look at
    ///    text without moving the caret is the whole point, and snapping back would make the
    ///    key do nothing visible.
    ///
    /// Everything else that can change the caret or the layout keeps the caret on screen.
    fn skips_ensure_caret_visible(action: Action) -> bool {
        matches!(
            action,
            Action::NextTab
                | Action::PrevTab
                | Action::NewFile
                | Action::CloseTab
                | Action::CloseOtherTabs
                | Action::CloseAllTabs
                | Action::ReopenClosedTab
                | Action::ScrollPageUp
                | Action::ScrollPageDown
        )
    }

    /// Dispatch one command. Every path through it ends by keeping the caret visible.
    ///
    /// The dispatcher itself is `execute_inner`, which has a dozen early returns because each
    /// action arm finishes independently. Wrapping rather than appending is what guarantees a
    /// movement that returns early still scrolls the viewport -- an ensure-visible call at the
    /// end of the function would have been skipped by exactly the actions that need it, which is
    /// how the caret came to be able to walk off the bottom of the screen.
    pub fn execute(&mut self, action: Action) {
        self.execute_inner(action);
        if !Self::skips_ensure_caret_visible(action) {
            crate::view::ensure_caret_visible(self);
        }
    }

    fn execute_inner(&mut self, action: Action) {
        // FR-F9: a read-only buffer refuses mutation inside `Document`, and most of the
        // editing helpers there drop the `CoreError` because they run one keystroke at a
        // time. Gating here -- the one point every input path converges on, whether the
        // request came from a key, a menu item, a palette row, or a context-menu click --
        // means the refusal is always said out loud instead of a cursor simply refusing to
        // move. Read-only is checked before dispatch rather than in each handler so a new
        // mutating action cannot forget it.
        if self.is_readonly() && is_mutating_action(action) {
            self.report_readonly();
            return;
        }
        match action {
            Action::NewFile => self.action_new_file(),
            Action::CloseTab => {
                self.close_active_tab();
            }
            Action::NextTab => self.cycle_tab(1),
            Action::PrevTab => self.cycle_tab(-1),
            Action::Undo => {
                if let Some(doc) = self.doc_mut() {
                    if !doc.undo_one() {
                        self.push_toast("Nothing to undo", false);
                    }
                }
                self.dirty = true;
            }
            Action::Redo => {
                if let Some(doc) = self.doc_mut() {
                    if !doc.redo_one() {
                        self.push_toast("Nothing to redo", false);
                    }
                }
                self.dirty = true;
            }
            Action::SelectAll => {
                if let Some(doc) = self.doc_mut() {
                    doc.select_all();
                }
                self.dirty = true;
            }
            Action::ToggleOvertype => {
                self.overtype = !self.overtype;
                let overtype = self.overtype;
                if let Some(doc) = self.doc_mut() {
                    doc.overtype = overtype;
                }
                self.dirty = true;
            }
            Action::Exit => self.request_exit(),
            Action::CloseOtherTabs => self.request_close_others(),
            Action::CloseAllTabs => self.request_close_all(),
            Action::ReopenClosedTab => self.reopen_closed_tab(),
            Action::DocumentList => self.open_document_list(),

            // ---- file actions (S5) -------------------------------------------
            Action::Save => self.action_save(),
            Action::SaveAs => {
                // FR-F4: the Save As dialog. `pending_save_as` still records which tab owes a
                // path, because the confirmation and batch-close paths read it; the dialog is
                // what supplies the answer.
                self.pending_save_as = Some(self.active);
                self.open_file_picker(crate::app::FilePurpose::SaveAs);
            }

            // ---- search (S10, FR-S1..S7) ------------------------------------
            Action::Find => crate::find::open_find(self, false),
            Action::Replace => crate::find::open_find(self, true),
            Action::FindNext => {
                let out = crate::find::find_next(self);
                self.run_find(out);
            }
            Action::FindPrev => {
                self.find.forward = false;
                let out = crate::find::find_next(self);
                self.find.forward = true;
                self.run_find(out);
            }
            Action::FindWordNext => {
                let out = crate::find::find_word(self, true);
                self.run_find(out);
            }
            Action::FindWordPrev => {
                let out = crate::find::find_word(self, false);
                self.run_find(out);
            }
            Action::ReplaceAll => {
                let out = crate::find::replace_all(self);
                self.run_find(out);
            }
            Action::LanguagePicker => {
                self.open_language_picker();
            }
            Action::ThemePicker => {
                self.open_theme_picker();
            }
            Action::GoToLine => {
                self.goto_input.clear();
                self.goto_error = None;
                self.push_mode(Mode::Dialog(DialogKind::GoTo));
                self.reset_dialog_focus();
            }
            Action::FindInFiles => {
                crate::fif::open(self);
            }

            Action::ReloadFile => self.action_reload(None),
            Action::ReloadAsEncoding => {
                // FR-N3: re-decode from the bytes on disk. The codec arrives in
                // `pending_encoding`, set by whichever menu or choice row fired this.
                match self.pending_encoding.take() {
                    Some(enc) => self.reload_active_as(enc),
                    None => self.open_encoding_menu(),
                }
            }
            Action::ConvertEncoding => {
                // FR-N3: re-encode in memory; the save converts. With no codec chosen, the
                // documented S13 fallback (cycle) still runs, so a bare keybinding on this
                // action keeps working.
                match self.pending_encoding.take() {
                    Some(enc) => self.set_active_encoding(enc),
                    None => self.action_cycle_encoding(),
                }
            }
            Action::OpenFile => {
                self.open_file_picker(crate::app::FilePurpose::Open);
            }
            Action::EncodingMenu => self.open_encoding_menu(),
            Action::EolMenu => self.open_eol_menu(),
            Action::RecentFiles => self.open_recent_files(),
            Action::Preferences => self.open_preferences(),
            Action::About => self.open_about(),
            Action::HelpOverlay => self.open_help(),
            Action::CommandPalette => self.open_palette(),
            Action::SetEol(eol) => self.action_set_eol(eol),
            Action::ToggleReadonly => self.action_toggle_readonly(),

            // ---- clipboard (FR-C1..C5) ---------------------------------------
            Action::Copy => self.action_copy(),
            Action::Cut => self.action_cut(),
            Action::Paste => self.action_paste(),
            Action::CopyPath => self.action_copy_path(),
            Action::CopyFilename => self.action_copy_filename(),
            Action::Indent => self.action_indent(),
            Action::Outdent => self.action_outdent(),
            Action::InsertDateTime => self.action_insert_date_time(),

            // Movement: always through Document so goal-column behavior is inherited
            // (FR-E2), which is the whole point of keeping it out of the UI layer.
            Action::MenuBar(m) => self.open_menu(m),
            a if is_movement(a) => self.execute_movement(a),
            a if is_line_op(a) => self.execute_line_op(a),

            // ---- view: wrap, folding, bookmarks (S13, FR-V1..V8) --------------
            Action::ToggleWordWrap => crate::view::toggle(self, crate::view::Toggle::WordWrap),
            Action::ToggleLineNumbers => {
                crate::view::toggle(self, crate::view::Toggle::LineNumbers);
            }
            Action::ToggleWhitespace => {
                crate::view::toggle(self, crate::view::Toggle::Whitespace);
            }
            Action::ToggleEolMarks => crate::view::toggle(self, crate::view::Toggle::EolMarks),
            Action::FoldAll => crate::view::fold_all(self),
            Action::UnfoldAll => crate::view::unfold_all(self),
            Action::ToggleFold => crate::view::toggle_fold_under_caret(self),
            Action::ToggleBookmark => crate::view::toggle_bookmark_under_caret(self),
            Action::NextBookmark => crate::view::next_bookmark(self),
            Action::PrevBookmark => crate::view::prev_bookmark(self),
            Action::ClearBookmarks => crate::view::clear_bookmarks(self),

            // Everything else is wired by a later stage. `Nop` and these arms are the
            // documented "bound but not yet implemented" state.
            _ => {}
        }
    }

    /// Turn a search outcome into a toast, the one place that does (see `find`'s module docs).
    pub(crate) fn run_find(&mut self, outcome: crate::find::FindOutcome) {
        if let Some(msg) = outcome.toast() {
            self.push_toast(msg, outcome.is_error());
        }
        self.dirty = true;
    }

    fn action_new_file(&mut self) {
        self.push_untitled();
    }

    /// Move the open dialog's list cursor to row `row`, without acting on it.
    ///
    /// The keyboard needs this separately from activation: `Tab` and the arrow keys choose a
    /// row, and Enter then runs it. Keeping the two apart is what lets the highlight, the
    /// focus ring, and the eventual accept all read the same value.
    pub fn select_dialog_row_cursor(&mut self, row: usize) {
        let Mode::Dialog(kind) = self.mode() else {
            return;
        };
        match kind {
            DialogKind::Open | DialogKind::SaveAs => {
                if row < self.file_entries.len() {
                    self.file_selected = row;
                }
            }
            DialogKind::LanguagePicker => {
                if row < self.picker_candidates(kind).len() {
                    self.lang_selected = row;
                }
            }
            DialogKind::ThemePicker => {
                if row < self.picker_candidates(kind).len() {
                    self.theme_selected = row;
                }
            }
            DialogKind::DocumentList => {
                if row < self.documents.len() {
                    self.document_list_selected = row;
                }
            }
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles
                if row < crate::ui::dialogs::choice_rows(self).len() =>
            {
                self.dialog_focus = row;
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Point the open dialog's list cursor at row `row`, and act on it the way that dialog's
    /// Enter key would.
    ///
    /// Every list dialog keeps its cursor in a different field (`file_selected`,
    /// `lang_selected`, `document_list_selected`, `dialog_focus`), and each has an accept
    /// path that already knows what Enter means for it. Routing clicks through here means the
    /// two input devices cannot disagree about what a given row does.
    pub fn select_dialog_row(&mut self, row: usize) {
        self.click_dialog_row(row, true);
    }

    /// The absolute list row a focus child points at, if it points at one.
    ///
    /// `None` for a field, a button, or a checkbox: those are controls, and a control's mouse
    /// behavior is its own. The `Option` is what lets one caller handle every dialog without
    /// knowing which dialog it is handling.
    pub fn focus_child_row(&self, i: usize) -> Option<usize> {
        match self.dialog_focus_items().get(i).map(|item| item.id)? {
            crate::ui::framework::FocusId::ListRow(row) => Some(row),
            _ => None,
        }
    }

    /// Run the open dialog's accept path for focus child `i`.
    ///
    /// The single place a mouse activation is turned into what the dialog's Enter key would do.
    /// Routing clicks through here is what guarantees that clicking a row and pressing Enter on it
    /// cannot behave differently, which is the property the user's reports kept tripping over:
    /// a Save prompt whose buttons moved focus but never fired, a Themes list that answered the
    /// mouse and not the keyboard, and a Settings list that answered neither.
    pub fn activate_dialog_child(&mut self, i: usize) {
        let Mode::Dialog(kind) = self.mode() else {
            return;
        };
        let Some(id) = self.dialog_focus_items().get(i).map(|item| item.id) else {
            return;
        };
        use crate::ui::framework::FocusId;
        match id {
            FocusId::ListRow(row) => self.click_dialog_row(row, true),
            FocusId::Button(n) => crate::ui::dialog_keys::press_button(self, n),
            FocusId::Checkbox(n) => crate::ui::dialog_keys::toggle_dialog_checkbox(self, n),
            // A field takes focus on press and nothing else; the caret is the whole interaction.
            FocusId::Field(_) | FocusId::List => {}
            // Close is answered by the hit-test, which routes it to `Hit::CloseStrip` before
            // any focus index is involved, so it cannot arrive here.
            FocusId::CloseStrip => {}
        }
        let _ = kind;
        self.dirty = true;
    }

    /// Point the open dialog's cursor at row `row`, optionally acting on it.
    ///
    /// # Why activation is a choice and not automatic
    ///
    /// Every list dialog's accept path is destructive in its own way -- it opens a file, applies a
    /// theme, switches a tab, runs an action -- and a *press* is not a decision to do any of
    /// those. A press is also the first half of a drag, and a drag along a list is a person
    /// scanning it, not choosing it. Acting on press meant that sliding the pointer down a long
    /// listing fired every row it crossed, and that a single click on a file closed the dialog
    /// before the user had looked at the rest. So a press selects, and only a release that stayed
    /// where it pressed -- a click -- activates. `activate` is that decision, passed in by the
    /// caller who knows which half of the gesture just happened.
    pub fn click_dialog_row(&mut self, row: usize, activate: bool) {
        let Mode::Dialog(kind) = self.mode() else {
            return;
        };
        // The cursor moves first, whichever kind this is: the highlighted row, the focused row,
        // and the row the accept path below reads are then the same row, with no per-dialog
        // bookkeeping to keep in step.
        self.set_list_cursor(row);
        if !activate {
            return;
        }
        match kind {
            DialogKind::Open | DialogKind::SaveAs => self.accept_clicked_file_row(row),
            DialogKind::LanguagePicker | DialogKind::ThemePicker => {
                let count = self.picker_candidates(kind).len();
                if row >= count {
                    return;
                }
                match kind {
                    DialogKind::LanguagePicker => self.lang_selected = row,
                    _ => self.theme_selected = row,
                }
                self.picker_accept(kind);
            }
            DialogKind::DocumentList => {
                if row >= self.documents.len() {
                    return;
                }
                self.document_list_selected = row;
                self.document_list_activate();
            }
            DialogKind::Preferences => {
                // A double-click commits the row exactly as Enter does, so the two devices reach
                // the same outcome for the same gesture shape. Enter is the model: it flips a
                // bool, cycles an enum forward, and opens an inline edit on a number.
                //
                // The row is read from the shared cursor rather than from `row`, and the two are
                // the same value here -- `set_list_cursor` ran first -- but reading the cursor is
                // what the keyboard path does, and keeping both devices on one source is what
                // stops them disagreeing later.
                let index = self.list_cursor();
                let Some(row) = crate::ui::prefs::rows().get(index).cloned() else {
                    return;
                };
                match row.field {
                    crate::ui::prefs::Field::Bool => {
                        crate::ui::prefs::toggle(self, index, 1);
                    }
                    crate::ui::prefs::Field::Enum(_) => {
                        crate::ui::prefs::toggle(self, index, 1);
                    }
                    crate::ui::prefs::Field::Num => {
                        let current = crate::ui::prefs::value_text(self, index);
                        self.prefs_edit =
                            Some((index, crate::ui::text_input::TextInput::new(current)));
                    }
                }
            }
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                if row >= crate::ui::dialogs::choice_rows(self).len() {
                    return;
                }
                self.dialog_focus = row;
                // Enter is the accept path for a choice list; reuse it rather than
                // re-implementing the encoding-hint recovery it performs.
                if let Some(action) = crate::ui::dialog_keys::accept_choice(self) {
                    self.execute(action);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Focus the list row `row`, if it is on screen.
    ///
    /// A row can only be focused when it is visible, which is the same rule a click obeys:
    /// the focus items are built from the visible window. A row that has scrolled out is
    /// left alone rather than silently focusing whatever occupies its screen slot.
    pub fn focus_dialog_row(&mut self, row: usize) {
        let items = self.dialog_focus_items();
        if let Some(i) = items
            .iter()
            .position(|it| it.id == crate::ui::framework::FocusId::ListRow(row))
        {
            self.dialog_focus = i;
            self.pointer_row = None;
            self.wheel_choice = None;
            // The row cursor is authoritative, so a caller that positions a row has to move it
            // -- setting only the index would be undone by `focused_id`, which answers a
            // row-pointing index out of the cursor.
            self.set_list_cursor(row);
            self.dirty = true;
        }
    }

    /// The row cursor of whichever choice list is open.
    ///
    /// A choice list has exactly one list child, so `dialog_focus` doubles as its row cursor.
    /// That is a deliberate simplification of a dialog with no other focusable widget, not an
    /// accident -- the accessors exist so the doubling is named in one place.
    /// How many rows the open list dialog has, 0 if it has no list.
    ///
    /// The single answer to "how long is this list", shared by the keyboard cursor, the
    /// renderer's window, the mouse row resolution, and the framework's clamping. It used to be
    /// spelled five different ways at five call sites, and a list whose length was computed
    /// differently in two of them is a cursor that can point past the end.
    pub fn dialog_row_count(&self) -> usize {
        let Mode::Dialog(kind) = self.mode() else {
            return 0;
        };
        match kind {
            DialogKind::Open | DialogKind::SaveAs => self.file_entries.len(),
            DialogKind::LanguagePicker | DialogKind::ThemePicker => {
                self.picker_candidates(kind).len()
            }
            DialogKind::DocumentList => self.documents.len(),
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                crate::ui::dialogs::choice_rows(self).len()
            }
            // Preferences is a list. Its rows are settings rather than entries you open, but
            // they are rows: windowed, highlighted, arrowed through, and clicked, and every one
            // of those needs a row count. It was left out on the reasoning that a preference row
            // is a control rather than an entry, and the result was a dialog whose row count
            // answered 0 -- so the shared cursor clamped to row 0 no matter where the pointer or
            // the arrow keys went, and the mouse could not select anything in it.
            DialogKind::Preferences => crate::ui::prefs::rows().len(),
            _ => 0,
        }
    }

    /// Scroll the open dialog's list by `rows` under the wheel, keeping the user's choice.
    ///
    /// # Why the window is stored rather than derived
    ///
    /// A dialog list used to have no scroll offset of its own: `framework::list_window` derived the
    /// first visible row from the *cursor*, so the only way to move the view was to move the cursor
    /// -- which is also what Enter acts on. To keep a scroll from silently re-selecting a row, the
    /// wheel parked the cursor at `start + visible - 1`, the last row of the window it wanted, and
    /// let `pointer_row` re-state the real choice.
    ///
    /// Parking the cursor at the bottom of the window is what the reports were about, and it fails
    /// two ways:
    ///
    /// - **The highlight lands on the bottom row.** Nothing else draws the selection bar, so a
    ///   cursor parked at the window's last row paints it at the bottom. The first wheel notch in a
    ///   freshly opened picker threw the highlight to the bottom of the box, which is what "the
    ///   selection jumps to the bottom item regardless of direction" describes.
    /// - **The wheel dies inside a dead zone.** `list_window` answers 0 for *every* cursor below
    ///   `visible`, so many cursors map to one window and the mapping cannot be inverted. From a
    ///   parked cursor the next notch computes a window that the cursor already produces, and the
    ///   list stops moving: on a Theme list with 14 rows in a 14-row window, one notch moved the
    ///   cursor 12 -> 13 and every notch after that was a no-op in both directions. Preferences
    ///   wedged the same way at row 25 of 26.
    ///
    /// So a dialog list now owns `dialog_scroll`, and the cursor means only what a cursor should
    /// mean: the row the user chose. The window is moved directly, then the cursor is pulled back
    /// onto screen if it scrolled off. Both the renderer and the hit-test read the same offset, so
    /// a highlighted row and a clickable row cannot disagree.
    ///
    /// # Why a prior choice is only preserved when there was one
    ///
    /// A dialog nobody has clicked yet has no `pointer_row`, and there the cursor *is* the
    /// selection. Re-arming `pointer_row` to that cursor would freeze the highlight at row 0 while
    /// the window scrolled away from it -- so with no prior choice the wheel keeps the selection
    /// pinned to the same screen position, which is what a keyboard user expects from the gesture.
    ///
    /// # What has no list
    ///
    /// A confirm prompt, About, Go To and the Find/Replace strip have no list rows. They swallow
    /// the wheel: the box is modal, and sliding the text behind it while the user is reading a
    /// question about this file is worse than doing nothing.
    pub fn scroll_dialog_list(&mut self, rows: i32) {
        // The keymap cheatsheet is a plain text scroll, not a list: it has no rows to select, so
        // its offset is all the gesture can mean. It was already routed here by the wheel handler
        // but fell straight through the `Mode::Dialog` test below, which is why the overlay ignored
        // the mouse entirely.
        if matches!(self.mode(), Mode::HelpOverlay) {
            let count = crate::ui::help::help_lines(self).len();
            let visible = crate::ui::help::visible_lines(self);
            let start = self.wheel_window_start(self.help_scroll, rows, count, visible);
            self.help_scroll = start;
            self.dirty = true;
            return;
        }
        // The palette keeps its own cursor and its own entry list; it is not a `Mode::Dialog`.
        if matches!(self.mode(), Mode::Palette) {
            let query = self.palette_query.clone();
            let count = crate::ui::palette::palette_entries(self, &query).len();
            let visible =
                crate::ui::palette::list_area(&crate::ui::palette::palette_layout(self)).h as usize;
            let start = self.wheel_window_start(self.palette_scroll, rows, count, visible);
            self.palette_scroll = start;
            // Keep the highlighted row on screen as the view moves, without inventing a choice.
            if count > 0 {
                let last = start + visible.saturating_sub(1);
                self.palette_selected = self.palette_selected.clamp(start, last.min(count - 1));
            }
            self.dirty = true;
            return;
        }
        if !matches!(self.mode(), Mode::Dialog(_)) {
            return;
        }
        let count = self.dialog_row_count();
        if count == 0 {
            return;
        }
        // The visible row count comes from the window the focus items were built for, which is the
        // window the renderer drew -- so the wheel moves by what the user can see rather than by a
        // height recomputed a second way.
        let visible = self
            .dialog_focus_items()
            .iter()
            .filter(|it| matches!(it.id, crate::ui::framework::FocusId::ListRow(_)))
            .count()
            .max(1);
        // The choice to preserve is whichever the user last made: a row they clicked, or a row an
        // earlier wheel step was already holding. Reading both once, before the window moves, is
        // what makes repeated wheel steps idempotent about the choice.
        let chosen = self.wheel_choice.or(self.pointer_row);

        let before_start = self.dialog_scroll;
        let start = self.wheel_window_start(before_start, rows, count, visible);
        // Whether the window was already resting against the end the wheel is pushing toward.
        // That is the whole difference between the two gestures below: mid-list the wheel pans the
        // view, and at an end there is nothing left to pan, so the selection takes over the
        // remaining travel -- reported as "the wheel stops doing anything once the list reaches its
        // first or last row".
        let at_top = self.dialog_scroll == 0;
        let at_bottom = self.dialog_scroll >= count.saturating_sub(visible);
        let pinned = (rows < 0 && at_top) || (rows > 0 && at_bottom);
        self.dialog_scroll = start;
        let last = start + visible.saturating_sub(1);
        if let Some(row) = chosen {
            // A choice the user made is held, and it stays held as an absolute row number even
            // after the window slides past it. The cursor follows only while the row is on screen;
            // off screen the cursor parks on the near edge so that *something* sensible is
            // highlighted, and `accept_row` is what keeps Enter naming the row the user chose.
            //
            // Moving the cursor to the nearest visible row instead -- which is what an earlier
            // version of this did -- was the reported "wheel makes a selection I did not make".
            // The highlight genuinely cannot follow a row off the edge of the screen, but the
            // answer must still be that row, and those two facts need two fields.
            if (start..=last).contains(&row) {
                self.set_list_cursor(row);
            } else {
                let edge = if row < start {
                    start
                } else {
                    last.min(count - 1)
                };
                self.set_list_cursor(edge);
            }
        } else if pinned {
            // The window cannot move, so the selection moves instead, one item per notch. Clamping
            // at both ends is what puts the highlight on the first row for the first upward notch
            // and on the last row for the last downward one.
            let next = crate::ui::framework::clamp_step(self.list_cursor(), rows, count);
            self.set_list_cursor(next);
        } else {
            // No prior choice and room left to pan: the cursor is the selection, so carry it the
            // same distance the window moved. That keeps the highlight on the same screen row the
            // user is watching, and it is what stops the highlight from sitting frozen on the
            // window's bottom corner for the last page of a long list.
            let moved = start as i64 - before_start as i64;
            let target = (self.list_cursor() as i64 + moved).clamp(0, count as i64 - 1) as usize;
            self.set_list_cursor(target);
        }
        // Hand ownership to `wheel_choice` rather than leaving both set. `pointer_row` is cleared
        // by the keyboard's row movement and by a listing re-read, neither of which the wheel
        // participates in, so a choice held in both places would be released by one owner and
        // silently kept by the other.
        self.wheel_choice = chosen;
        if chosen.is_some() {
            self.pointer_row = None;
        }
        self.dirty = true;
    }

    /// The window start a wheel step of `rows` lands on, clamped to both ends.
    ///
    /// Shared by the dialog lists and the palette so one notch means the same distance in both,
    /// and so neither can scroll past the last row or above the first. The target is a *window
    /// start* clamped to `count - visible`, not a cursor clamped to `count`: clamping the cursor
    /// stops the wheel one page short of the end, the complaint already settled for the
    /// scrollbar's trough -- a control that cannot reach its own end does not do what it is
    /// pointed at.
    fn wheel_window_start(&self, current: usize, rows: i32, count: usize, visible: usize) -> usize {
        if visible == 0 || count <= visible {
            return 0;
        }
        let max_start = count - visible;
        (current as i64 + rows as i64).clamp(0, max_start as i64) as usize
    }

    /// The open dialog's row cursor.
    ///
    /// Reads `dialog_row`, which every list dialog now shares, and clamps to the row count so a
    /// list that shrank underneath the cursor (a filter that matched fewer themes, a directory
    /// with fewer entries) cannot leave it past the end.
    pub fn list_cursor(&self) -> usize {
        let count = self.dialog_row_count();
        if count == 0 {
            return 0;
        }
        self.dialog_row.min(count - 1)
    }

    /// Set the open dialog's row cursor, clamped, and mirror it into the field that dialog
    /// renders from.
    ///
    /// The mirror is not redundancy for its own sake: `file_selected` and friends are what the
    /// *renderers* read, and they are also what session save/restore writes. Keeping them in
    /// step here is what lets the framework drive every list while the drawing code and the
    /// persistence code keep reading the field they have always read.
    pub fn set_list_cursor(&mut self, row: usize) {
        let count = self.dialog_row_count();
        let clamped = if count == 0 { 0 } else { row.min(count - 1) };
        self.dialog_row = clamped;
        let Mode::Dialog(kind) = self.mode() else {
            self.dirty = true;
            return;
        };
        match kind {
            DialogKind::Open | DialogKind::SaveAs => self.file_selected = clamped,
            DialogKind::LanguagePicker => self.lang_selected = clamped,
            DialogKind::ThemePicker => self.theme_selected = clamped,
            DialogKind::DocumentList => self.document_list_selected = clamped,
            DialogKind::Preferences => self.prefs_selected = clamped,
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                // These lists use `dialog_focus` as their cursor for historical reasons; the
                // framework keeps them working rather than rewriting their renderers.
                //
                // A row the wheel is holding wins here. `accept_choice` reads `choice_cursor()`,
                // which is this field, so leaving the parked cursor in it would make Enter open
                // whichever Recent File the view happened to stop on rather than the one clicked.
                let held = self
                    .wheel_choice
                    .map(|row| row.min(count.saturating_sub(1)))
                    .unwrap_or(clamped);
                self.dialog_focus = held;
                if self.wheel_choice.is_some() {
                    self.dialog_row = held;
                }
            }
            _ => {}
        }
        // The window follows the selection. A dialog list has one rule for both devices: the
        // chosen row is drawn. A keyboard arrow that walked past the bottom edge used to move the
        // cursor out of the window, where `focused_id` stops honouring a row that is not among the
        // focus items -- the list looked frozen while the cursor walked off screen.
        //
        // The wheel is the one caller that must not do this, and it does not: it sets the cursor
        // *after* it has chosen the window, so scrolling to a window is never undone by the cursor
        // pulling it back.
        self.scroll_to_cursor();
        self.dirty = true;
    }

    /// Load this dialog's own cursor field into the shared one, when a dialog opens.
    ///
    /// Without this a freshly opened picker would start at whatever row the *previous* dialog
    /// left selected, which is the same class of bug as inheriting `dialog_focus` across opens.
    ///
    /// # Direction of the copy, and why it only ever runs on open
    ///
    /// `dialog_row` is authoritative from here on: the keyboard, the mouse, the focus
    /// resolution and the clamping all read it. The per-dialog fields (`file_selected` and
    /// friends) are what the *renderers* and the session code read, so they are mirrors kept in
    /// step by [`App::set_list_cursor`]. Reading a mirror back after the dialog is open is a
    /// bug, not a convenience: a caller that writes `app.file_selected = 5` directly -- a test,
    /// or a future navigate path -- would otherwise have its choice silently overwritten by the
    /// stale mirror value on the next refresh. Only `set_list_cursor` may write those fields
    /// while a dialog is open, and `dialog_row` is what everyone reads.
    pub fn sync_list_cursor_from_kind(&mut self) {
        let Mode::Dialog(kind) = self.mode() else {
            self.dialog_row = 0;
            return;
        };
        self.dialog_row = match kind {
            DialogKind::Open | DialogKind::SaveAs => self.file_selected,
            DialogKind::LanguagePicker => self.lang_selected,
            DialogKind::ThemePicker => self.theme_selected,
            DialogKind::DocumentList => self.document_list_selected,
            DialogKind::Preferences => self.prefs_selected,
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                self.dialog_focus
            }
            _ => 0,
        };
        // The mirror direction on open: push the authoritative value into the fields the
        // renderers read, so a dialog whose list is shorter than the incoming cursor draws the
        // row it will actually act on rather than a row past the end.
        self.mirror_list_cursor();
    }

    /// Push the shared row cursor into the per-dialog field the renderer reads.
    ///
    /// One direction only, and only ever called from somewhere that has already decided the
    /// shared value is correct. This is what keeps `draw_list`'s `selected` argument and
    /// `focused_id`'s answer about to be the same row.
    fn mirror_list_cursor(&mut self) {
        let Mode::Dialog(kind) = self.mode() else {
            return;
        };
        let row = self.dialog_row;
        match kind {
            DialogKind::Open | DialogKind::SaveAs => self.file_selected = row,
            DialogKind::LanguagePicker => self.lang_selected = row,
            DialogKind::ThemePicker => self.theme_selected = row,
            DialogKind::DocumentList => self.document_list_selected = row,
            DialogKind::Preferences => self.prefs_selected = row,
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                self.dialog_focus = row;
            }
            _ => {}
        }
    }

    pub fn choice_cursor(&self) -> usize {
        self.dialog_focus
    }

    /// The file-picker row that `[Open]` / `[Save]` / Enter should act on, if any.
    ///
    /// # The problem this decides
    ///
    /// The picker holds two candidate answers to "what should open": the highlighted row and the
    /// path field. `[Open]` consulted only the field, and in the Open dialog the field is the
    /// directory being listed, so a highlighted file was ignored and the `EnterDir` branch reset
    /// the selection to row 0 with the dialog still open -- the user watched their choice jump to
    /// the top of the list and nothing happen. Deciding the precedence in one named place is the
    /// point: the button, Enter and the fallback all ask here rather than each holding an opinion.
    ///
    /// # The test is whether the row was *chosen*
    ///
    /// A selected row is not evidence of intent. `file_selected` always holds a valid row -- the
    /// picker opens with it on 0, which is `../` -- so "is one selected" is always true and using
    /// it would hijack the older, equally important behavior: type `/tmp/notes.txt` into the field
    /// and press Enter, and `notes.txt` must open rather than the parent directory. That is why
    /// [`App::file_row_chosen`] exists. A click on a row or an arrow-key move sets it; editing the
    /// path field or re-reading the listing clears it.
    ///
    /// So both gestures survive on the same rule, with no Open/Save As special case: an explicit
    /// choice wins, and an untouched default highlight yields to the field.
    ///
    /// Returns `None` when nothing was chosen or the row is out of range, so the caller falls back
    /// to the path field.
    pub fn picker_accept_row(&self) -> Option<usize> {
        if !self.file_row_chosen {
            return None;
        }
        // A row the wheel is holding outranks the cursor. The cursor exists to position the view
        // and the highlight while the held row is off screen, and letting it answer here would
        // mean a scroll down the Recent Files list silently changed which file Enter opens.
        let row = self.accept_row();
        if row >= self.file_entries.len() {
            return None;
        }
        Some(row)
    }

    /// The row the open dialog's accept path acts on.
    ///
    /// Normally that is the row cursor, which is also the highlighted row. The one exception is a
    /// row the wheel is still holding after the window scrolled away from it: `focused_id` stops
    /// honouring an off-screen row because a highlight has to be drawn somewhere, but the choice
    /// was made deliberately and Enter must keep honouring it.
    ///
    /// Every accept path reads this rather than `list_cursor` directly, so a row cannot be
    /// highlighted as one thing and opened as another.
    pub fn accept_row(&self) -> usize {
        if let Some(held) = self.wheel_choice {
            return held;
        }
        self.list_cursor()
    }

    /// Select the file-picker row a click landed on, opening it when it is a file (FR-U5).
    ///
    /// A click supplies a row *index* while the keyboard path works from `file_selected`, so
    /// this sets the selection and then follows the same accept rules. One accept path is what
    /// guarantees a mouse-open and an Enter-open cannot behave differently.
    pub fn accept_clicked_file_row(&mut self, row: usize) {
        if row >= self.file_entries.len() {
            return;
        }
        self.file_selected = row;
        let Some(entry) = self.file_entries.get(row).cloned() else {
            return;
        };
        if entry.is_dir {
            self.file_input
                .set_text(crate::ui::file_picker::dir_text(&entry.path));
            self.last_dir = Some(entry.path.clone());
            self.file_selected = 0;
            // Descend. `refresh_file_entries` drops the click's row claim, which is what
            // keeps the next click from being read against this directory's row numbers.
            self.refresh_file_entries();
            return;
        }
        self.file_input.set_text(entry.path.display().to_string());
        self.pop_mode();
        self.open_path(&entry.path);
    }

    /// Read a very large file on a worker thread rather than freezing the editor (FR-F10).
    ///
    /// The tab is not created here. The worker returns a finished `LoadedFile` and
    /// `fif::apply_loaded` builds the document on the UI thread, because a `Document` carries
    /// undo state, cursor and bookmarks, and spec §5.1 puts all of that under sole UI-thread
    /// ownership.
    ///
    /// A placeholder tab *is* pushed immediately, for one specific reason: without it the
    /// editor looks unchanged for the whole read, and a user who sees no response presses the
    /// key again. The placeholder carries the path so the tab bar names the file being read,
    /// and `apply_loaded` replaces its contents rather than adding a second tab.
    ///
    /// `sync_search` makes the read happen on the calling thread, exactly as it does for
    /// Find in Files, so a test can assert on the result without joining a thread or racing
    /// the queue.
    fn spawn_background_load(&mut self, path: &std::path::Path) {
        let worker = self.bg.clone();
        let id = worker.next_request_id();
        self.pending_load_request = Some(id);
        let cancel = worker.begin_request();

        // A named empty buffer that is not modified, so the read can replace it without a
        // "discard unsaved changes?" prompt on a document the user never typed into.
        let mut placeholder = np_core::Document::from_decoded(
            path.to_path_buf(),
            "",
            self.config.default_encoding(),
            self.config.default_eol(),
            false,
        );
        placeholder.overtype = self.overtype;
        placeholder.tab_width = self.config.editor.tab_width.max(1);
        placeholder.readonly = true;
        self.open_document(placeholder);
        self.note_recent(path);
        self.push_toast(
            format!(
                "loading {} in the background",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ),
            false,
        );

        let sink = worker.clone();
        let load_path = path.to_path_buf();
        if self.sync_search {
            let result = crate::bg::loader::load_with_progress(&load_path, &cancel, &mut |_| ());
            sink.send(crate::bg::BgMessage::FileLoaded {
                request: id,
                result,
            });
        } else {
            let handle = crate::bg::loader::spawn_load(id, load_path, cancel, move |m| {
                sink.send(m);
            });
            // Dropped, not joined: the UI thread must not wait on the read it just avoided.
            drop(handle);
        }
    }

    /// Load a file chosen inside the editor and open it as a tab (FR-F2).
    ///
    /// `main.rs` has its own path for files named on the command line, and this is not a
    /// duplicate of it: that one reports to stderr before a terminal exists, this one reports
    /// through toasts after one does. The failure behavior is the shared FR-F1 rule -- a file
    /// we cannot read still opens as an empty named buffer, because the user asked to edit
    /// that path and may intend to write it.
    pub fn open_path(&mut self, path: &std::path::Path) {
        // Already open? Activate it rather than adding a second tab for the same file, which
        // would give two unsaved buffers that both write to one path.
        if let Some(i) = self
            .documents
            .iter()
            .position(|d| d.path.as_deref() == Some(path))
        {
            if i != self.active {
                self.active = i;
                self.load_active_view();
            }
            self.note_recent(path);
            return;
        }
        // FR-F10: a file big enough that reading it would block the UI thread goes to the
        // background worker instead. The stat happens first, because the decision has to
        // precede the read -- deciding after loading would have already spent the time the
        // background path exists to avoid.
        if let Ok(meta) = std::fs::metadata(path) {
            if np_core::file::open_policy(meta.len()) == np_core::file::OpenDecision::Background {
                self.spawn_background_load(path);
                return;
            }
        }
        match np_core::file::load(path, None) {
            Ok(loaded) => {
                let binary = loaded.binary;
                let size = loaded.size;
                let mut doc = np_core::Document::from_loaded(&loaded);
                doc.overtype = self.overtype;
                doc.tab_width = self.config.editor.tab_width.max(1);
                self.open_document(doc);
                self.note_recent(path);
                if binary {
                    // FR-N5: the tab is already up, so this reports rather than prompts. A
                    // pre-open confirm dialog is wired for the CLI path's own prompt state;
                    // an in-session open of a binary file is a deliberate act.
                    self.push_toast(format!("{} looks like a binary file", path.display()), true);
                }
                if matches!(
                    np_core::file::open_policy(size),
                    np_core::file::OpenDecision::AskReadOnly
                ) {
                    self.push_toast(
                        format!("large file: {} bytes, consider read-only", size),
                        false,
                    );
                }
            }
            Err(err) => {
                let message = err.user_message();
                let mut doc = np_core::Document::from_decoded(
                    path.to_path_buf(),
                    "",
                    self.config.default_encoding(),
                    self.config.default_eol(),
                    false,
                );
                doc.overtype = self.overtype;
                doc.modified = false;
                doc.tab_width = self.config.editor.tab_width.max(1);
                self.open_document(doc);
                self.note_recent(path);
                self.push_toast(format!("{}: {message}", path.display()), true);
            }
        }
    }

    /// Append a fresh untitled document, inheriting the current overtype state.
    ///
    /// Inheriting is the S3 contract, and it is the right one: overtype is a mode the user
    /// put the editor in, so a document created one keystroke later starts in the mode they
    /// are still in. The distinction that matters is *inherit on creation* versus *carry
    /// across a switch* -- switching tabs must never move one document's mode onto another,
    /// which is what `load_active_view` guarantees.
    pub(crate) fn push_untitled(&mut self) {
        let enc = self.config.default_encoding();
        let eol = self.config.default_eol();
        let overtype = self.overtype;
        let (mut doc, counter) = tabs::new_document(enc, eol, self.untitled_counter);
        doc.overtype = overtype;
        self.untitled_counter = counter;
        self.open_document(doc);
    }

    fn cycle_tab(&mut self, delta: i64) {
        if self.documents.len() < 2 {
            return;
        }
        // `rem_euclid` gives wraparound in both directions, so PrevTab on the first tab
        // lands on the last rather than doing nothing.
        let n = self.documents.len() as i64;
        let next = (self.active as i64 + delta).rem_euclid(n);
        self.active = next as usize;
        self.load_active_view();
    }

    fn open_menu(&mut self, menu: MenuId) {
        // F10 activates the bar itself; Alt+letter opens one menu directly.
        self.menu_bar_active = true;
        if self.mode()
            == (Mode::Menu {
                open: menu,
                item: 0,
                parent: None,
            })
        {
            return;
        }
        // Opening a top-level menu *replaces* the menu level rather than stacking another one.
        //
        // Stacking was the old behaviour, and it turned exploring the bar into a trap: clicking
        // File, then Edit, then Search, then View pushed four levels, and `Esc` then walked back
        // through View, Search, Edit and File before finally closing -- the user's report was
        // that the menu "return to previous visited submenu until the first visited and only then
        // closes". A menu bar is a peer set, not a hierarchy: one level holds it, and `Esc`
        // closes it in one press.
        //
        // Submenus are the genuine nested level and they still push (see
        // `crate::ui::chrome::App::push_submenu`), so `Esc` peels a submenu first and the menu
        // second, which is the two-level behaviour the bar actually has.
        if matches!(self.mode(), Mode::Menu { .. }) {
            self.set_menu_level(menu);
        } else {
            self.push_mode(Mode::Menu {
                open: menu,
                item: 0,
                parent: None,
            });
        }
    }

    /// Replace the top menu level in place, dropping any submenu open above it.
    fn set_menu_level(&mut self, menu: MenuId) {
        while matches!(self.mode_stack.last(), Some(Mode::Menu { .. })) {
            self.pop_mode();
        }
        self.push_mode(Mode::Menu {
            open: menu,
            item: 0,
            parent: None,
        });
        self.submenu_open = false;
        self.dirty = true;
    }

    /// Move the caret while extending the selection (FR-E3 `Shift+`).
    ///
    /// `Document` exposes extension through `set_cursor(pos, extend)`, which needs a
    /// target position; so each extending form computes the target with the plain helper
    /// on a *copy* of the caret and then applies it with `extend = true`. Document has no
    /// `*_select` methods of its own, and adding twelve near-identical ones would duplicate
    /// the clamping logic for no gain.
    fn move_select(&mut self, target: impl FnOnce(&Document) -> LineCol) {
        let Some(doc) = self.doc() else {
            return;
        };
        let next = target(doc);
        if let Some(doc) = self.doc_mut() {
            doc.set_cursor(next, true);
        }
        self.dirty = true;
    }

    fn execute_movement(&mut self, action: Action) {
        // Non-extending movement delegates straight to `Document`, which owns goal-column
        // bookkeeping (FR-E2). Extending movement needs a target position first, so it
        // reads the caret through a copy and re-applies it with `extend = true`.
        match action {
            Action::MoveLeft => {
                if let Some(d) = self.doc_mut() {
                    d.move_left();
                }
            }
            Action::MoveRight => {
                if let Some(d) = self.doc_mut() {
                    d.move_right();
                }
            }
            Action::MoveUp => {
                if let Some(d) = self.doc_mut() {
                    d.move_vertical(-1, false);
                }
            }
            Action::MoveDown => {
                if let Some(d) = self.doc_mut() {
                    d.move_vertical(1, false);
                }
            }
            Action::WordLeft => {
                if let Some(d) = self.doc_mut() {
                    d.word_left();
                }
            }
            Action::WordRight => {
                if let Some(d) = self.doc_mut() {
                    d.word_right();
                }
            }
            Action::LineStart => {
                if let Some(d) = self.doc_mut() {
                    d.line_start();
                }
            }
            Action::LineEnd => {
                if let Some(d) = self.doc_mut() {
                    d.line_end();
                }
            }
            Action::DocStart => {
                if let Some(d) = self.doc_mut() {
                    d.doc_start();
                }
            }
            Action::DocEnd => {
                if let Some(d) = self.doc_mut() {
                    d.doc_end();
                }
            }
            Action::ScrollPageUp => {
                self.scroll_page(-1);
                return;
            }
            Action::ScrollPageDown => {
                self.scroll_page(1);
                return;
            }
            Action::MoveLeftSelect => {
                self.move_select(left_of);
                return;
            }
            Action::MoveRightSelect => {
                self.move_select(right_of);
                return;
            }
            Action::MoveUpSelect => {
                self.move_select(|d| vertical_of(d, -1));
                return;
            }
            Action::MoveDownSelect => {
                self.move_select(|d| vertical_of(d, 1));
                return;
            }
            Action::WordLeftSelect => {
                self.move_select(|d| d.buffer.word_start_before(d.cursor.pos));
                return;
            }
            Action::WordRightSelect => {
                self.move_select(|d| d.buffer.word_end_after(d.cursor.pos));
                return;
            }
            Action::LineStartSelect => {
                self.move_select(|d| LineCol::new(d.cursor.pos.line, 0));
                return;
            }
            Action::LineEndSelect => {
                self.move_select(|d| {
                    let len = d.buffer.line_len_chars(d.cursor.pos.line);
                    LineCol::new(d.cursor.pos.line, len)
                });
                return;
            }
            _ => {}
        }
        self.dirty = true;
    }

    // --------------------------------------------------------- clipboard

    /// Copy the selection, or the whole current line when nothing is selected (FR-C1).
    ///
    /// The whole-line fallback is Notepad++ behavior and is why `Ctrl+C` with no selection
    /// is useful rather than a no-op. The line's terminator is included when it has one, so
    /// a repeated copy-paste grows downward instead of concatenating lines.
    fn action_copy(&mut self) {
        let Some(text) = self.copyable_text() else {
            return;
        };
        let block = self.active_selection_is_block();
        let target = self.clipboard.copy(&text);
        // A block copy also seeds the primary selection, so middle-click pastes the
        // rectangle the user just selected (FR-C4).
        if block {
            self.clipboard.copy_primary(&text);
        }
        let _ = target;
        self.dirty = true;
    }

    /// Cut: copy, then delete the selection as one undo unit (FR-C2).
    fn action_cut(&mut self) {
        let Some(text) = self.copyable_text() else {
            return;
        };
        let block = self.active_selection_is_block();
        self.clipboard.copy(&text);
        if block {
            self.clipboard.copy_primary(&text);
        }
        let now = self.clock;
        let has_selection = self
            .doc()
            .and_then(|d| d.selection)
            .is_some_and(|sel| !sel.is_empty());
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let result = if has_selection {
            doc.delete_selection(now).map(|_| ())
        } else {
            // No selection means the whole line was copied, so the whole line goes.
            doc.delete_line(now)
        };
        match result {
            Ok(_) => self.dirty = true,
            Err(e) => self.push_toast(e.user_message(), true),
        }
    }

    /// Paste at the caret, replacing the selection, as one undo unit (FR-C2, FR-C13).
    ///
    /// EOLs are normalized by `Document::insert_text`, which is S5's single normalization
    /// point, so a Windows clipboard blob pasted into an LF document cannot smuggle CRLF
    /// into the buffer.
    fn action_paste(&mut self) {
        let Some(text) = self.clipboard.paste() else {
            self.push_toast("clipboard is empty", false);
            return;
        };
        self.insert_paste_text(&text);
    }

    /// Middle-click paste of the primary selection (spec §6.9 row 7, closed from S6).
    fn action_paste_primary(&mut self) {
        let Some(text) = self.clipboard.paste_primary() else {
            self.push_toast("nothing in the primary selection", false);
            return;
        };
        self.insert_paste_text(&text);
    }

    /// Shared paste body: block rectangles go through `paste_block`, everything else through
    /// `insert_text`, so a pasted rectangle keeps its column instead of becoming a flat blob.
    fn insert_paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.is_readonly() {
            self.report_readonly();
            return;
        }
        let now = self.clock;
        let block = self.paste_should_be_block(text);
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let result = if block {
            doc.paste_block(text, now)
        } else {
            doc.insert_text(text, np_core::UndoGroup::Atomic, now)
        };
        match result {
            Ok(_) => self.dirty = true,
            Err(e) => self.push_toast(e.user_message(), true),
        }
    }

    /// Decide whether a paste is a rectangle.
    ///
    /// A multi-line payload whose rows are all the same width is a block the user copied as
    /// a block, and pasting it as a plain insert would misalign it against the surrounding
    /// text. A single line is never a block: that would turn an ordinary paste into a
    /// column-overwrite, which is a far more destructive surprise.
    fn paste_should_be_block(&self, text: &str) -> bool {
        if !text.contains('\n') {
            return false;
        }
        // A block paste is a rectangle a person dragged: tens of rows, not tens of
        // thousands. `Document::paste_block` rewrites the document, so letting a whole-file
        // paste take that path costs seconds on the UI thread. Anything this tall is an
        // ordinary text paste, which the rope handles in well under a second.
        if text.len() > MAX_BLOCK_PASTE_BYTES {
            return false;
        }
        // The active selection being a block is a strong signal, but only when the payload
        // does not match what that selection already holds. Pasting a rectangle back over
        // the very selection it was copied from must insert, not overwrite the same band
        // with itself -- which is a no-op the user would read as "paste is broken".
        if self.active_selection_is_block() {
            let same_as_selection = self
                .doc()
                .and_then(|d| d.selected_text())
                .is_some_and(|sel| sel == text);
            if !same_as_selection {
                return true;
            }
        }
        let mut widths = text.split('\n');
        let Some(first) = widths.next() else {
            return false;
        };
        let first_len = first.chars().count();
        if first_len == 0 {
            return false;
        }
        // A trailing empty row is the rectangle's terminator, not a row of its own.
        let rows: Vec<&str> = text.split('\n').collect();
        let body = if rows.last().unwrap_or(&"").is_empty() {
            &rows[..rows.len().saturating_sub(1)]
        } else {
            &rows[..]
        };
        if body.len() < 2 {
            return false;
        }
        // Bounded scan. Without the cap this inspects every row of the payload, which on a
        // 10 MB paste is seconds of work on the UI thread before the paste even starts --
        // precisely the freeze FR-E13 forbids. A rectangle tall enough to exceed the cap is
        // pasted as ordinary text, which is the safe direction: it never overwrites a column
        // band the user did not select.
        const MAX_BLOCK_ROWS_INSPECTED: usize = 64;
        body.iter()
            .take(MAX_BLOCK_ROWS_INSPECTED)
            .all(|r| r.chars().count() == first_len)
    }

    /// The text a Copy/Cut should produce for the current state.
    fn copyable_text(&self) -> Option<String> {
        let doc = self.doc()?;
        if let Some(sel) = doc.selection {
            if !sel.is_empty() {
                return doc.selected_text();
            }
        }
        // Whole-line fallback (FR-C1). Includes the terminator when the line has one, which
        // is what makes repeated yank-and-paste stack lines instead of joining them.
        let line = doc.cursor.pos.line;
        let mut text = doc.buffer.line_string(line);
        let has_terminator = line + 1 < doc.buffer.line_count();
        if has_terminator {
            text.push('\n');
        }
        Some(text)
    }

    fn active_selection_is_block(&self) -> bool {
        self.doc()
            .and_then(|d| d.selection)
            .is_some_and(|s| s.kind == np_core::SelectionKind::Block && !s.is_empty())
    }

    // ------------------------------------------------------------- indent

    /// `Tab` with a selection, or `Action::Indent`: indent every touched line once (FR-E6).
    fn action_indent(&mut self) {
        let width = self
            .doc()
            .map(|d| d.tab_width())
            .unwrap_or(self.config.editor.tab_width)
            .max(1);
        let unit = if self.config.editor.insert_spaces {
            " ".repeat(width)
        } else {
            "\t".to_string()
        };
        let now = self.clock;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        if let Err(e) = doc.indent_lines(&unit, now) {
            self.push_toast(e.user_message(), true);
        }
        self.dirty = true;
    }

    /// `Shift+Tab`, or `Action::Outdent`: remove one indent unit from every touched line.
    fn action_outdent(&mut self) {
        let width = self
            .doc()
            .map(|d| d.tab_width())
            .unwrap_or(self.config.editor.tab_width)
            .max(1);
        let now = self.clock;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        if let Err(e) = doc.outdent_lines(width, now) {
            self.push_toast(e.user_message(), true);
        }
        self.dirty = true;
    }

    /// Backspace, with the smart dedent rule (FR-E5).
    ///
    /// When `smart_backspace` is on and only whitespace precedes the caret, one backspace
    /// removes the whole indent step back to the previous tab stop rather than a single
    /// space. That is what makes a mis-indented line fixable in one keystroke instead of
    /// four, and it is off-switchable precisely because some people want one space at a time.
    fn execute_backspace(&mut self) {
        if self.is_readonly() {
            self.report_readonly();
            return;
        }
        let now = self.clock;
        let smart = self.config.editor.smart_backspace;
        let tab = self
            .doc()
            .map(|d| d.tab_width())
            .unwrap_or(self.config.editor.tab_width)
            .max(1);
        // Decide the delete target before borrowing the document mutably.
        let target = if smart {
            self.doc().and_then(|d| smart_backspace_target(d, tab))
        } else {
            None
        };
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let result = match target {
            Some(from) => {
                let to = doc.cursor.pos;
                doc.delete_range(from, to, np_core::UndoGroup::Typing, now)
                    .map(|_| Some(doc.cursor.pos))
            }
            None => doc.delete_char_before(now),
        };
        match result {
            Ok(_) => self.dirty = true,
            Err(e) => self.push_toast(e.user_message(), true),
        }
    }

    /// Insert the current date and time at the caret (FR-E10).
    ///
    /// The clock reading is a parameter rather than a call inside, so tests pass a fixed
    /// instant and assert the exact rendered string instead of racing the wall clock.
    pub fn insert_date_time_at(&mut self, when: chrono::DateTime<chrono::Local>) {
        let fmt = self.config.editor.datetime_format.clone();
        let text = when.format(&fmt).to_string();
        let now = self.clock;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        if let Err(e) = doc.insert_text(&text, np_core::UndoGroup::Atomic, now) {
            self.push_toast(e.user_message(), true);
        }
        self.dirty = true;
    }

    /// Insert date/time using the real clock. The action path.
    fn action_insert_date_time(&mut self) {
        let when = chrono::Local::now();
        self.insert_date_time_at(when);
    }

    /// Copy the active document's full path (FR-C5).
    fn action_copy_path(&mut self) {
        let Some(doc) = self.doc() else {
            return;
        };
        let text = match doc.path.as_ref() {
            Some(path) => path.display().to_string(),
            // An untitled document has no path. Copying its display name is more useful than
            // copying nothing, and matches what the tab shows.
            None => doc.display_name(),
        };
        self.clipboard.copy(&text);
        self.push_toast(format!("copied {text}"), false);
        self.dirty = true;
    }

    /// Copy just the file name (FR-C5).
    fn action_copy_filename(&mut self) {
        let Some(doc) = self.doc() else {
            return;
        };
        let text = match doc.path.as_ref().and_then(|p| p.file_name()) {
            Some(name) => name.to_string_lossy().into_owned(),
            None => doc.display_name(),
        };
        self.clipboard.copy(&text);
        self.push_toast(format!("copied {text}"), false);
        self.dirty = true;
    }

    fn execute_line_op(&mut self, action: Action) {
        let now = self.clock;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let result = match action {
            Action::DuplicateLine => doc.duplicate_line(now),
            Action::DeleteLine => doc.delete_line(now),
            Action::MoveLineUp => doc.move_line_up(now),
            Action::MoveLineDown => doc.move_line_down(now),
            Action::SortLinesAsc => doc.sort_lines(SortOrder::Ascending, now),
            Action::SortLinesDesc => doc.sort_lines(SortOrder::Descending, now),
            Action::UpperCase => doc.change_case(np_core::CaseMode::Upper, now),
            Action::LowerCase => doc.change_case(np_core::CaseMode::Lower, now),
            Action::TitleCase => doc.change_case(np_core::CaseMode::Title, now),
            Action::ToggleComment => doc.toggle_comment("//", now),
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.push_toast(e.user_message(), true);
        }
        self.dirty = true;
    }

    /// Scroll by whole pages of the editor area (FR-V4).
    ///
    /// Counted in **visual rows**, not logical lines. With wrap on, a page is a screenful of
    /// rows and a line can be several rows tall, so paging by line count moves a variable and
    /// unpredictable distance: a screenful of short lines and a screenful of one wrapped
    /// paragraph would both advance 40 lines, and only one of them stays on the screen.
    fn scroll_page(&mut self, direction: i64) {
        // The viewport's rows, not the editor band's: the band's last row belongs to the
        // horizontal scrollbar, and paging by a count that includes it walks the view one row
        // further than the screen can show each time.
        let page = self.geometry.viewport_rows().max(1);
        let geom = self.geometry;
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        let current = row_offset_of_top(doc, &geom);
        let target = (current as i64 + direction * page as i64).max(0) as usize;
        // The last page starts `page` rows from the end, so a page-down at the bottom leaves
        // the view where it was rather than scrolling blank space.
        let clamped = target.min(total.saturating_sub(page));
        doc.scroll.top_line = np_core::line_at_row_offset(&doc.buffer, &geom, &doc.folds, clamped)
            .unwrap_or(0)
            .min(doc.buffer.line_count().saturating_sub(1));
        self.dirty = true;
    }

    // ----------------------------------------------------------- geometry

    // ---------------------------------------------------------------- file

    /// The save-time options, read from config so the UI does not restate the mapping.
    /// [`Self::save_options`] for use by sibling modules (autosave).
    pub fn save_options_public(&self) -> np_core::SaveOptions {
        self.save_options()
    }

    /// [`Self::eol_mode`] for use by sibling modules (autosave).
    pub fn eol_mode_public(&self) -> np_core::EolMode {
        self.eol_mode()
    }

    fn save_options(&self) -> np_core::SaveOptions {
        np_core::SaveOptions {
            trim_trailing_ws: self.config.editor.trim_trailing_ws_on_save,
            ensure_final_newline: self.config.editor.ensure_final_newline,
            backup: self.config.editor.backup_on_save,
        }
    }

    /// The EOL policy from `[editor] eol_mode` (FR-N4).
    fn eol_mode(&self) -> np_core::EolMode {
        np_core::EolMode::parse(&self.config.editor.eol_mode)
    }

    /// Save the active document (FR-F3).
    ///
    /// An untitled document cannot be saved without a path, so it is parked in
    /// `pending_save_as` for the Save As dialog instead of failing silently.
    fn action_save(&mut self) {
        // Take what we need out of self before calling into the document, which borrows it
        // mutably while save_options/eol_mode read the config.
        let opts = self.save_options();
        let mode = self.eol_mode();
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if doc.path.is_none() {
            self.pending_save_as = Some(self.active);
            return;
        }
        let saved_path = doc.path.clone();
        let result = doc.save(mode, &opts).map(|info| info.bytes_written);
        // The document borrow ends here so the recent-file update below can take `&mut self`.
        match result {
            Ok(bytes) => {
                let name = self
                    .documents
                    .get(self.active)
                    .map(|d| d.display_name())
                    .unwrap_or_else(|| "document".to_string());
                self.push_toast(format!("saved {name} ({bytes} bytes)"), false);
                // A saved file is one the user will want back, and this is often the first
                // time the path exists at all (FR-F8).
                if let Some(path) = saved_path {
                    self.note_recent(&path);
                }
                // A successful save means the disk no longer differs from the document.
                self.banner = None;
                self.dirty = true;
            }
            Err(err) => {
                // Sticky error toast; the document stays modified (FR-F3, §8).
                self.push_toast(err.user_message(), true);
            }
        }
    }

    /// Save the active document to an explicit path (Save As, once S14 supplies one).
    pub fn save_active_as(&mut self, path: &std::path::Path) -> bool {
        let opts = self.save_options();
        let mode = self.eol_mode();
        let Some(doc) = self.documents.get_mut(self.active) else {
            return false;
        };
        let target = path.to_path_buf();
        // The error is captured rather than swallowed: a failed Save As must toast, and the
        // document must not claim a path it never wrote.
        let result = doc.save_to(&target, mode, &opts);
        match result {
            Ok(_) => {
                self.pending_save_as = None;
                self.note_recent(&target);
                self.dirty = true;
                true
            }
            Err(err) => {
                self.push_toast(err.user_message(), true);
                false
            }
        }
    }

    /// Re-read the active document from disk (FR-F6).
    fn action_reload(&mut self, force: Option<np_core::EncodingId>) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if doc.path.is_none() {
            self.push_toast("nothing to reload: the document has no file", false);
            return;
        }
        // The name is read before the result is matched because `push_toast` needs `&mut
        // self`, which the borrow of `doc` would forbid.
        let outcome = doc.reload(force);
        match outcome {
            Ok(()) => {
                let name = self
                    .documents
                    .get(self.active)
                    .map(|d| d.display_name())
                    .unwrap_or_else(|| "document".to_string());
                self.push_toast(format!("reloaded {name}"), false);
                self.banner = None;
                self.dirty = true;
            }
            Err(err) => self.push_toast(err.user_message(), true),
        }
    }

    /// Reload with an explicit encoding, the "Reload as…" half of FR-N3.
    pub fn reload_active_as(&mut self, enc: np_core::EncodingId) {
        self.pending_reload_encoding = Some(enc.clone());
        self.action_reload(Some(enc));
    }

    /// Cycle the active document's encoding (FR-N3 "Convert to…").
    fn action_cycle_encoding(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let all = np_core::EncodingId::all();
        let idx = all.iter().position(|e| *e == doc.encoding).unwrap_or(0);
        let next = all[(idx + 1) % all.len()].clone();
        doc.convert_encoding(next.clone());
        self.toasts
            .push(format!("encoding set to {} (save to apply)", next.label()));
        self.dirty = true;
    }

    /// Set the encoding directly, for the Encoding menu (FR-N3).
    pub fn set_active_encoding(&mut self, enc: np_core::EncodingId) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            doc.convert_encoding(enc);
            self.dirty = true;
        }
    }

    /// Convert all line endings (FR-N4 EOL menu).
    fn action_set_eol(&mut self, eol: np_core::Eol) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            doc.convert_eol(eol);
            self.toasts
                .push(format!("line endings set to {}", eol.label()));
            self.dirty = true;
        }
    }

    /// Flip the read-only flag (FR-F9).
    ///
    /// Clearing read-only is only honored if the file is actually writable, so the flag
    /// cannot lie about what a save would do.
    fn action_toggle_readonly(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if doc.readonly {
            let writable = doc
                .path
                .as_ref()
                .map(|p| np_core::file::is_writable(p))
                .unwrap_or(true);
            if !writable {
                self.toasts
                    .push_error("file is not writable (check permissions)");
                return;
            }
            doc.readonly = false;
            self.push_toast("read-only off", false);
        } else {
            doc.readonly = true;
            self.push_toast("read-only on", false);
        }
        self.dirty = true;
    }

    /// Check every document for external changes and raise a banner (FR-F7).
    ///
    /// Called on focus change and before a save. Only the active document's state is shown,
    /// because a banner names one file at a time; the rest are checked so their banners are
    /// ready when the user switches to them.
    pub fn check_external_changes(&mut self) {
        let mut notice: Option<String> = None;
        for doc in self.documents.iter() {
            if doc.path.is_none() {
                continue;
            }
            if doc.file_deleted() {
                notice = Some(format!(
                    "{} was deleted on disk — save will create it again",
                    doc.display_name()
                ));
                break;
            }
            if doc.changed_on_disk() {
                notice = Some(format!(
                    "{} changed on disk — reload to discard your edits",
                    doc.display_name()
                ));
                break;
            }
        }
        if let Some(message) = notice {
            self.banner = Some(Banner { message });
            self.dirty = true;
        }
    }

    /// Whether the active document has unsaved changes.
    pub fn active_is_modified(&self) -> bool {
        self.documents
            .get(self.active)
            .map(|d| d.modified)
            .unwrap_or(false)
    }

    /// Indices of all documents with unsaved changes (FR-F3, for the quit confirm).
    pub fn modified_indices(&self) -> Vec<usize> {
        self.documents
            .iter()
            .enumerate()
            .filter(|(_, d)| d.modified)
            .map(|(i, _)| i)
            .collect()
    }

    // ------------------------------------------------------------ mouse

    /// Handle one mouse event: route it, then apply the resulting intent (contract §15).
    ///
    /// Deliberately thin. Every decision lives in `mouse::route`, which is pure and tested
    /// exhaustively; this function only performs the state change the intent asks for, so
    /// there is exactly one place where a mouse event can mutate the editor.
    ///
    /// `now` is the same injected clock the undo grouping uses, which keeps multi-click
    /// timing under the control of tests that call `advance_clock`.
    pub fn handle_mouse_at(&mut self, ev: &crossterm::event::MouseEvent, now: Instant) {
        // Record the pointer before routing: `route` is pure and takes `&App`, so an intent
        // that needs the click position (the context menu's anchor) reads it from here.
        self.last_pointer = (ev.column, ev.row);
        let (intent, next_click) = crate::mouse::route_with_click(ev, self, &self.click, now);
        self.click = next_click;
        self.apply_mouse_intent(intent);
    }

    /// The production entry point, reading the batch clock like the key path does.
    pub fn handle_mouse(&mut self, ev: &crossterm::event::MouseEvent) {
        let now = self.clock;
        self.handle_mouse_at(ev, now);
    }

    /// Close whichever docked service strip is up, leaving the editor as it was.
    ///
    /// The same teardown the keyboard's `Esc` performs, reached from the mouse: Find and Replace
    /// persist their options on the way out, and a running Find in Files search is cancelled
    /// before its panel goes, so a click on Close and a press of `Esc` cannot diverge.
    fn close_service_strip(&mut self) {
        if crate::ui::find_bar::is_open(self) {
            crate::find::close_find(self);
            return;
        }
        if matches!(self.mode(), Mode::FindPanel) || self.find_panel_height > 0 {
            if self.fif.phase == crate::fif::FifPhase::Running {
                crate::fif::cancel(self);
            }
            crate::fif::close(self);
            while matches!(self.mode_stack.last(), Some(Mode::FindPanel)) {
                self.pop_mode();
            }
            self.dirty = true;
        }
    }

    /// Close whatever menu-shaped mode summoned an action, then run it.
    ///
    /// A menu that stayed open after its item ran would be one Esc away from a second,
    /// unintended run, so the mode always goes first. Menu bar, context menu and palette all
    /// arrive here, which is why one arm used to serve all three.
    fn apply_menu_action(&mut self, action: Action) {
        self.close_menu();
        while matches!(self.mode_stack.last(), Some(Mode::ContextMenu(_))) {
            self.pop_mode();
        }
        if matches!(self.mode_stack.last(), Some(Mode::Palette)) {
            self.pop_mode();
        }
        self.execute(action);
    }

    /// Turn one intent into state. The only place mouse input changes the editor.
    fn apply_mouse_intent(&mut self, intent: crate::mouse::MouseIntent) {
        use crate::mouse::MouseIntent as I;
        match intent {
            I::Nothing => {}
            I::Action(action) => self.execute(action),
            I::SelectFindHit(row) => {
                // Selecting a result must not steal focus from the editor: the user may be
                // clicking to *see* a row before deciding to jump. Enter or a double-click
                // opens it, and that path does move focus.
                crate::fif::select(self, row);
            }
            I::OpenFindHit(row) => {
                // Select first, then run the same path Enter takes, so a double-click and a
                // keyboard accept cannot diverge -- the property every dialog click already holds.
                crate::fif::select(self, row);
                crate::fif::open_selected(self);
            }
            I::FocusFindPanel => {
                self.fif.focused = true;
                self.push_mode(Mode::FindPanel);
                self.dirty = true;
            }
            I::SetCaret { lc, extend } => {
                // A press in the text ends both drags: `gutter_drag_anchor` has to go or a
                // later gutter drag would extend the range from a line the user stopped caring
                // about several clicks ago.
                self.gutter_drag_anchor = None;
                let now = self.clock;
                if let Some(doc) = self.doc_mut() {
                    doc.set_cursor(lc, extend);
                    // A plain click ends any drag; an extending click keeps the anchor the
                    // selection already had.
                    if !extend {
                        self.drag_anchor = None;
                    }
                }
                let _ = now;
                self.dirty = true;
            }
            I::BeginSelection {
                kind,
                anchor,
                focus,
            } => {
                self.gutter_drag_anchor = None;
                self.drag_anchor = Some(anchor);
                if let Some(doc) = self.doc_mut() {
                    doc.selection = Some(np_core::Selection {
                        anchor,
                        focus,
                        kind,
                    });
                    doc.cursor.pos = focus;
                    doc.cursor.goal_col = None;
                }
                self.dirty = true;
            }
            I::ExtendSelection { focus } => {
                let anchor = self.drag_anchor;
                let kind = self
                    .doc()
                    .and_then(|d| d.selection)
                    .map(|s| s.kind)
                    .unwrap_or(np_core::SelectionKind::Range);
                let Some(doc) = self.doc_mut() else { return };
                let anchor = anchor.unwrap_or(doc.cursor.pos);
                doc.selection = Some(np_core::Selection {
                    anchor,
                    focus,
                    kind,
                });
                doc.cursor.pos = focus;
                doc.cursor.goal_col = None;
                self.dirty = true;
            }
            I::SelectLine { line, keep_caret } => {
                // Row 11 press: remember the line so a following drag extends from here rather
                // than collapsing to the row under the pointer.
                self.gutter_drag_anchor = Some(line);
                let Some(doc) = self.doc_mut() else { return };
                let focus = if line + 1 < doc.buffer.line_count() {
                    LineCol::new(line + 1, 0)
                } else {
                    LineCol::new(line, doc.buffer.line_len_chars(line))
                };
                let anchor = LineCol::new(line, 0);
                doc.selection = Some(np_core::Selection {
                    anchor,
                    focus,
                    kind: np_core::SelectionKind::Range,
                });
                if !keep_caret {
                    doc.cursor.pos = focus;
                }
                self.dirty = true;
            }
            I::SelectLineRange { anchor, focus } => {
                // Row 11 drag: the range, caret left where it was. `drag_anchor` is deliberately
                // untouched so a gutter sweep cannot poison a later text drag.
                let anchor_line = anchor.line;
                self.gutter_drag_anchor = Some(anchor_line);
                let Some(doc) = self.doc_mut() else { return };
                doc.selection = Some(np_core::Selection {
                    anchor,
                    focus,
                    kind: np_core::SelectionKind::Range,
                });
                self.dirty = true;
            }
            I::ActivateTab(i) => self.activate_tab(i),
            I::CloseTab(i) => self.close_tab_index(i),
            I::CycleTabs(delta) => self.cycle_tab(delta as i64),
            I::Scroll { lines } => self.scroll_lines(lines),
            I::ScrollH { cols } => self.scroll_columns(cols),
            I::ScrollTo(per_mille) => {
                let fraction = per_mille as f32 / 1000.0;
                let pos = crate::mouse::scroll_for_fraction(self, fraction);
                if let Some(doc) = self.doc_mut() {
                    doc.scroll = pos;
                }
                self.dirty = true;
            }
            I::ScrollToRow(row, grab) => {
                self.scrollbar_drag = grab.map(|b| *b);
                self.scroll_to_row(row);
            }
            I::ScrollToCol(col, grab) => {
                self.scrollbar_drag = grab.map(|b| *b);
                self.scroll_to_col(col);
            }
            I::HoldRowScroll(grab) => {
                // A thumb grab: record the anchor, move nothing.
                self.scrollbar_drag = grab.map(|b| *b);
            }
            I::HoldColScroll(grab) => {
                self.scrollbar_drag = grab.map(|b| *b);
            }
            I::ScrollToStart(kind) => match kind {
                crate::mouse::VerticalOrHorizontal::Vertical => self.scroll_to_row(0),
                crate::mouse::VerticalOrHorizontal::Horizontal => self.scroll_to_col(0),
            },
            I::EndDialogPressAndDrag => {
                self.scrollbar_drag = None;
                self.dialog_press = None;
            }
            I::SelectDialogRow(i) => {
                // Resolve the child, move the row cursor onto it, and remember the press --
                // including the row it meant, which is the part that cannot be recovered later.
                //
                // `pointer_row` is deliberately left alone. It holds a row *identity* that
                // outranks the cursor in `focused_id`, and the cursor is now that same row, so
                // setting it would be a second opinion on the same fact.
                let row = self.focus_child_row(i);
                self.dialog_press = Some(DialogPress { child: i, row });
                self.dialog_focus = i;
                if let Some(row) = row {
                    self.click_dialog_row(row, false);
                    // A click on a row is an explicit choice, which is what `[Open]` needs to
                    // distinguish from the cursor merely resting on row 0 after an open.
                    self.file_row_chosen = true;
                    // Record the row as the pointer's choice, the same way a click on a
                    // non-list control does below. Without this a click leaves `pointer_row`
                    // empty, and the wheel -- which parks the cursor wherever the window needs
                    // it -- would then move the highlight off the row the user just clicked.
                    // With it, `focused_id` keeps naming the clicked row while the list scrolls
                    // beneath it, which is the two-role split every other part of this file
                    // already assumes: the cursor positions, `pointer_row` chooses.
                    self.pointer_row = Some(row);
                    // A fresh click is a fresh choice: it supersedes whatever row a wheel step
                    // was holding, or the older one would outrank the row just clicked.
                    self.wheel_choice = None;
                }
            }
            I::ActivateDialogRow(_child, row) => {
                self.dialog_press = None;
                self.click_dialog_row(row, true);
            }
            I::ScrollDialogList { rows } => self.scroll_dialog_list(rows),
            I::SelectPaletteRow(row) => {
                // Move the palette's cursor, run nothing. The command itself fires on a
                // double-click or on Enter in the query field, both of which read this same
                // cursor, so the pointer and the keyboard cannot disagree about what will run.
                self.palette_selected = row;
                self.dirty = true;
            }
            I::ScrollToEnd(kind) => {
                // The ceiling comes from the bar, which is the only thing that knows both the
                // document's extent and the geometry's limit on where a page may start.
                let Some(scope) = crate::mouse::scroll_scope(self, kind) else {
                    return;
                };
                let end = scope.max_offset();
                match kind {
                    crate::mouse::VerticalOrHorizontal::Vertical => self.scroll_to_row(end),
                    crate::mouse::VerticalOrHorizontal::Horizontal => self.scroll_to_col(end),
                }
            }
            I::OpenContextMenu(kind, lc) => {
                // Row 18: record what was clicked so S14 can build the right items, and open
                // the menu without moving the caret.
                self.context_menu_target = Some(lc);
                // The *screen* cell the pointer was over, which is where the menu opens from.
                // Separate from `context_menu_target` on purpose: that is a document position,
                // and using it as a column would open the menu at a character offset.
                self.context_menu_cell = (self.last_pointer.0, self.last_pointer.1);
                self.push_mode(Mode::ContextMenu(kind));
                self.reset_dialog_focus();
            }
            I::OpenRecentFile(path) => {
                self.close_menu();
                self.open_path(&path);
            }
            I::OpenSubmenu(menu, label) => {
                // The pointer opened a submenu level rather than running anything.
                self.push_submenu(menu, &label);
            }
            I::RunMenuItem(entry) => {
                // A codec row carries its index in `hint`; `execute` reads `pending_encoding`.
                // Recovering it here is what makes a *clicked* Convert To row behave like a
                // keyboard-activated one -- the keyboard path does the same in
                // `chrome::activate_menu_item`.
                if matches!(
                    entry.action,
                    Some(Action::ConvertEncoding) | Some(Action::ReloadAsEncoding)
                ) {
                    if let Ok(i) = entry.hint.parse::<usize>() {
                        if let Some(enc) = np_core::EncodingId::all().get(i) {
                            self.pending_encoding = Some(enc.clone());
                        }
                    }
                }
                if let Some(action) = entry.action {
                    self.apply_menu_action(action);
                }
            }
            I::RunAction(action) => {
                self.apply_menu_action(action);
            }
            I::FocusDialogChild(i) => {
                // Focus the child the pointer landed on, then run its activation. A press on a
                // control -- button, checkbox, field -- acts immediately, which is what makes a
                // dialog's OK button feel immediate and is what every toolkit does.
                //
                // This arm used to carry its own copy of the activation rules, and the copy was
                // how the dialogs drifted apart: it called the Find dialog's flag toggle for any
                // checkbox it saw, so a Preferences row click ran Find's option table against an
                // unrelated index, and it knew nothing at all about rows in dialogs it had not
                // been taught about. Activation now lives in one method, `activate_dialog_child`,
                // shared with the keyboard and with the click-release path.
                self.dialog_focus = i;
                self.dialog_press = Some(DialogPress {
                    child: i,
                    row: self.focus_child_row(i),
                });
                self.sync_legacy_field_focus();
                // Record or drop the pointer's row choice. A click on a non-row releases it, so
                // the field and the buttons are not held hostage by an earlier click.
                self.pointer_row = self.focus_child_row(i);
                self.activate_dialog_child(i);
            }
            I::BannerAction(reload) => {
                // FR-F7: Reload re-reads the file, Ignore drops the notice and keeps the
                // editor's version.
                self.banner = None;
                if reload {
                    self.action_reload(None);
                }
                self.dirty = true;
            }
            I::CloseServiceStrip => self.close_service_strip(),
            I::RunFindPanelSearch => {
                // Exactly what `Enter` runs in the query field, so the word and the key cannot
                // disagree about what starting a search means.
                crate::fif::start(self);
            }
            I::ScrollFindResults { lines } => {
                crate::fif::scroll_window(self, lines);
            }
            I::SelectFindField(field) => {
                // Clicking a field both focuses the panel and points the keyboard at that field,
                // which is what a click on a text field means everywhere else in the app.
                self.fif.editing = true;
                self.fif.editing_field = field;
                // A field click clears the toggle highlight: leaving it set would mean the next
                // Space typed a space *and* the painter drew a highlight on two stops at once.
                self.fif.focused_toggle = None;
                self.fif.focused = true;
                if !matches!(self.mode(), Mode::FindPanel) {
                    self.push_mode(Mode::FindPanel);
                }
                self.dirty = true;
            }
            I::ToggleFindFlag(toggle) => {
                // A click on a box flips it and leaves the highlight there, so the pointer and the
                // keyboard agree about where the user is.
                toggle.toggle(self);
                self.fif.focused_toggle = Some(toggle);
                self.fif.editing = false;
                self.fif.focused = true;
                if !matches!(self.mode(), Mode::FindPanel) {
                    self.push_mode(Mode::FindPanel);
                }
                self.dirty = true;
            }
            I::DismissMenuThen(inner) => {
                self.close_menu();
                self.apply_mouse_intent(*inner);
            }
            I::DismissContextMenu => {
                while matches!(self.mode_stack.last(), Some(Mode::ContextMenu(_))) {
                    self.pop_mode();
                }
                self.context_menu_target = None;
            }
            I::CloseDialog => {
                // A click outside the box means the same thing to the user as `Esc`, so it runs
                // the same code rather than a second implementation of "close a dialog". That
                // reuse is the point: `handle_escape` owns the cases where closing is not just a
                // mode pop -- Find and Replace write their options back to `[find]` config (FR-S1),
                // and a running Find in Files search must be cancelled before it can be closed, so
                // that a second press reads the partial results. A bare `pop_mode` here would drop
                // both, and the two paths would drift the first time either rule changed.
                self.handle_escape();
            }
            I::Hover(target) => {
                // Only redraw when the highlight actually changed: the pointer generates a
                // `Moved` for every cell it crosses, and repainting the whole screen for each
                // one is the classic way to make a mouse-driven TUI feel sluggish.
                if self.hover != target {
                    self.hover = target;
                    self.dirty = true;
                }
            }
            I::PastePrimary => {
                // Row 7, closed: the primary selection now has a provider (S7).
                self.action_paste_primary();
            }
            I::ToggleFold(line) => {
                crate::view::toggle_fold_at(self, line);
            }
            I::ToggleBookmark(line) => {
                crate::view::toggle_bookmark_at(self, line);
            }
        }
    }

    /// Make tab `i` active, ignoring an out-of-range index rather than clamping: a stale
    /// index means the tab vanished between the click and now, and switching to an unrelated
    /// tab would be worse than doing nothing.
    fn activate_tab(&mut self, i: usize) {
        if i < self.documents.len() && i != self.active {
            self.active = i;
            // Everything the chrome shows -- overtype, the status path segment, the gutter
            // width -- is per document, so a switch has to re-sync all of it.
            self.load_active_view();
        }
    }

    /// Close a specific tab index, honoring the unsaved-changes confirmation.
    fn close_tab_index(&mut self, i: usize) {
        if i >= self.documents.len() {
            return;
        }
        self.request_close(i, PendingOp::CloseTab(i));
    }

    /// Vertical wheel scroll, keeping the caret visible the way a wheel should.
    ///
    /// `lines` is a count of **visual rows** (FR-V8, and the same unit `Scroll` means
    /// everywhere else). Under wrap the wheel must scroll rows or a single tall wrapped line
    /// becomes an unscrollable wall: scrolling by line index would step the whole paragraph at
    /// once and the rows in the middle of it could never be brought to the top.
    fn scroll_lines(&mut self, lines: i32) {
        let height = self.geometry.viewport_rows().max(1);
        let geom = self.geometry;
        let Some(doc) = self.doc() else { return };
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        let current = row_offset_of_top(doc, &geom);
        let target = (current as i64 + lines as i64).max(0) as usize;
        let last_top = total.saturating_sub(1);
        let clamped = target.min(last_top);
        let top = np_core::line_at_row_offset(&doc.buffer, &geom, &doc.folds, clamped)
            .unwrap_or(0)
            .min(doc.buffer.line_count().saturating_sub(1));
        // The caret follows the viewport only when it would otherwise be off screen. A wheel
        // that drags the caret along with every scroll makes it impossible to scroll ahead to
        // read and keep your place.
        let mut caret = doc.cursor.pos;
        if caret.line < top {
            caret.line = top;
        } else if caret.line >= top + height {
            caret.line = top + height - 1;
        }
        let (caret, top) = (caret, top);
        if let Some(doc) = self.doc_mut() {
            doc.scroll.top_line = top;
            doc.cursor.pos = caret;
        }
        self.dirty = true;
    }

    /// Horizontal wheel scroll. Inert while wrap is on, because there is no horizontal
    /// position when every line fits (FR-V1).
    fn scroll_columns(&mut self, cols: i32) {
        if self.geometry.word_wrap {
            return;
        }
        let Some(doc) = self.doc() else { return };
        let left = if cols < 0 {
            doc.scroll.left_col.saturating_sub((-cols) as usize)
        } else {
            doc.scroll.left_col.saturating_add(cols as usize)
        };
        if let Some(doc) = self.doc_mut() {
            doc.scroll.left_col = left;
        }
        self.dirty = true;
    }

    /// How much scrollable content one trough cell stands for, for the given bar.
    ///
    /// Returns `(scrollable_offsets, reachable_thumb_cells)`, or `None` when the bar does not
    /// exist. A drag converts pointer cells through this pair, which is the same reachable-span
    /// reasoning [`crate::ui::gutter::ScrollbarGeom::offset_at`] uses, so a press and a drag of
    /// the same gesture move the document on the same scale.
    pub fn scroll_density(&self, vertical: bool) -> Option<(usize, usize)> {
        let (bar, content, viewport) = if vertical {
            self.vertical_scrollbar_detail()?
        } else {
            self.horizontal_scrollbar_detail()?
        };
        let reachable = bar.track_len.saturating_sub(bar.thumb_len) as usize;
        Some((content.saturating_sub(viewport), reachable))
    }

    /// Scroll so document row `row` is at the top of the viewport.
    ///
    /// The row is clamped to the scrollable span, so a drag past the bottom of the trough parks
    /// the view on the last page rather than on blank space below the document.
    fn scroll_to_row(&mut self, row: usize) {
        // Every caller means "as close to this row as the document allows". A wrapped document
        // can make a requested row unreachable: its first row belongs to a line whose earlier
        // rows are above it, so starting the viewport at the *line* shows that line's topmost row
        // instead. Snapping to a line boundary is what keeps a scrollbar drag monotonic -- without
        // it the last cell of the trough can ask for a row that renders as the previous page.
        let geom = self.geometry;
        // Both ceilings are read before the document is borrowed mutably: they ask the painter
        // questions, and the painter takes `&self`.
        let max_offset = self.max_viewport_row_offset();
        let deepest = self.deepest_visible_top_line();
        let height = self.geometry.viewport_rows().max(1);
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let last = doc.buffer.line_count().saturating_sub(1);
        // A request at the ceiling means "the last page", and the row arithmetic below cannot
        // answer that. A wrapped page is only as long as the whole lines that fit in it -- with
        // 6-row wrapped lines in a 27-row viewport, 25 -- so `total_rows - viewport_rows` is not
        // the row the last page starts at, and resolving a clamped request through it lands a
        // page short of the end with the ceiling refusing to go further. That is how the last few
        // rows of a wrapped file became unreachable by any gesture. The painter's own answer is
        // used instead, and it agrees with the bar because the bar is scaled from the same line.
        if row >= max_offset {
            doc.scroll.top_line = deepest.min(last);
            self.dirty = true;
            return;
        }
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        let max_row = total.saturating_sub(height);
        let target = np_core::line_at_row_offset(&doc.buffer, &geom, &doc.folds, row.min(max_row))
            .unwrap_or(0)
            .min(last);
        doc.scroll.top_line = target;
        self.dirty = true;
    }

    /// The deepest line the viewport can start at and still show the document's last row.
    ///
    /// This is the real ceiling on scrolling. The scrollbar is scaled against its row offset
    /// ([`Self::max_viewport_row_offset`]) and `scroll_to_row` sends a request at that ceiling to
    /// this line, so both the bar and the scroller agree with the painter by asking the painter.
    ///
    /// # Why it is found by searching rather than by arithmetic
    ///
    /// A wrapped viewport shows whole lines only -- `layout_visible_rows` drops a line whose
    /// remaining rows do not fit rather than painting half of it -- so a page is as long as the
    /// last whole lines before the viewport's height add up to, and there is often no page of
    /// exactly `viewport_rows` rows. Lines that wrap to 6 rows in a viewport of 27 give pages of
    /// 25. That makes `total_rows - viewport_rows` the wrong ceiling: it names a row whose line
    /// starts a page stopping a whole line short of the end, and because every request is clamped
    /// to the ceiling the final rows become unreachable by any gesture. Scrolling to the bottom
    /// showed the second-to-last line and stopped, permanently.
    ///
    /// So: walk backwards from the deepest line that could plausibly start a page, and take the
    /// first page that reaches the document's last line. The walk is bounded by the viewport's
    /// height in lines, since a page starting earlier than one screen of lines cannot show the end
    /// either. Mid-document requests never reach this, and here it costs one layout.
    pub fn deepest_visible_top_line(&self) -> usize {
        let geom = self.geometry;
        let Some(doc) = self.doc() else {
            return 0;
        };
        let height = geom.viewport_rows().max(1);
        let last = doc.buffer.line_count().saturating_sub(1);
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        if total <= height {
            return 0;
        }
        if !geom.word_wrap {
            return last.saturating_sub(height - 1);
        }
        // Seed: the line containing the row offset a full page would start at, if pages were
        // exactly the viewport's height. Because a wrapped page shows whole lines only it is
        // *shorter* than the viewport, so the deepest page starts at a line *later* than the seed
        // -- walking forwards, not backwards. Walking backwards goes up the document and away
        // from the end, which is the mistake this note exists to record.
        let seed = np_core::line_at_row_offset(&doc.buffer, &geom, &doc.folds, total - height)
            .unwrap_or(last)
            .min(last);
        // Walk forwards from the seed to the last line whose page still reaches the document's
        // end. Pages shrink as the walk goes on and reach the end for a while (the tail lines are
        // short), so the deepest one is the *last* passing test, and the first failure ends the
        // run: past that point every page starts below the end's own line. Bounded by `last`,
        // which is a hard stop because a page starting at the last line is the shortest possible.
        let mut best = seed;
        let mut line = seed;
        while line < last {
            let next = line + 1;
            let reaches = np_core::layout_visible_rows(&doc.buffer, &geom, next, &doc.folds)
                .last()
                .is_some_and(|r| r.line >= last);
            if !reaches {
                break;
            }
            best = next;
            line = next;
        }
        best
    }

    /// The row offset of [`Self::deepest_visible_top_line`], which is the unit the scrollbar
    /// scales in.
    ///
    /// Kept as its own name because the bar's geometry, the trough's click scaling, and the
    /// "go to the end" intent all want the offset rather than the line, and a second derivation
    /// of either is how a bar ends up offering a position the painter refuses.
    pub fn max_viewport_row_offset(&self) -> usize {
        let geom = self.geometry;
        let Some(doc) = self.doc() else {
            return 0;
        };
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        let top = self.deepest_visible_top_line();
        np_core::row_offset_of_line(&doc.buffer, &geom, &doc.folds, top).min(total)
    }

    /// Scroll so display column `col` is at the left edge. Inert under wrap, where there is no
    /// horizontal position to have (FR-V1).
    fn scroll_to_col(&mut self, col: usize) {
        if self.geometry.word_wrap {
            return;
        }
        // Geometry read into locals first: the width loop below borrows the document mutably,
        // and reading `self.geometry` from inside the closure would borrow `self` a second time.
        let geom = self.geometry;
        let tab = geom.tab_width.max(1);
        let visible = geom.viewport_cols().max(1);
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let longest = (0..doc.buffer.line_count())
            .map(|line| {
                doc.buffer
                    .display_width_range(line, 0, doc.buffer.line_len_chars(line), tab)
            })
            .max()
            .unwrap_or(0);
        doc.scroll.left_col = col.min(longest.saturating_sub(visible));
        self.dirty = true;
    }

    /// The editor area's visible row count, used for paging and the viewport.
    /// Re-derive the find mark list from the active document.
    ///
    /// A method on `App` rather than a call through `app.find.refresh_marks(doc)` because the
    /// borrow that would require is `&self.find` plus `&self.documents` at once, which the
    /// borrow checker refuses. This takes `&mut self` and hands the two out in turn.
    /// Open any collapsed range hiding the caret's line, and re-detect folds if that changed
    /// the gutter's marker columns (FR-V5).
    ///
    /// Cheap: `unfold_containing` walks the fold list, which is small, and the geometry is only
    /// recomputed when a marker column actually appeared or disappeared -- which for a document
    /// that already has folds is never.
    fn unfold_caret_line(&mut self) {
        let Some(line) = self.doc().map(|d| d.cursor.pos.line) else {
            return;
        };
        let had_folds = self.doc().map(|d| !d.folds.is_empty()).unwrap_or(false);
        if !had_folds {
            return;
        }
        let hidden = self.doc().map(|d| d.line_is_hidden(line)).unwrap_or(false);
        if !hidden {
            return;
        }
        if let Some(doc) = self.doc_mut() {
            doc.unfold_line(line);
        }
        self.session_dirty = true;
        let (w, h) = self.frame_size();
        self.recompute_geometry(w, h);
    }

    pub fn refresh_marks(&mut self) {
        let marks = if self.find.mark_matches
            && self.find.error.is_none()
            && !self.find.needle.is_empty()
        {
            self.doc().map(|d| {
                np_core::Matcher::new(&d.buffer, &self.find.needle, &self.find.opts)
                    .map(|m| m.matches().to_vec())
                    .unwrap_or_default()
            })
        } else {
            None
        };
        match marks {
            Some(m) => self.find.marks = m,
            None => self.find.marks.clear(),
        }
    }

    pub fn editor_height(&self) -> usize {
        self.regions.editor.h as usize
    }

    /// The size this `App` was last laid out at, so a tab switch can re-layout without the
    /// caller supplying a terminal size it already knows.
    ///
    /// Derived from the regions rather than stored, because the regions are the ground
    /// truth of the last layout and a cached copy would drift the first time a resize
    /// landed between two tab switches.
    pub fn frame_size(&self) -> (u16, u16) {
        let width = self.regions.menu.w;
        let height = self
            .regions
            .status
            .bottom()
            .max(self.regions.editor.bottom());
        (width, height)
    }

    /// Recompute the four regions and the editor geometry for a terminal size.
    ///
    /// Stacking order is spec §5.4: menu bar, tab bar, editor, status bar. The editor takes
    /// everything left over, so a one-row terminal still renders something rather than
    /// underflowing.
    pub fn recompute_geometry(&mut self, width: u16, height: u16) {
        let menu_h = 1u16;
        let tabs_h = 1u16;
        let status_h = 1u16;
        // On a very short terminal the chrome cannot all fit. Give the editor whatever is
        // left, down to zero, rather than wrapping the subtraction: a 2-row terminal shows
        // menu + status and no editor, which is degraded but honest.
        let chrome = menu_h + tabs_h + status_h;
        let editor_h = height.saturating_sub(chrome);

        let menu = RectCore {
            x: 0,
            y: 0,
            w: width,
            h: menu_h.min(height),
        };
        let tabs = RectCore {
            x: 0,
            y: menu.bottom().min(height),
            w: width,
            h: tabs_h.min(height.saturating_sub(menu.bottom())),
        };
        let band = RectCore {
            x: 0,
            y: tabs.bottom().min(height),
            w: width,
            h: editor_h,
        };
        // Docked service lines own the bottom rows of the band. They have to come out of the
        // region the text is laid out in, or the two draw into the same cells: the Find in Files
        // panel was painted over the middle of the buffer and the text reappeared below it, so
        // header lines and text lines interleaved on the same rows.
        //
        // Find/Replace and Find in Files are mutually exclusive -- one is a dialog mode and the
        // other takes the band when it is not -- so the reservation is whichever strip is open,
        // not the sum. `max` rather than `+` keeps that true even if a future state ever shows
        // both, where a sum would reserve rows nobody painted.
        //
        // The height is clamped against the *band* here rather than against `editor`, and
        // `fif::editor_room` asks for the same thing, so the two cannot disagree about how much
        // room the text has. Keeping the floor of three rows is `fif`'s rule and is applied here
        // too, because a panel that leaves no text to look at is not a search result.
        let strip = crate::ui::find_bar::height(self).max(
            self.find_panel_height
                .min(band.h.saturating_sub(FIF_MIN_EDITOR_ROWS)),
        );
        let strip = strip.min(band.h.saturating_sub(FIF_MIN_EDITOR_ROWS));
        let editor = RectCore {
            x: band.x,
            y: band.y,
            w: band.w,
            h: band.h.saturating_sub(strip),
        };
        let status = RectCore {
            x: 0,
            y: band.bottom().min(height),
            w: width,
            h: status_h.min(height.saturating_sub(band.bottom())),
        };
        self.regions = Regions {
            menu,
            tabs,
            editor,
            status,
            band,
        };

        // The gutter's column layout comes from `ui::gutter`, the one place it is computed, so
        // the renderer and the hit-tester cannot disagree about which cell a click landed on.
        // Marker columns appear only when the document has something to put in them: a plain
        // text file keeps the exact gutter width it had before this stage.
        let gutter = crate::ui::gutter::Gutter::new(
            editor.x,
            self.doc().map(|d| d.buffer.line_count()).unwrap_or(1),
            self.config.ui.line_numbers,
            self.doc().map(|d| !d.folds.is_empty()).unwrap_or(false),
            self.doc().map(|d| !d.bookmarks.is_empty()).unwrap_or(false),
        );
        let tab_width = self
            .doc()
            .map(|d| d.tab_width())
            .unwrap_or(self.config.editor.tab_width);
        let gutter_width = gutter.width as usize;
        let text_area = RectCore {
            x: gutter_width as u16,
            y: editor.y,
            w: width.saturating_sub(gutter_width as u16),
            h: editor.h,
        };
        let word_wrap = self.config.ui.word_wrap;
        // The fold list is cloned rather than borrowed: the fixed-point loop below asks the
        // row-count question, which takes `&self.geometry`, while `self.doc()` is borrowed.
        let folds = self.doc().map(|d| d.folds.clone()).unwrap_or_default();
        let buffer = self.doc().map(|d| &d.buffer);

        let tab = tab_width.max(1);
        // Widest line in the document, in display cells. Cached on the buffer revision, so
        // this is free while typing and costs one walk after an edit.
        let content_cols = buffer
            .map(|b| b.max_line_width(tab))
            .filter(|_| !word_wrap)
            .unwrap_or(0);

        // Two passes, because the two bars depend on each other. The horizontal bar's row
        // shrinks the viewport height, which changes whether the vertical bar is needed; the
        // vertical bar's column shrinks the width, which changes whether a long line still
        // overflows and needs the horizontal bar. One pass with the pre-bar width decides the
        // horizontal bar against a width that will not exist, which is exactly how a bar can
        // appear, vanish, and appear again while the user holds down a resize. A second pass
        // over two booleans is not a cost worth arguing about; a third would only be needed if
        // the answers oscillated, and `BarOccupancy::compute` is monotone in both inputs, so
        // they cannot.
        let mut bars = np_core::BarOccupancy::default();
        for _ in 0..2 {
            let content_rows = buffer
                .map(|b| np_core::total_row_count(b, &self.geometry, &folds))
                .unwrap_or(1);
            let next = np_core::BarOccupancy::compute(
                editor.h,
                text_area.w,
                content_rows,
                content_cols,
                word_wrap,
            );
            if next == bars {
                break;
            }
            bars = next;
        }

        self.geometry = ViewGeometry {
            gutter_width,
            text_area,
            tab_width: tab,
            word_wrap,
            bars,
        };
        self.dirty = true;
    }

    /// Mark everything as needing a redraw.
    pub fn mark_all_dirty(&mut self) {
        self.dirty = true;
    }

    // ------------------------------------------------------------ scrollbars

    /// Where the vertical scrollbar's thumb is, or `None` when there is nothing to scroll.
    ///
    /// Computed from the document and the stored geometry rather than read out of a draw call,
    /// which is what lets the mouse layer and the renderer agree on the thumb's cells without
    /// either one owning the other. See [`crate::ui::gutter::ScrollbarGeom`] for why that
    /// agreement has to be by shared arithmetic rather than by convention.
    pub fn vertical_scrollbar(&self) -> Option<crate::ui::gutter::ScrollbarGeom> {
        let (bar, _total, _viewport) = self.vertical_scrollbar_detail()?;
        Some(bar)
    }

    /// [`Self::vertical_scrollbar`] with the two numbers a click needs: the document's total row
    /// count and the viewport height in rows.
    ///
    /// Returning them alongside the geometry is not redundancy for its own sake. A click resolves
    /// to a scroll offset as a fraction of `total - viewport`, and both of those come from the
    /// *current* geometry; a caller that re-derived them could disagree with the bar it just
    /// hit-tested if anything changed in between, and the disagreement would be a thumb that
    /// lands a few rows off -- small enough to pass a casual look, big enough to annoy.
    pub fn vertical_scrollbar_detail(
        &self,
    ) -> Option<(crate::ui::gutter::ScrollbarGeom, usize, usize)> {
        let doc = self.doc()?;
        let geom = self.geometry;
        // `bars.cols` is the single answer to "is this bar drawn": `recompute_geometry` decided
        // it, the row layout was built against it, and the renderer draws what it says. Asking
        // `total <= viewport` here as well would be a second opinion, and a second opinion that
        // disagreed would draw a bar in a column the text already occupies.
        if geom.bars.cols == 0 {
            return None;
        }
        let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
        let viewport = geom.viewport_rows();
        if viewport == 0 {
            return None;
        }
        // The track is the text area's column of rows, one cell wide at the editor's right edge.
        let area = self.regions.editor;
        // Ratatui renders a vertical bar into the area's last *column*, so its track runs the
        // full band height -- including the row the horizontal bar occupies. The corner cell
        // belongs to the vertical bar, which is why the track is the band's height rather than
        // the viewport's. `scrollbar_hit` resolves that same cell the same way.
        let track_len = area.h;
        // The bar is drawn over the *range of offsets the view can take*, not over the document's
        // row count. That distinction is what makes the thumb and the clicks agree:
        // `scroll_to_row` clamps to `max_viewport_row_offset`, so a thumb positioned against
        // `total` sits at a cell whose click resolves past that clamp. The bottom of the trough
        // then asks for something the scroller refuses, the view snaps back, and the bar reads
        // exactly as reported: "jumps under the pointer, the text jumps to the first line and
        // returns".
        //
        // So `content` here is the scrollable span plus the viewport. The thumb's length stays
        // the viewport's share of the visible range, which is what a thumb means, and its travel
        // is bounded by the same maximum the scroller enforces.
        let max_offset = self.max_viewport_row_offset();
        let span = max_offset.saturating_add(viewport).max(1);
        let top_row = self.top_row_offset(max_offset);
        let bar =
            crate::ui::gutter::ScrollbarGeom::new(area.y, track_len, span, viewport, top_row)?;
        Some((bar, total, viewport))
    }

    /// Where the horizontal scrollbar's thumb is, or `None` when it should not exist at all.
    ///
    /// `None` under wrap, unconditionally: FR-V1 disables horizontal scrolling when wrapping,
    /// and a scrollbar that cannot be dragged is worse than no scrollbar.
    pub fn horizontal_scrollbar(&self) -> Option<crate::ui::gutter::ScrollbarGeom> {
        let (bar, _longest, _visible) = self.horizontal_scrollbar_detail()?;
        Some(bar)
    }

    /// [`Self::horizontal_scrollbar`] plus the content width and visible width in columns, for
    /// the same reason as [`Self::vertical_scrollbar_detail`].
    pub fn horizontal_scrollbar_detail(
        &self,
    ) -> Option<(crate::ui::gutter::ScrollbarGeom, usize, usize)> {
        let doc = self.doc()?;
        let geom = self.geometry;
        // See `vertical_scrollbar_detail`: one decision, made in `recompute_geometry`.
        if geom.bars.rows == 0 {
            return None;
        }
        let tab = geom.tab_width.max(1);
        // Cached on the buffer's revision. The uncached walk is O(document) per frame and
        // measured 2.5 s on a 100 MB file.
        let longest = doc.buffer.max_line_width(tab);
        let visible = geom.viewport_cols().max(1);
        let area = self.regions.editor;
        // The track stops one cell short when the vertical bar is drawn, which is the cell
        // `draw_scrollbars` narrows ratatui's area to and the cell the vertical bar paints.
        // Measuring the thumb against the full band width would put it a cell off from the
        // trough the user can see, and a grab target that is not the painted thumb is the one
        // scrollbar bug a user notices immediately.
        let track_len = area.w.saturating_sub(geom.bars.cols).max(1).min(area.w);
        crate::ui::gutter::ScrollbarGeom::new(
            area.x,
            track_len,
            longest,
            visible,
            doc.scroll.left_col,
        )
        .map(|bar| (bar, longest, visible))
    }

    /// The row offset the viewport's top line sits at, in the whole-document row list.
    ///
    /// A scroll position past the end -- which the layout clamps and paints at the bottom --
    /// reports the last scrollable offset rather than the document's length, so the thumb sits
    /// at the bottom of the trough instead of vanishing past it.
    pub fn top_row_offset(&self, max_offset: usize) -> usize {
        let Some(doc) = self.doc() else {
            return 0;
        };
        let offset = row_offset_of_top(doc, &self.geometry);
        // Clamped to the range the bar was drawn over, so a scroll position the layout has
        // already clamped at paint time cannot put the thumb past the trough's last cell.
        offset.min(max_offset)
    }

    // -------------------------------------------------------- key routing

    /// Route a key event: mode first, then keymap (spec §5.2).
    ///
    /// Returns the action that was executed, so callers and tests can see what a key did.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Option<Action> {
        if ev.kind != KeyEventKind::Press {
            return None;
        }
        // Esc is structural rather than a command, so it is handled here and never enters
        // the keymap: it must work even when every binding has been remapped away.
        if ev.code == KeyCode::Esc {
            self.handle_escape();
            return None;
        }
        // The S14 chrome owns input for every mode that is not plain editing, in one place:
        // the menu bar, the palette, a context menu, the help overlay, and every dialog.
        // Routing them through one call is what guarantees a printable character typed in a
        // dialog can never reach the document -- the pre-S14 version listed each dialog
        // separately, and any dialog missing from the list leaked keystrokes into the buffer.
        if let Some(action) = self.route_chrome_key(ev) {
            // A chrome handler returns the command the input *selected*, not one it already
            // ran: the menu bar, palette and context menu all close themselves before handing
            // the action back, so running it here is what keeps `execute` the single place
            // state changes. Reporting without executing would make Enter a no-op.
            if action != Action::Nop {
                self.execute(action);
            }
            return Some(action);
        }
        // The Document List is the one dialog with real keyboard behavior before S14: it
        // is a list, not a text field, so arrows and Enter are all it needs.
        if self.mode() == Mode::Dialog(DialogKind::DocumentList) {
            return self.handle_document_list_key(ev);
        }
        // The Find and Replace dialogs own their keys completely (S10).
        if matches!(
            self.mode(),
            Mode::Dialog(DialogKind::Find) | Mode::Dialog(DialogKind::Replace)
        ) {
            return crate::ui::dialogs::handle_find_key(self, ev);
        }
        if self.mode() == Mode::Dialog(DialogKind::GoTo) {
            return self.handle_goto_key(ev);
        }
        // The Language and Theme pickers are filtered lists (S12, FR-H2/H3): typing narrows
        // the list rather than the document, so they must own input before the editing path.
        if matches!(
            self.mode(),
            Mode::Dialog(DialogKind::LanguagePicker) | Mode::Dialog(DialogKind::ThemePicker)
        ) {
            let which = match self.mode() {
                Mode::Dialog(k) => k,
                _ => return None,
            };
            return self.handle_picker_key(which, ev).then_some(Action::Nop);
        }
        // The Find in Files panel owns the keyboard while focused (S11). Typing goes to the
        // query field, arrows move the result selection, Enter opens the selected hit.
        if self.mode() == Mode::FindPanel {
            return self.handle_find_panel_key(ev);
        }
        // While any other dialog or the palette owns the keyboard, printable characters
        // belong to its text field, not to the document. Those widgets arrive in S14; until
        // then they consume everything except Esc so typing cannot leak into the buffer.
        if !matches!(self.mode(), Mode::Editing) {
            return None;
        }
        // FR-V5: an edit inside a collapsed range unfolds it. Done here rather than inside
        // `Document::insert_text` because the rule is about the *screen*: the caret is on a line
        // the layout is not showing, and text arriving there with the fold still shut means the
        // user is typing into a block they cannot see. Every mutation path -- typing, backspace,
        // Enter, Tab, and every bound editing action -- passes through this one point, so one
        // check here covers them all instead of one call at each of a dozen edit sites.
        self.unfold_caret_line();
        if let Some(action) = self.keymap.resolve(ev) {
            self.execute(action);
            return Some(action);
        }
        // Everything below mutates the document and moves the caret without going through
        // `execute`, so the viewport is reeled back in once at the end of the editing path
        // rather than at each of these four sites.
        let out = self.editing_key(ev);
        crate::view::ensure_caret_visible(self);
        out
    }

    /// The editing-mode keystrokes that do not correspond to a bound action: text,
    /// Backspace, Enter and Tab.
    fn editing_key(&mut self, ev: KeyEvent) -> Option<Action> {
        // Unmapped printable input is text.
        if let KeyCode::Char(c) = ev.code {
            // Alt and Ctrl chords that are not bound must not insert their letter, or a
            // mistyped shortcut would scribble in the buffer.
            if ev.modifiers.contains(KeyModifiers::ALT)
                || ev.modifiers.contains(KeyModifiers::CONTROL)
            {
                return None;
            }
            self.insert_char(c);
            return Some(Action::Nop);
        }
        if ev.code == KeyCode::Backspace {
            self.execute_backspace();
            return Some(Action::Delete);
        }
        if ev.code == KeyCode::Enter {
            self.execute_enter();
            return Some(Action::Nop);
        }
        if ev.code == KeyCode::Tab || ev.code == KeyCode::BackTab {
            self.press_tab(ev.code == KeyCode::BackTab);
            return Some(Action::Nop);
        }
        None
    }

    /// Route input for every non-editing mode (S14).
    ///
    /// Returns `None` only when the mode is `Editing`, which is the signal to fall through to
    /// the keymap. For any chrome mode the answer is always `Some`, including for keys the
    /// widget ignores, because falling through would type into the document.
    fn route_chrome_key(&mut self, ev: KeyEvent) -> Option<Action> {
        match self.mode() {
            Mode::Editing => None,
            Mode::Menu { .. } => crate::ui::chrome::menu_key(self, ev),
            Mode::Palette => crate::ui::chrome::palette_key(self, ev),
            Mode::ContextMenu(_) => crate::ui::chrome::context_menu_key(self, ev),
            Mode::HelpOverlay => crate::ui::chrome::help_key(self, ev),
            Mode::FindPanel => None,
            Mode::Dialog(kind) => crate::ui::dialog_keys::dialog_key(self, kind, ev),
        }
    }

    /// Keys for the Document List: arrows move, Enter activates, everything else is eaten.
    ///
    /// Eating the rest is not laziness -- without it a stray `Ctrl+S` while the list is open
    /// would save whichever document happened to be active, not the one highlighted.
    fn handle_document_list_key(&mut self, ev: KeyEvent) -> Option<Action> {
        match ev.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.document_list_move(1);
                Some(Action::Nop)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.document_list_move(-1);
                Some(Action::Nop)
            }
            KeyCode::Enter => {
                self.document_list_activate();
                Some(Action::Nop)
            }
            KeyCode::Tab => {
                self.document_list_move(1);
                Some(Action::Nop)
            }
            KeyCode::BackTab => {
                self.document_list_move(-1);
                Some(Action::Nop)
            }
            _ => None,
        }
    }

    /// Keys for the Go To dialog (FR-S6).
    ///
    /// Digits, `:` and Backspace edit the field; Enter commits through `find::go_to`, which
    /// clamps and reports bad input inline. Everything else is eaten so a stray keystroke
    /// cannot reach the document.
    /// Keyboard while the Find in Files panel has focus (S11, FR-FF2/FF3).
    ///
    /// Which field typing goes to is decided by [`crate::fif::FifState::editing`]: Enter runs the
    /// search, arrows move the result selection, and a second Enter on a results view opens the
    /// selected hit. Everything is consumed, so a keystroke can never reach the document while
    /// the panel is focused -- the same rule the Go To dialog follows.
    fn handle_find_panel_key(&mut self, ev: crossterm::event::KeyEvent) -> Option<Action> {
        let shift = ev.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
        match ev.code {
            KeyCode::Enter => {
                if self.fif.editing {
                    crate::fif::start(self);
                } else {
                    crate::fif::open_selected(self);
                }
                Some(Action::FindInFiles)
            }
            KeyCode::Up => {
                crate::fif::move_selection(self, -1);
                Some(Action::Nop)
            }
            KeyCode::Down => {
                crate::fif::move_selection(self, 1);
                Some(Action::Nop)
            }
            KeyCode::PageUp => {
                let page = crate::ui::find_in_files::result_rows(self.find_panel_height) as isize;
                crate::fif::move_selection(self, -page.max(1));
                Some(Action::Nop)
            }
            KeyCode::PageDown => {
                let page = crate::ui::find_in_files::result_rows(self.find_panel_height) as isize;
                crate::fif::move_selection(self, page.max(1));
                Some(Action::Nop)
            }
            KeyCode::Tab => {
                // Find -> Dir -> Files -> the three toggles -> Find.
                //
                // The toggles used to sit outside this cycle, reachable only by their `Ctrl+`
                // chords. That made them invisible and, in the user's words, "can't be reached by
                // any other way" -- a control with one undiscoverable binding is not a control.
                // The chords still work and are now also printed on the row.
                self.fif_step_focus(1);
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::BackTab => {
                self.fif_step_focus(-1);
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char(' ') => {
                // Space flips the toggle the keyboard is on. It only means that while a toggle
                // is highlighted: in a text field space is a character, and a search term with
                // spaces in it is completely normal.
                if let Some(toggle) = self.fif.focused_toggle {
                    toggle.toggle(self);
                    return Some(Action::Nop);
                }
                let text = self.fif_field_text_mut();
                text.push(' ');
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Left | KeyCode::Right => {
                // Once the highlight is on a toggle, Left/Right move along the three of them
                // rather than the caret -- there is no text caret to move at that point.
                if self.fif.focused_toggle.is_some() {
                    let dir = if ev.code == KeyCode::Left { -1 } else { 1 };
                    self.fif_step_toggle(dir);
                    self.dirty = true;
                    return Some(Action::Nop);
                }
                // Otherwise arrows move the results selection, as Up/Down already do.
                crate::fif::move_selection(self, if ev.code == KeyCode::Left { -1 } else { 1 });
                Some(Action::Nop)
            }
            KeyCode::Backspace => {
                let text = self.fif_field_text_mut();
                text.pop();
                self.fif.error = None;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char('c')
                if ev
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.fif.opts.case_sensitive = !self.fif.opts.case_sensitive;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char('w')
                if ev
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.fif.opts.whole_word = !self.fif.opts.whole_word;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char('r')
                if ev
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.fif.opts.regex = !self.fif.opts.regex;
                self.fif.error = None;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char('g')
                if ev
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.fif.use_gitignore = !self.fif.use_gitignore;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char('h')
                if ev
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.fif.include_hidden = !self.fif.include_hidden;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Char(c) => {
                // Alt chords are not bound here and must not type, matching the editor rule:
                // a mistyped shortcut should not scribble in a field.
                if ev.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                    return Some(Action::Nop);
                }
                let text = self.fif_field_text_mut();
                text.push(c);
                self.fif.error = None;
                self.dirty = true;
                Some(Action::Nop)
            }
            _ => {
                let _ = shift;
                Some(Action::Nop)
            }
        }
    }

    /// Move the panel's keyboard highlight by `delta` stops.
    ///
    /// The stops are one flat list -- three fields then three toggles -- held in
    /// [`crate::fif::FIF_STOPS`] so the painter, this walk and the mouse cannot each carry their
    /// own order. Landing on a field turns typing back on; landing on a toggle turns it off, which
    /// is what makes Space mean "flip this" and `Enter` mean "run the search".
    fn fif_step_focus(&mut self, delta: i32) {
        let stops = crate::fif::FIF_STOPS;
        let here = self.fif_stop_index();
        let len = stops.len() as i32;
        let next = (((here as i32 + delta) % len) + len) % len;
        self.apply_fif_stop(stops[next as usize]);
    }

    /// Move the highlight along the toggles only, by `delta`.
    fn fif_step_toggle(&mut self, delta: i32) {
        let toggles = crate::fif::FifToggle::ALL;
        let len = toggles.len() as i32;
        let here = self
            .fif
            .focused_toggle
            .and_then(|t| toggles.iter().position(|x| *x == t))
            .unwrap_or(0) as i32;
        let next = (((here + delta) % len) + len) % len;
        self.fif.focused_toggle = Some(toggles[next as usize]);
        self.fif.editing = false;
    }

    /// Which stop the highlight is on now.
    fn fif_stop_index(&self) -> usize {
        if let Some(toggle) = self.fif.focused_toggle {
            return crate::fif::FIF_STOPS
                .iter()
                .position(|s| *s == crate::fif::FifStop::Toggle(toggle))
                .unwrap_or(0);
        }
        crate::fif::FIF_STOPS
            .iter()
            .position(|s| *s == crate::fif::FifStop::Field(self.fif.editing_field))
            .unwrap_or(0)
    }

    /// Put the highlight on one stop.
    fn apply_fif_stop(&mut self, stop: crate::fif::FifStop) {
        match stop {
            crate::fif::FifStop::Field(field) => {
                self.fif.editing_field = field;
                self.fif.focused_toggle = None;
                self.fif.editing = true;
            }
            crate::fif::FifStop::Toggle(toggle) => {
                self.fif.focused_toggle = Some(toggle);
                // Typing is off while a toggle is highlighted, so Space flips it and `Enter`
                // still runs the search rather than inserting a space into a term.
                self.fif.editing = false;
            }
        }
    }

    /// The text of the field currently being edited.
    fn fif_field_text_mut(&mut self) -> &mut String {
        match self.fif.editing_field {
            crate::fif::FifField::Query => &mut self.fif.query,
            crate::fif::FifField::Dir => &mut self.fif.dir,
            crate::fif::FifField::Glob => &mut self.fif.glob,
        }
    }

    fn handle_goto_key(&mut self, ev: crossterm::event::KeyEvent) -> Option<Action> {
        match ev.code {
            KeyCode::Char(c) if c.is_ascii_digit() || c == ':' => {
                self.goto_input.push(c);
                self.goto_error = None;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Backspace => {
                self.goto_input.pop();
                self.goto_error = None;
                self.dirty = true;
                Some(Action::Nop)
            }
            KeyCode::Enter => {
                let input = self.goto_input.clone();
                let outcome = crate::find::go_to(self, &input);
                match outcome {
                    crate::find::FindOutcome::Found { .. } => {
                        self.goto_input.clear();
                        self.goto_error = None;
                        self.pop_mode();
                    }
                    crate::find::FindOutcome::Invalid(msg) => {
                        // Inline, not a toast: the field is right there (FR-S6).
                        self.goto_error = Some(msg);
                    }
                    other => {
                        if let Some(msg) = other.toast() {
                            self.push_toast(msg, true);
                        }
                        self.pop_mode();
                    }
                }
                self.dirty = true;
                Some(Action::Nop)
            }
            _ => Some(Action::Nop),
        }
    }

    /// Enter: newline plus the previous line's leading whitespace (FR-E5).
    ///
    /// The indent is computed first and the document borrowed once, because `auto_indent`
    /// and `insert_spaces` both live in `config` and the document borrow is mutable.
    fn execute_enter(&mut self) {
        if self.is_readonly() {
            self.report_readonly();
            return;
        }
        let now = self.clock;
        let auto_indent = self.config.editor.auto_indent;
        let insert_spaces = self.config.editor.insert_spaces;
        let indent = if auto_indent {
            self.doc()
                .map(|d| {
                    let text = d.buffer.line_string(d.cursor.pos.line);
                    let trimmed = text.trim_start();
                    text.chars()
                        .take(text.len() - trimmed.len())
                        .map(|c| if c == '\t' && insert_spaces { ' ' } else { c })
                        .collect::<String>()
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let Some(doc) = self.doc_mut() else {
            return;
        };
        let _ = doc.insert_text(&format!("\n{indent}"), np_core::UndoGroup::Typing, now);
        self.dirty = true;
    }
}

/// The partner character that `c` should be closed with, if it opens a pair (FR-E11).
///
/// Quote characters pair with themselves, which is why this is a table rather than a
/// `match` on distinct open/close pairs. Deliberately excludes `<`: typing a less-than in
/// code or prose is common enough that auto-pairing it would fight the user constantly.
/// Whether `action` would change the buffer, for the read-only gate in
/// [`App::execute`] (FR-F9).
///
/// Deliberately a whitelist of writes rather than a blacklist of reads: an action added later
/// that forgets to appear here is merely *not* gated, which is the same behavior as before
/// this check existed, whereas an action wrongly listed here would make a working command
/// refuse to run. Navigation, selection, view toggles and dialogs are all absent by design --
/// a read-only file is still perfectly readable.
fn is_mutating_action(action: Action) -> bool {
    matches!(
        action,
        Action::Indent
            | Action::Outdent
            | Action::InsertDateTime
            | Action::DuplicateLine
            | Action::DeleteLine
            | Action::MoveLineUp
            | Action::MoveLineDown
            | Action::SortLinesAsc
            | Action::SortLinesDesc
            | Action::UpperCase
            | Action::LowerCase
            | Action::TitleCase
            | Action::ToggleComment
            | Action::ToggleBookmark
            | Action::ClearBookmarks
            | Action::Paste
            | Action::Cut
            | Action::Delete
            | Action::ReplaceAll
            | Action::SetEol(_)
    )
}

fn closing_for(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

/// Where a smart backspace should delete from, or `None` to fall through to a plain delete.
///
/// Applies only when every character between the previous tab stop and the caret is a space
/// or tab, and the caret is not at the line start. Deleting to the previous stop is what a
/// "dedent" keystroke means; anything else is an ordinary character delete.
fn smart_backspace_target(doc: &Document, tab_width: usize) -> Option<LineCol> {
    let pos = doc.cursor.pos;
    if pos.col == 0 {
        return None;
    }
    let line = doc.buffer.line_string(pos.line);
    let before: String = line.chars().take(pos.col).collect();
    if before.is_empty() || !before.chars().all(|c| c == ' ' || c == '\t') {
        return None;
    }
    // The stop to fall back to, in display cells.
    let cells = doc.buffer.display_col(pos.line, pos.col, tab_width);
    let stop = cells.saturating_sub(1) / tab_width.max(1) * tab_width.max(1);
    if stop >= cells {
        return None;
    }
    let from_col = doc.buffer.col_from_display(pos.line, stop, tab_width);
    if from_col >= pos.col {
        return None;
    }
    Some(LineCol::new(pos.line, from_col))
}

/// The position one character left of the caret, mirroring `Document::move_left`.
fn left_of(doc: &Document) -> LineCol {
    let pos = doc.cursor.pos;
    if pos.col > 0 {
        LineCol::new(pos.line, pos.col - 1)
    } else if pos.line > 0 {
        LineCol::new(pos.line - 1, doc.buffer.line_len_chars(pos.line - 1))
    } else {
        pos
    }
}

/// The position one character right of the caret, mirroring `Document::move_right`.
fn right_of(doc: &Document) -> LineCol {
    let pos = doc.cursor.pos;
    let len = doc.buffer.line_len_chars(pos.line);
    if pos.col < len {
        LineCol::new(pos.line, pos.col + 1)
    } else if pos.line + 1 < doc.buffer.line_count() {
        LineCol::new(pos.line + 1, 0)
    } else {
        pos
    }
}

/// The position one visual line away, honoring `goal_col` the way
/// `Document::move_vertical` does, so Shift+Arrow tracks plain Arrow.
fn vertical_of(doc: &Document, delta: i64) -> LineCol {
    let pos = doc.cursor.pos;
    let line_count = doc.buffer.line_count();
    let target_line = if delta < 0 {
        pos.line.saturating_sub((-delta) as usize)
    } else {
        (pos.line + delta as usize).min(line_count.saturating_sub(1))
    };
    if target_line == pos.line {
        return pos;
    }
    let goal = doc
        .cursor
        .goal_col
        .unwrap_or_else(|| doc.buffer.display_col(pos.line, pos.col, doc.tab_width()));
    let len = doc.buffer.line_len_chars(target_line);
    let col = doc
        .buffer
        .col_from_display(target_line, goal, doc.tab_width())
        .min(len);
    LineCol::new(target_line, col)
}

fn is_movement(a: Action) -> bool {
    matches!(
        a,
        Action::MoveLeft
            | Action::MoveRight
            | Action::MoveUp
            | Action::MoveDown
            | Action::MoveLeftSelect
            | Action::MoveRightSelect
            | Action::MoveUpSelect
            | Action::MoveDownSelect
            | Action::WordLeft
            | Action::WordRight
            | Action::WordLeftSelect
            | Action::WordRightSelect
            | Action::LineStart
            | Action::LineEnd
            | Action::LineStartSelect
            | Action::LineEndSelect
            | Action::DocStart
            | Action::DocEnd
            | Action::ScrollPageUp
            | Action::ScrollPageDown
    )
}

fn is_line_op(a: Action) -> bool {
    matches!(
        a,
        Action::DuplicateLine
            | Action::DeleteLine
            | Action::MoveLineUp
            | Action::MoveLineDown
            | Action::SortLinesAsc
            | Action::SortLinesDesc
            | Action::UpperCase
            | Action::LowerCase
            | Action::TitleCase
            | Action::ToggleComment
    )
}

/// The row index of the first row belonging to `doc.scroll.top_line`, over the whole document.
///
/// The single answer to "where is the viewport in row space", shared by the scrollbar thumb,
/// page scrolling and wheel scrolling. It is a free function rather than a method because the
/// callers hold `&mut Document` for the write and only need `&Document` plus the geometry for
/// this read, and because `np_core::Document` already has a private equivalent -- this is the
/// app-side one, keyed on the *app's* geometry, which is the one the screen is drawn with.
///
/// `position(|r| r.line >= top)` rather than `== top`: a top line that names a folded-away line
/// has no row of its own, and the row that stands in for it is the next visible one. Matching
/// exactly would return `None` and drop the thumb to the document's end.
pub fn row_offset_of_top(doc: &np_core::Document, geom: &np_core::ViewGeometry) -> usize {
    // `row_offset_of_line` counts rows without materializing them, and answers the same
    // question: the row index a line starts at. The previous implementation built the whole
    // document's `Vec<VisualRow>` and searched it, which `document_rows`' own doc comment
    // forbids per frame -- on a 20 MB file it cost ~670 ms per frame, twice per frame counting
    // both scrollbar paths, because building the vector re-walked every line.
    np_core::row_offset_of_line(&doc.buffer, geom, &doc.folds, doc.scroll.top_line)
}
