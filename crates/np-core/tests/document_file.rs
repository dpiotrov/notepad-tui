//! S5 acceptance: `Document`'s file lifecycle, tested against real files on disk.
//!
//! `file.rs`'s own unit tests cover the byte mechanics. These cover the *document* contract:
//! that load populates every field, that a failed save cannot lose work, and that reload does
//! what it documents (including throwing away undo history).
//!
//! The acceptance item "no path exists by which a failed save loses document state" is
//! asserted here rather than in `file.rs`, because losing state would take the interaction of
//! the two, not either one alone.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use np_core::eol::EolMode;
use np_core::file::{self, SaveOptions};
use np_core::{Buffer, CoreError, Document, EncodingId, Eol, LineCol, UndoGroup};

fn t0() -> Instant {
    Instant::now()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write");
    path
}

fn opts() -> SaveOptions {
    SaveOptions::default()
}

// ------------------------------------------------------------------- loading

#[test]
fn from_loaded_populates_every_file_field() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "a.txt", b"one\r\ntwo\r\n");
    let loaded = file::load(&path, None).expect("load");
    let doc = Document::from_loaded(&loaded);

    assert_eq!(doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(doc.buffer.text_string(), "one\ntwo\n", "buffer normalizes");
    assert_eq!(doc.encoding, EncodingId::Utf8);
    assert_eq!(doc.eol, Eol::Crlf, "dominant ending is the loaded one");
    assert_eq!(doc.line_eols, vec![Eol::Crlf, Eol::Crlf]);
    assert!(!doc.modified, "a freshly loaded document is not modified");
    assert_eq!(doc.file_size, Some(10));
    assert!(
        doc.file_mtime.is_some(),
        "the stamp must be set or FR-F7 is dead"
    );
    assert_eq!(doc.cursor.pos, LineCol::new(0, 0));
}

#[test]
fn from_decoded_records_per_line_endings() {
    let doc = Document::from_decoded(
        "/x/y.txt".into(),
        "a\r\nb\nc\r",
        EncodingId::Utf8,
        Eol::Crlf,
        false,
    );
    assert_eq!(doc.line_eols, vec![Eol::Crlf, Eol::Lf, Eol::Cr]);
}

#[test]
fn load_detects_readonly_and_sets_the_flag() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ro.txt", b"x");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("chmod");
    }
    let loaded = file::load(&path, None).expect("load");
    let doc = Document::from_loaded(&loaded);
    assert!(doc.readonly, "FR-F9");
    // An edit attempt is an error, not a silent no-op.
    let err = doc
        .clone()
        .insert_text_noop_check()
        .expect_err("read-only must refuse");
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
}

/// Helper on a clone so the read-only refusal can be asserted without mutating.
trait InsertCheck {
    fn insert_text_noop_check(self) -> Result<(), CoreError>;
}
impl InsertCheck for Document {
    fn insert_text_noop_check(mut self) -> Result<(), CoreError> {
        self.insert_text("x", UndoGroup::Typing, t0()).map(|_| ())
    }
}

// -------------------------------------------------------------- stamp / FR-F7

#[test]
fn changed_on_disk_detects_an_external_edit() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ext.txt", b"original");
    let loaded = file::load(&path, None).expect("load");
    let doc = Document::from_loaded(&loaded);
    assert!(!doc.changed_on_disk(), "an untouched file has not changed");

    fs::write(&path, b"changed by someone else").expect("write");
    assert!(
        doc.changed_on_disk(),
        "FR-F7: an external write must be noticed"
    );
}

#[test]
fn changed_on_disk_detects_a_deletion() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "gone.txt", b"x");
    let doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    fs::remove_file(&path).expect("remove");
    assert!(doc.file_deleted(), "spec §8: a deleted file offers Save As");
    assert!(doc.changed_on_disk());
}

#[test]
fn an_untitled_document_is_never_changed_on_disk() {
    let doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    assert!(!doc.changed_on_disk(), "there is no file to disagree with");
    assert!(!doc.file_deleted());
}

