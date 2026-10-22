//! Session and recent-file plumbing on top of `np_core::session` (FR-CG3, FR-F8).
//!
//! # Why this lives in `np-tui`
//!
//! `np_core::session` owns the *format*: what a `SessionTab` is, how it serializes, when a
//! corrupt file becomes `None`. This module owns the *policy*: when a save happens, what an
//! App looks like restored, and which files we refuse to persist. Splitting them keeps the
//! engine free of the notion of an application session, and keeps the debouncing and
//! autosave decisions next to the event loop that drives them.
//!
//! # Writes are debounced, not per-keystroke
//!
//! A session save rewrites every open buffer's untitled content and re-serializes the lot.
//! Doing that per keystroke would be a file write per character. Instead a change sets a
//! dirty flag and the save happens once per event-loop iteration at most, and only when the
//! flag is set -- a 1000-keystroke burst produces one write. `App::mark_session_dirty` is the
//! only way to raise the flag, so no mutation path has to remember to schedule it beyond
//! calling that.
//!
//! # Autosave saves documents; session save saves state
//!
//! These are different operations with different failure modes, and conflating them is how
//! an editor ends up writing a file the user never asked to save. Autosave writes modified
//! *documents* on a timer, skipping untitled and read-only ones -- an untitled buffer has no
//! path, and inventing one would create files the user did not ask for. Session save writes
//! `session.toml`, which is never user content except for retained untitled text.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use np_core::{Document, LanguageId, Session, SessionTab};

use crate::app::App;

impl App {
    /// Capture the current state as a session (FR-CG3).
    pub fn capture_session(&self) -> (Session, Vec<String>) {
        Session::from_tabs(self.documents.iter(), self.active, self.recent.clone())
    }

    /// Write the session now, if a save is owed (FR-CG3).
    ///
    /// Returns whether a write happened, which is what the debounce test counts. Errors are
    /// logged rather than surfaced: a session that cannot be written must not interrupt
    /// editing, and the next attempt has the same information.
    pub fn flush_session(&mut self) -> bool {
        if !self.session_dirty {
            return false;
        }
        self.session_dirty = false;
        let Some(path) = self.session_path.clone() else {
            // No data directory: session support is simply off.
            return false;
        };
        let (session, warnings) = self.capture_session();
        for warning in warnings {
            log::warn!("session: {warning}");
        }
        match session.save(&path) {
            Ok(()) => {
                self.session_writes += 1;
                true
            }
            Err(err) => {
                log::warn!("cannot save session at {}: {err}", path.display());
                false
            }
        }
    }

    /// Restore tabs, active tab, bookmarks, folds, language and recents from a session
    /// (FR-CG3).
    ///
    /// A file that no longer exists becomes an empty read-only document flagged missing, and
    /// the restore continues. Refusing the whole session because one file was deleted would
    /// punish the user for cleaning up their disk, and they would have no way to know which
    /// file caused it.
    ///
    /// Returns the notes a user should see: missing files, and buffers whose retained content
    /// was truncated.
    pub fn restore_session(&mut self, session: &Session) -> Vec<String> {
        let mut notes = Vec::new();
        let mut restored = Vec::new();
        for tab in &session.tab {
            match self.restore_tab(tab) {
                Ok(doc) => restored.push(doc),
                Err(note) => {
                    notes.push(note);
                    if let Some(doc) = self.restore_tab_lossy(tab) {
                        restored.push(doc);
                    }
                }
            }
        }
        if restored.is_empty() {
            // The zero-document rule from S8 still applies to a session with no tabs. Go
            // through `documents` rather than `push_untitled` alone: the App already holds the
            // startup untitled tab, and appending to it would leave two.
            self.documents.clear();
            self.push_untitled();
            return notes;
        }
        self.documents = restored;
        self.active = session.active_tab.min(self.documents.len() - 1);
        self.recent = session.recent.clone();
        self.recent.truncate(self.config.ui.max_recent_files);
        self.document_list_selected = self.active;
        // Fold detection for every restored tab, with the session's collapsed ranges preserved,
        // and only then `load_active_view`. The order matters: the plain re-detect that a tab
        // switch triggers treats detection as the authority and drops a closed range it cannot
        // reproduce, so running it after the restore would undo the user's folds on the very
        // first frame. Restoring the session is the one moment the recorded answer wins.
        self.refresh_restored_folds();
        self.load_active_view();
        notes
    }

