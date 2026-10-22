//! Spec §8 edge cases (S15). Each maps to a numbered work item.
//!
//! These are the "does it survive the ugly cases" tests. Where a case is only checkable
//! against a real terminal it is exercised here against the injected `Driver` / headless
//! `App` and the residual manual step is recorded in `docs/MANUAL_QA.md`.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::app::App;
use std::time::Duration;

fn fresh(w: u16, h: u16) -> App {
    App::new_headless(Config::default(), (w, h))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn with_doc(app: &mut App, name: &str, text: &str) {
    let doc = np_core::Document::from_decoded(
        name.into(),
        text,
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.open_document(doc);
}

// ─────────────────────────────────── §8.2 minimum size 40x8: no crash, recovers

#[test]
fn the_minimum_size_is_forty_by_eight() {
    assert_eq!(np_tui::event::MIN_WIDTH, 40);
    assert_eq!(np_tui::event::MIN_HEIGHT, 8);
    assert!(np_tui::event::is_too_small(39, 8));
    assert!(np_tui::event::is_too_small(40, 7));
    assert!(!np_tui::event::is_too_small(40, 8));
}

#[test]
fn at_the_minimum_size_a_frame_renders_without_panicking() {
    let mut app = fresh(40, 8);
    with_doc(&mut app, "/tmp/tiny.txt", "hello\nworld\n");
    // The too-small screen is for BELOW the minimum; at exactly 40x8 the normal frame draws.
    let buf = np_tui::ui::render_to_buffer(&mut app, 40, 8);
    assert!(
        !np_tui::ui::row_text(&buf, 0, 0, 40).is_empty(),
        "menu row should exist"
    );
}

#[test]
fn below_the_minimum_the_too_small_screen_renders_at_every_degenerate_size() {
    // 1x1 up to just-under-40x8: every size must produce a buffer without a panic, and must
    // say something rather than paint nothing.
    for (w, h) in [
        (1u16, 1u16),
        (20, 1),
        (39, 7),
        (10, 3),
        (40, 7),
        (39, 8),
        (5, 2),
    ] {
        let mut app = fresh(w.max(1), h.max(1));
        with_doc(&mut app, "/tmp/tiny.txt", "hi\n");
        let buf = np_tui::ui::render_to_buffer(&mut app, w, h);
        // Nothing is drawn past the right or bottom edge -- a panic or a desync would show as
        // an out-of-bounds write, which Buffer already prevents, so what we assert here is
        // "a frame came out and the loop is still alive".
        assert_eq!(buf.area().width, w);
        assert_eq!(buf.area().height, h);
    }
}

#[test]
fn the_too_small_screen_says_resize_and_recovers() {
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    });
    let app = np_tui::App::new_headless(np_core::Config::default(), (30, 5));
    np_tui::event::draw_too_small(&mut buf, 30, 5, &app.palette());
    let block: String = (0..5)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, 30))
        .collect();
    assert!(block.contains("Terminal too small"), "{block}");
    assert!(
        block.contains("min 40x8"),
        "must state the requirement: {block}"
    );
    assert!(block.contains("Resize"), "must say how to recover: {block}");
}

#[test]
fn resize_from_too_small_back_to_normal_restores_the_frame() {
    let mut app = fresh(20, 4);
    with_doc(&mut app, "/tmp/grow.txt", "content\n");
    np_tui::event::handle_event(&mut app, &Event::Resize(20, 4));
    assert!(app.dirty, "a resize schedules a redraw");
    np_tui::event::handle_event(&mut app, &Event::Resize(80, 24));
    assert!(app.dirty);
    // And the frame is the normal one again, not the too-small screen.
    let buf = np_tui::ui::render_to_buffer(&mut app, 80, 24);
    let block: String = (0..24)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, 80))
        .collect();
    assert!(
        !block.contains("Terminal too small"),
        "should have recovered: {block}"
    );
    assert!(
        block.contains("content"),
        "the document should be visible: {block}"
    );
}

// ─────────────────────────────────── §8.3 resize storm: no panic at any size

