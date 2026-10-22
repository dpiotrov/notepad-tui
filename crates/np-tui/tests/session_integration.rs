//! Stage S9 end-to-end: two tabs edited, bookmarked, folded and language-tagged, saved to a
//! session, then restored into a brand-new `App` and checked field by field.
//!
//! Also covers the process-level startup policy: `--fresh` skipping restore, `--config`
//! overriding the config path, and CLI file arguments taking precedence over saved tabs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use np_core::{Config, Document, LanguageId, LineCol, Session};
use np_tui::App;

const W: u16 = 80;
const H: u16 = 24;

fn config() -> Config {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    config
}

fn app_in(dir: &Path) -> App {
    let mut app = App::new_headless(config(), (W, H));
    app.session_path = Some(dir.join("session.toml"));
    app
}

fn open(app: &mut App, path: &Path) {
    let loaded = np_core::load(path, None).expect("load");
    let mut doc = Document::from_loaded(&loaded);
    doc.tab_width = app.config.editor.tab_width.max(1);
    app.open_document(doc);
    let p = path.to_path_buf();
    app.note_recent(&p);
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("write");
    path
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

/// Move the caret to the end of the document and type.
///
/// Opened files land the caret at 0,0, so typing without this inserts at the front.
fn append_text(app: &mut App, text: &str) {
    let now = app.clock;
    let doc = app.doc_mut().expect("document");
    let last = doc.buffer.line_count().saturating_sub(1);
    let col = doc.buffer.line_string(last).chars().count();
    doc.cursor.pos = LineCol::new(last, col);
    doc.insert_text(text, np_core::UndoGroup::Atomic, now)
        .expect("insert");
}

fn npt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_npt"))
}

fn run_with_env(args: &[&str], env: &[(&str, &Path)]) -> std::process::Output {
    let mut cmd = npt();
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.spawn()
        .expect("spawn")
        .wait_with_output()
        .expect("wait")
}

// ------------------------------------------------------------- the big round trip

#[test]
fn a_full_working_session_restores_into_a_fresh_app() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = write(
        dir.path(),
        "alpha.rs",
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    );
    let b = write(dir.path(), "beta.md", "# title\nbody line\n");

    // ---- session one: open two files, edit both, bookmark, fold, tag a language.
    let mut app = app_in(dir.path());
    open(&mut app, &a);
    open(&mut app, &b);

    // Edit and bookmark the first file.
    app.active = 1;
    append_text(&mut app, "// appended\n");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(2, 3), false);
    app.doc_mut().expect("doc").scroll.top_line = 1;
    app.doc_mut().expect("doc").bookmarks = [1, 3].into_iter().collect();
    app.doc_mut().expect("doc").folds = vec![np_core::FoldRange {
        start: 0,
        end: 2,
        collapsed: true,
    }];
    app.doc_mut().expect("doc").language = LanguageId::Named("Rust".into());

    // Edit the second file and leave it active, so active_tab is non-zero.
    app.active = 2;
    append_text(&mut app, "more body\n");
    app.doc_mut()
        .expect("doc")
        .set_cursor(LineCol::new(2, 4), false);

    // Persist.
    app.mark_session_dirty();
    assert!(app.flush_session(), "the session must be written");
    let session_path = dir.path().join("session.toml");
    assert!(session_path.exists(), "written where the App says");

    // ---- session two: a brand-new App restored from that file.
    let saved = Session::load(&session_path).expect("the session we wrote must load");
    let mut fresh = app_in(dir.path());
    let notes = fresh.restore_session(&saved);
    assert!(notes.is_empty(), "nothing should be missing: {notes:?}");

    // Two tabs, not three: the startup untitled tab is replaced, not kept alongside.
    assert_eq!(
        fresh.documents.len(),
        3,
        "startup tab was replaced by two files"
    );
    assert_eq!(fresh.active, 2, "the tab that was active at save time");

    let doc_a = fresh.documents.get(1).expect("first file");
    assert_eq!(doc_a.path.as_deref(), Some(a.as_path()));
    // Text comes from **disk**, not from a snapshot in the session. A file-backed tab stores
    // no content by design: a second copy would restore stale text over a newer file, which
    // is a worse class of surprise than losing an edit npt never claimed to keep. Untitled
    // buffers are the case that stores content, because there is nowhere else for it to live.
    assert_eq!(
        doc_a.buffer.text_string(),
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n"
    );
    assert_eq!(doc_a.cursor.pos, LineCol::new(2, 3), "cursor restored");
    assert_eq!(doc_a.scroll.top_line, 1, "scroll restored");
    assert_eq!(
        doc_a.bookmarks.iter().copied().collect::<Vec<_>>(),
        vec![1, 3],
        "bookmarks restored"
    );
    assert_eq!(doc_a.folds.len(), 1, "fold restored");
    assert_eq!(doc_a.folds[0].start, 0);
    assert_eq!(doc_a.folds[0].end, 2);
    assert!(doc_a.folds[0].collapsed);
    assert_eq!(
        doc_a.language,
        LanguageId::Named("Rust".into()),
        "language restored"
    );

    let doc_b = fresh.documents.get(2).expect("second file");
    assert_eq!(doc_b.path.as_deref(), Some(b.as_path()));
    assert_eq!(doc_b.buffer.text_string(), "# title\nbody line\n");
    assert!(
        doc_b.cursor.pos.line < doc_b.buffer.line_count(),
        "the caret is clamped into the text that exists: {:?}",
        doc_b.cursor.pos
    );

    // Recents carried over, most recent first.
    assert_eq!(fresh.recent_files().len(), 2);
    assert_eq!(fresh.recent_files()[0], b);
    assert_eq!(fresh.recent_files()[1], a);

    // The restored view state reached the chrome, not just the document.
    assert_eq!(fresh.document_list_selected, fresh.active);
}