#[test]
fn saving_clears_the_change_flag() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "s.txt", b"a\nb\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    // changed_on_disk compares the FILE against the stamp taken at load, so it reports
    // EXTERNAL changes only. Unsaved in-memory edits do not make the file differ from the
    // stamp, and must not: otherwise every open document would look externally modified.
    assert!(
        !doc.changed_on_disk(),
        "in-memory edits are not an external change"
    );
    doc.insert_text("!", UndoGroup::Typing, t0())
        .expect("insert");
    doc.save(EolMode::File, &opts()).expect("save");
    doc.save(EolMode::File, &opts()).expect("save");
    assert!(
        !doc.changed_on_disk(),
        "after saving, disk and document agree"
    );
}

// -------------------------------------------------------------------- saving

#[test]
fn save_writes_and_clears_modified() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("new.txt");
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    doc.insert_text("hello\nworld", UndoGroup::Typing, t0())
        .expect("insert");
    assert!(doc.modified);

    let info = doc.save_to(&path, EolMode::File, &opts()).expect("save");
    assert_eq!(fs::read_to_string(&path).expect("read"), "hello\nworld");
    assert!(!doc.modified, "a successful save clears the flag (FR-F3)");
    assert_eq!(doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(info.bytes_written, 11);
    assert_eq!(doc.file_size, Some(11), "the stamp is refreshed");
}

#[test]
fn untitled_save_is_an_error_not_a_silent_noop() {
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    doc.insert_text("x", UndoGroup::Typing, t0())
        .expect("insert");
    let err = doc.save(EolMode::File, &opts()).expect_err("no path");
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    // FR-F3 routes this to Save As; the flag must survive so nothing is lost.
    assert!(doc.modified);
    assert_eq!(doc.buffer.text_string(), "x");
}

#[test]
fn a_failed_save_loses_nothing() {
    // The acceptance item, stated as the property that matters.
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "locked.txt", b"disk original");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).expect("chmod");
    }

    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.insert_text(" typed work", UndoGroup::Typing, t0())
        .expect("insert");
    let text_before = doc.buffer.text_string();
    let modified_before = doc.modified;

    let result = doc.save(EolMode::File, &opts());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod back");
    }

    assert!(result.is_err(), "the save must report the failure");
    assert!(modified_before, "the document was dirty before the save");
    assert_eq!(
        doc.buffer.text_string(),
        text_before,
        "in-memory text is untouched"
    );
    assert!(
        doc.modified,
        "and the document stays modified so the user can retry"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "disk original",
        "the file on disk is untouched"
    );
    // Retrying after the cause is fixed works, with no loss.
    doc.save(EolMode::File, &opts()).expect("retry saves");
    assert_eq!(doc.buffer.text_string(), text_before);
    assert!(!doc.modified);
}

#[test]
fn read_only_document_refuses_to_save() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("ro.txt");
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    doc.readonly = true;
    let err = doc
        .save_to(&path, EolMode::File, &opts())
        .expect_err("refused");
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    assert!(!path.exists(), "nothing is written");
}

#[test]
fn save_preserves_the_files_permissions() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "p.txt", b"original");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");
    }
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.insert_text(" more", UndoGroup::Typing, t0())
        .expect("insert");
    doc.save(EolMode::File, &opts()).expect("save");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "FR-F3: the mode survives an atomic save");
    }
}

#[test]
fn saving_to_a_new_path_moves_the_document() {
    // Save As: the document adopts the new path and the old file is untouched.
    let dir = tempfile::tempdir().expect("tmpdir");
    let original = write(dir.path(), "a.txt", b"content");
    let copy = dir.path().join("b.txt");
    let mut doc = Document::from_loaded(&file::load(&original, None).expect("load"));
    doc.save_to(&copy, EolMode::File, &opts()).expect("save as");
    assert_eq!(doc.path.as_deref(), Some(copy.as_path()));
    assert_eq!(fs::read_to_string(&original).expect("read"), "content");
    assert_eq!(fs::read_to_string(&copy).expect("read"), "content");
}

