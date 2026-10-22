//! The Open / Save As picker (FR-F2, FR-U5).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::app::{App, DialogKind, FilePurpose, Mode};
use np_tui::ui::file_picker::{
    dir_text, filter_entries, list_dir, resolve, split_query, start_dir, Resolved,
};
use std::fs;
use std::path::{Path, PathBuf};

const W: u16 = 100;
const H: u16 = 30;

fn app() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    }
}

/// A small tree: `root/{alpha.txt, beta.txt, .hidden.txt, src/main.rs, src/util.rs, .git/config}`.
fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("alpha.txt"), "a\n").expect("write alpha");
    fs::write(root.join("beta.txt"), "b\n").expect("write beta");
    fs::write(root.join(".hidden.txt"), "h\n").expect("write hidden");
    fs::create_dir(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write main");
    fs::write(root.join("src/util.rs"), "fn util() {}\n").expect("write util");
    fs::create_dir(root.join(".git")).expect("mkdir .git");
    fs::write(root.join(".git/config"), "c\n").expect("write git config");
    dir
}

fn names(entries: &[np_tui::ui::file_picker::Entry]) -> Vec<String> {
    entries.iter().map(|e| e.name.clone()).collect()
}

// ------------------------------------------------------------------ listing

#[test]
fn directories_sort_before_files() {
    let dir = tree();
    let (entries, err) = list_dir(dir.path(), false);
    assert!(err.is_none(), "a readable directory reports no error");
    let got = names(&entries);
    // `..` then the directories, then the files. Directories carry a trailing slash, which
    // is both the visual affordance and what the fuzzy matcher scores against.
    assert_eq!(got[0], "../", "`..` must be first, always");
    assert_eq!(
        got[1], "src/",
        "the only visible directory, and the only non-file"
    );
    assert!(
        !got.contains(&".hidden.txt".to_string()),
        "dotfiles are off by default: {got:?}"
    );
    assert!(
        !got.contains(&".git/".to_string()),
        "a hidden directory is hidden too: {got:?}"
    );
    assert_eq!(&got[2..], ["alpha.txt", "beta.txt"]);
}

#[test]
fn hidden_files_appear_only_when_toggled() {
    let dir = tree();
    let (shown, _) = list_dir(dir.path(), true);
    let got = names(&shown);
    assert!(got.contains(&".hidden.txt".to_string()), "{got:?}");
    assert!(got.contains(&".git/".to_string()), "{got:?}");
    // And the ordering rule survives the toggle: dirs first, `..` first of all.
    assert_eq!(got[0], "../");
    assert_eq!(got[1], ".git/");
}

#[test]
fn an_unreadable_directory_reports_a_message_and_keeps_the_listing_open() {
    // FR-F2: the picker must not collapse on a permissions problem.
    let (entries, err) = list_dir(Path::new("/definitely/not/here"), false);
    // Nothing real to show; only the `..` row, which is added unconditionally.
    assert_eq!(
        names(&entries),
        vec!["../"],
        "a failed read lists no real entries"
    );
    let msg = err.expect("a missing directory must say so");
    assert!(
        msg.contains("definitely/not/here"),
        "the message should name the path: {msg}"
    );
}

#[test]
fn a_directory_named_in_the_field_gets_a_trailing_slash() {
    assert_eq!(dir_text(Path::new("/tmp")), "/tmp/");
    assert_eq!(dir_text(Path::new("/tmp/")), "/tmp/");
}

// ------------------------------------------------------------------ querying

#[test]
fn a_path_with_a_separator_is_a_path_not_a_filter() {
    // Typing an absolute path means the path: list `/tmp`, filter by `alpha`.
    let (dir, filter) = split_query("/tmp/alpha");
    assert_eq!(dir, PathBuf::from("/tmp"));
    assert_eq!(filter, "alpha");
    // A trailing separator means "list this", no filter at all.
    let (dir, filter) = split_query("/tmp/");
    assert_eq!(dir, PathBuf::from("/tmp"));
    assert_eq!(filter, "");
    // Bare text filters the current directory.
    let (dir, filter) = split_query("alpha");
    assert_eq!(dir, PathBuf::from("."));
    assert_eq!(filter, "alpha");
}

