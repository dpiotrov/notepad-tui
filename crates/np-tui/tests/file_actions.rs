//! S5: file actions at the App layer (FR-F1..F4, F6..F10, FR-N1..N5).
//!
//! These drive `App::execute` rather than `Document` directly, so they cover what exists
//! only in the UI layer: which actions park a pending dialog, what the user is told, and
//! whether config-driven save options actually reach the disk.

use std::fs;
use std::path::{Path, PathBuf};

use np_core::{Buffer, Config, Document, EncodingId, Eol, EolMode, UndoGroup};
use np_tui::{Action, App, PendingConfirm};

fn app() -> App {
    App::new_headless(Config::default(), (80, 24))
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write fixture");
    path
}

/// Set the permission bits on a path, so a test can create a genuinely unwritable file.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}

/// The mode bits, masked to the low 9.
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

/// Open a file as a tab the way the CLI does, so tests use the same load path.
fn open(app: &mut App, path: &Path) {
    let loaded = np_core::file::load(path, None).expect("load fixture");
    let mut doc = Document::from_loaded(&loaded);
    doc.overtype = app.overtype;
    app.open_document(doc);
}

/// Insert through the App's own clock, which is what `execute` uses.
fn type_at(app: &mut App, text: &str) {
    let now = app.clock;
    let doc = app.doc_mut().expect("document");
    let _ = doc.insert_text(text, UndoGroup::Atomic, now);
}

fn toasts(app: &App) -> Vec<String> {
    app.toasts.iter().map(|t| t.message.clone()).collect()
}

fn errors(app: &App) -> Vec<String> {
    app.toasts
        .iter()
        .filter(|t| t.error)
        .map(|t| t.message.clone())
        .collect()
}

fn text(app: &App) -> String {
    app.doc().expect("document").buffer.text_string()
}

fn untitled(app: &mut App) {
    app.execute(Action::NewFile);
}

// ------------------------------------------------------------------ save

#[test]
fn save_on_an_untitled_document_asks_for_a_path_instead_of_failing() {
    let mut app = app();
    untitled(&mut app);
    type_at(&mut app, "draft");
    assert!(app.active_is_modified());

    app.execute(Action::Save);

    // No path exists, so the UI owes the user a Save As dialog, not an error.
    assert_eq!(app.pending_save_as, Some(app.active));
    assert!(
        app.toasts.is_empty(),
        "a missing path is a dialog, not an error: {:?}",
        toasts(&app)
    );
    assert!(app.active_is_modified(), "nothing was saved, still dirty");
}

#[test]
fn save_writes_the_file_and_clears_the_modified_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "a.txt", b"hello\n");

    let mut app = app();
    open(&mut app, &path);
    assert!(!app.active_is_modified());

    {
        let doc = app.doc_mut().expect("document");
        doc.set_cursor(np_core::LineCol::new(0, 5), false);
    }
    type_at(&mut app, " there");
    assert!(app.active_is_modified());

    app.execute(Action::Save);

    assert_eq!(fs::read(&path).expect("read back"), b"hello there\n");
    assert!(!app.active_is_modified());
    assert_eq!(app.pending_save_as, None);
    assert!(!app.doc().expect("document").changed_on_disk());
}

#[test]
fn a_successful_save_clears_the_external_change_banner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "banner.txt", b"mine\n");

    let mut app = app();
    open(&mut app, &path);
    fs::write(&path, b"theirs\n").expect("external write");
    app.check_external_changes();
    assert!(app.banner.is_some());

    app.execute(Action::ReloadFile);
    assert_eq!(text(&app), "theirs\n");

    type_at(&mut app, "x");
    app.execute(Action::Save);
    assert!(app.banner.is_none(), "a save means disk matches again");
}

#[test]
fn a_failed_save_keeps_the_document_modified_and_intact() {
    // Saving over an existing directory fails at the rename. This is the failure mode that
    // actually holds here: `set_readonly(true)` on a file we own does not, because an atomic
    // save replaces the inode rather than writing through it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a-directory");
    fs::create_dir(&path).expect("mkdir");

    let mut app = app();
    untitled(&mut app);
    // Point the document at the directory directly; a save is what must fail.
    app.doc_mut().expect("document").path = Some(path.clone());
    type_at(&mut app, "edited");

    app.execute(Action::Save);

    assert!(
        app.active_is_modified(),
        "a failed save must not clear the flag"
    );
    assert!(!errors(&app).is_empty(), "the failure must be visible");
    assert!(text(&app).contains("edited"), "the edit must survive");
    assert!(path.is_dir(), "the directory must be untouched");
}

