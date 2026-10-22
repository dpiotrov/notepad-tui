//! Stage S9 acceptance on the application side: session restore, debounced writes, autosave,
//! recent files, and the CLI/startup policy (FR-CG1..CG4, FR-F8).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use np_core::{Config, Document, EncodingId, Eol, LineCol, Session, SessionTab};
use np_tui::App;

fn config() -> Config {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config
}

fn app() -> App {
    App::new_headless(config(), (80, 24))
}

fn app_in(dir: &Path) -> App {
    let mut app = app();
    app.session_path = Some(dir.join("session.toml"));
    app
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

fn open(app: &mut App, path: &Path) {
    let loaded = np_core::load(path, None).expect("load");
    let mut doc = Document::from_loaded(&loaded);
    doc.tab_width = app.config.editor.tab_width.max(1);
    app.open_document(doc);
    let p = path.to_path_buf();
    app.note_recent(&p);
}

fn write_file(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, body).expect("write");
    path
}

/// Append text to a document whose caret is at the start.
///
/// `from_loaded` leaves the caret at 0,0, so a bare `insert_text` prepends. Tests that mean
/// "edit the end" have to move the caret there first.
fn append(app: &mut App, added: &str) {
    let now = app.clock;
    let doc = app.doc_mut().expect("document");
    let last = doc.buffer.line_count().saturating_sub(1);
    let col = doc.buffer.line_string(last).chars().count();
    doc.cursor.pos = LineCol::new(last, col);
    doc.insert_text(added, np_core::UndoGroup::Atomic, now)
        .expect("insert");
}

/// Same, for a document that is not the active one.
fn append_at(app: &mut App, index: usize, added: &str) {
    let now = app.clock;
    let doc = app.documents.get_mut(index).expect("document");
    let last = doc.buffer.line_count().saturating_sub(1);
    let col = doc.buffer.line_string(last).chars().count();
    doc.cursor.pos = LineCol::new(last, col);
    doc.insert_text(added, np_core::UndoGroup::Atomic, now)
        .expect("insert");
}

fn text(app: &App) -> String {
    app.doc().expect("document").buffer.text_string()
}

// ------------------------------------------------------------- restore basics

#[test]
fn restoring_a_session_replaces_the_startup_untitled_tab() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write_file(&dir, "a.txt", "alpha\n");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(a.clone()),
        ..Default::default()
    });
    app.restore_session(&session);
    assert_eq!(app.documents.len(), 1, "no leftover Untitled-1");
    assert_eq!(app.doc().expect("doc").path.as_deref(), Some(a.as_path()));
}

#[test]
fn restoring_an_empty_session_still_leaves_one_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.restore_session(&Session::default());
    assert_eq!(app.documents.len(), 1, "the zero-document rule still holds");
}

#[test]
fn restore_brings_back_cursor_scroll_bookmarks_folds_language_readonly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "big.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
    let mut app = app_in(dir.path());
    open(&mut app, &path);
    // Point the caret and viewport somewhere specific, then capture.
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(5, 1), false);
    app.doc_mut().expect("doc").scroll.top_line = 4;
    app.doc_mut().expect("doc").bookmarks = [2, 6].into_iter().collect();
    app.doc_mut().expect("doc").folds = vec![np_core::FoldRange {
        start: 1,
        end: 4,
        collapsed: true,
    }];
    app.doc_mut().expect("doc").language = np_core::LanguageId::Named("Rust".into());

    let (session, _) = app.capture_session();
    session
        .save(&dir.path().join("session.toml"))
        .expect("save session");

    // A brand-new App, restored from the file.
    let mut fresh = app_in(dir.path());
    let loaded = Session::load(&dir.path().join("session.toml")).expect("load");
    fresh.restore_session(&loaded);

    let doc = fresh.doc().expect("restored document");
    assert_eq!(doc.cursor.pos, LineCol::new(5, 1), "cursor");
    assert_eq!(doc.scroll.top_line, 4, "scroll");
    assert_eq!(
        doc.bookmarks.iter().copied().collect::<Vec<_>>(),
        vec![2, 6],
        "bookmarks"
    );
    assert_eq!(doc.folds.len(), 1, "folds");
    assert_eq!(doc.folds[0].start, 1);
    assert_eq!(doc.folds[0].end, 4);
    assert!(doc.folds[0].collapsed);
    assert_eq!(
        doc.language,
        np_core::LanguageId::Named("Rust".into()),
        "language"
    );
}