#[test]
fn fuzzy_filter_ranks_and_an_empty_query_changes_nothing() {
    let dir = tree();
    // Scoped to `src/`, whose two names are similar enough that ranking is visible.
    let (entries, _) = list_dir(&dir.path().join("src"), false);
    // Empty query is identity, which is what keeps the sorted listing on screen at open.
    assert_eq!(filter_entries(&entries, ""), entries);
    let hits = names(&filter_entries(&entries, "ain"));
    assert!(
        hits.contains(&"main.rs".to_string()),
        "`ain` should match main.rs: {hits:?}"
    );
    assert!(
        !hits.contains(&"util.rs".to_string()),
        "a match list must not contain names the query cannot explain: {hits:?}"
    );
    // Subsequence matching is the point: `us` reaches `util.rs` across the dot.
    let us = names(&filter_entries(&entries, "us"));
    assert!(
        us.contains(&"util.rs".to_string()),
        "`us` should match util.rs: {us:?}"
    );
    assert!(!us.contains(&"main.rs".to_string()), "{us:?}");
    // A query that explains nothing leaves only the escape hatch, not the whole list.
    assert_eq!(names(&filter_entries(&entries, "qwxz")), vec!["../"]);
}

// ------------------------------------------------------------------ resolve

#[test]
fn resolve_tells_a_directory_a_file_and_a_target_apart() {
    let dir = tree();
    let root = dir.path();
    assert_eq!(
        resolve(&dir_text(&root.join("src")), false),
        Resolved::EnterDir(root.join("src"))
    );
    assert_eq!(
        resolve(&root.join("alpha.txt").display().to_string(), false),
        Resolved::OpenFile(root.join("alpha.txt"))
    );
    // Open on a nonexistent path is an error; Save As on the same path is a target.
    let missing = root.join("new.txt");
    let invalid = resolve(&missing.display().to_string(), false);
    assert!(
        matches!(invalid, Resolved::Invalid(_)),
        "Open must not invent a file"
    );
    assert_eq!(
        resolve(&missing.display().to_string(), true),
        Resolved::CreateAt(missing)
    );
    // Save As into a folder that is not there fails with the folder named.
    let far = root.join("nope/deeper.txt");
    match resolve(&far.display().to_string(), true) {
        Resolved::Invalid(msg) => assert!(
            msg.contains("nope"),
            "the message should name the missing directory: {msg}"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert_eq!(
        resolve("   ", true),
        Resolved::Invalid("no path given".to_string()),
        "an empty field is rejected, not written"
    );
}

#[test]
fn start_dir_prefers_cli_then_history_then_the_document() {
    let dir = tree();
    let root = dir.path();
    let other = root.join("src");
    // CLI wins when it is a directory.
    assert_eq!(start_dir(Some(&other), Some(root), None), other);
    // History next.
    assert_eq!(
        start_dir(None, Some(&other), Some(&root.join("alpha.txt"))),
        other
    );
    // Then the active document's own folder.
    assert_eq!(
        start_dir(None, None, Some(&root.join("src/main.rs"))),
        other
    );
    // A CLI directory that does not exist is ignored rather than trusted.
    assert_eq!(
        start_dir(Some(&root.join("gone")), Some(&other), None),
        other
    );
    // With nothing at all it lands somewhere that exists.
    let fallback = start_dir(None, None, None);
    assert!(fallback.is_dir(), "{fallback:?} should exist");
}

// -------------------------------------------------------------- the dialog

#[test]
fn open_picker_lists_the_start_directory() {
    let dir = tree();
    let mut app = app();
    // Seed the "last directory" the way a previous navigation would have.
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Open));
    assert_eq!(app.file_purpose, FilePurpose::Open);
    let got = names(&app.file_entries);
    assert_eq!(got[0], "../");
    assert!(got.contains(&"alpha.txt".to_string()), "{got:?}");
    assert!(
        !got.contains(&".hidden.txt".to_string()),
        "hidden files start off: {got:?}"
    );
}