#[test]
fn save_as_moves_the_document_and_clears_pending_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("out.txt");

    let mut app = app();
    untitled(&mut app);
    type_at(&mut app, "content\n");

    assert!(app.save_active_as(&target));

    assert_eq!(fs::read(&target).expect("read"), b"content\n");
    assert_eq!(
        mode_of(&target) & 0o022,
        0,
        "a new file is not group/world writable"
    );
    assert_eq!(
        app.doc().expect("document").path.as_deref(),
        Some(target.as_path())
    );
    assert!(!app.active_is_modified());
    assert_eq!(app.pending_save_as, None);

    // A later plain Save goes to the new path with no dialog. The caret sits on the empty line
    // that "content\n" opened, so this appends a second line.
    type_at(&mut app, "more");
    app.execute(Action::Save);
    assert_eq!(fs::read(&target).expect("read"), b"content\nmore");
    assert_eq!(app.pending_save_as, None);
}

#[test]
fn save_as_as_keeps_the_path_when_the_write_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_dir = dir.path().join("nope");
    let target = missing_dir.join("out.txt");

    let mut app = app();
    untitled(&mut app);
    type_at(&mut app, "content");

    assert!(
        !app.save_active_as(&target),
        "cannot write into a missing dir"
    );
    assert_eq!(
        app.doc().expect("document").path,
        None,
        "a failed Save As must not claim a path it did not write"
    );
    assert!(app.active_is_modified());
    assert!(
        app.pending_save_as.is_none() || app.pending_save_as == Some(app.active),
        "the dialog state must stay coherent: {:?}",
        app.pending_save_as
    );
    assert!(!errors(&app).is_empty());
}

#[test]
fn backup_on_save_leaves_a_tilde_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "bak.txt", b"first\n");

    let mut config = Config::default();
    config.editor.backup_on_save = true;
    let mut app = App::new_headless(config, (80, 24));
    open(&mut app, &path);
    {
        let doc = app.doc_mut().expect("document");
        doc.set_cursor(np_core::LineCol::new(1, 0), false);
    }
    type_at(&mut app, "second\n");

    app.execute(Action::Save);

    let backup = dir.path().join("bak.txt~");
    assert!(backup.exists(), "expected a ~ backup next to the file");
    assert_eq!(fs::read(&backup).expect("backup"), b"first\n");
    assert_eq!(fs::read(&path).expect("file"), b"first\nsecond\n");
}

// ---------------------------------------------------------- external change

#[test]
fn an_external_write_raises_a_banner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "ext.txt", b"mine\n");

    let mut app = app();
    open(&mut app, &path);
    app.check_external_changes();
    assert!(app.banner.is_none(), "nothing changed yet");

    fs::write(&path, b"theirs\n").expect("external write");
    app.check_external_changes();

    let banner = app.banner.clone().expect("banner set");
    assert!(
        banner.message.contains("ext.txt") && banner.message.contains("changed on disk"),
        "unexpected banner: {banner:?}",
    );
}

#[test]
fn unsaved_edits_alone_are_not_an_external_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "quiet.txt", b"a\n");

    let mut app = app();
    open(&mut app, &path);
    type_at(&mut app, "local edit");

    app.check_external_changes();
    assert!(
        app.banner.is_none(),
        "in-memory edits must not raise a banner"
    );
}

#[test]
fn a_deleted_file_raises_its_own_banner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "gone.txt", b"bye\n");

    let mut app = app();
    open(&mut app, &path);
    fs::remove_file(&path).expect("remove");

    app.check_external_changes();

    let banner = app.banner.clone().expect("banner set");
    assert!(banner.message.contains("deleted"), "{banner:?}");
}

// ------------------------------------------------------------------ reload