#[test]
fn the_active_tab_index_is_restored_and_clamped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    for i in 0..3 {
        let p = write_file(&dir, &format!("f{i}.txt"), "x\n");
        session.tab.push(SessionTab {
            path: Some(p),
            ..Default::default()
        });
    }
    session.active_tab = 2;
    app.restore_session(&session);
    assert_eq!(app.active, 2);

    // A session recorded with more tabs than the file describes must not panic.
    let mut stale = session.clone();
    stale.active_tab = 99;
    let mut app2 = app_in(dir.path());
    app2.restore_session(&stale);
    assert_eq!(app2.active, 2, "clamped to what exists");
}

#[test]
fn a_missing_file_becomes_an_empty_read_only_buffer_with_a_note() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gone = dir.path().join("deleted.txt");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(gone.clone()),
        ..Default::default()
    });

    let notes = app.restore_session(&session);
    assert_eq!(app.documents.len(), 1, "the restore must not fail");
    let doc = app.doc().expect("document");
    assert!(doc.readonly, "a file we cannot read must not look writable");
    assert!(
        doc.path.as_deref() == Some(gone.as_path()),
        "keeps its name"
    );
    assert_eq!(doc.buffer.text_string(), "");
    assert_eq!(notes.len(), 1, "the user must be told: {notes:?}");
    assert!(notes[0].contains("deleted.txt"), "{notes:?}");
}

#[test]
fn one_missing_file_does_not_abort_the_rest_of_the_restore() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = write_file(&dir, "good.txt", "still here\n");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(dir.path().join("gone.txt")),
        ..Default::default()
    });
    session.tab.push(SessionTab {
        path: Some(good.clone()),
        ..Default::default()
    });

    app.restore_session(&session);
    assert_eq!(app.documents.len(), 2);
    assert!(app.documents[0].readonly, "the missing one");
    assert!(!app.documents[1].readonly, "the readable one");
    assert_eq!(app.documents[1].buffer.text_string(), "still here\n");
}

#[test]
fn a_restored_untitled_buffer_is_marked_modified() {
    // Retained text exists on no disk. Clearing the flag would let the user quit without
    // ever being asked about it, which is data loss by silence.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        content: Some("scratch work".into()),
        ..Default::default()
    });
    app.restore_session(&session);
    assert_eq!(text(&app), "scratch work");
    assert!(app.doc().expect("doc").modified, "must still be unsaved");
}

#[test]
fn a_restored_file_backed_tab_is_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "clean.txt", "pristine\n");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(path),
        ..Default::default()
    });
    app.restore_session(&session);
    assert!(!app.doc().expect("doc").modified, "the disk matches it");
}

#[test]
fn a_read_only_session_entry_stays_read_only_even_when_the_file_is_writable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "locked.txt", "content\n");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(path),
        readonly: true,
        ..Default::default()
    });
    app.restore_session(&session);
    assert!(
        app.doc().expect("doc").readonly,
        "the user set that flag deliberately"
    );
}

#[test]
fn a_restored_cursor_is_clamped_into_the_restored_buffer() {
    // The file shrank since the session was written. A caret at line 400 in a 2-line file
    // must land inside the file, not point at nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "shrunk.txt", "a\nb\n");
    let mut app = app_in(dir.path());
    let mut session = Session::default();
    session.tab.push(SessionTab {
        path: Some(path),
        cursor_line: 400,
        cursor_col: 40,
        scroll_top: 900,
        ..Default::default()
    });
    app.restore_session(&session);
    let doc = app.doc().expect("doc");
    assert!(
        doc.cursor.pos.line < doc.buffer.line_count(),
        "caret in range"
    );
    assert!(
        doc.scroll.top_line < doc.buffer.line_count(),
        "scroll in range"
    );
}

// --------------------------------------------------------------- recents

#[test]
fn opening_a_file_records_it_as_recent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write_file(&dir, "a.txt", "a\n");
    let b = write_file(&dir, "b.txt", "b\n");
    let mut app = app_in(dir.path());
    open(&mut app, &a);
    open(&mut app, &b);
    assert_eq!(
        app.recent_files(),
        &[b.clone(), a.clone()],
        "most recent first"
    );
}