#[test]
fn enter_on_a_directory_descends_and_remembers_it() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let src = app
        .file_entries
        .iter()
        .position(|e| e.name == "src/")
        .expect("src listed");
    app.file_selected = src;
    // Focus that row so Enter means "accept this row".
    app.focus_dialog_row(src);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode(),
        Mode::Dialog(DialogKind::Open),
        "descending must keep the dialog open"
    );
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.path().join("src").as_path())
    );
    let got = names(&app.file_entries);
    assert_eq!(got[1..], ["main.rs", "util.rs"], "{got:?}");
}

#[test]
fn dotdot_row_returns_to_the_parent() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().join("src"));
    app.open_file_picker(FilePurpose::Open);
    assert!(app.file_entries.iter().any(|e| e.name == "main.rs"));
    app.file_selected = 0; // the `..` row
    app.focus_dialog_row(0);
    press(&mut app, KeyCode::Enter);
    let got = names(&app.file_entries);
    assert!(got.contains(&"alpha.txt".to_string()), "{got:?}");
    assert_eq!(app.last_dir.as_deref(), Some(dir.path()));
}

#[test]
fn typing_a_path_filters_the_listing_and_a_second_type_narrows_it() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let before = app.file_entries.len();
    type_str(&mut app, "al");
    let after = names(&app.file_entries);
    assert!(
        app.file_entries.len() < before,
        "typing must narrow the list"
    );
    assert!(after.contains(&"alpha.txt".to_string()), "{after:?}");
    // The field keeps what the user typed, which is what makes Enter meaningful.
    assert!(
        app.file_input.text().ends_with("al"),
        "{:?}",
        app.file_input.text()
    );
}

#[test]
fn the_hidden_button_or_ctrl_h_flips_the_toggle() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    assert!(!app.show_hidden);
    // The button is row 1 of the dialog's button row.
    let mut presses = 0;
    while app.focused_id() != Some(np_tui::ui::framework::FocusId::Button(1)) && presses < 8 {
        app.cycle_dialog_focus(1);
        presses += 1;
    }
    assert_eq!(
        app.focused_id(),
        Some(np_tui::ui::framework::FocusId::Button(1))
    );
    press(&mut app, KeyCode::Enter);
    assert!(app.show_hidden, "the Hidden button must toggle");
    assert!(
        names(&app.file_entries).contains(&".hidden.txt".to_string()),
        "{:?}",
        names(&app.file_entries)
    );
}

#[test]
fn enter_on_a_file_path_opens_it_and_closes_the_dialog() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let target = dir.path().join("alpha.txt");
    app.file_input.set_text(target.display().to_string());
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing, "the dialog must close");
    let path = app.doc().and_then(|d| d.path.clone());
    assert_eq!(path.as_deref(), Some(target.as_path()));
    // One text line plus the trailing newline is two buffer lines; `alpha.txt` is "a\n".
    assert_eq!(app.doc().map(|d| d.buffer.line_count()), Some(2));
}

#[test]
fn opening_a_file_that_is_already_open_activates_the_existing_tab() {
    let dir = tree();
    let target = dir.path().join("beta.txt");
    let mut app = app();
    app.open_path(&target);
    let count = app.documents.len();
    app.open_file_picker(FilePurpose::Open);
    app.file_input.set_text(target.display().to_string());
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.documents.len(),
        count,
        "the same path must not become two tabs"
    );
}

#[test]
fn save_as_writes_to_a_path_that_does_not_exist_yet() {
    let dir = tree();
    let target = dir.path().join("brand-new.txt");
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::SaveAs);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::SaveAs));
    assert_eq!(app.file_purpose, FilePurpose::SaveAs);
    app.file_input.set_text(target.display().to_string());
    let now = app.clock;
    if let Some(doc) = app.doc_mut() {
        let _ = doc.insert_text("written from the picker\n", np_core::UndoGroup::Atomic, now);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing);
    let on_disk = fs::read_to_string(&target).unwrap_or_default();
    assert!(
        on_disk.contains("written from the picker"),
        "expected the file on disk, got {on_disk:?}"
    );
}

#[test]
fn a_bad_path_keeps_the_dialog_open_and_says_why() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let missing = dir.path().join("nowhere.txt");
    app.file_input.set_text(missing.display().to_string());
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode(),
        Mode::Dialog(DialogKind::Open),
        "a failure must not eat the dialog"
    );
    assert!(app.file_error.is_some(), "and must report");
    // Any further typing clears the message.
    type_str(&mut app, "x");
    assert!(app.file_error.is_none(), "the error must not linger");
}