#[test]
fn reload_brings_the_disk_content_back_and_drops_undo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "rel.txt", b"disk\n");

    let mut app = app();
    open(&mut app, &path);
    type_at(&mut app, "mine");
    assert!(app.active_is_modified());
    assert!(app.doc().expect("document").undo.can_undo());

    app.execute(Action::ReloadFile);

    let doc = app.doc().expect("document");
    assert_eq!(doc.buffer.text_string(), "disk\n");
    assert!(!doc.modified);
    assert!(!doc.undo.can_undo(), "reload drops undo (documented)");
}

#[test]
fn reload_of_a_missing_file_leaves_the_buffer_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "vanish.txt", b"was here\n");

    let mut app = app();
    open(&mut app, &path);
    fs::remove_file(&path).expect("remove");

    app.execute(Action::ReloadFile);

    assert_eq!(text(&app), "was here\n", "a failed reload loses nothing");
    assert!(!errors(&app).is_empty());
}

#[test]
fn reload_on_an_untitled_document_says_so_without_an_error() {
    let mut app = app();
    untitled(&mut app);

    app.execute(Action::ReloadFile);

    assert!(errors(&app).is_empty());
    assert!(
        toasts(&app).iter().any(|t| t.contains("no file")),
        "{:?}",
        toasts(&app)
    );
}

// -------------------------------------------------------------- EOL / codec

#[test]
fn set_eol_converts_every_line_and_reaches_the_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "eol.txt", b"one\ntwo\nthree\n");

    let mut app = app();
    open(&mut app, &path);
    assert_eq!(app.doc().expect("document").eol, Eol::Lf);

    app.execute(Action::SetEol(Eol::Crlf));

    let doc = app.doc().expect("document");
    assert_eq!(doc.eol, Eol::Crlf);
    assert!(doc.modified, "a conversion is an edit");
    assert!(doc.buffer.text_string().contains("one\ntwo\nthree"));

    app.execute(Action::Save);
    assert_eq!(
        fs::read(&path).expect("read"),
        b"one\r\ntwo\r\nthree\r\n",
        "the document's ending wins over the config default of `file`"
    );
}

#[test]
fn convert_encoding_cycles_and_leaves_the_text_alone() {
    let mut app = app();
    untitled(&mut app);
    type_at(&mut app, "caf\u{e9}\n");

    // Anchor on a known entry so the walk below is about the menu order, not on whatever the
    // configured default encoding happens to be.
    app.set_active_encoding(EncodingId::Utf8);
    let start = EncodingId::Utf8;

    app.execute(Action::ConvertEncoding);

    let doc = app.doc().expect("document");
    assert_ne!(doc.encoding, start, "the encoding must change");
    assert_eq!(
        doc.buffer.text_string(),
        "caf\u{e9}\n",
        "conversion is in-memory"
    );
    assert!(doc.modified, "a conversion is an edit");

    // A cycle step lands on the entry after the current one in menu order, and a full walk
    // visits every entry and wraps back to the start.
    let all = EncodingId::all();
    // `start` is all[0] and one step was already taken above, so the remaining visits are
    // all[2] .. all[0].
    let mut expected = all.iter().skip(2).cloned().collect::<Vec<_>>();
    expected.push(all[0].clone());
    expected.push(all[1].clone());
    let mut seen = Vec::new();
    for _ in 0..expected.len() {
        app.execute(Action::ConvertEncoding);
        seen.push(app.doc().expect("document").encoding.clone());
    }
    assert_eq!(seen, expected, "cycles through the menu in order and wraps");
}

#[test]
fn reload_as_encoding_records_the_choice_and_redecodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "w1252.txt", b"caf\xe9 plain\n");

    let mut app = app();
    open(&mut app, &path);

    app.reload_active_as(EncodingId::Label("windows-1252"));

    assert_eq!(
        app.pending_reload_encoding,
        Some(EncodingId::Label("windows-1252"))
    );
    let doc = app.doc().expect("document");
    assert_eq!(doc.encoding, EncodingId::Label("windows-1252"));
    assert!(
        doc.buffer.text_string().contains("caf\u{e9} plain"),
        "{:?}",
        doc.buffer.text_string()
    );
    assert!(!doc.modified, "a reload is not an edit");
}