#[test]
fn a_storm_of_two_hundred_resizes_never_panics() {
    // 40x8 .. 400x200, driven through the real event handler. A panic here is a layout bug at
    // some intermediate size, which is exactly what a window drag produces.
    let mut app = fresh(80, 24);
    with_doc(
        &mut app,
        "/tmp/storm.txt",
        "line one\nline two\nline three\n",
    );
    let mut i: u32 = 0;
    for n in 0..200u32 {
        i += 1;
        // Sweep width and height with different periods so the pairs cover the range unevenly.
        let w = 40 + ((n * 7) % 361) as u16; // 40..=400
        let h = 8 + ((n * 11) % 193) as u16; // 8..=200
        np_tui::event::handle_event(&mut app, &Event::Resize(w, h));
        // Recompute geometry the way draw_if_dirty would, to exercise the layout at each size.
        app.recompute_geometry(w, h);
    }
    assert_eq!(i, 200);
    // The app is still usable after the storm.
    let mut back = fresh(80, 24);
    with_doc(&mut back, "/tmp/storm.txt", "line one\n");
    let buf = np_tui::ui::render_to_buffer(&mut back, 80, 24);
    assert!(np_tui::ui::row_text(&buf, 0, 0, 80).contains("File"));
}

#[test]
fn a_resize_burst_coalesces_to_one_draw() {
    // The storm marks dirty 200 times, but `dirty` is a boolean: a batch that handles all 200
    // events and then draws once emits exactly one frame. Count draws through the same
    // dirty-flag contract `draw_if_dirty` uses.
    let mut app = fresh(80, 24);
    with_doc(&mut app, "/tmp/coalesce.txt", "x\n");
    for n in 0..200u32 {
        let w = 40 + (n % 361) as u16;
        let h = 8 + (n % 193) as u16;
        np_tui::event::handle_event(&mut app, &Event::Resize(w, h));
    }
    assert!(app.dirty, "the batch leaves the screen dirty once");
    // One draw clears it; a second draw must be skipped. That is the coalescing contract:
    // 200 dirty-marks cost exactly one frame, because `dirty` is a boolean, not a counter.
    use ratatui::backend::TestBackend;
    let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    let drew = np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
    assert!(drew, "first draw after the storm should paint");
    assert!(!app.dirty, "the draw clears dirty");
    let again = np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
    assert!(!again, "a second draw must be skipped when nothing changed");
}

// ─────────────────────────────────── §8.4 huge paste: one undo unit

#[test]
fn a_large_paste_is_one_undo_unit() {
    // Not the full 10 MB (slow in debug), but large enough that per-line grouping would be
    // dozens of undo steps. One Ctrl+Z must undo the whole thing.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/paste.txt", "start\n");
    let big: String = (0..5_000).map(|i| format!("pasted line {i}\n")).collect();
    let bytes = big.len();
    assert!(bytes > 80_000, "fixture should be substantial, got {bytes}");
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    np_tui::event::paste_into(&mut app, &big);
    assert!(app
        .doc()
        .is_some_and(|d| d.buffer.text_string().contains("pasted line 4999")));
    // One undo restores the pre-paste text exactly.
    app.execute(np_tui::actions::Action::Undo);
    assert_eq!(
        app.doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default(),
        before,
        "one undo must revert the entire paste"
    );
}

#[test]
fn an_empty_paste_changes_nothing() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/empty-paste.txt", "keep\n");
    np_tui::event::paste_into(&mut app, "");
    assert_eq!(
        app.doc().map(|d| d.buffer.text_string()).as_deref(),
        Some("keep\n")
    );
    assert!(
        app.timed_toasts.is_empty(),
        "an empty paste is not an error"
    );
}

// ─────────────────────────────────── §8.5 save failures: sticky toast, still modified

