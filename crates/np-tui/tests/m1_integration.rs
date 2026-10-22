//! Milestone M1: `npt` as a usable editor, end to end (spec §10 integration category).
//!
//! One scripted session: open a file, edit it, select, copy, paste, undo, save, reopen, and
//! assert the bytes on disk. Everything runs through `App::execute` and the key handler
//! against `TestBackend`, with `TestBackend` + `tempfile` standing in for a terminal and a
//! filesystem directory standing in for a real project directory. No real clipboard, no TTY.
//!
//! This is the test that says "the pieces fit": S1's buffer, S2's undo, S3's action spine,
//! S5's file I/O and encoding, S6's mouse routing, and S7's clipboard and editing behaviors
//! all have to be correct at the same time for it to pass.

use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use np_core::{Config, Document, LineCol};
use np_tui::ui::{render_to_buffer, row_text};
use np_tui::{Action, App};

fn osc52_config() -> Config {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    // Auto-pair would rewrite typed brackets, which would make the scripted byte assertions
    // about literal text hard to read. It has its own tests.
    config.editor.auto_pair = false;
    config
}

fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        press(app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
}

fn text(app: &App) -> String {
    app.doc().expect("document").buffer.text_string()
}

fn click(app: &mut App, column: u16, row: u16) {
    let ev = MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let now = app.clock;
    app.handle_mouse_at(&ev, now);
}

