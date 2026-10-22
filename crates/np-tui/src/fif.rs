//! Find in Files panel state (spec §6.5 FR-FF1..FF3).
//!
//! # Why the state lives here and not in the widget
//!
//! Same reason `FindState` does: the results outlive the panel's visibility, a search runs on a
//! worker while the user does something else, and the whole thing has to be drivable from a
//! test with no terminal. The panel in [`crate::ui::find_in_files`] renders this struct and
//! writes back into it, and owns nothing.
//!
//! # Panel selection is not editor selection
//!
//! The highlighted result row is [`FifState::selected`], an index into [`FifState::hits`]. It is
//! deliberately not a `Document::selection`: the user is choosing a *place to go*, and the
//! editor they are leaving must keep whatever selection it had. Conflating them would mean a
//! search silently destroyed the text the user was about to copy.
//!
//! # Staleness
//!
//! [`FifState::active_request`] is the run whose results are on screen. A message whose
//! `request` differs is dropped at the drain, which is what stops a cancelled or superseded
//! search from repopulating a panel the user already cleared or replaced. The cancel flag alone
//! cannot do this: the flag says "stop", not "these results are not the current ones".

use std::path::PathBuf;

use np_core::{FifRequest, SearchOpts};

use crate::app::App;
use crate::bg::find_files::spawn_fif;
use crate::bg::{BgMessage, BgWorker};

/// How many results the panel keeps.
///
/// A search over a source tree can return hundreds of thousands of hits, and the panel holds
/// each one's path plus a line of text. The cap is what keeps a runaway query from exhausting
/// memory, and it is far past what anyone can scroll through: 20 000 rows at ~120 bytes is a
/// couple of megabytes. Truncation is shown rather than silent, so "these are the first N" is
/// never mistaken for "this is everything".
pub const MAX_PANEL_HITS: usize = 20_000;

/// The default drain budget: messages applied per event-loop iteration.
///
/// Justified by measurement in `docs/STATUS.md`. The number has to be large enough that a
/// normal search's results appear within a frame or two, and small enough that appending them
/// cannot outlast a poll interval and queue keyboard input behind it.
pub const DRAIN_BUDGET: usize = 64;

/// Which panel field the keyboard edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FifField {
    #[default]
    Query,
    Dir,
    Glob,
}

/// Which panel toggle a click or a key refers to.
///
/// An address rather than three booleans named at each call site. The painter, the hit-tester and
/// the keyboard all resolve a toggle through [`FifToggle::get`] and [`FifToggle::set`], which is
/// what stops a click on `word` flipping `match case` -- the failure mode of three separate
/// `if label == ...` chains that each have to agree on an order nobody wrote down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FifToggle {
    Case,
    Word,
    Gitignore,
}

impl FifToggle {
    /// Every toggle, in the order the panel paints them.
    pub const ALL: [FifToggle; 3] = [FifToggle::Case, FifToggle::Word, FifToggle::Gitignore];

    /// The toggle's current state.
    pub fn get(self, app: &App) -> bool {
        match self {
            FifToggle::Case => app.fif.opts.case_sensitive,
            FifToggle::Word => app.fif.opts.whole_word,
            FifToggle::Gitignore => app.fif.use_gitignore,
        }
    }

    /// The toggle's current state, with `on`.
    pub fn set(self, app: &mut App, on: bool) {
        match self {
            FifToggle::Case => app.fif.opts.case_sensitive = on,
            FifToggle::Word => app.fif.opts.whole_word = on,
            FifToggle::Gitignore => app.fif.use_gitignore = on,
        }
    }

    /// Flip the toggle.
    pub fn toggle(self, app: &mut App) {
        let on = self.get(app);
        self.set(app, !on);
        app.dirty = true;
    }
}

/// One keyboard stop in the panel: a field or a toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FifStop {
    Field(FifField),
    Toggle(FifToggle),
}

/// The panel's keyboard stops, in the order `Tab` walks them.
///
/// One list rather than a switch in each of the painter, the keyboard and the mouse. The S20
/// report was that the toggles could not be reached at all; a shared ordered list is what makes
/// "the next Tab stop" one question with one answer.
pub const FIF_STOPS: [FifStop; 6] = [
    FifStop::Field(FifField::Query),
    FifStop::Field(FifField::Dir),
    FifStop::Field(FifField::Glob),
    FifStop::Toggle(FifToggle::Case),
    FifStop::Toggle(FifToggle::Word),
    FifStop::Toggle(FifToggle::Gitignore),
];