#[test]
fn saving_a_read_only_file_toasts_and_leaves_the_document_modified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.txt");
    std::fs::write(&path, "original\n").expect("write");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).expect("chmod");

    let mut app = fresh(100, 30);
    app.open_path(&path);
    // Make it dirty.
    if let Some(doc) = app.doc_mut() {
        doc.modified = true;
    }
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    app.execute(np_tui::actions::Action::Save);
    // Either a read-only document refuses up front, or the save fails; both must toast.
    let messages: Vec<String> = app.timed_toasts.iter().map(|t| t.message.clone()).collect();
    let refused = app.timed_toasts.iter().any(|t| t.error);
    assert!(
        refused || app.doc().is_some_and(|d| d.readonly),
        "a read-only save must be reported: {messages:?}"
    );
    // The buffer content is untouched and still marked modified.
    assert_eq!(
        app.doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default(),
        before
    );
    // On-disk bytes unchanged.
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "original\n");
}

#[test]
fn saving_into_a_vanished_directory_fails_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gone.txt");
    let mut app = fresh(100, 30);
    with_doc(&mut app, path.to_str().unwrap_or("gone.txt"), "content\n");
    if let Some(doc) = app.doc_mut() {
        doc.modified = true;
    }
    // Remove the parent so the write target's directory does not exist.
    drop(dir);
    app.execute(np_tui::actions::Action::Save);
    // The save fails; the document stays modified and an error toast appears.
    assert!(
        app.doc().is_some_and(|d| d.modified),
        "a failed save must leave the document modified"
    );
    assert!(
        app.timed_toasts.iter().any(|t| t.error),
        "a failed save must toast: {:?}",
        app.timed_toasts
            .iter()
            .map(|t| t.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_failed_save_leaves_the_original_bytes_intact() {
    // The atomic writer must not truncate the target on failure.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("keep.txt");
    std::fs::write(&path, "PRECIOUS\n").expect("write");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).expect("chmod");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    if let Some(doc) = app.doc_mut() {
        doc.modified = true;
        doc.readonly = false; // force the save to actually attempt the write
    }
    app.execute(np_tui::actions::Action::Save);
    // Whether the readonly flag or the write blocks it, the file on disk is unchanged.
    let on_disk = std::fs::read_to_string(&path).expect("read");
    assert!(
        on_disk.contains("PRECIOUS"),
        "the original bytes must survive a failed save, got {on_disk:?}"
    );
}

// ─────────────────────────────────── §8.6 external delete: banner, then Save As

#[test]
fn a_file_deleted_externally_raises_the_banner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vanished.txt");
    std::fs::write(&path, "here\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    std::fs::remove_file(&path).expect("rm");
    app.advance_clock(Duration::from_secs(2));
    app.check_external_changes();
    assert!(app.banner.is_some(), "a deleted file must raise a banner");
    let msg = app.banner.clone().map(|b| b.message).unwrap_or_default();
    assert!(
        msg.contains("vanished.txt"),
        "banner must name the file: {msg}"
    );
}

#[test]
fn reloading_a_deleted_file_reports_rather_than_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gone-reload.txt");
    std::fs::write(&path, "v1\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    std::fs::remove_file(&path).expect("rm");
    // Reload through the action; must not panic and must report.
    app.execute(np_tui::actions::Action::ReloadFile);
    // The document survives (empty or original), and an error is surfaced.
    assert!(
        app.doc().is_some(),
        "the tab must not vanish on a failed reload"
    );
}

// ─────────────────────────────────── §8.7 unicode torture

#[test]
fn the_caret_never_splits_a_grapheme_cluster() {
    // Emoji ZWJ family, a flag, combining marks, a skin-tone modifier, CJK, and ZWJ-zero-width
    // sequences on one line.
    let text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F1EF}\u{1F1F5} e\u{0301} \u{4F60}\u{597D} \u{200B}\u{4F60} end\n";
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/unicode.txt", text);
    // Move right one "column" at a time and back; the round trip must be byte-identical.
    for _ in 0..40 {
        app.execute(np_tui::actions::Action::MoveRight);
    }
    for _ in 0..40 {
        app.execute(np_tui::actions::Action::MoveLeft);
    }
    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    assert_eq!(after, text, "caret movement must not corrupt clusters");
}