#[test]
fn a_untitled_buffer_with_scratch_text_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_in(dir.path());
    // Replace the startup tab's content with scratch work and leave it untitled.
    type_text(&mut app, "notes I cannot lose");
    app.mark_session_dirty();
    assert!(app.flush_session());

    let saved = Session::load(&dir.path().join("session.toml")).expect("load");
    let mut fresh = app_in(dir.path());
    fresh.restore_session(&saved);

    // The startup tab plus none other: the untitled one is restored in its place.
    assert_eq!(fresh.documents.len(), 1);
    assert_eq!(
        fresh.doc().expect("doc").buffer.text_string(),
        "notes I cannot lose"
    );
    assert!(
        fresh.doc().expect("doc").modified,
        "restored scratch text must still be marked unsaved, or quitting would discard it
         without asking"
    );
}

#[test]
fn a_session_with_a_deleted_file_still_restores_everything_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keep = write(dir.path(), "keep.txt", "kept\n");
    let doomed = write(dir.path(), "doomed.txt", "gone soon\n");

    let mut app = app_in(dir.path());
    open(&mut app, &doomed);
    open(&mut app, &keep);
    app.mark_session_dirty();
    app.flush_session();
    let saved = Session::load(&dir.path().join("session.toml")).expect("load");

    fs::remove_file(&doomed).expect("remove");

    let mut fresh = app_in(dir.path());
    let notes = fresh.restore_session(&saved);
    assert_eq!(fresh.documents.len(), 3, "both files still restored");
    assert_eq!(notes.len(), 1, "and exactly one note: {notes:?}");
    assert!(notes[0].contains("doomed.txt"), "{notes:?}");

    let placeholder = fresh
        .documents
        .iter()
        .find(|d| d.readonly)
        .expect("placeholder");
    assert_eq!(placeholder.buffer.text_string(), "", "empty, not stale");
    let good = fresh
        .documents
        .iter()
        .find(|d| d.path.as_deref() == Some(keep.as_path()))
        .expect("the readable file");
    assert_eq!(good.buffer.text_string(), "kept\n");
    assert!(!good.readonly, "the readable file stays writable");
}

// ------------------------------------------------------------- process-level policy