/// One result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelHit {
    pub path: PathBuf,
    /// 1-based, as displayed (FR-FF3).
    pub line: usize,
    /// 1-based, as displayed.
    pub col: usize,
    /// The line's text, for display and for highlighting the match inside it.
    pub text: String,
    /// Match length in characters.
    pub match_len: usize,
}

impl PanelHit {
    /// `path:line:col:` as the panel prints it.
    pub fn location(&self) -> String {
        format!("{}:{}:{}:", self.path.display(), self.line, self.col)
    }
}

/// Whether the panel is idle, working, or finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FifPhase {
    /// No search has run, or the results are final.
    #[default]
    Idle,
    /// A worker is running.
    Running,
    /// The last run was cancelled (FR-FF2's "cancelled" note).
    Cancelled,
}

/// Everything the panel shows and does.
#[derive(Debug, Clone)]
pub struct FifState {
    /// The query field.
    pub query: String,
    /// The directory field. Empty means [`default_dir`].
    pub dir: String,
    /// The glob filter field, e.g. `*.rs`.
    pub glob: String,
    /// Search options shared with in-document search, so results agree.
    pub opts: SearchOpts,
    /// "Use gitignore" (FR-FF1).
    pub use_gitignore: bool,
    /// "Include hidden files".
    pub include_hidden: bool,
    /// Results, in arrival order.
    pub hits: Vec<PanelHit>,
    /// Highlighted row. Independent of editor selection, by design.
    pub selected: usize,
    /// First row drawn. The panel scrolls; the selection follows.
    pub scroll: usize,
    /// Files scanned so far (FR-FF2's live header).
    pub scanned: usize,
    /// Hits so far.
    pub matches: usize,
    /// Skip and error notes, newest last, capped so a noisy tree cannot grow them forever.
    pub notes: Vec<String>,
    /// Notes beyond the cap. Counted rather than discarded silently, so the panel can say
    /// "and N more" instead of implying the list was complete.
    pub notes_dropped: usize,
    pub phase: FifPhase,
    /// The run whose results are on screen. Anything else is stale.
    pub active_request: Option<usize>,
    /// The panel is open and owns keyboard focus.
    pub focused: bool,
    /// Which field typing edits. Cycled by `Tab`.
    pub editing_field: FifField,
    /// Whether the keyboard is editing a field rather than moving through results.
    ///
    /// Two-mode by design: a panel whose arrows always move the caret in a text field cannot
    /// scroll its results, and one whose letters always move the selection cannot be typed in.
    /// Enter runs the search from a field and opens a hit from the results view, which is the
    /// handoff between the two.
    pub editing: bool,
    /// The toggle the keyboard is currently sitting on, if any.
    ///
    /// `Tab` walks the fields and then the three toggles, so the toggles are reachable without
    /// remembering `Ctrl+C` / `Ctrl+W` / `Ctrl+G` -- S20 reported them as unreachable, which they
    /// were to anyone who did not already know the chords. `None` means a text field has the
    /// keyboard, which is the common case and the reason this is an `Option` rather than a
    /// non-empty position.
    pub focused_toggle: Option<FifToggle>,
    /// The last inline error (invalid pattern, bad glob).
    pub error: Option<String>,
    /// Results were dropped at [`MAX_PANEL_HITS`].
    pub truncated: bool,
}

impl Default for FifState {
    fn default() -> Self {
        FifState {
            query: String::new(),
            dir: String::new(),
            glob: String::new(),
            opts: SearchOpts::default(),
            use_gitignore: true,
            include_hidden: false,
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            scanned: 0,
            matches: 0,
            notes: Vec::new(),
            notes_dropped: 0,
            phase: FifPhase::Idle,
            active_request: None,
            focused: false,
            editing_field: FifField::default(),
            editing: true,
            focused_toggle: None,
            error: None,
            truncated: false,
        }
    }
}

/// Notes are capped: a tree with ten thousand unreadable files should not grow `App` without
/// bound. The count of what was dropped is kept so the panel can say so.
const MAX_NOTES: usize = 50;

impl FifState {
    /// Seed the query from the in-document search, which is what Notepad++ does and what makes
    /// "search here, then search everywhere" one keystroke rather than retyping.
    pub fn seed_from_find(&mut self, find: &crate::find::FindState) {
        if self.query.is_empty() {
            self.query = find.needle.clone();
        }
        self.opts.case_sensitive = find.opts.case_sensitive;
        self.opts.whole_word = find.opts.whole_word;
        self.opts.regex = find.opts.regex;
    }
}