// -------------------------------------------------------------------- reload

#[test]
fn reload_picks_up_external_changes() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "r.txt", b"first\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    fs::write(&path, "second\nthird\n").expect("external write");

    doc.reload(None).expect("reload");
    assert_eq!(doc.buffer.text_string(), "second\nthird\n");
    assert!(!doc.modified, "a reload is the disk state, not an edit");
    assert!(!doc.changed_on_disk());
}

#[test]
fn reload_re_detects_encoding_and_eol() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "e.txt", "plain\n".as_bytes());
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert_eq!(doc.encoding, EncodingId::Utf8);

    // Replace with UTF-16LE content.
    fs::write(
        &path,
        np_core::encoding::encode("reloaded", EncodingId::Utf16Le),
    )
    .expect("write");
    doc.reload(None).expect("reload");
    assert_eq!(
        doc.encoding,
        EncodingId::Utf16Le,
        "FR-F6: re-detect on reload"
    );
    assert_eq!(doc.buffer.text_string(), "reloaded");
}

#[test]
fn reload_keeps_the_cursor_line_but_not_the_column() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "c.txt", b"l1\nl2\nl3\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.set_cursor(LineCol::new(1, 2), false);
    fs::write(&path, "a\nb\n").expect("write");

    doc.reload(None).expect("reload");
    assert_eq!(doc.cursor.pos.line, 1, "the line is kept where possible");
    assert_eq!(
        doc.cursor.pos.col, 0,
        "the column is dropped: it referred to old text"
    );
}

#[test]
fn reload_clamps_the_cursor_into_the_new_document() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "cl.txt", b"1\n2\n3\n4\n5\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.set_cursor(LineCol::new(4, 1), false);
    fs::write(&path, "only\n").expect("write");
    doc.reload(None).expect("reload");
    // "only\n" is two rope lines (the second is the trailing empty one), so line 4 clamps to
    // line 1. The point is that it lands inside the document rather than out of range.
    assert_eq!(doc.cursor.pos.line, 1, "clamped to the last line");
    assert!(
        doc.cursor.pos.line < doc.buffer.line_count(),
        "inside the document"
    );
}

#[test]
fn reload_discards_undo_history_as_documented() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "u.txt", b"start\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.insert_text("typed\n", UndoGroup::Typing, t0())
        .expect("insert");
    assert!(!doc.undo.is_empty(), "there is history to discard");

    fs::write(&path, "from disk\n").expect("write");
    doc.reload(None).expect("reload");
    assert_eq!(
        doc.undo.len(),
        0,
        "FR-F6: undo history is discarded, because it refers to text that is gone"
    );
    assert!(!doc.undo_one(), "undo after reload must do nothing");
    assert_eq!(doc.buffer.text_string(), "from disk\n");
}

#[test]
fn a_failed_reload_leaves_the_document_intact() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "fr.txt", b"good content\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.set_cursor(LineCol::new(0, 12), false);
    doc.insert_text(" unsaved", UndoGroup::Typing, t0())
        .expect("insert");
    fs::remove_file(&path).expect("remove");

    assert!(
        doc.reload(None).is_err(),
        "a deleted file cannot be re-read"
    );
    assert_eq!(
        doc.buffer.text_string(),
        "good content unsaved\n",
        "the failed reload must not blank the document"
    );
    assert!(doc.modified);
}

#[test]
fn reload_of_an_untitled_document_is_an_error() {
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    let err = doc.reload(None).expect_err("no file");
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
}

// ------------------------------------------------------------- conversions

#[test]
fn convert_encoding_marks_modified_and_changes_the_target() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ce.txt", b"text\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert!(!doc.modified);
    doc.convert_encoding(EncodingId::Utf16Le);
    assert_eq!(doc.encoding, EncodingId::Utf16Le);
    assert!(doc.modified, "a conversion is a pending change to the file");
    doc.save(EolMode::File, &opts()).expect("save");
    let bytes = fs::read(&path).expect("read");
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xFE],
        "the conversion took effect on save"
    );
}