    /// Detect folds on every restored tab, keeping the ranges the session recorded.
    ///
    /// Every tab needs it, not just the active one, or an inactive tab shows no fold markers in
    /// its gutter until the user clicks through it and a tab switch happens to re-detect.
    ///
    /// A tab whose file could not be read is opened as an empty placeholder by
    /// `restore_tab_lossy`, and detection on an empty buffer proposes nothing -- which is right,
    /// because `clamp_folds` also drops recorded ranges whose lines no longer exist.
    fn refresh_restored_folds(&mut self) {
        let active = self.active;
        for index in 0..self.documents.len() {
            self.active = index;
            crate::view::refresh_folds_preserving(self);
        }
        self.active = active;
    }

    /// One session entry as a document, or a note about why it could not be read.
    fn restore_tab(&self, tab: &SessionTab) -> Result<Document, String> {
        let Some(path) = tab.path.as_ref() else {
            // Untitled: the retained content is the whole story.
            let mut doc =
                Document::new_untitled(self.config.default_encoding(), self.config.default_eol());
            if let Some(content) = &tab.content {
                doc.buffer = np_core::Buffer::from_str_lossy(content);
                // Retained text is not on disk, so it must stay marked unsaved. Clearing the
                // flag would let the user quit without ever being asked about it.
                doc.modified = true;
            }
            apply_view_state(&mut doc, tab);
            return Ok(doc);
        };

        match np_core::load(path, None) {
            Ok(loaded) => {
                let mut doc = Document::from_loaded(&loaded);
                // A session that recorded a read-only tab stays read-only even if the file's
                // write bit has since come back: the user set that flag deliberately.
                apply_view_state(&mut doc, tab);
                // After the view state, which sets `readonly` from the session: a session
                // that recorded a read-only tab stays read-only even if the file's write bit
                // has since come back, and a file we now cannot write is still read-only.
                doc.readonly |= tab.readonly;
                Ok(doc)
            }
            Err(err) => Err(format!(
                "{} is no longer readable ({}); opened as an empty read-only buffer",
                path.display(),
                err.user_message()
            )),
        }
    }

    /// Fallback for a tab whose file is gone: an empty read-only placeholder under its name.
    ///
    /// The retained `content` wins over an empty buffer when there is any, which is the case
    /// for a modified untitled buffer whose path also vanished.
    fn restore_tab_lossy(&self, tab: &SessionTab) -> Option<Document> {
        let mut doc =
            Document::new_untitled(self.config.default_encoding(), self.config.default_eol());
        match &tab.content {
            Some(content) => {
                doc.buffer = np_core::Buffer::from_str_lossy(content);
                doc.modified = true;
            }
            None => {
                doc.buffer = np_core::Buffer::from_str_lossy("");
            }
        }
        doc.path = tab.path.clone();
        apply_view_state(&mut doc, tab);
        // Forced *after* `apply_view_state`, which copies `tab.readonly`. A file we cannot
        // read must never come back writable, whatever the session recorded: the session
        // describes intent, and an unreadable file overrides it.
        doc.readonly = true;
        Some(doc)
    }

    /// Record a file as recently used (FR-F8).
    ///
    /// Called on both open and save: a file the user saved is one they will want back, and a
    /// save is often the first time a path exists at all.
    pub fn note_recent(&mut self, path: &Path) {
        let max = self.config.ui.max_recent_files;
        if max == 0 {
            return;
        }
        // Delegated to `Session::push_recent`, which owns the dedup, move-to-front and
        // truncate rules and is tested against them in np-core. Reimplementing the same three
        // lines here is how the two lists drift apart, and a menu that disagrees with the
        // session file is worse than either.
        let mut holder = Session {
            recent: std::mem::take(&mut self.recent),
            ..Default::default()
        };
        holder.push_recent(path.to_path_buf(), max);
        self.recent = holder.recent;
        self.mark_session_dirty();
    }

