//! Stage S9 acceptance for the session format: round-trip fidelity, the untitled content
//! cap, recents, and resilience to a broken file (FR-CG3, FR-F8).

use std::path::{Path, PathBuf};

use np_core::{Document, Session, SessionSnapshot, SessionTab, UNTITLED_SESSION_MAX_BYTES};

fn session_in(dir: &Path) -> PathBuf {
    dir.join("session.toml")
}

// ------------------------------------------------------------- the 1 MB cap

/// A document with no path and the given content length in bytes.
fn untitled_with_bytes(n: usize) -> Document {
    let mut doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    doc.buffer = np_core::Buffer::from_str_lossy(&"x".repeat(n));
    doc
}

fn saved_len(doc: Document) -> usize {
    let (session, _) = Session::from_tabs([doc], 0, Vec::new());
    session.tab[0]
        .content
        .as_ref()
        .map(|c| c.len())
        .unwrap_or(0)
}

#[test]
fn content_just_under_the_cap_is_kept_whole() {
    let n = UNTITLED_SESSION_MAX_BYTES - 1;
    assert_eq!(saved_len(untitled_with_bytes(n)), n);
}

#[test]
fn content_at_exactly_the_cap_is_kept_whole() {
    // The boundary is inclusive: exactly 1 MB is affordable, so it must not be truncated.
    let n = UNTITLED_SESSION_MAX_BYTES;
    assert_eq!(saved_len(untitled_with_bytes(n)), n);
}

#[test]
fn content_over_the_cap_is_truncated_and_warns_but_the_tab_survives() {
    let n = UNTITLED_SESSION_MAX_BYTES + 1;
    let (session, warnings) = Session::from_tabs([untitled_with_bytes(n)], 0, Vec::new());
    assert_eq!(
        session.tab.len(),
        1,
        "the tab must be kept even when its content is cut"
    );
    let kept = session.tab[0].content.as_ref().expect("content kept");
    assert!(
        kept.len() <= UNTITLED_SESSION_MAX_BYTES,
        "kept {}",
        kept.len()
    );
    assert_eq!(kept.len(), UNTITLED_SESSION_MAX_BYTES);
    assert_eq!(
        warnings.len(),
        1,
        "truncation must be reported: {warnings:?}"
    );
    assert!(warnings[0].contains("untitled"), "{warnings:?}");
}

#[test]
fn truncation_lands_on_a_char_boundary() {
    // Multibyte text over the cap must not be cut through a character.
    let mut doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    let text = "é".repeat(UNTITLED_SESSION_MAX_BYTES); // 2 bytes each
    doc.buffer = np_core::Buffer::from_str_lossy(&text);
    let (session, warnings) = Session::from_tabs([doc], 0, Vec::new());
    let kept = session.tab[0].content.as_ref().expect("content");
    assert!(kept.is_char_boundary(kept.len()), "cut through a character");
    assert!(kept.len() <= UNTITLED_SESSION_MAX_BYTES);
    assert_eq!(warnings.len(), 1);
}

#[test]
fn a_file_backed_tab_never_stores_content() {
    // The file is the content. A second copy would restore stale text over a newer file.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "on disk").expect("write");
    let loaded = np_core::load(&path, None).expect("load");
    let mut doc = Document::from_loaded(&loaded);
    doc.modified = true;
    let (session, _) = Session::from_tabs([doc], 0, Vec::new());
    assert_eq!(session.tab[0].path.as_deref(), Some(path.as_path()));
    assert!(
        session.tab[0].content.is_none(),
        "a modified file-backed tab must not carry a snapshot of its text"
    );
}

// ------------------------------------------------------------- round trip

#[test]
fn round_trip_preserves_folds_bookmarks_language_and_readonly() {
    // Every §7.2 field, because a session that quietly drops folds is the bug this stage
    // would most plausibly introduce.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());

    let mut doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    doc.buffer = np_core::Buffer::from_str_lossy("a\nb\nc\nd\ne\nf\n");
    doc.cursor.pos = np_core::LineCol::new(3, 2);
    doc.scroll.top_line = 2;
    doc.bookmarks = [1, 4].into_iter().collect();
    doc.folds = vec![np_core::FoldRange {
        start: 0,
        end: 3,
        collapsed: true,
    }];
    doc.language = np_core::LanguageId::Named("Rust".into());
    doc.readonly = true;

    let (session, warns) = Session::from_tabs([doc], 0, vec![PathBuf::from("/x/y.rs")]);
    assert!(warns.is_empty(), "{warns:?}");
    session.save(&path).expect("save");

    let back = Session::load(&path).expect("load");
    assert_eq!(back.tab[0].cursor_line, 3);
    assert_eq!(back.tab[0].cursor_col, 2);
    assert_eq!(back.tab[0].scroll_top, 2);
    assert_eq!(back.tab[0].bookmarks, vec![1, 4]);
    assert_eq!(back.tab[0].folded, vec![[0, 3]]);
    assert_eq!(back.tab[0].language, "Rust");
    assert!(back.tab[0].readonly);
    assert_eq!(back.recent, vec![PathBuf::from("/x/y.rs")]);
}

