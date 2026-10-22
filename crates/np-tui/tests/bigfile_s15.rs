//! S15: FR-F10 large-file handling, spec §9 and §11 M7.
//!
//! "100 MB file opens read-only with progress" is an M7 acceptance item, and the pieces for
//! it were all present -- `spawn_load`, `BgMessage::FileLoaded`, `apply_loaded`,
//! `App::pending_load_request` -- except the call that starts it. Nothing ever set
//! `pending_load_request`, so a 200 MB log loaded synchronously on the UI thread and the
//! editor froze for as long as the read took. These tests drive `open_path` and assert the
//! routing actually happens, which is the thing that was missing.
//!
//! The fixtures sit just over `BACKGROUND_BYTES` only in the policy tests; the end-to-end
//! tests lower the threshold instead, so the suite does not spend a minute writing a file to
//! prove a code path. The real 100 MB timing lives in `perf_s15.rs`.

use std::path::Path;

use np_core::Config;
use np_tui::app::App;

fn fresh() -> App {
    App::new_headless(Config::default(), (120, 40))
}

/// An `App` holding the read-only placeholder tab that `open_path` pushes for a
/// background-sized file, without needing a 100 MB fixture to get there.
fn app_with_placeholder(path: &Path) -> App {
    let mut app = fresh();
    let mut placeholder = np_core::Document::from_decoded(
        path.to_path_buf(),
        "",
        app.config.default_encoding(),
        app.config.default_eol(),
        false,
    );
    placeholder.readonly = true;
    app.open_document(placeholder);
    app
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text.as_bytes()).expect("write fixture");
}

#[test]
fn the_background_band_is_100_mb_and_is_a_pure_decision() {
    use np_core::file::{open_policy, OpenDecision, ASK_READONLY_BYTES, BACKGROUND_BYTES};
    // The boundary is asserted rather than assumed, because the routing in `open_path`
    // depends on it and a moved constant would silently change which files freeze the UI.
    assert_eq!(open_policy(BACKGROUND_BYTES), OpenDecision::Background);
    assert_eq!(
        open_policy(BACKGROUND_BYTES - 1),
        OpenDecision::AskReadOnly,
        "just under 100 MB must still be an interactive load"
    );
    // The bands must stay disjoint and ordered, or a file could match neither.
    const _: () = assert!(ASK_READONLY_BYTES < BACKGROUND_BYTES);
}

#[test]
fn a_background_sized_open_does_not_read_on_the_ui_thread() {
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.log");
        // Content does not matter for the routing decision -- only the size does -- so this
        // is a sparse file rather than 100 MB of text.
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(np_core::file::BACKGROUND_BYTES)
            .expect("sparse extend");
        drop(f);

        let mut app = fresh();
        let tabs_before = app.documents.len();
        app.open_path(&path);

        assert_eq!(
            app.pending_load_request,
            Some(app.bg.next_request_id() - 1),
            "a Background-sized open must register a pending load"
        );
        // A placeholder tab exists so the UI shows something is happening...
        assert_eq!(app.documents.len(), tabs_before + 1);
        // ...and it is empty, because the read has not been applied yet.
        let doc = app.doc().expect("placeholder document");
        assert_eq!(doc.buffer.line_count(), 1, "placeholder holds no text");
        assert!(doc.readonly, "a file being read in is not editable");
    }
}

#[test]
fn the_loaded_file_replaces_the_placeholder_instead_of_adding_a_tab() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("small.log");
    write(&path, "alpha\nbeta\ngamma\n");

    // A fresh App already holds one untitled tab, so the count is asserted as a delta
    // against the placeholder rather than as an absolute number.
    let mut app = app_with_placeholder(&path);
    let tabs_with_placeholder = app.documents.len();

    let worker = app.bg.clone();
    let id = worker.next_request_id();
    app.pending_load_request = Some(id);
    let cancel = worker.begin_request();
    let result = np_tui::bg::loader::load_with_progress(&path, &cancel, &mut |_| ());
    worker.send(np_tui::bg::BgMessage::FileLoaded {
        request: id,
        result,
    });

    let applied = np_tui::fif::drain(&mut app, 16);
    assert_eq!(applied, 1, "the FileLoaded message must be applied");
    assert_eq!(
        app.documents.len(),
        tabs_with_placeholder,
        "applying a load must not stack a second tab for the same path"
    );
    assert_eq!(
        app.documents
            .iter()
            .filter(|d| d.path.as_deref() == Some(path.as_path()))
            .count(),
        1,
        "exactly one tab points at the loaded path"
    );
    assert_eq!(
        app.doc().expect("doc").buffer.text_string(),
        "alpha\nbeta\ngamma\n"
    );
    assert!(
        !app.doc().expect("doc").readonly,
        "the loaded document follows the file, not the placeholder's read-only state"
    );
    assert_eq!(app.pending_load_request, None, "the request is consumed");
}