#[test]
fn convert_eol_rewrites_every_line_ending() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ce2.txt", b"a\r\nb\r\nc\r\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert_eq!(doc.eol, Eol::Crlf);
    doc.convert_eol(Eol::Lf);
    assert_eq!(doc.eol, Eol::Lf);
    doc.save(EolMode::File, &opts()).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc\n");
}

// ------------------------------------------------------------- full cycles

#[test]
fn write_open_edit_save_reread_round_trips() {
    // The integration sequence from the stage spec, end to end.
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("cycle.txt");
    fs::write(&path, "alpha\nbravo\ncharlie\n").expect("write");

    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert_eq!(
        doc.buffer.line_count(),
        4,
        "ropey counts the trailing empty line"
    );
    doc.set_cursor(LineCol::new(1, 5), false);
    doc.insert_text("!", UndoGroup::Typing, t0()).expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    // Read the raw bytes, not the document, to prove what is really on disk.
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "alpha\nbravo!\ncharlie\n"
    );

    // Reopen and confirm the text matches.
    let reopened = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert_eq!(reopened.buffer.text_string(), "alpha\nbravo!\ncharlie\n");
    assert!(!reopened.modified);
}

#[test]
fn mixed_eol_file_survives_an_edit_and_save() {
    // FR-N4 end to end: a mixed file, edited once, still keeps the untouched lines' endings.
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "m.txt", b"a\r\nb\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    assert_eq!(doc.line_eols, vec![Eol::Crlf, Eol::Lf]);

    doc.set_cursor(LineCol::new(0, 1), false);
    doc.insert_text("X", UndoGroup::Typing, t0()).expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"aX\r\nb\n");
}

#[test]
fn a_file_that_ended_without_a_newline_still_does_after_an_edit() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "nn.txt", b"one\ntwo");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.set_cursor(LineCol::new(1, 3), false);
    doc.insert_text("!", UndoGroup::Typing, t0()).expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo!");
}

#[test]
fn ensure_final_newline_config_adds_one_on_save() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ef.txt", b"one\ntwo");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    let o = SaveOptions {
        ensure_final_newline: true,
        ..Default::default()
    };
    doc.save(EolMode::File, &o).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo\n");
}

#[test]
fn trim_trailing_ws_config_applies_on_save() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "tw.txt", b"a   \nb\t\nc\n");
    let doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    let mut d2 = doc;
    let o = SaveOptions {
        trim_trailing_ws: true,
        ..Default::default()
    };
    d2.save(EolMode::File, &o).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc\n");
}

#[test]
fn eol_mode_lf_overrides_mixed_endings_on_save() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "ov.txt", b"a\r\nb\nc\r");
    let doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    let mut d = doc;
    d.save(EolMode::Lf, &opts()).expect("save");
    assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc\n");
}

#[test]
fn large_file_policy_is_consistent_with_file_module() {
    // The document does not decide this itself, but the UI reads it from here; confirm the
    // thresholds are the ones file.rs defines.
    assert_eq!(file::open_policy(1024), file::OpenDecision::Open);
    assert_eq!(
        file::open_policy(file::BACKGROUND_BYTES),
        file::OpenDecision::Background
    );
}

#[test]
fn save_is_repeatable_without_drift() {
    // Saving twice must produce identical bytes, or repeated autosaves would corrupt files.
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "d.txt", b"a\r\nb\nc");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    doc.save(EolMode::File, &opts()).expect("first");
    let first = fs::read(&path).expect("read");
    std::thread::sleep(Duration::from_millis(2));
    doc.save(EolMode::File, &opts()).expect("second");
    let second = fs::read(&path).expect("read");
    assert_eq!(first, second, "a second save must be byte-identical");
}