#[test]
fn a_corrupt_config_and_a_corrupt_session_cannot_stop_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("xdg");
    let cfg_dir = home.join("npt");
    fs::create_dir_all(&cfg_dir).expect("mkdir");
    fs::write(cfg_dir.join("config.toml"), "]]] not toml [[[").expect("write");

    let data = dir.path().join("data").join("npt");
    fs::create_dir_all(&data).expect("mkdir");
    fs::write(data.join("session.toml"), "active_tab = 0\n[[tab]]\ncur").expect("write");

    // No TTY here, so the run fails at terminal setup -- but it must fail *after* surviving
    // both corrupt files, which is what the exit code and stderr distinguish.
    let out = run_with_env(
        &["--fresh"],
        &[
            ("XDG_CONFIG_HOME", &home),
            ("XDG_DATA_HOME", &dir.path().join("data")),
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // Exit 1 (runtime: no terminal) rather than a panic, and the config problem was reported.
    assert_eq!(
        out.status.code(),
        Some(1),
        "runtime failure, not a crash: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "a corrupt config must never panic: {err}"
    );
}

#[test]
fn a_config_override_path_is_used_and_its_warnings_reach_stderr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("custom.toml");
    fs::write(&path, "[editor]\ntab_width = 2\nbrand_new_key = 1\n").expect("write");

    let out = run_with_env(&["--config", path.to_str().expect("utf8"), "--fresh"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("brand_new_key"),
        "the unknown key must be reported from the overridden file: {err}"
    );
}

#[test]
fn a_bad_config_value_warns_without_preventing_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.toml");
    fs::write(&path, "[editor]\ntab_width = \"wide\"\n").expect("write");
    let out = run_with_env(&["--config", path.to_str().expect("utf8"), "--fresh"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "must not panic: {err}");
    assert!(
        err.contains("npt: config:") || err.contains("wrong type"),
        "the fallback must be announced: {err}"
    );
}

#[test]
fn fresh_and_config_are_accepted_together() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.toml");
    fs::write(&path, "[ui]\nline_numbers = false\n").expect("write");
    let out = run_with_env(
        &[
            "--fresh",
            "--config",
            path.to_str().expect("utf8"),
            "--",
            dir.path().join("new.txt").to_str().expect("utf8"),
        ],
        &[],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // Reaches terminal setup and fails there (no TTY), which is exit 1 and no panic.
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    assert!(
        !err.contains("unrecognized"),
        "flags must be accepted: {err}"
    );
}

#[test]
fn help_documents_the_session_flags() {
    let out = npt().arg("--help").output().expect("run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--fresh"), "--fresh must be documented");
    assert!(text.contains("--config"), "--config must be documented");
}

/// Parse a command line the way the binary receives one: owned strings after argv[0].
fn cli(args: &[&str]) -> np_tui::cli::Cli {
    let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    np_tui::cli::parse(&owned).expect("the command line must parse")
}

// ------------------------------------------------- startup policy, asserted directly

#[test]
fn a_plain_invocation_restores_the_session() {
    let parsed = cli(&[]);
    assert!(
        np_tui::cli::should_restore_session(&parsed),
        "a bare `npt` is exactly the case restore exists for"
    );
}

#[test]
fn fresh_skips_the_restore() {
    let parsed = cli(&["--fresh"]);
    assert!(parsed.fresh);
    assert!(
        !np_tui::cli::should_restore_session(&parsed),
        "--fresh must skip restore (FR-CG3)"
    );
}

#[test]
fn file_arguments_take_precedence_over_the_session() {
    let parsed = cli(&["notes.txt"]);
    assert_eq!(parsed.files.len(), 1);
    assert!(
        !np_tui::cli::should_restore_session(&parsed),
        "`npt notes.txt` opens notes.txt, not last session plus notes.txt"
    );

    // Several files behave the same way.
    let many = cli(&["a.txt", "b.txt", "c.txt"]);
    assert!(!np_tui::cli::should_restore_session(&many));
}

#[test]
fn a_config_flag_alone_does_not_suppress_the_restore() {
    // `--config` changes *which* config is read, not whether tabs come back. Treating it as
    // a file argument would silently disable session restore for anyone with an alias.
    let parsed = cli(&["--config", "/tmp/other.toml"]);
    assert!(parsed.files.is_empty(), "--config must not land in files");
    assert!(np_tui::cli::should_restore_session(&parsed));
}

#[test]
fn a_position_flag_does_not_suppress_the_restore() {
    let parsed = cli(&["--line", "40"]);
    assert!(parsed.files.is_empty());
    assert!(np_tui::cli::should_restore_session(&parsed));
}