#[test]
fn esc_leaves_the_picker_without_touching_anything() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    let count = app.documents.len();
    app.open_file_picker(FilePurpose::Open);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(app.documents.len(), count);
}

#[test]
fn a_double_click_on_a_listing_row_descends_and_a_single_one_only_selects() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let layout = np_tui::ui::layout::FilePicker::new(&app);
    let list = layout.list_area();
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 2,
        row: list.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    // The listing hit-tests per row, so the hit names the row's focus item rather than the
    // whole region. Row 1 of the listing is `src/`.
    let hit = np_tui::mouse::hit_test(&click, &app);
    let np_tui::mouse::Hit::DialogWidget(focus_index) = hit else {
        panic!("expected a dialog widget hit, got {hit:?}");
    };
    // The hit itself is the assertion about routing: it names row 1's focus item, not the
    // listing as a whole.
    assert_eq!(
        np_tui::ui::framework::focus_at(&app.dialog_focus_items(), click.column, click.row),
        Some(focus_index),
        "the click must resolve to one row's focus item"
    );
    assert!(
        matches!(
            app.dialog_focus_items()[focus_index].id,
            np_tui::ui::framework::FocusId::ListRow(1)
        ),
        "focus item {focus_index} must be the row clicked, got {:?}",
        app.dialog_focus_items()[focus_index].id
    );
    app.handle_mouse(&click);
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: click.column,
        row: click.row,
        modifiers: KeyModifiers::NONE,
    });
    // S22: one click no longer opens. It selects, and the release that stayed on the row keeps the
    // press/release split that lets a pointer be dragged down a listing to scan it. Opening needs a
    // second click, which is what every desktop file list does.
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.path()),
        "a single click must select, not descend"
    );
    app.handle_mouse(&click);
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: click.column,
        row: click.row,
        modifiers: KeyModifiers::NONE,
    });
    // A directory row descends on double-click, the same thing Enter does.
    assert_eq!(
        app.mode(),
        Mode::Dialog(DialogKind::Open),
        "descending must keep the dialog open"
    );
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.path().join("src").as_path())
    );
    assert!(
        names(&app.file_entries).contains(&"main.rs".to_string()),
        "{:?}",
        names(&app.file_entries)
    );
}

#[test]
fn a_direct_row_open_uses_the_same_accept_path_as_enter() {
    // `accept_clicked_file_row` is what a row-level click would call; it must agree with Enter
    // so the two input devices cannot develop different ideas about a directory.
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let src = app
        .file_entries
        .iter()
        .position(|e| e.name == "src/")
        .expect("src listed");
    app.accept_clicked_file_row(src);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Open));
    assert_eq!(
        app.last_dir.as_deref(),
        Some(dir.path().join("src").as_path())
    );
    assert_eq!(
        names(&app.file_entries)[1..],
        ["main.rs", "util.rs"],
        "descending must re-list"
    );
    // A file row opens; one accept path serves both devices.
    let main = app
        .file_entries
        .iter()
        .position(|e| e.name == "main.rs")
        .expect("main after descending");
    app.accept_clicked_file_row(main);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(
        app.doc().and_then(|d| d.path.clone()).as_deref(),
        Some(dir.path().join("src/main.rs").as_path())
    );
    // A row index past the end is ignored rather than a panic.
    let count = app.file_entries.len();
    app.accept_clicked_file_row(count + 5);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn the_picker_renders_its_field_listing_and_buttons() {
    let dir = tree();
    let mut app = app();
    app.last_dir = Some(dir.path().to_path_buf());
    app.open_file_picker(FilePurpose::Open);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(block.contains("Open"), "title missing: {block}");
    assert!(block.contains("alpha.txt"), "listing missing: {block}");
    assert!(block.contains("Hidden"), "buttons missing: {block}");
    // FR-U4: the backdrop is dimmed behind a dialog.
    let corners = [
        np_tui::ui::cell_text(&buf, 0, H / 2),
        np_tui::ui::cell_text(&buf, W - 1, H / 2),
    ];
    assert_eq!(corners[0], corners[1], "the backdrop should be uniform");
}