#[test]
fn buffer_and_document_agree_on_line_count_for_a_terminated_file() {
    // Guards the reconciliation in `eols_for_save`: the two line models must not fight.
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "lc.txt", b"a\nb\nc\n");
    let doc = Document::from_loaded(&file::load(&path, None).expect("load"));
    let eols = doc.eols_for_save();
    assert_eq!(eols.len(), 3, "three terminators for three lines");
    assert_eq!(
        Buffer::from_str_lossy("a\nb\nc\n").line_count(),
        4,
        "ropey counts a trailing empty line; eols_for_save accounts for it"
    );
}

// ------------------------------------------------- FR-N4: appended lines
//
// `line_eols` only ever describes the lines that were in the file, so these are the cases
// where an appended line has to be given an ending at save time rather than silently losing
// it (lines gluing together) or gaining a second terminator.

#[test]
fn appending_a_line_to_a_terminated_file_keeps_one_terminator() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "app.txt", b"one\ntwo\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    doc.set_cursor(LineCol::new(2, 0), false);
    doc.insert_text("three\n", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo\nthree\n");
}

#[test]
fn appending_a_line_without_a_terminator_still_separates_it() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "sep.txt", b"one\ntwo\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    // "three" with no trailing newline. The point is that it is not glued onto "two"; whether
    // the file now ends in a newline is the separate question the next test answers.
    doc.set_cursor(LineCol::new(2, 0), false);
    doc.insert_text("three", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo\nthree");
}

#[test]
fn an_unterminated_file_gains_a_terminator_on_the_old_last_line_when_content_is_added_below() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "grow.txt", b"one\ntwo");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    // Typing a newline after "two" means "two" is no longer the final line, so it needs an
    // ending; the newly open final line stays without one.
    doc.set_cursor(LineCol::new(1, 3), false);
    doc.insert_text("\n", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo\n");
}

#[test]
fn an_unterminated_file_stays_unterminated_after_appending_a_line() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "tail.txt", b"one\ntwo");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    doc.set_cursor(LineCol::new(1, 3), false);
    doc.insert_text("\nthree", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"one\ntwo\nthree");
}

#[test]
fn appended_lines_take_the_document_ending_not_the_loaded_dominant_one() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // Mixed file, LF dominant. Converting to CRLF then appending must write CRLF on the new
    // line while the loaded lines keep what they had.
    let path = write(dir.path(), "mix.txt", b"a\nb\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    doc.convert_eol(Eol::Crlf);
    doc.set_cursor(LineCol::new(2, 0), false);
    doc.insert_text("c\n", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"a\r\nb\r\nc\r\n");
}

#[test]
fn a_new_untitled_document_writes_one_terminator_per_line() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("made.txt");
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);

    doc.insert_text("a\nb\nc", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save_to(&path, EolMode::File, &opts()).expect("save");

    // The last line was never given a newline, so the file does not end in one. That is the
    // FR-N2 property, and it holds for a new document exactly as it does for a loaded one.
    assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc");
}

#[test]
fn a_newline_typed_at_the_end_of_a_buffer_is_written() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("nl.txt");
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);

    doc.insert_text("a\nb\nc\n", UndoGroup::Typing, t0())
        .expect("edit");
    doc.save_to(&path, EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"a\nb\nc\n");
}

#[test]
fn a_file_without_a_trailing_newline_keeps_it_through_an_unrelated_edit() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "keep.txt", b"head\nlast");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    doc.set_cursor(LineCol::new(0, 0), false);
    doc.insert_text("H", UndoGroup::Typing, t0()).expect("edit");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"Hhead\nlast");
}

#[test]
fn backspacing_the_final_newline_stops_the_file_ending_in_one() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = write(dir.path(), "bs.txt", b"a\nb\n");
    let mut doc = Document::from_loaded(&file::load(&path, None).expect("load"));

    // Delete the implied empty line: the caret sits at the end of "b" and backspace eats the
    // terminator, so the file must stop ending with one.
    doc.set_cursor(LineCol::new(2, 0), false);
    doc.delete_char_before(t0()).expect("backspace");
    doc.save(EolMode::File, &opts()).expect("save");

    assert_eq!(fs::read(&path).expect("read"), b"a\nb");
}