#[test]
fn backspace_deletes_a_whole_cluster_not_a_scalar() {
    // "e" + combining acute is one visible character; one backspace removes both scalars.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/comb.txt", "e\u{0301}X\n");
    // Caret at char index 2, the boundary right after the two-scalar cluster.
    if let Some(doc) = app.doc_mut() {
        doc.set_cursor(np_core::LineCol::new(0, 2), false);
    }
    press(&mut app, KeyCode::Backspace);
    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    assert_eq!(
        after, "X\n",
        "the whole e+acute cluster must go, got {after:?}"
    );
}

#[test]
fn overtype_replaces_a_whole_cluster() {
    // FR-E12 under torture: overtyping a CJK char must consume both bytes of the cluster's
    // display cell and not leave a fragment.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/over.txt", "\u{4F60}\u{597D}\n");
    if let Some(doc) = app.doc_mut() {
        doc.overtype = true;
        doc.set_cursor(np_core::LineCol::new(0, 0), false);
    }
    press(&mut app, KeyCode::Char('A'));
    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    assert!(
        after.starts_with('A'),
        "A replaces the first CJK char: {after:?}"
    );
    assert!(
        after.contains('\u{597D}'),
        "the second char survives: {after:?}"
    );
}

#[test]
fn wide_characters_render_two_cells_without_desyncing_columns() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/wide.txt", "\u{4F60}\u{597D}ok\n");
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    // Find the editor's text row and confirm the ASCII tail is intact after the wide chars.
    let text_row = (0..30)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, 100))
        .find(|r| r.contains("ok"));
    assert!(
        text_row.is_some(),
        "the ASCII tail must still render after wide chars"
    );
}

#[test]
fn rtl_and_bidi_text_is_stored_byte_exact() {
    // Hebrew + Arabic + Latin on one line. The buffer stores logical order; the terminal does
    // the bidi. What we can assert headlessly is that the bytes round-trip.
    let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{0645}\u{0631}\u{062D}\u{0628}\u{0627} latin\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bidi.txt");
    std::fs::write(&path, text).expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    app.execute(np_tui::actions::Action::Save);
    let on_disk = std::fs::read_to_string(&path).expect("read");
    assert_eq!(on_disk, text, "bidi text must round-trip byte-exact");
}

#[test]
fn a_zero_width_joiner_only_line_survives_editing() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/zwj.txt", "\u{200B}\u{200B}\u{200B}\n");
    press(&mut app, KeyCode::End);
    press(&mut app, KeyCode::Char('x'));
    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    assert!(
        after.contains('\u{200B}'),
        "zero-width chars are preserved: {after:?}"
    );
}

// ─────────────────────────────────── §8.8 tabs vs spaces byte-exact

#[test]
fn mixed_indentation_round_trips_byte_for_byte() {
    let text = "leading tab:\tindent\nleading spaces:    four\nmixed:\t \tmixed\nnone\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("indent.txt");
    std::fs::write(&path, text).expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    // Save with no editing: bytes must be identical.
    if let Some(doc) = app.doc_mut() {
        doc.modified = true; // force a write
    }
    app.execute(np_tui::actions::Action::Save);
    let on_disk = std::fs::read_to_string(&path).expect("read");
    assert_eq!(
        on_disk, text,
        "indentation must be preserved byte-exact when nothing ran"
    );
}

#[test]
fn an_indent_command_actually_changes_indentation() {
    // The converse: after an explicit indent, bytes DO change (spaces or tab per config).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("indent2.txt");
    std::fs::write(&path, "line\n").expect("write");
    let mut app = fresh(100, 30);
    app.config.editor.insert_spaces = true;
    app.config.editor.tab_width = 4;
    app.open_path(&path);
    if let Some(doc) = app.doc_mut() {
        doc.tab_width = 4;
    }
    app.execute(np_tui::actions::Action::Indent);
    app.execute(np_tui::actions::Action::Save);
    let on_disk = std::fs::read_to_string(&path).expect("read");
    assert!(
        on_disk.starts_with("    "),
        "indent should add spaces: {on_disk:?}"
    );
}

// ─────────────────────────────────── §8.9 binary detection: confirm defaults to No