#[test]
fn reopening_a_recent_file_moves_it_to_the_front_without_duplicating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write_file(&dir, "a.txt", "a\n");
    let b = write_file(&dir, "b.txt", "b\n");
    let mut app = app_in(dir.path());
    open(&mut app, &a);
    open(&mut app, &b);
    open(&mut app, &a);
    assert_eq!(app.recent_files().len(), 2);
    assert_eq!(app.recent_files()[0], a);
}

#[test]
fn saving_a_file_records_it_as_recent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "saved.txt", "orig\n");
    let mut app = app_in(dir.path());
    // Build a document without going through the open path, so the save is what records it.
    let loaded = np_core::load(&path, None).expect("load");
    let doc = Document::from_loaded(&loaded);
    app.open_document(doc);
    app.recent.clear();

    append(&mut app, " more");
    app.execute(np_tui::Action::Save);
    assert_eq!(
        app.recent_files(),
        std::slice::from_ref(&path),
        "a saved file is one the user will want back"
    );
}

#[test]
fn recents_are_capped_at_max_recent_files() {
    let mut config = config();
    config.ui.max_recent_files = 3;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(config, (80, 24));
    app.session_path = Some(dir.path().join("session.toml"));
    for i in 0..6 {
        let p = write_file(&dir, &format!("f{i}.txt"), "x\n");
        app.note_recent(&p);
    }
    assert_eq!(app.recent_files().len(), 3);
    assert!(app.recent_files()[0].ends_with("f5.txt"));
}

#[test]
fn a_zero_recent_cap_records_nothing() {
    let mut config = config();
    config.ui.max_recent_files = 0;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(config, (80, 24));
    let p = write_file(&dir, "a.txt", "a\n");
    app.note_recent(&p);
    assert!(app.recent_files().is_empty());
}

// -------------------------------------------------------- debounced writes

#[test]
fn a_thousand_keystrokes_produce_one_session_write() {
    // The debounce claim, measured. Without it this is a file write per character.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.mark_session_dirty();

    let now = app.clock;
    for _ in 0..1000 {
        app.doc_mut()
            .expect("doc")
            .insert_text("x", np_core::UndoGroup::Typing, now)
            .expect("insert");
        // One timer pass per event, exactly as the event loop does it.
        app.run_timers(Instant::now());
    }
    assert_eq!(
        app.session_writes, 1,
        "a burst must coalesce into a single write"
    );
    assert!(dir.path().join("session.toml").exists());
}

#[test]
fn an_idle_loop_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    for _ in 0..50 {
        app.run_timers(Instant::now());
    }
    assert_eq!(app.session_writes, 0, "no change, no write");
    assert!(
        !dir.path().join("session.toml").exists(),
        "an idle session must not create a file"
    );
}

#[test]
fn a_change_after_a_write_is_written_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    app.mark_session_dirty();
    app.run_timers(Instant::now());
    assert_eq!(app.session_writes, 1);
    app.run_timers(Instant::now());
    assert_eq!(app.session_writes, 1, "nothing new owed");
    app.mark_session_dirty();
    app.run_timers(Instant::now());
    assert_eq!(app.session_writes, 2, "and the new change reached disk");
}

#[test]
fn with_no_session_path_nothing_is_written_and_nothing_panics() {
    // No data directory means session support is simply off, not an error state.
    let mut app = app();
    app.session_path = None;
    app.mark_session_dirty();
    app.run_timers(Instant::now());
    assert_eq!(app.session_writes, 0);
}

// ------------------------------------------------------------------ autosave

#[test]
fn autosave_writes_a_modified_file_on_the_interval() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "auto.txt", "orig\n");
    let mut config = config();
    config.editor.autosave_seconds = 30;
    let mut app = App::new_headless(config, (80, 24));
    app.session_path = Some(dir.path().join("session.toml"));
    open(&mut app, &path);

    append(&mut app, "edited");

    // First call only establishes the baseline: autosaving the instant a file opens would
    // rewrite it for no reason.
    let t0 = Instant::now();
    app.run_timers(t0);
    assert_eq!(fs::read_to_string(&path).expect("read"), "orig\n");

    // After the interval, the edit reaches disk without the user asking.
    app.run_timers(t0 + Duration::from_secs(31));
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "orig\nedited",
        "autosave must write"
    );
    assert!(
        !app.doc().expect("doc").modified,
        "and the document is no longer dirty"
    );
}