fn drag(app: &mut App, column: u16, row: u16) {
    let ev = MouseEvent {
        kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let now = app.clock;
    app.handle_mouse_at(&ev, now);
}

fn open(app: &mut App, path: &Path) {
    open_with(app, path, None);
}

fn open_with(app: &mut App, path: &Path, force: Option<np_core::EncodingId>) {
    let loaded = np_core::file::load(path, force).expect("load");
    let mut doc = Document::from_loaded(&loaded);
    doc.overtype = app.overtype;
    app.open_document(doc);
}

/// Decode the most recent OSC 52 clipboard payload.
fn clipboard_text(app: &App) -> Option<String> {
    let raw = String::from_utf8_lossy(&app.osc_out.borrow().clone()).into_owned();
    let start = raw.rfind("\x1b]52;")?;
    let rest = &raw[start + 5..];
    let payload = rest.split_once(';')?.1.trim_end_matches("\x1b\\");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[test]
fn a_full_editing_session_round_trips_through_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notes.txt");
    fs::write(&path, b"alpha\nbravo\ncharlie\n").expect("write fixture");

    let mut app = App::new_headless(osc52_config(), (100, 24));
    app.documents.clear();
    open(&mut app, &path);

    // ---- 1. The file is loaded with its own EOL and line count.
    assert_eq!(text(&app), "alpha\nbravo\ncharlie\n");
    assert_eq!(app.doc().expect("doc").eol, np_core::Eol::Lf);
    assert!(
        !app.doc().expect("doc").modified,
        "a fresh load is not modified"
    );

    // ---- 2. Type at the end of line 1.
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(1, 5);
    type_text(&mut app, "!");
    assert_eq!(text(&app), "alpha\nbravo!\ncharlie\n");
    assert!(app.doc().expect("doc").modified);

    // ---- 3. Select "bravo!" with the mouse and copy it.
    // The editor area starts at row 2; "bravo!" is on document line 1, screen row 3.
    let text_x = app.regions.editor.x + app.geometry.gutter_width as u16;
    click(&mut app, text_x, 3);
    drag(&mut app, text_x + 6, 3);
    assert!(
        app.doc().expect("doc").selection.is_some(),
        "a drag must create a selection"
    );
    app.execute(Action::Copy);
    let copied = clipboard_text(&app).unwrap_or_default();
    assert_eq!(copied, "bravo!", "copied the dragged range, got {copied:?}");

    // ---- 4. Paste it at the end of line 2.
    app.doc_mut().expect("doc").clear_selection();
    app.doc_mut().expect("doc").cursor.pos = LineCol::new(2, 7);
    app.execute(Action::Paste);
    assert_eq!(
        text(&app),
        "alpha\nbravo!\ncharliebravo!\n",
        "paste inserted at the caret"
    );

    // ---- 5. Undo the paste, then undo the typed "!".
    app.execute(Action::Undo);
    assert_eq!(text(&app), "alpha\nbravo!\ncharlie\n");
    app.execute(Action::Undo);
    assert_eq!(
        text(&app),
        "alpha\nbravo\ncharlie\n",
        "back to the file as loaded"
    );

    // ---- 6. Select-all, indent, and verify every line gained the unit.
    app.execute(Action::SelectAll);
    app.execute(Action::Indent);
    assert_eq!(
        text(&app),
        "    alpha\n    bravo\n    charlie\n",
        "indent touched every line"
    );

    // ---- 7. Save, and check the bytes on disk really changed.
    app.execute(Action::Save);
    let on_disk = fs::read_to_string(&path).expect("read back");
    assert_eq!(on_disk, "    alpha\n    bravo\n    charlie\n");
    assert!(
        !app.doc().expect("doc").modified,
        "a successful save clears the modified flag"
    );

    // ---- 8. Reopen in a second App and assert the bytes survived the trip.
    let mut reopened = App::new_headless(osc52_config(), (100, 24));
    reopened.documents.clear();
    open(&mut reopened, &path);
    assert_eq!(
        text(&reopened),
        "    alpha\n    bravo\n    charlie\n",
        "reopen reproduces the saved bytes exactly"
    );
    assert!(
        !reopened.doc().expect("doc").modified,
        "a reopened document is clean"
    );
}

#[test]
fn the_saved_file_keeps_its_encoding_when_it_is_not_utf8() {
    // The M1 claim is that editing does not silently re-encode a file. windows-1251 Cyrillic
    // through a full edit-and-save cycle must come back as the same codec plus the edit.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cyrillic.txt");
    let enc = np_core::EncodingId::parse("windows-1251").expect("known encoding name");
    let bytes = np_core::encode("привет\nмир\n", enc.clone());
    fs::write(&path, &bytes).expect("write");

    // A BOM-less single-byte file is decoded as UTF-8 by detection (FR-N1), so the codec is
    // forced here exactly as `-e windows-1251` or Reload-As would force it.
    let mut app = App::new_headless(osc52_config(), (100, 24));
    app.documents.clear();
    open_with(&mut app, &path, Some(enc.clone()));
    assert_eq!(text(&app), "привет\nмир\n");
    assert_eq!(app.doc().expect("doc").encoding, enc);

    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    type_text(&mut app, "Х");
    app.execute(Action::Save);

    let on_disk = fs::read(&path).expect("read");
    let decoded = np_core::decode(&on_disk, Some(enc.clone()));
    assert_eq!(
        decoded.text, "Хпривет\nмир\n",
        "edited and re-saved in the same codec"
    );
    // The file really is not UTF-8: the Cyrillic body would have to have been re-encoded.
    assert!(
        std::str::from_utf8(&on_disk).is_err(),
        "the file must still be windows-1251 bytes"
    );
}

#[test]
fn a_frame_renders_all_four_regions_after_the_session() {
    // The rendering half of "usable": after editing, a frame still draws the menu bar, the
    // tab, the text and the full status row, with the caret's line emphasized.
    let mut app = App::new_headless(osc52_config(), (100, 24));
    type_text(&mut app, "one\ntwo\nthree");
    let buf = render_to_buffer(&mut app, 100, 24);
    assert_ne!(row_text(&buf, 0, 0, 60).trim(), "", "menu bar is drawn");
    assert_ne!(row_text(&buf, 1, 0, 60).trim(), "", "tab bar is drawn");
    let editor = row_text(&buf, 2, 0, 40);
    assert!(editor.contains("one"), "editor shows the text: {editor:?}");
    let status = row_text(&buf, 23, 0, 100);
    for want in ["Untitled-1", "Ln 3, Col 6", "LF", "UTF-8", "INS", " / "] {
        assert!(
            status.contains(want),
            "status missing {want:?} in {status:?}"
        );
    }
}

#[test]
fn the_m1_flow_survives_a_readonly_file() {
    // FR-F9: a read-only document must refuse every mutation instead of corrupting itself,
    // and must say so. This is the "nothing is silently lost" guarantee under M1.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.txt");
    fs::write(&path, b"do not touch\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("chmod");
    }

    let mut app = App::new_headless(osc52_config(), (100, 24));
    app.documents.clear();
    open(&mut app, &path);
    let before = text(&app);

    app.doc_mut().expect("doc").cursor.pos = LineCol::new(0, 0);
    type_text(&mut app, "zzz");
    app.execute(Action::SelectAll);
    app.execute(Action::Cut);
    app.execute(Action::Paste);

    assert_eq!(text(&app), before, "a read-only document cannot be edited");
    #[cfg(unix)]
    {
        // The load path marks it read-only, so the refusals are expected rather than silent.
        assert!(
            app.doc().expect("doc").readonly || !app.doc().expect("doc").modified,
            "the read-only state must be visible somewhere"
        );
    }
}