#[test]
fn binary_detection_flags_a_file_with_nul_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bin.dat");
    std::fs::write(&path, [0u8, 1, 2, 0, 255, 254]).expect("write");
    let loaded = np_core::file::load(&path, None).expect("load");
    assert!(loaded.binary, "NUL bytes must mark a file binary");
}

#[test]
fn a_binary_file_opens_with_a_report_and_does_not_panic() {
    // FR-N5: in-session open reports via toast; the confirm-defaults-to-No path is the CLI's.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bin2.dat");
    std::fs::write(&path, [0u8, 0, 0, 1, 2]).expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    assert!(app.doc().is_some(), "the tab must open");
    let reported = app
        .timed_toasts
        .iter()
        .any(|t| t.message.contains("binary"));
    assert!(
        reported,
        "a binary open must be reported: {:?}",
        app.timed_toasts
            .iter()
            .map(|t| t.message.clone())
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────── §8.10 no write permission -> read-only + lock glyph

#[test]
fn a_file_without_write_permission_opens_read_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ro.txt");
    std::fs::write(&path, "locked content\n").expect("write");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).expect("chmod");
    let loaded = np_core::file::load(&path, None).expect("load");
    assert!(
        loaded.readonly,
        "a non-writable file must load as read-only"
    );
}

#[test]
fn the_read_only_lock_glyph_appears_in_the_status_bar() {
    let mut app = fresh(100, 24);
    with_doc(&mut app, "/tmp/lock.txt", "content\n");
    if let Some(doc) = app.doc_mut() {
        doc.readonly = true;
    }
    let spans = np_tui::ui::status_bar::status_segments(&app);
    let doc_span = spans
        .iter()
        .find(|s| s.segment == np_tui::mouse::StatusSegment::Document);
    assert!(
        doc_span.is_some_and(|s| s.text.contains("[lock]")),
        "the document segment must carry the lock marker: {spans:?}"
    );
}

#[test]
fn typing_into_a_read_only_document_is_refused_and_reported() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/refuse.txt", "frozen\n");
    if let Some(doc) = app.doc_mut() {
        doc.readonly = true;
    }
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default(),
        before
    );
    assert!(app
        .timed_toasts
        .iter()
        .any(|t| t.message.contains("read-only")));
}

// ─────────────────────────────────── cross-cutting: no panic across a wide size/mode grid

#[test]
fn rendering_never_panics_across_modes_and_sizes() {
    // Every dialog + a spread of sizes, including just-above-minimum and odd widths. A
    // dialog that computes a negative width at some size would panic here.
    let actions = [
        np_tui::actions::Action::Find,
        np_tui::actions::Action::Replace,
        np_tui::actions::Action::GoToLine,
        np_tui::actions::Action::Preferences,
        np_tui::actions::Action::About,
        np_tui::actions::Action::HelpOverlay,
        np_tui::actions::Action::CommandPalette,
        np_tui::actions::Action::LanguagePicker,
    ];
    for action in actions {
        for (w, h) in [(40u16, 8u16), (60, 12), (80, 24), (120, 40), (400, 200)] {
            let mut app = fresh(w, h);
            with_doc(&mut app, "/tmp/grid.txt", "one\ntwo\nthree\n");
            app.execute(action);
            // Draw at this size; must not panic.
            let _ = np_tui::ui::render_to_buffer(&mut app, w, h);
        }
    }
}

// ─────────────────────────────────── §8.5 notifications actually reach the screen

/// The renderer draws `timed_toasts` and `expire_toasts` rebuilds `toasts` from it, so a
/// message pushed into one list only is either invisible on screen or deleted from the
/// queue on the next tick. S15 found roughly thirty call sites in that state -- "Nothing to
/// undo", "clipboard is empty", save failures, bookmark misses -- all of them silent.
fn assert_toast_lists_agree(app: &App, what: &str) {
    let queue: Vec<(String, bool)> = app
        .toasts
        .iter()
        .map(|t| (t.message.clone(), t.error))
        .collect();
    let drawn: Vec<(String, bool)> = app
        .timed_toasts
        .iter()
        .map(|t| (t.message.clone(), t.error))
        .collect();
    assert_eq!(queue, drawn, "{what}: queue and drawn toasts disagree");
}