#[test]
fn autosave_skips_untitled_documents() {
    // An untitled buffer has no path. Picking one would create files the user never named.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config();
    config.editor.autosave_seconds = 1;
    let mut app = App::new_headless(config, (80, 24));
    app.session_path = Some(dir.path().join("session.toml"));
    type_text(&mut app, "scratch");
    assert!(app.doc().expect("doc").path.is_none());

    app.autosave_now();
    let left: Vec<String> = fs::read_dir(dir.path())
        .expect("dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !left.iter().any(|n| n.contains("Untitled")),
        "autosave invented a file: {left:?}"
    );
    assert!(app.doc().expect("doc").modified, "still unsaved");
}

#[test]
fn autosave_skips_read_only_documents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "locked.txt", "content\n");
    let mut config = config();
    config.editor.autosave_seconds = 1;
    let mut app = App::new_headless(config, (80, 24));
    open(&mut app, &path);
    app.doc_mut().expect("doc").readonly = true;
    app.doc_mut().expect("doc").modified = true;

    app.autosave_now();
    assert_eq!(fs::read_to_string(&path).expect("read"), "content\n");
}

#[test]
fn autosave_off_never_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "manual.txt", "orig\n");
    let mut app = app_in(dir.path()); // autosave_seconds defaults to 0
    open(&mut app, &path);
    append(&mut app, "x");

    for i in 0..10 {
        app.run_timers(Instant::now() + Duration::from_secs(i * 60));
    }
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "orig\n",
        "autosave_seconds = 0 must mean off"
    );
}

#[test]
fn autosave_writes_every_eligible_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write_file(&dir, "a.txt", "A\n");
    let b = write_file(&dir, "b.txt", "B\n");
    let mut app = app_in(dir.path());
    open(&mut app, &a);
    open(&mut app, &b);
    // Both modified, but autosave is off by default so nothing happens yet.
    for i in 0..app.documents.len() {
        append_at(&mut app, i, "!");
    }
    app.config.editor.autosave_seconds = 1;
    let saved = app.autosave_now();
    assert_eq!(saved, 2, "both files saved");
    assert_eq!(fs::read_to_string(&a).expect("read"), "A\n!");
    assert_eq!(fs::read_to_string(&b).expect("read"), "B\n!");
}

// ------------------------------------------------------------ backup_on_save

#[test]
fn backup_on_save_leaves_the_previous_contents_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "bak.txt", "first\n");
    let mut config = config();
    config.editor.backup_on_save = true;
    let mut app = App::new_headless(config, (80, 24));
    open(&mut app, &path);
    append(&mut app, "second\n");
    app.execute(np_tui::Action::Save);

    assert_eq!(fs::read_to_string(&path).expect("read"), "first\nsecond\n");
    let bak = dir.path().join("bak.txt~");
    assert!(bak.exists(), "the backup must exist at {}", bak.display());
    assert_eq!(fs::read_to_string(&bak).expect("backup"), "first\n");
}

// ------------------------------------------------------------- config applied

#[test]
fn config_tab_width_reaches_newly_opened_documents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "t.txt", "x\n");
    let mut config = config();
    config.editor.tab_width = 8;
    let mut app = App::new_headless(config, (80, 24));
    let loaded = np_core::load(&path, None).expect("load");
    let mut doc = Document::from_loaded(&loaded);
    doc.tab_width = app.config.editor.tab_width.max(1);
    app.open_document(doc);
    assert_eq!(app.doc().expect("doc").tab_width, 8);
}

#[test]
fn config_geometry_settings_are_read_by_the_layout() {
    let mut config = config();
    config.ui.line_numbers = false;
    let mut app = App::new_headless(config, (80, 24));
    assert_eq!(
        app.geometry.gutter_width, 0,
        "line_numbers off means no gutter"
    );
    app.config.ui.line_numbers = true;
    app.recompute_geometry(80, 24);
    assert!(app.geometry.gutter_width > 0, "and on means a gutter");
}