/// The directory a search defaults to: the active document's parent, else the process cwd.
///
/// FR-FF1 says "parent dir of the active document, else cwd". An untitled buffer has no parent,
/// and neither does a document whose file was deleted, so both fall through to the cwd.
pub fn default_dir(app: &App) -> PathBuf {
    app.doc()
        .and_then(|d| d.path.as_ref())
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The directory a search will actually walk, resolving the field.
pub fn effective_dir(app: &App) -> PathBuf {
    let typed = app.fif.dir.trim();
    if typed.is_empty() {
        return default_dir(app);
    }
    let path = PathBuf::from(typed);
    if path.is_absolute() {
        path
    } else {
        // Relative to the default, so a user who types `src` from a project file's directory
        // gets `<project>/src` rather than something depending on where the process started.
        default_dir(app).join(path)
    }
}

/// Start a search (FR-FF1). Returns the `request_id` for it.
///
/// Clears the previous results before spawning, so the panel never shows old hits under a new
/// query's header. The worker's messages carry the new `request_id`, and anything from the old
/// run is dropped at the drain.
///
/// Returns `None` when the search was refused (empty query, bad pattern, missing directory),
/// with the reason left in [`FifState::error`] for the panel to show.
pub fn start(app: &mut App) -> Option<usize> {
    let query = app.fif.query.trim().to_string();
    if query.is_empty() {
        app.fif.error = Some("enter a search term".to_string());
        return None;
    }
    let opts = SearchOpts {
        // Direction and wrap are meaningless for a file walk; the fields exist because the same
        // struct drives in-document search.
        forward: true,
        wrap_around: false,
        ..app.fif.opts.clone()
    };
    // Validate before spawning: a broken regex should be reported now, not after a directory
    // traversal on another thread.
    if let Some(msg) = np_core::query_error_message(&query, &opts) {
        app.fif.error = Some(msg);
        return None;
    }
    app.fif.error = None;

    let dir = effective_dir(app);
    if !dir.exists() {
        app.fif.error = Some(format!("no such directory: {}", dir.display()));
        return None;
    }

    let glob = {
        let g = app.fif.glob.trim().to_string();
        if g.is_empty() {
            None
        } else {
            Some(g)
        }
    };
    let use_gitignore = app.fif.use_gitignore;
    let include_hidden = app.fif.include_hidden;

    let req = FifRequest {
        query,
        dir,
        glob,
        opts,
        use_gitignore,
        include_hidden,
        // FR-F10: a 500 MB binary in the tree must not be slurped into a search.
        respect_size_cap: true,
    };

    // Fresh run: clear everything the last one produced.
    app.fif.hits.clear();
    app.fif.notes.clear();
    app.fif.notes_dropped = 0;
    app.fif.selected = 0;
    app.fif.scroll = 0;
    app.fif.scanned = 0;
    app.fif.matches = 0;
    app.fif.truncated = false;
    app.fif.phase = FifPhase::Running;

    let worker = app.bg.clone();
    let id = worker.next_request_id();
    app.fif.active_request = Some(id);
    let cancel = worker.begin_request();

    // Whether the search runs on a worker or on the calling thread is a property of the app,
    // not of the caller's convenience. `App::new_headless` sets `sync_search` so a test gets a
    // deterministic, joinable search with no thread; production leaves it false. A test that
    // wanted to drive `start` against a real thread would otherwise get *both* a worker's
    // results and its own, which is how duplicate hits first appeared.
    if app.sync_search {
        let sink = worker.clone();
        crate::bg::find_files::run_fif(id, &req, &cancel, &mut |m| {
            sink.send(m);
        });
    } else {
        let sink = worker.clone();
        let handle = spawn_fif(id, req, cancel, move |m| {
            sink.send(m);
        });
        // The handle is dropped: nobody joins a background search. See `bg::find_files`.
        drop(handle);
    }

    // A search implies a visible, focused panel. Doing this only in `open` meant a search
    // started any other way -- Enter in the panel is fine because it is already focused, but a
    // scripted or future palette-triggered start -- left results accumulating in a panel with no
    // keyboard, so arrows did nothing and the user had to reopen to use them.
    if app.mode() != crate::app::Mode::FindPanel {
        app.push_mode(crate::app::Mode::FindPanel);
    }
    app.fif.focused = true;
    // Running a search moves the keyboard to the results: Enter already ran the query, so the
    // next Enter should open a hit rather than re-run it.
    app.fif.editing = false;
    app.find_panel_height = crate::ui::find_in_files::PANEL_HEIGHT.min(editor_room(app));
    app.dirty = true;
    Some(id)
}

/// Cancel a running search (FR-FF2).
pub fn cancel(app: &mut App) {
    if app.fif.phase != FifPhase::Running {
        return;
    }
    app.bg.cancel();
    app.fif.phase = FifPhase::Cancelled;
    // Stale from here on: the run's remaining messages must not append to this list.
    app.fif.active_request = None;
    push_note(app, "search cancelled");
    app.dirty = true;
}

/// Close the panel. A running search is cancelled first, so closing never leaves a worker
/// appending to an invisible panel.
pub fn close(app: &mut App) {
    if app.fif.phase == FifPhase::Running {
        cancel(app);
    }
    app.fif.focused = false;
    app.find_panel_height = 0;
    while app.mode() == crate::app::Mode::FindPanel {
        app.pop_mode();
    }
    app.dirty = true;
}

/// Open or focus the panel (Ctrl+Shift+F).
pub fn open(app: &mut App) {
    app.fif.focused = true;
    // Only take the keyboard once, or a second Ctrl+Shift+F would stack a second FindPanel mode
    // and need two Esc to escape.
    if app.mode() != crate::app::Mode::FindPanel {
        app.push_mode(crate::app::Mode::FindPanel);
    }
    app.fif.seed_from_find(&app.find);
    if app.fif.dir.trim().is_empty() {
        app.fif.dir = default_dir(app).to_string_lossy().into_owned();
    }
    app.find_panel_height = crate::ui::find_in_files::PANEL_HEIGHT.min(editor_room(app));
    app.dirty = true;
}

/// Room the panel can take without eating the whole editor. A tiny terminal gets a one-row
/// panel rather than a zero-height editor.
fn editor_room(app: &App) -> u16 {
    // Measured against the band, not `regions.editor`: `editor` is already reduced by the panel's
    // height, so subtracting the floor from it would apply the reduction twice and shrink the
    // panel a little more on every resize.
    let band_h = app.regions.band.h;
    // Leave at least three editor rows: a two-row editor is unusable, and a panel that squeezes
    // it to nothing is worse than a panel that is itself too short.
    band_h.saturating_sub(crate::app::FIF_MIN_EDITOR_ROWS)
}

/// Record a note, keeping the list bounded and counting what was dropped.
pub fn push_note(app: &mut App, note: impl Into<String>) {
    let note = note.into();
    if app.fif.notes.len() >= MAX_NOTES {
        app.fif.notes_dropped += 1;
        return;
    }
    app.fif.notes.push(note);
}

/// Apply one background message (contract §16). Returns true when the message was applied.
///
/// The staleness test is the whole point: a message from any run other than
/// [`FifState::active_request`] is dropped, which is what stops a search the user cancelled or
/// replaced from writing into the panel.
fn apply(app: &mut App, msg: BgMessage) -> bool {
    // Progress carries no request id and is never stale in a harmful way: it only moves a
    // counter, and a late tick on a finished panel reads as "still working" for one frame.
    if let BgMessage::FifProgress { scanned, matches } = msg {
        if app.fif.phase != FifPhase::Running {
            return false;
        }
        app.fif.scanned = app.fif.scanned.max(scanned);
        app.fif.matches = app.fif.matches.max(matches);
        app.dirty = true;
        return true;
    }
    let stale = match msg.request() {
        Some(id) => Some(id) != app.fif.active_request,
        None => false,
    };
    if stale {
        return false;
    }
    match msg {
        BgMessage::FifMatch {
            path,
            line,
            col,
            text,
            match_len,
            ..
        } => {
            if app.fif.hits.len() >= MAX_PANEL_HITS {
                app.fif.truncated = true;
                return false;
            }
            app.fif.hits.push(PanelHit {
                path,
                line,
                col,
                text,
                match_len,
            });
            app.dirty = true;
        }
        BgMessage::FifSkipped { path, reason, .. } => {
            push_note(app, format!("{}: {reason}", path.display()));
            app.dirty = true;
        }
        BgMessage::FifError { path, message, .. } => {
            let note = if path.as_os_str().is_empty() {
                message
            } else {
                format!("{}: {message}", path.display())
            };
            push_note(app, note);
            app.dirty = true;
        }
        BgMessage::FifDone { total, .. } => {
            app.fif.matches = total;
            app.fif.phase = FifPhase::Idle;
            app.fif.active_request = None;
            let note = format!(
                "{} files scanned, {} matches",
                app.fif.scanned, app.fif.matches
            );
            push_note(app, note);
            app.dirty = true;
        }
        BgMessage::FifCancelled { .. } => {
            app.fif.phase = FifPhase::Cancelled;
            app.fif.active_request = None;
            app.dirty = true;
        }
        BgMessage::FileLoaded { .. } | BgMessage::FifProgress { .. } => {
            // Loads are handled by `apply_loaded`, which needs the document path.
            return false;
        }
    }
    true
}

/// Apply one `FileLoaded` message (FR-F10). Split out because building a `Document` from a
/// `LoadedFile` is document construction, which happens here on the UI thread.
fn apply_loaded(
    app: &mut App,
    request: usize,
    result: Result<np_core::LoadedFile, String>,
) -> bool {
    if Some(request) != app.pending_load_request {
        return false;
    }
    app.pending_load_request = None;
    match result {
        Ok(loaded) => {
            let mut doc = np_core::Document::from_loaded(&loaded);
            doc.overtype = app.overtype;
            doc.tab_width = app.config.editor.tab_width.max(1);
            // Replace the placeholder `open_path` pushed, rather than opening a second tab.
            // `open_document` activates by path, and the placeholder already owns this path,
            // so calling it here would leave two tabs pointing at one file -- the exact state
            // `open_path` goes out of its way to avoid for a normal open.
            let target = app
                .documents
                .iter()
                .position(|d| d.path.as_deref() == Some(loaded.path.as_path()));
            match target {
                Some(i) => {
                    app.documents[i] = doc;
                    app.active = i;
                    app.load_active_view();
                }
                // No placeholder (a load started by something other than `open_path`, or the
                // user closed the tab while the read was in flight): open it normally. A
                // vanished placeholder should not lose the file that just finished reading.
                None => app.open_document(doc),
            }
            push_note(app, format!("loaded {}", loaded.path.display()));
        }
        Err(message) => {
            push_note(app, format!("load failed: {message}"));
            app.push_toast(message, true);
        }
    }
    app.dirty = true;
    true
}

/// Drain at most `budget` background messages (contract §16).
///
/// Called once per event-loop iteration. The budget is the responsiveness guarantee: a search
/// that produces half a million hits delivers them over many iterations rather than in one
/// unbounded loop that holds the UI thread and queues keyboard input behind it.
///
/// Returns how many messages were applied, which is what the budget assertion counts.
pub fn drain(app: &mut App, budget: usize) -> usize {
    // Cloned out of `app` rather than borrowed: the loop mutates `app` while reading the queue,
    // and `BgWorker` is a cheap handle to shared state, so the clone is the borrow checker's
    // answer and costs three `Arc` bumps.
    let worker: BgWorker = app.bg.clone();
    let mut applied = 0usize;
    for _ in 0..budget {
        let Some(msg) = worker.try_recv() else {
            break;
        };
        match msg {
            BgMessage::FileLoaded { request, result } => {
                if apply_loaded(app, request, result) {
                    applied += 1;
                }
            }
            other => {
                if apply(app, other) {
                    applied += 1;
                }
            }
        }
    }
    applied
}

/// Move the panel selection, keeping it in range and the scroll window on it.
pub fn select(app: &mut App, index: usize) {
    if app.fif.hits.is_empty() {
        app.fif.selected = 0;
        app.fif.scroll = 0;
        return;
    }
    app.fif.selected = index.min(app.fif.hits.len() - 1);
    scroll_to_selection(app);
    app.dirty = true;
}

/// Move the selection by a delta, clamped at both ends.
pub fn move_selection(app: &mut App, delta: isize) {
    let len = app.fif.hits.len();
    if len == 0 {
        return;
    }
    let cur = app.fif.selected as isize;
    let next = (cur + delta).clamp(0, len as isize - 1) as usize;
    select(app, next);
}

/// Scroll the result window by `delta` rows, clamped to the hits.
///
/// Deliberately independent of `move_selection`: the wheel scrolls the *view*, not the
/// highlighted row. Tying the two together would mean the selection jumped to whatever row the
/// wheel landed on, so a user reading results two screens down could not scroll to look without
/// changing which hit `Enter` opens.
///
/// The selection is left where it is even when it scrolls off screen, and the next
/// `scroll_to_selection` (from a selection move) brings it back. That is what a wheel means: the
/// highlighted row is still selected, it is just not in view at the moment.
pub fn scroll_window(app: &mut App, delta: i32) {
    let visible = crate::ui::find_in_files::result_rows(app.find_panel_height).max(1);
    let max_top = app.fif.hits.len().saturating_sub(visible);
    let next = (app.fif.scroll as i64 + i64::from(delta)).clamp(0, max_top as i64) as usize;
    if next != app.fif.scroll {
        app.fif.scroll = next;
        app.dirty = true;
    }
}

/// Scroll so the selected row is visible.
fn scroll_to_selection(app: &mut App) {
    let visible = panel_rows(app).max(1);
    let sel = app.fif.selected;
    let top = app.fif.scroll;
    if sel < top {
        app.fif.scroll = sel;
    } else if sel >= top + visible {
        app.fif.scroll = sel + 1 - visible;
    }
}

/// How many result rows the panel can draw.
fn panel_rows(app: &App) -> usize {
    crate::ui::find_in_files::result_rows(app.find_panel_height)
}

/// Open the selected hit in a tab and put the caret on it (FR-FF3).
///
/// Reuses the same load path as any other open, then sets cursor and scroll. The hit's 1-based
/// coordinates become 0-based here and nowhere else, and the position is clamped into the
/// document that was actually loaded -- a file changed on disk between the search and the click
/// is a real race, and landing on the last line of a file that shrank beats a bogus position.
pub fn open_selected(app: &mut App) {
    let Some(hit) = app.fif.hits.get(app.fif.selected).cloned() else {
        app.push_toast("no result selected", false);
        return;
    };
    open_hit(app, &hit);
}

/// Open one hit. Public so a click can use it with a row index other than the selection.
pub fn open_hit(app: &mut App, hit: &PanelHit) {
    match np_core::load(&hit.path, None) {
        Ok(loaded) => {
            // Reuse the tab if the file is already open, which is what a jump should do: the
            // user asked to see a location, not for a second copy of a buffer with unsaved edits
            // in it. Opening a second one would let the two diverge and one of them is a lie.
            let existing = app
                .documents
                .iter()
                .position(|d| d.path.as_deref() == Some(hit.path.as_path()));
            if let Some(index) = existing {
                app.active = index;
                app.document_list_selected = index;
            } else {
                let mut doc = np_core::Document::from_loaded(&loaded);
                doc.overtype = app.overtype;
                app.open_document(doc);
            }
            let (line, col) = np_core::zero_based(hit.line, hit.col);
            let geom = app.geometry;
            if let Some(doc) = app.doc_mut() {
                let target = doc.buffer.clamp(np_core::LineCol::new(line, col));
                doc.set_cursor(target, false);
                // Center-ish, in visual rows. `scroll_to_show` is the fold- and wrap-aware
                // version: it centers on the target's *row*, which is not its line index once a
                // line wraps, and it unfolds a collapsed range rather than jumping to a
                // location the screen cannot show (S13, FR-V5).
                doc.scroll_to_show(&geom, target);
            }
            app.fif.focused = false;
            app.dirty = true;
        }
        Err(err) => {
            app.push_toast(err.user_message(), true);
        }
    }
}

/// The header line: "N files scanned, M matches" (FR-FF2), with the phase appended.
///
/// The truncation note is shown in every phase, not just Idle. A list that stopped growing
/// because it hit a cap is misleading *while* it is still being filled -- the count looks live
/// and complete, and the user has no reason to suspect otherwise. Saying so as soon as it is
/// true is the whole point of the note.
pub fn header(app: &App) -> String {
    let base = format!(
        "{} files scanned, {} matches",
        app.fif.scanned, app.fif.matches
    );
    let truncated = if app.fif.truncated {
        format!("  [truncated at {MAX_PANEL_HITS}]")
    } else {
        String::new()
    };
    match app.fif.phase {
        FifPhase::Running => format!("{base}  [searching...] Esc cancels{truncated}"),
        FifPhase::Cancelled => format!("{base}  [cancelled]{truncated}"),
        FifPhase::Idle => format!("{base}{truncated}"),
    }
}