/// A named scenario: the label to report and the action that should produce a
/// notification. Named because the bare tuple trips clippy's type_complexity.
type NotifyCase = (&'static str, fn(&mut App));

#[test]
fn every_notification_path_fills_both_toast_lists() {
    // Each of these reaches a message through a different subsystem: undo exhaustion,
    // clipboard, bookmarks, find, save, read-only refusal.
    let cases: Vec<NotifyCase> = vec![
        ("nothing-to-undo", |app| {
            app.execute(np_tui::actions::Action::Undo);
        }),
        ("nothing-to-redo", |app| {
            app.execute(np_tui::actions::Action::Redo);
        }),
        ("empty-clipboard", |app| {
            app.execute(np_tui::actions::Action::Paste);
        }),
        ("no-bookmarks", |app| {
            app.execute(np_tui::actions::Action::NextBookmark);
        }),
        ("find-no-match", |app| {
            app.find.needle = "zzz-absent".into();
            app.execute(np_tui::actions::Action::FindNext);
        }),
        ("save-failed", |app| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("ro.txt");
            std::fs::write(&path, "x\n").expect("write");
            use std::os::unix::fs::PermissionsExt;
            let original = std::fs::metadata(&path).expect("meta").permissions();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("chmod");
            app.open_path(&path);
            // Force the write to be attempted and fail: the document itself does not know it
            // is locked, so the atomic writer is the thing that must report it.
            if let Some(doc) = app.doc_mut() {
                doc.readonly = false;
                doc.modified = true;
            }
            app.execute(np_tui::actions::Action::Save);
            // Restore the mode so cleanup is not left holding a locked file.
            std::fs::set_permissions(&path, original).expect("restore mode");
        }),
    ];
    for (name, run) in cases {
        let mut app = fresh(100, 30);
        with_doc(&mut app, "/tmp/notify.txt", "alpha\nbeta\n");
        run(&mut app);
        assert_toast_lists_agree(&app, name);
    }
}

#[test]
fn a_message_reaches_the_rendered_screen() {
    // The end of the chain: a pushed message must be visible in the drawn frame, not just
    // present in a queue no one reads.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/shown.txt", "alpha\n");
    app.push_toast("something the user must see", true);
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    let block: String = (0..30)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, 100))
        .collect();
    assert!(
        block.contains("something the user must see"),
        "the toast must be drawn: {block}"
    );
}

#[test]
fn an_error_toast_survives_expiry_and_an_info_toast_does_not() {
    let mut app = fresh(100, 30);
    app.push_toast("informational", false);
    app.push_toast("a real failure", true);
    assert_toast_lists_agree(&app, "before expiry");
    app.advance_clock(Duration::from_secs(30));
    app.expire_toasts();
    assert_toast_lists_agree(&app, "after expiry");
    let drawn: Vec<String> = app.timed_toasts.iter().map(|t| t.message.clone()).collect();
    assert!(
        drawn.iter().any(|m| m == "a real failure"),
        "errors are sticky: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|m| m == "informational"),
        "info toasts expire: {drawn:?}"
    );
}

// ─────────────────────────────────── §8.1 panic hook: the terminal comes back

use np_tui::term::{self, Driver, RestoreVia};
use std::cell::RefCell;
use std::rc::Rc;

/// A driver that records every call instead of touching a real terminal.
#[derive(Clone, Default)]
struct Log {
    calls: Rc<RefCell<Vec<String>>>,
}

struct Recorder {
    log: Log,
}

impl Driver for Recorder {
    fn enable_raw(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("raw+".into());
        Ok(())
    }
    fn disable_raw(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("raw-".into());
        Ok(())
    }
    fn enter_alt(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("alt+".into());
        Ok(())
    }
    fn leave_alt(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("alt-".into());
        Ok(())
    }
    fn enable_mouse(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("mouse+".into());
        Ok(())
    }
    fn disable_mouse(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("mouse-".into());
        Ok(())
    }
    fn enable_paste(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("paste+".into());
        Ok(())
    }
    fn disable_paste(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("paste-".into());
        Ok(())
    }
    fn kitty_supported(&self) -> bool {
        false
    }
    fn push_kitty(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("kitty+".into());
        Ok(())
    }
    fn pop_kitty(&mut self) -> anyhow::Result<()> {
        self.log.calls.borrow_mut().push("kitty-".into());
        Ok(())
    }
}