    /// The recent-files list, most recent first (FR-F8). S14's menu reads this.
    pub fn recent_files(&self) -> &[PathBuf] {
        &self.recent
    }

    /// Queue a session save for the end of this event-loop iteration.
    pub fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    /// Run anything the timers owe: autosave and the debounced session save (FR-CG3).
    ///
    /// Called once per event-loop iteration, not per event, which is what bounds a typing
    /// burst to a single session write.
    pub fn run_timers(&mut self, now: Instant) {
        self.autosave_tick(now);
        // Autosave marks documents saved, which is itself a session-relevant change, so the
        // session flush runs after it rather than before.
        self.flush_session();
    }

    /// Save modified documents if the autosave interval has elapsed (FR-CG3).
    fn autosave_tick(&mut self, now: Instant) {
        let seconds = self.config.editor.autosave_seconds;
        if seconds == 0 {
            return;
        }
        let interval = Duration::from_secs(seconds);
        if let Some(last) = self.last_autosave {
            if now.duration_since(last) < interval {
                return;
            }
        } else {
            // First call establishes the baseline. Autosaving immediately at startup would
            // rewrite files the instant they were opened, for no benefit.
            self.last_autosave = Some(now);
            return;
        }
        self.last_autosave = Some(now);
        self.autosave_now();
    }

    /// Save every document that can be saved silently.
    ///
    /// Untitled documents are skipped: there is no path, and picking one would create a file
    /// the user never named. Read-only documents are skipped for the obvious reason. Neither
    /// is reported as an error, because a silent background operation that toasts about the
    /// state of the world every thirty seconds is worse than one that says nothing.
    pub fn autosave_now(&mut self) -> usize {
        let opts = self.save_options_public();
        let mode = self.eol_mode_public();
        let mut saved = 0usize;
        for i in 0..self.documents.len() {
            let eligible = self
                .documents
                .get(i)
                .map(|d| d.modified && d.path.is_some() && !d.readonly)
                .unwrap_or(false);
            if !eligible {
                continue;
            }
            if let Some(doc) = self.documents.get_mut(i) {
                if let Some(path) = doc.path.clone() {
                    if doc.save_to(&path, mode, &opts).is_ok() {
                        saved += 1;
                    }
                }
            }
        }
        if saved > 0 {
            self.dirty = true;
            self.session_dirty = true;
        }
        saved
    }
}

/// Push the per-tab view state a session remembers, onto a document whose content is already
/// correct.
///
/// Separate from construction because the content and the view arrive from different places:
/// the text comes from the file or the retained content, while the cursor and scroll come from
/// the session. Applying them in one place keeps the clamp rules in one place too -- the
/// cursor is clamped into the restored buffer, so a session recorded in a 500-line file
/// restored over a 3-line file lands on line 3 rather than pointing at nothing.
pub fn apply_view_state(doc: &mut Document, tab: &SessionTab) {
    doc.set_cursor(
        np_core::LineCol {
            line: tab.cursor_line,
            col: tab.cursor_col,
        },
        false,
    );
    // Clamped to the restored buffer for the same reason as the cursor: a session recorded
    // against a longer file must not leave the viewport scrolled past the end, which would
    // show a blank editor over a document that has content.
    let last_line = doc.buffer.line_count().saturating_sub(1);
    doc.scroll.top_line = tab.scroll_top.min(last_line);
    doc.bookmarks = tab.bookmarks.iter().copied().collect();
    doc.folds = tab
        .folded
        .iter()
        .map(|[start, end]| np_core::FoldRange {
            start: *start,
            end: *end,
            collapsed: true,
        })
        .collect();
    doc.language = LanguageId::from_session_str(&tab.language);
    doc.readonly = tab.readonly;
}