#[test]
fn reload_as_a_wrong_codec_then_the_right_one_restores_the_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("u16.txt");
    let body = "alpha\nbeta\n";
    let mut bytes: Vec<u8> = Vec::new();
    for unit in body.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&path, &bytes).expect("write utf16");

    let mut app = app();
    open(&mut app, &path);
    // FR-N1: detection is BOM-then-UTF-8. UTF-16 without a BOM is read as UTF-8 on purpose,
    // and FR-N3's answer is that the user reloads it explicitly -- which is what follows.
    assert_eq!(app.doc().expect("document").encoding, EncodingId::Utf8);

    app.reload_active_as(EncodingId::Utf16Le);
    assert_eq!(text(&app), body);

    app.reload_active_as(EncodingId::Label("windows-1252"));
    assert!(
        text(&app).contains('\0'),
        "a wrong forced codec exposes the raw bytes"
    );

    app.reload_active_as(EncodingId::Utf16Le);
    assert_eq!(text(&app), body);
}

#[test]
fn reload_as_encoding_on_an_untitled_document_is_harmless() {
    let mut app = app();
    untitled(&mut app);

    app.reload_active_as(EncodingId::Utf16Be);

    assert_eq!(
        app.pending_reload_encoding,
        Some(EncodingId::Utf16Be),
        "the intent is still recorded for the dialog"
    );
    assert_eq!(text(&app), "");
    assert!(errors(&app).is_empty() || toasts(&app).iter().any(|t| t.contains("no file")));
}

#[test]
fn reload_as_encoding_opens_the_picker_via_the_action() {
    use np_tui::Mode;
    let mut app = app();
    untitled(&mut app);

    app.execute(Action::ReloadAsEncoding);

    assert!(
        matches!(app.mode(), Mode::Dialog(_)),
        "the picker mode must be on the stack: {:?}",
        app.mode()
    );
}

// -------------------------------------------------------------- read-only

#[test]
fn toggle_readonly_blocks_editing_and_reports_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "ro.txt", b"locked\n");

    let mut app = app();
    open(&mut app, &path);
    assert!(!app.doc().expect("document").readonly);

    app.execute(Action::ToggleReadonly);
    assert!(app.doc().expect("document").readonly);
    assert!(
        toasts(&app).iter().any(|t| t.contains("read-only on")),
        "{:?}",
        toasts(&app)
    );

    // A read-only buffer refuses the edit rather than silently accepting it.
    type_at(&mut app, "x");
    assert_eq!(text(&app), "locked\n");

    app.execute(Action::ToggleReadonly);
    assert!(!app.doc().expect("document").readonly);
    type_at(&mut app, "x");
    assert!(text(&app).starts_with("xlocked"));
    assert_eq!(fs::read(&path).expect("read"), b"locked\n", "still unsaved");
}

#[test]
fn a_readonly_file_opens_read_only_and_save_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "perm.txt", b"locked\n");
    set_mode(&path, 0o444);

    let mut app = app();
    open(&mut app, &path);
    assert!(app.doc().expect("document").readonly, "FR-F9");

    // Turning read-only off must be refused: the file itself is not writable.
    app.execute(Action::ToggleReadonly);
    assert!(app.doc().expect("document").readonly);
    assert!(!errors(&app).is_empty(), "the refusal must be visible");

    set_mode(&path, 0o644);
}

// ------------------------------------------------------------------ config

#[test]
fn config_save_options_reach_the_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "trim.txt", b"kept   \nnolast");

    let mut config = Config::default();
    config.editor.trim_trailing_ws_on_save = true;
    config.editor.ensure_final_newline = true;
    let mut app = App::new_headless(config, (80, 24));
    open(&mut app, &path);

    app.execute(Action::Save);

    assert_eq!(fs::read(&path).expect("read"), b"kept\nnolast\n");
}

#[test]
fn config_eol_mode_override_applies_on_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "mode.txt", b"a\nb\n");

    let mut config = Config::default();
    config.editor.eol_mode = "crlf".to_string();
    let mut app = App::new_headless(config, (80, 24));
    open(&mut app, &path);

    app.execute(Action::Save);

    assert_eq!(fs::read(&path).expect("read"), b"a\r\nb\r\n");
    // The forced mode is a save-time policy; the document keeps what it loaded.
    assert_eq!(app.doc().expect("document").eol, Eol::Lf);
}