#[test]
fn an_expanded_fold_is_not_persisted() {
    let mut doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    doc.folds = vec![
        np_core::FoldRange {
            start: 0,
            end: 3,
            collapsed: false,
        },
        np_core::FoldRange {
            start: 5,
            end: 9,
            collapsed: true,
        },
    ];
    let (session, _) = Session::from_tabs([doc], 0, Vec::new());
    assert_eq!(
        session.tab[0].folded,
        vec![[5, 9]],
        "only collapsed folds carry state worth restoring"
    );
}

#[test]
fn auto_language_serializes_as_the_empty_string() {
    // spec §7.2 pins `""` for auto-detect.
    let doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    let (session, _) = Session::from_tabs([doc], 0, Vec::new());
    assert_eq!(session.tab[0].language, "");
    assert_eq!(
        np_core::LanguageId::from_session_str(&session.tab[0].language),
        np_core::LanguageId::Auto
    );
}

// ------------------------------------------------------------- broken files

#[test]
fn a_torn_file_loads_as_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    std::fs::write(
        &path,
        "active_tab = 1\nrecent = [\"a\"]\n[[tab]]\ncursor_li",
    )
    .expect("write");
    assert_eq!(Session::load(&path), None);
}

#[test]
fn an_empty_file_loads_as_an_empty_session() {
    // An empty TOML document is valid and describes an empty session, which is harmless: the
    // restorer has a zero-document rule and falls back to a fresh untitled tab. Refusing it
    // would be arbitrary, and the case genuinely worth rejecting -- a file truncated
    // mid-structure -- is covered by `a_torn_file_loads_as_none`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    std::fs::write(&path, "").expect("write");
    let s = Session::load(&path).expect("an empty file is valid TOML");
    assert_eq!(s, Session::default());
}

#[test]
fn a_session_of_the_wrong_shape_loads_as_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    std::fs::write(&path, "active_tab = 0\ntab = \"not a list\"\n").expect("write");
    assert_eq!(Session::load(&path), None);
}

#[test]
fn a_minimal_session_with_only_active_tab_loads() {
    // Forward and backward compatibility: extra fields default rather than fail.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    std::fs::write(&path, "active_tab = 0\n").expect("write");
    let s = Session::load(&path).expect("must load");
    assert_eq!(s.active_tab, 0);
    assert!(s.tab.is_empty());
    assert!(s.recent.is_empty());
}

// ------------------------------------------------------------- atomic writes

#[test]
fn a_failed_save_leaves_the_previous_session_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    let first = Session {
        active_tab: 3,
        ..Default::default()
    };
    first.save(&path).expect("save");

    // Make the destination impossible to rename onto: a directory in the way.
    let blocking = dir.path().join("blocked");
    std::fs::create_dir_all(&blocking).expect("mkdir");
    let result = first.save(&blocking);
    assert!(result.is_err(), "renaming onto a directory must fail");

    // The original is untouched.
    let back = Session::load(&path).expect("original still loads");
    assert_eq!(back.active_tab, 3);
}

#[test]
fn a_second_save_replaces_the_first_rather_than_appending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    let a = Session {
        active_tab: 1,
        ..Default::default()
    };
    a.save(&path).expect("save");
    let b = Session {
        active_tab: 7,
        ..Default::default()
    };
    b.save(&path).expect("save");
    let back = Session::load(&path).expect("load");
    assert_eq!(back.active_tab, 7);
    assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 1);
}

// ------------------------------------------------------------------ recents

#[test]
fn recents_survive_a_round_trip_in_order() {
    let mut s = Session::default();
    for name in ["old.rs", "mid.rs", "new.rs"] {
        s.push_recent(PathBuf::from(name), 10);
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = session_in(dir.path());
    s.save(&path).expect("save");
    let back = Session::load(&path).expect("load");
    assert_eq!(
        back.recent,
        vec![
            PathBuf::from("new.rs"),
            PathBuf::from("mid.rs"),
            PathBuf::from("old.rs")
        ]
    );
}

#[test]
fn a_real_file_opened_by_two_spellings_dedups_to_one_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "hi").expect("write");

    let mut s = Session::default();
    s.push_recent(file.clone(), 10);
    // A path with a redundant component resolving to the same file.
    let indirect = dir.path().join(".").join("notes.txt");
    s.push_recent(indirect, 10);
    assert_eq!(s.recent.len(), 1, "{:?}", s.recent);
}

#[test]
fn a_cap_of_one_keeps_only_the_newest() {
    let mut s = Session::default();
    for name in ["a", "b", "c"] {
        s.push_recent(PathBuf::from(name), 1);
    }
    assert_eq!(s.recent, vec![PathBuf::from("c")]);
}

/// Keeps the `SessionSnapshot` import meaningful: the trait must be in scope for
/// `from_tabs` to accept documents through the blanket reference impl.
#[test]
fn the_snapshot_trait_is_the_public_way_to_capture() {
    let doc = Document::new_untitled(np_core::EncodingId::Utf8, np_core::Eol::Lf);
    let tab: SessionTab = doc.snapshot();
    assert!(tab.path.is_none());
    assert_eq!(tab.content.as_deref(), Some(""));
}