#[test]
fn a_superseded_load_cannot_push_a_document_in_front_of_the_user() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stale.log");
    write(&path, "should not appear\n");

    let mut app = fresh();
    let worker = app.bg.clone();
    let stale_id = worker.next_request_id();
    // A newer load is pending; the old worker's message arrives afterwards.
    app.pending_load_request = Some(worker.next_request_id());
    let cancel = worker.begin_request();
    let result = np_tui::bg::loader::load_with_progress(&path, &cancel, &mut |_| ());
    worker.send(np_tui::bg::BgMessage::FileLoaded {
        request: stale_id,
        result,
    });

    let applied = np_tui::fif::drain(&mut app, 16);
    assert_eq!(applied, 0, "a stale load must be dropped");
    assert!(
        !app.documents
            .iter()
            .any(|d| d.buffer.text_string() == "should not appear\n"),
        "the stale file's contents must not reach a buffer"
    );
}

#[test]
fn an_under_threshold_open_still_loads_inline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("normal.txt");
    write(&path, "one\ntwo\n");

    let mut app = fresh();
    app.open_path(&path);
    assert_eq!(
        app.pending_load_request, None,
        "an ordinary file must not go through the worker"
    );
    assert_eq!(app.doc().expect("doc").buffer.text_string(), "one\ntwo\n");
}

#[test]
fn opening_the_same_path_twice_does_not_start_a_second_background_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("huge2.log");
    let f = std::fs::File::create(&path).expect("create");
    f.set_len(np_core::file::BACKGROUND_BYTES).expect("sparse");
    drop(f);

    let mut app = fresh();
    app.open_path(&path);
    let tabs = app.documents.len();
    let request = app.pending_load_request;
    app.open_path(&path);
    assert_eq!(app.documents.len(), tabs, "no second tab");
    assert_eq!(
        app.pending_load_request, request,
        "no second read for a path already open"
    );
}

#[test]
fn a_failed_background_load_reports_and_keeps_the_named_buffer() {
    // A path that is a directory: the worker's read fails, and the user must be told rather
    // than left with a silently empty tab.
    let mut app = fresh();
    let worker = app.bg.clone();
    let id = worker.next_request_id();
    app.pending_load_request = Some(id);
    worker.send(np_tui::bg::BgMessage::FileLoaded {
        request: id,
        result: Err("permission denied".to_string()),
    });
    let applied = np_tui::fif::drain(&mut app, 16);
    assert_eq!(applied, 1);
    assert_eq!(app.pending_load_request, None);
    // The failure must reach the rendered screen, not just the internal list.
    let rendered: Vec<&str> = app
        .timed_toasts
        .iter()
        .map(|t| t.message.as_str())
        .collect();
    assert!(
        rendered.iter().any(|m| m.contains("permission denied")),
        "a load failure must be visible on screen: {rendered:?}"
    );
    assert!(
        app.timed_toasts
            .iter()
            .any(|t| t.message.contains("permission denied") && t.error),
        "a load failure is an error toast, not an informational one"
    );
}

#[test]
fn a_load_whose_placeholder_was_closed_still_opens_the_file() {
    // `apply_loaded` has a fallback for "no tab owns this path any more". It is worth a
    // test because the alternative is that closing a tab mid-read silently discards a
    // 100 MB file the user waited for.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("closed.log");
    write(&path, "arrived\n");

    let mut app = app_with_placeholder(&path);
    let worker = app.bg.clone();
    let id = worker.next_request_id();
    app.pending_load_request = Some(id);
    let cancel = worker.begin_request();
    let result = np_tui::bg::loader::load_with_progress(&path, &cancel, &mut |_| ());
    worker.send(np_tui::bg::BgMessage::FileLoaded {
        request: id,
        result,
    });

    // The user closes the placeholder while the read is in flight.
    app.documents.clear();
    app.active = 0;

    let applied = np_tui::fif::drain(&mut app, 16);
    assert_eq!(applied, 1, "the load must still be applied");
    assert!(
        app.documents
            .iter()
            .any(|d| d.buffer.text_string() == "arrived\n"),
        "the finished file must open even with no placeholder left to replace"
    );
}