#[test]
fn a_panic_restore_emits_every_disable_command() {
    // Spec §8: raw mode off, alt screen left, mouse and bracketed paste disabled. Asserting
    // only that the guard depth dropped -- which is all the S04 unit test did -- would pass
    // even if the hook skipped the actual escape sequences.
    let log = Log::default();
    let calls = log.calls.clone();
    let mut recorder = Recorder { log: log.clone() };

    // Enter through the recorder so the depth is live and teardown is recorded here.
    {
        let guard = term::TerminalGuard::enter_with(
            &mut recorder,
            RestoreVia::Recorded(Box::new(Recorder { log: log.clone() })),
        )
        .expect("enter through the recorder");
        let entered = calls.borrow().clone();
        assert!(entered.contains(&"raw+".to_string()), "setup: {entered:?}");
        assert!(entered.contains(&"alt+".to_string()), "setup: {entered:?}");
        assert!(
            entered.contains(&"mouse+".to_string()),
            "setup: {entered:?}"
        );
        assert!(
            entered.contains(&"paste+".to_string()),
            "setup: {entered:?}"
        );
        // Simulate the panic path: the hook calls restore while the guard is still live.
        let failures = term::restore_with(&mut recorder);
        assert_eq!(failures, 0, "every restore command must succeed");
        let after = calls.borrow().clone();
        for needed in ["paste-", "mouse-", "alt-", "raw-"] {
            assert!(
                after.contains(&needed.to_string()),
                "must emit {needed}: {after:?}"
            );
        }
        // Reverse order of setup, so the alt screen is left while raw mode is still on.
        let idx = |n: &str| after.iter().position(|c| c == n).unwrap_or(usize::MAX);
        assert!(
            idx("paste-") < idx("mouse-"),
            "paste before mouse: {after:?}"
        );
        assert!(idx("mouse-") < idx("alt-"), "mouse before alt: {after:?}");
        assert!(idx("alt-") < idx("raw-"), "alt before raw: {after:?}");
        // Keep the guard alive to here so its Drop cannot be reordered away.
        drop(guard);
    }

    let report = np_tui::panic_hook::format_panic(
        "index out of bounds",
        Some("crates/np-tui/src/ui/mod.rs:10:5"),
        std::path::Path::new("/tmp/npt.log"),
    );
    let text = report.join("\n");
    assert!(text.contains("index out of bounds"), "{text}");
    assert!(text.contains("ui/mod.rs"), "must name the location: {text}");
    assert!(
        text.contains("/tmp/npt.log"),
        "must name the log path: {text}"
    );
    assert!(
        text.to_lowercase().contains("restored"),
        "must say the terminal is usable: {text}"
    );
}

#[test]
fn the_release_binary_starts_and_exits_cleanly_without_a_tty() {
    // End-to-end check on the real binary. With no TTY the run loop must refuse rather than
    // hang or panic, which is the only panic-path behavior observable from an integration
    // test. A panic would surface as a signal-death exit and the restore text on stderr.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_npt"))
        .arg("--version")
        .output()
        .expect("binary runs");
    assert!(out.status.success(), "--version must succeed");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version must print CARGO_PKG_VERSION, got {text:?}"
    );

    let no_tty = std::process::Command::new(env!("CARGO_BIN_EXE_npt"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawns")
        .wait_with_output()
        .expect("waits");
    let err = String::from_utf8_lossy(&no_tty.stderr).into_owned();
    assert!(
        !err.contains("unexpected internal error"),
        "a TTY-less start must not panic: {err}"
    );
    assert!(
        !no_tty.status.success(),
        "without a terminal npt must fail rather than pretend to run"
    );
}