#[test]
fn eol_mode_file_keeps_the_loaded_endings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "keep.txt", b"a\r\nb\r\n");

    let mut app = app();
    open(&mut app, &path);
    assert_eq!(app.doc().expect("document").eol, Eol::Crlf);

    app.execute(Action::Save);
    assert_eq!(fs::read(&path).expect("read"), b"a\r\nb\r\n");
}

// ------------------------------------------------------------- multi-tab

#[test]
fn modified_indices_lists_every_dirty_tab() {
    let mut app = app();
    untitled(&mut app);
    untitled(&mut app);
    untitled(&mut app);
    assert!(app.modified_indices().is_empty());

    let now = app.clock;
    app.documents
        .get_mut(0)
        .expect("tab")
        .insert_text("x", UndoGroup::Atomic, now)
        .expect("insert");
    assert_eq!(app.modified_indices(), vec![0]);

    let now = app.clock;
    app.documents
        .get_mut(2)
        .expect("tab")
        .insert_text("y", UndoGroup::Atomic, now)
        .expect("insert");
    assert_eq!(app.modified_indices(), vec![0, 2]);
}

#[test]
fn closing_a_modified_tab_records_a_confirmation() {
    let mut app = app();
    untitled(&mut app);
    type_at(&mut app, "unsaved");

    let active = app.active;
    let before = app.documents.len();
    app.execute(Action::CloseTab);

    assert_eq!(app.pending_confirm, Some(PendingConfirm::CloseTab(active)));
    assert_eq!(
        app.documents.len(),
        before,
        "must not close behind the user's back"
    );
    assert!(
        matches!(
            app.mode(),
            np_tui::Mode::Dialog(np_tui::DialogKind::Confirm)
        ),
        "the confirmation dialog must be on screen: {:?}",
        app.mode()
    );
}

// ------------------------------------------------------------- size policy

#[test]
fn open_policy_boundaries_are_exact() {
    use np_core::file::{open_policy, OpenDecision, ASK_READONLY_BYTES, BACKGROUND_BYTES};
    assert_eq!(open_policy(ASK_READONLY_BYTES - 1), OpenDecision::Open);
    assert_eq!(open_policy(ASK_READONLY_BYTES), OpenDecision::AskReadOnly);
    assert_eq!(open_policy(BACKGROUND_BYTES - 1), OpenDecision::AskReadOnly);
    assert_eq!(open_policy(BACKGROUND_BYTES), OpenDecision::Background);
}

#[test]
fn a_binary_file_loads_with_the_flag_set_and_still_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "blob.bin", b"\x00\x01\x02text");

    let loaded = np_core::file::load(&path, None).expect("load");
    assert!(loaded.binary);

    // Refusing to open is the UI's call, not the loader's.
    let mut app = app();
    open(&mut app, &path);
    assert!(text(&app).contains("text"));
}

#[test]
fn a_forced_encoding_that_is_not_a_codec_falls_back_to_detection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "fallback.txt", b"plain\n");

    // np-tui does not own this decision, but the CLI depends on both halves agreeing:
    // an unparseable name must degrade to `None`, which is detection.
    assert!(EncodingId::parse("not-a-codec").is_none());
    let loaded = np_core::file::load(&path, EncodingId::parse("not-a-codec")).expect("load");
    assert_eq!(loaded.decoded.encoding, EncodingId::Utf8);
    assert!(!loaded.decoded.lossy);
}

#[test]
fn the_buffer_and_document_agree_on_the_line_count_after_a_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Trailing terminator: ropey reports 3 lines, the loaded EOL vector 2 + implicit.
    let path = write_file(dir.path(), "count.txt", b"a\nb\n");

    let mut app = app();
    open(&mut app, &path);
    let doc = app.doc().expect("document");
    let buffer_lines = doc.buffer.line_count();
    let eols = doc.eols_for_save();
    assert!(
        eols.len() <= buffer_lines,
        "eols {eols:?} must never exceed the {buffer_lines} live lines"
    );

    let lines: Vec<String> = (0..buffer_lines)
        .map(|i| doc.buffer.line_string(i))
        .collect();
    let round = np_core::join_for_save(&lines, &eols, EolMode::File, doc.eol, false);
    assert_eq!(round, "a\nb\n");
    // Sanity: an equivalent Buffer sees the same text.
    assert_eq!(Buffer::from_str_lossy(&round).text_string(), "a\nb\n");
}