#[test]
fn mouse_scroll_lines_from_config_drives_the_wheel() {
    let mut config = config();
    config.ui.mouse_scroll_lines = 7;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "many.txt", &"line\n".repeat(100));
    let mut app = App::new_headless(config, (80, 24));
    app.session_path = Some(dir.path().join("session.toml"));
    open(&mut app, &path);

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollDown,
        column: app.geometry.text_area.x + 1,
        row: app.geometry.text_area.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse_at(&ev, Instant::now());
    assert_eq!(
        app.doc().expect("doc").scroll.top_line,
        7,
        "the configured step, not a hardcoded 3"
    );
}

// ------------------------------------------------------------- persistence of state

#[test]
fn closing_a_tab_and_saving_the_session_forgets_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "gone.txt", "x\n");
    let mut app = app_in(dir.path());
    open(&mut app, &path);
    assert_eq!(app.documents.len(), 2);

    // Close the file tab (clean, so no confirm).
    let idx = 1;
    app.active = idx;
    app.execute(np_tui::Action::CloseTab);
    app.mark_session_dirty();
    assert!(app.flush_session(), "the close must be recorded");

    let loaded = Session::load(&dir.path().join("session.toml")).expect("load");
    assert_eq!(
        loaded.tab.len(),
        app.documents.len(),
        "the session describes what is open, no more"
    );
}

#[test]
fn a_session_written_by_this_build_loads_cleanly_and_repeatedly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session_path = dir.path().join("session.toml");
    let a = write_file(&dir, "a.txt", "A\n");
    let mut app = app_in(dir.path());
    open(&mut app, &a);
    append(&mut app, "typed");
    app.mark_session_dirty();
    assert!(app.flush_session());

    // Load twice: a file that only parses once is a file that will fail on a later launch.
    let first = Session::load(&session_path).expect("first load");
    let second = Session::load(&session_path).expect("second load");
    assert_eq!(first, second);

    let mut restored = app_in(dir.path());
    restored.restore_session(&first);
    assert_eq!(restored.documents.len(), app.documents.len());
    // What is restored is what is **on disk**, not the unsaved buffer. A file-backed tab
    // stores no content snapshot by design: a second copy of the file would restore stale
    // text over a newer one, which is a worse class of surprise than losing an edit the user
    // was never told npt keeps. Untitled buffers are the case that stores content, because
    // there is nowhere else for it to live.
    assert_eq!(text(&restored), "A\n");
    assert_eq!(
        restored.doc().expect("doc").path,
        app.doc().expect("doc").path
    );
    let caret = restored.doc().expect("doc").cursor.pos;
    assert!(
        caret.line < restored.doc().expect("doc").buffer.line_count(),
        "the caret is clamped into the text that actually exists, not the text the session
         remembered: {caret:?}"
    );
}

#[test]
fn encoding_survives_a_session_round_trip_through_a_file() {
    // The document's encoding comes from the file, so restoring re-detects it; assert the
    // restored text is byte-identical rather than assuming the stored encoding is reused.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("utf16.txt");
    let bytes = np_core::encode("héllo\n", EncodingId::Utf16Le);
    fs::write(&path, &bytes).expect("write");

    let mut app = app_in(dir.path());
    open(&mut app, &path);
    assert_eq!(text(&app), "héllo\n");
    app.mark_session_dirty();
    app.flush_session();

    let loaded = Session::load(&dir.path().join("session.toml")).expect("load");
    let mut fresh = app_in(dir.path());
    fresh.restore_session(&loaded);
    assert_eq!(text(&fresh), "héllo\n");
    assert_eq!(
        fresh.doc().expect("doc").encoding,
        app.doc().expect("doc").encoding,
        "the codec is re-detected from the file, not carried over stale"
    );
}

#[test]
fn a_restored_document_keeps_its_original_line_endings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("crlf.txt");
    fs::write(&path, "one\r\ntwo\r\n").expect("write");
    let mut app = app_in(dir.path());
    open(&mut app, &path);
    app.mark_session_dirty();
    app.flush_session();

    let loaded = Session::load(&dir.path().join("session.toml")).expect("load");
    let mut fresh = app_in(dir.path());
    fresh.restore_session(&loaded);
    let doc = fresh.doc().expect("doc");
    assert_eq!(doc.eol, Eol::Crlf, "the dominant ending came from the file");
    assert_eq!(
        doc.buffer.text_string(),
        "one\ntwo\n",
        "normalized in the buffer"
    );
}
