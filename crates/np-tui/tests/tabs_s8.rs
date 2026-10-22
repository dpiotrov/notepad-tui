//! Stage S8 acceptance: tab lifecycle, confirm-on-close, closed-tab history, and the
//! Document List (FR-F1, FR-F5, FR-U2, FR-U4).
//!
//! Every close path is checked for data loss, because that is the failure mode this stage
//! can introduce silently: a tab that closes without asking, or reopens with the file's
//! content instead of the user's edits, loses work with no error to show for it.

use std::fs;
use std::path::Path;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{Config, Document, LineCol};
use np_tui::ui::tab_bar;
use np_tui::{Action, App, ConfirmButton, PendingOp};

const W: u16 = 80;
const H: u16 = 24;

fn config() -> Config {
    let mut config = Config::default();
    // OSC 52 keeps the clipboard in-process; the system clipboard is unavailable headless
    // and a caching provider would make test order matter.
    config.ui.clipboard = "osc52".into();
    config
}

fn app() -> App {
    App::new_headless(config(), (W, H))
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn text(app: &App) -> String {
    app.doc().expect("document").buffer.text_string()
}

fn texts(app: &App) -> Vec<String> {
    app.documents
        .iter()
        .map(|d| d.buffer.text_string())
        .collect()
}

/// Write a file and open it as a new tab.
fn open_file(_app: &App, path: &Path) -> Document {
    let loaded = np_core::load(path, None).expect("load");
    Document::from_loaded(&loaded)
}

fn push_doc(app: &mut App, path: &Path) {
    let doc = open_file(app, path);
    app.open_document(doc);
}

/// Append text to a freshly opened document.
///
/// `from_loaded` leaves the caret at 0,0, so typing straight into an opened file inserts at
/// the front. Tests that mean "edit the end of this file" have to move the caret there.
fn edit_end(app: &mut App, added: &str) {
    let now = app.clock;
    let doc = app.doc_mut().expect("document");
    let last = doc.buffer.line_count().saturating_sub(1);
    let end_col = doc.buffer.line_string(last).chars().count();
    doc.cursor.pos = LineCol::new(last, end_col);
    doc.insert_text(added, np_core::UndoGroup::Atomic, now)
        .expect("insert");
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

/// The centre of tab `i` as drawn this frame.
fn tab_x(app: &App, i: usize) -> u16 {
    let spans = tab_bar::spans(app);
    let span = spans
        .iter()
        .find(|s| s.index == i)
        .unwrap_or_else(|| panic!("tab {i} not visible in {} spans", spans.len()));
    span.x + span.width / 2
}

// ------------------------------------------------------------ untitled naming

#[test]
fn untitled_documents_are_numbered_in_order() {
    let mut app = app();
    assert_eq!(app.doc().unwrap().display_name(), "Untitled-1");
    app.execute(Action::NewFile);
    assert_eq!(app.doc().unwrap().display_name(), "Untitled-2");
    app.execute(Action::NewFile);
    assert_eq!(app.doc().unwrap().display_name(), "Untitled-3");
    assert_eq!(
        app.documents
            .iter()
            .map(|d| d.display_name())
            .collect::<Vec<_>>(),
        vec!["Untitled-1", "Untitled-2", "Untitled-3"]
    );
}

#[test]
fn untitled_numbers_are_never_reused_after_a_close() {
    // Reusing a name would leave two tabs reading `Untitled-2` at different times, and the
    // user could not tell which one their keystrokes went to.
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    assert_eq!(app.doc().unwrap().display_name(), "Untitled-3");

    // Close the middle tab (index 1), clean so no confirm.
    app.close_tab_for_test(1);
    assert_eq!(app.documents.len(), 2);

    app.execute(Action::NewFile);
    assert_eq!(
        app.doc().unwrap().display_name(),
        "Untitled-4",
        "the number must move forward, not refill the gap"
    );
}

// --------------------------------------------------------- close and neighbours

#[test]
fn closing_the_only_document_leaves_a_fresh_untitled_one() {
    let mut app = app();
    app.execute(Action::CloseTab);
    assert_eq!(app.documents.len(), 1, "never zero documents");
    assert!(!app.doc().unwrap().modified);
    assert_eq!(app.active, 0);
}

#[test]
fn closing_a_middle_tab_activates_the_left_neighbour() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    assert_eq!(app.active, 2);

    app.close_tab_for_test(1);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.active, 0, "left neighbour of the closed tab");
}

#[test]
fn closing_the_first_tab_activates_what_slides_into_zero() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    app.active = 0;

    app.close_tab_for_test(0);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.active, 0, "nothing is left of index 0");
}

#[test]
fn closing_the_last_tab_activates_the_one_before_it() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    assert_eq!(app.active, 2);

    app.close_tab_for_test(2);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.active, 1);
}

#[test]
fn neighbor_rule_is_a_pure_function_over_indices() {
    use np_tui::tabs::neighbor_after_close;
    assert_eq!(neighbor_after_close(0, 1), (0, false));
    assert_eq!(neighbor_after_close(0, 3), (0, true));
    assert_eq!(neighbor_after_close(1, 3), (0, true));
    assert_eq!(neighbor_after_close(2, 3), (1, true));
    // A stale index is clamped, not trusted.
    assert_eq!(neighbor_after_close(9, 3), (1, true));
}

// ---------------------------------------------------------- confirm on close

#[test]
fn closing_a_modified_tab_asks_and_closes_nothing() {
    let mut app = app();
    type_text(&mut app, "precious");
    let before = app.documents.len();

    app.execute(Action::CloseTab);

    assert_eq!(
        app.documents.len(),
        before,
        "must not close behind the user"
    );
    assert!(app.confirm.is_some(), "a prompt is owed");
    let confirm = app.confirm.clone().expect("confirm");
    assert_eq!(
        confirm.buttons,
        vec![
            ConfirmButton::Save,
            ConfirmButton::DontSave,
            ConfirmButton::Cancel
        ]
    );
    assert_eq!(confirm.op, PendingOp::CloseTab(0));
    assert!(confirm.prompt.contains("Untitled-1"));
    assert_eq!(
        app.mode(),
        np_tui::Mode::Dialog(np_tui::DialogKind::Confirm)
    );
}

#[test]
fn cancel_leaves_the_document_exactly_as_it_was() {
    let mut app = app();
    type_text(&mut app, "precious");
    app.execute(Action::CloseTab);

    let confirm = app.confirm.clone().expect("confirm");
    app.resolve_confirm(ConfirmButton::Cancel);

    assert!(app.confirm.is_none(), "the prompt is finished");
    assert_eq!(app.documents.len(), 1);
    assert_eq!(text(&app), "precious");
    assert!(app.doc().unwrap().modified, "still unsaved");
    assert_eq!(app.mode(), np_tui::Mode::Editing);
    assert_eq!(app.confirm, None);
    assert_eq!(
        app.pending_confirm, None,
        "the legacy view must not outlive the prompt"
    );
    let _ = confirm;
}

#[test]
fn dont_save_closes_and_the_text_is_recoverable() {
    let mut app = app();
    type_text(&mut app, "precious");
    app.execute(Action::CloseTab);
    app.resolve_confirm(ConfirmButton::DontSave);

    // The last tab closed, so a fresh untitled one took its place.
    assert_eq!(app.documents.len(), 1);
    assert_eq!(text(&app), "", "the editor shows the new document");
    // But nothing was lost: the closed tab is in history with its unsaved text.
    assert_eq!(app.closed_tabs.len(), 1);
    assert_eq!(
        app.closed_tabs[0].unsaved_text.as_deref(),
        Some("precious"),
        "closing a modified tab must not destroy the edits"
    );
}

#[test]
fn save_writes_the_file_then_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notes.txt");
    fs::write(&path, "original").expect("write");

    let mut app = app();
    push_doc(&mut app, &path);
    edit_end(&mut app, " edited");
    assert!(app.doc().unwrap().modified);

    app.execute(Action::CloseTab);
    assert_eq!(app.documents.len(), 2, "asked first");
    app.resolve_confirm(ConfirmButton::Save);

    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "original edited",
        "Save must reach the disk before the tab goes"
    );
    assert_eq!(
        app.documents.len(),
        1,
        "the saved tab closed, leaving the original untitled one"
    );
    assert!(!app.documents.is_empty());
}

#[test]
fn save_on_an_untitled_document_does_not_close_it() {
    // An untitled buffer exists only in memory. Saving it is impossible without a path, and
    // closing it anyway would be unrecoverable data loss.
    let mut app = app();
    type_text(&mut app, "never saved");
    app.execute(Action::CloseTab);
    app.resolve_confirm(ConfirmButton::Save);

    assert_eq!(app.documents.len(), 1, "still open");
    assert_eq!(text(&app), "never saved");
    assert!(
        app.pending_save_as.is_some(),
        "parked for the S14 Save As dialog"
    );
}

#[test]
fn a_failed_save_keeps_the_prompt_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.txt");
    fs::write(&path, "on disk").expect("write");
    let mut perms = fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms).expect("chmod");

    let mut app = app();
    let doc = open_file(&app, &path);
    let readonly = doc.readonly;
    app.open_document(doc);
    if !readonly {
        // Running as root ignores the permission bits, so force the flag the save path
        // checks. The behavior under test is the read-only rejection, not the OS.
        app.doc_mut().expect("doc").readonly = true;
    }
    // A read-only document rejects edits, so mark it modified the way an edit made before
    // the file was locked would have.
    app.doc_mut().expect("doc").modified = true;

    app.execute(Action::CloseTab);
    let op = app.confirm.clone().expect("confirm").op;
    app.resolve_confirm(ConfirmButton::Save);

    assert_eq!(app.documents.len(), 2, "the tab is still there");
    assert!(
        app.confirm.is_some(),
        "a failed save must re-ask rather than drop the close"
    );
    assert_eq!(app.confirm.clone().expect("re-asked").op, op);
}

// ------------------------------------------------------------- exit behaviour

#[test]
fn exit_with_nothing_modified_quits_immediately() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::Exit);
    assert!(
        app.quit_requested,
        "no prompt when there is nothing to lose"
    );
    assert!(app.confirm.is_none());
}

#[test]
fn exit_with_a_modified_document_confirms() {
    let mut app = app();
    type_text(&mut app, "work in progress");
    app.execute(Action::Exit);

    assert!(!app.quit_requested, "must not quit past unsaved work");
    let confirm = app.confirm.clone().expect("confirm");
    assert_eq!(confirm.op, PendingOp::Exit);
    assert!(confirm.prompt.contains("Untitled-1"));
}

#[test]
fn exit_cancel_leaves_every_document_intact() {
    let mut app = app();
    type_text(&mut app, "one");
    app.execute(Action::NewFile);
    type_text(&mut app, "two");
    let before = texts(&app);

    app.execute(Action::Exit);
    app.resolve_confirm(ConfirmButton::Cancel);

    assert!(!app.quit_requested);
    assert_eq!(texts(&app), before, "nothing changed at all");
    assert_eq!(app.modified_indices(), vec![0, 1]);
}

#[test]
fn exit_dont_save_quits_and_history_keeps_the_work() {
    let mut app = app();
    type_text(&mut app, "throwaway");
    app.execute(Action::Exit);
    app.resolve_confirm(ConfirmButton::DontSave);

    assert!(app.quit_requested, "the user said do not save, so quit");
    assert_eq!(app.closed_tabs.len(), 1);
    assert_eq!(
        app.closed_tabs[0].unsaved_text.as_deref(),
        Some("throwaway")
    );
}

#[test]
fn exit_saves_every_modified_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "A").expect("write");
    fs::write(&b, "B").expect("write");

    let mut app = app();
    push_doc(&mut app, &a);
    edit_end(&mut app, "1");
    // Switch to the second file and edit it too.
    let bdoc = open_file(&app, &b);
    app.open_document(bdoc);
    edit_end(&mut app, "2");

    // Both files modified; exit must ask, and each Save must write.
    app.execute(Action::Exit);
    assert!(!app.quit_requested);
    let mut rounds = 0;
    while app.confirm.is_some() && rounds < 5 {
        let target = app.confirm.clone().expect("confirm").target;
        app.resolve_confirm(ConfirmButton::Save);
        rounds += 1;
        let _ = target;
    }
    assert!(app.quit_requested, "quit once everything is saved");
    assert_eq!(fs::read_to_string(&a).expect("read"), "A1");
    assert_eq!(fs::read_to_string(&b).expect("read"), "B2");
}

// ------------------------------------------------------------- reopen (FR-U2)

#[test]
fn reopen_restores_a_clean_files_path_and_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("data.txt");
    fs::write(&path, "content on disk").expect("write");

    let mut app = app();
    push_doc(&mut app, &path);
    app.execute(Action::CloseTab);
    assert_eq!(app.documents.len(), 1);

    app.execute(Action::ReopenClosedTab);
    assert_eq!(app.documents.len(), 2);
    assert_eq!(app.doc().unwrap().path.as_deref(), Some(path.as_path()));
    assert_eq!(text(&app), "content on disk");
    assert!(!app.doc().unwrap().modified);
}

#[test]
fn reopen_restores_unsaved_text_and_stays_modified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("data.txt");
    fs::write(&path, "ON DISK").expect("write");

    let mut app = app();
    push_doc(&mut app, &path);
    edit_end(&mut app, " my edits");
    app.execute(Action::CloseTab);
    app.resolve_confirm(ConfirmButton::DontSave);

    app.execute(Action::ReopenClosedTab);
    let doc = app.doc().expect("reopened");
    assert_eq!(doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(
        text(&app),
        "ON DISK my edits",
        "the unsaved edits must come back, not the file's older content"
    );
    assert!(doc.modified, "and it must still be marked unsaved");
}

#[test]
fn reopen_pops_the_most_recent_and_runs_out() {
    let mut app = app();
    type_text(&mut app, "a");
    app.execute(Action::CloseTab);
    app.resolve_confirm(ConfirmButton::DontSave);
    type_text(&mut app, "b");
    app.execute(Action::CloseTab);
    app.resolve_confirm(ConfirmButton::DontSave);
    assert_eq!(app.closed_tabs.len(), 2);

    app.execute(Action::ReopenClosedTab);
    assert_eq!(app.closed_tabs.len(), 1);
    app.execute(Action::ReopenClosedTab);
    assert_eq!(app.closed_tabs.len(), 0);
    app.execute(Action::ReopenClosedTab);
    assert_eq!(app.closed_tabs.len(), 0, "nothing left, nothing invented");
}

#[test]
fn closed_history_is_capped_at_max_recent_files() {
    let mut config = config();
    config.ui.max_recent_files = 2;
    let mut app = App::new_headless(config, (W, H));

    for _ in 0..5 {
        type_text(&mut app, "x");
        app.execute(Action::CloseTab);
        app.resolve_confirm(ConfirmButton::DontSave);
    }
    assert_eq!(app.closed_tabs.len(), 2, "capped");
    // Oldest dropped, so the two survivors are the two most recent closes.
    let kept: Vec<Option<String>> = app
        .closed_tabs
        .iter()
        .map(|c| c.unsaved_text.clone())
        .collect();
    assert_eq!(kept, vec![Some("x".into()), Some("x".into())]);
}

// ---------------------------------------------------- per-document state guard

#[test]
fn cursor_and_selection_are_not_shared_between_documents() {
    // The classic one-Cursor-for-all-tabs bug.
    let mut app = app();
    type_text(&mut app, "first document");
    app.execute(Action::NewFile);
    type_text(&mut app, "second");

    // Select all in the second document only.
    app.execute(Action::SelectAll);
    assert!(app.doc().unwrap().selection.is_some());

    app.execute(Action::PrevTab);
    let first = app.doc().expect("first");
    assert!(
        first.selection.is_none(),
        "the first document must not inherit a selection"
    );
    assert_eq!(
        first.cursor.pos,
        LineCol::new(0, 14),
        "its own caret, where its own typing left it"
    );
}

#[test]
fn scroll_position_is_per_document() {
    let mut app = app();
    for i in 0..60 {
        let now = app.clock;
        app.doc_mut()
            .expect("doc")
            .insert_text(&format!("line {i}\n"), np_core::UndoGroup::Atomic, now)
            .expect("insert");
    }
    app.doc_mut().expect("doc").scroll.top_line = 20;
    let saved = app.doc().expect("doc").scroll.top_line;

    app.execute(Action::NewFile);
    assert_eq!(
        app.doc().expect("doc").scroll.top_line,
        0,
        "a new tab starts at top"
    );
    app.execute(Action::PrevTab);
    assert_eq!(
        app.doc().expect("doc").scroll.top_line,
        saved,
        "and the old tab keeps its own scroll"
    );
}

#[test]
fn three_files_keep_their_own_buffers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    let mut paths = Vec::new();
    for name in ["one.txt", "two.txt", "three.txt"] {
        let p = dir.path().join(name);
        fs::write(&p, format!("start of {name}")).expect("write");
        paths.push(p);
    }
    for p in &paths {
        push_doc(&mut app, p);
        edit_end(&mut app, " EDITED");
    }

    // Walk back through them and check each holds its own text.
    app.active = 1;
    assert_eq!(text(&app), "start of one.txt EDITED");
    app.active = 2;
    assert_eq!(text(&app), "start of two.txt EDITED");
    app.active = 3;
    assert_eq!(text(&app), "start of three.txt EDITED");
    assert_eq!(app.documents[0].buffer.text_string(), "");
}

// ------------------------------------------------------------- wheel cycling

#[test]
fn wheel_cycle_wraps_in_both_directions() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    assert_eq!(app.documents.len(), 3);
    app.active = 0;

    app.cycle_tab_for_test(-1);
    assert_eq!(app.active, 2, "backward past the front wraps to the last");
    app.cycle_tab_for_test(1);
    assert_eq!(app.active, 0, "forward past the end wraps to the first");

    // Over the tab bar itself, via the mouse.
    let y = app.regions.tabs.y;
    app.handle_mouse_at(&mouse(MouseEventKind::ScrollUp, 3, y), Instant::now());
    assert_eq!(app.active, 2);
    app.handle_mouse_at(&mouse(MouseEventKind::ScrollDown, 3, y), Instant::now());
    assert_eq!(app.active, 0);
}

// ------------------------------------------------------ middle-click closing

#[test]
fn middle_click_on_a_clean_tab_closes_it() {
    let mut app = app();
    app.execute(Action::NewFile);
    let x = tab_x(&app, 1);
    let y = app.regions.tabs.y;

    app.handle_mouse_at(
        &mouse(MouseEventKind::Down(MouseButton::Middle), x, y),
        Instant::now(),
    );
    assert_eq!(app.documents.len(), 1);
    assert!(app.confirm.is_none(), "nothing to ask about");
}

#[test]
fn middle_click_on_a_modified_tab_opens_the_confirm() {
    let mut app = app();
    app.execute(Action::NewFile);
    type_text(&mut app, "dirty");
    let x = tab_x(&app, 1);
    let y = app.regions.tabs.y;

    app.handle_mouse_at(
        &mouse(MouseEventKind::Down(MouseButton::Middle), x, y),
        Instant::now(),
    );
    assert_eq!(app.documents.len(), 2, "not closed");
    let confirm = app.confirm.clone().expect("confirm");
    assert_eq!(confirm.op, PendingOp::CloseTab(1));
    assert!(confirm.prompt.contains("Untitled-2"));
}

// ------------------------------------------------------- close others / all

#[test]
fn close_other_tabs_keeps_the_active_one() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    type_text(&mut app, "keep me");

    app.execute(Action::CloseOtherTabs);
    assert_eq!(app.documents.len(), 1);
    assert_eq!(text(&app), "keep me");
    assert!(app.doc().unwrap().modified, "the kept tab is untouched");
}

#[test]
fn close_other_tabs_asks_before_discarding_a_modified_one() {
    let mut app = app();
    type_text(&mut app, "unsaved in tab one");
    app.execute(Action::NewFile);
    let y = app.regions.tabs.y;
    let _ = y;

    app.execute(Action::CloseOtherTabs);
    assert_eq!(app.documents.len(), 2, "the modified tab must not vanish");
    let confirm = app.confirm.clone().expect("confirm");
    assert!(confirm.prompt.contains("Untitled-1"));
    assert_eq!(confirm.op, PendingOp::CloseOthers(1));
}

#[test]
fn close_all_asks_per_modified_document_and_saves_each() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "A").expect("write");
    fs::write(&b, "B").expect("write");

    let mut app = app();
    push_doc(&mut app, &a);
    edit_end(&mut app, "1");
    push_doc(&mut app, &b);
    edit_end(&mut app, "2");

    app.execute(Action::CloseAllTabs);
    let mut rounds = 0;
    while app.confirm.is_some() && rounds < 6 {
        app.resolve_confirm(ConfirmButton::Save);
        rounds += 1;
    }
    assert!(
        rounds >= 2,
        "asked once per modified document, got {rounds}"
    );
    assert_eq!(fs::read_to_string(&a).expect("read"), "A1");
    assert_eq!(fs::read_to_string(&b).expect("read"), "B2");
}

#[test]
fn close_all_with_clean_documents_needs_no_prompt() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    app.execute(Action::CloseAllTabs);
    assert!(app.confirm.is_none());
    assert_eq!(app.documents.len(), 1, "a fresh untitled tab");
    assert_eq!(app.active, 0);
}

// ------------------------------------------------------- document list (FR-U4)

#[test]
fn document_list_opens_and_shows_every_document() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    type_text(&mut app, "dirty");

    app.execute(Action::DocumentList);
    assert_eq!(
        app.mode(),
        np_tui::Mode::Dialog(np_tui::DialogKind::DocumentList)
    );
    let rows = np_tui::ui::dialogs::document_list_rows(&app);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text, "Untitled-1");
    assert_eq!(rows[2].text, "Untitled-3 \u{25cf}", "modified marker shown");
    assert!(rows[2].modified);
}

#[test]
fn document_list_arrows_move_and_enter_activates() {
    let mut app = app();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    app.execute(Action::DocumentList);
    assert_eq!(app.document_list_selected, 2, "starts on the active tab");

    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.document_list_selected, 0);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.document_list_selected, 0, "clamped, not wrapped");

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.active, 0, "Enter activates the highlighted row");
    assert_eq!(app.mode(), np_tui::Mode::Editing);
}

#[test]
fn document_list_escape_cancels_without_switching() {
    let mut app = app();
    app.execute(Action::NewFile);
    let active_before = app.active;
    app.execute(Action::DocumentList);
    press(&mut app, KeyCode::Up);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), np_tui::Mode::Editing);
    assert_eq!(app.active, active_before, "cancelling does not switch");
}

#[test]
fn document_list_swallows_other_keys_so_they_cannot_reach_the_buffer() {
    let mut app = app();
    app.execute(Action::NewFile);
    type_text(&mut app, "clean");
    app.execute(Action::DocumentList);

    // Typing must not insert, and Ctrl+S must not save the wrong document.
    for ch in "sz".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(text(&app), "clean", "no keystroke reached the document");
    assert_eq!(
        app.mode(),
        np_tui::Mode::Dialog(np_tui::DialogKind::DocumentList)
    );
}

// --------------------------------------------------------- status bar follows

#[test]
fn switching_tabs_updates_the_status_bar_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("alpha.txt");
    let b = dir.path().join("beta.txt");
    fs::write(&a, "A").expect("write");
    fs::write(&b, "B").expect("write");

    let mut app = app();
    push_doc(&mut app, &a);
    push_doc(&mut app, &b);

    let segments = np_tui::ui::status_bar::status_segments(&app);
    assert!(
        segments[0].text.contains("beta.txt"),
        "status shows the active document: {:?}",
        segments[0].text
    );

    app.execute(Action::PrevTab);
    let segments = np_tui::ui::status_bar::status_segments(&app);
    assert!(
        segments[0].text.contains("alpha.txt"),
        "and follows the switch: {:?}",
        segments[0].text
    );
}

#[test]
fn modified_status_marker_follows_the_active_tab() {
    let mut app = app();
    type_text(&mut app, "dirty");
    let with_dot = np_tui::ui::status_bar::status_segments(&app);
    assert!(with_dot[0].text.contains('\u{25cf}'));

    app.execute(Action::NewFile);
    let clean = np_tui::ui::status_bar::status_segments(&app);
    assert!(
        !clean[0].text.contains('\u{25cf}'),
        "a clean tab shows no unsaved dot: {:?}",
        clean[0].text
    );
}

// ------------------------------------------------------------------ helpers

trait AppTestHooks {
    /// Close by index through the public confirmation flow.
    fn close_tab_for_test(&mut self, i: usize);
    fn cycle_tab_for_test(&mut self, delta: i64);
}

impl AppTestHooks for App {
    fn close_tab_for_test(&mut self, i: usize) {
        // A clean tab closes with no prompt, which is what these tests assume.
        assert!(!self.documents[i].modified, "test precondition: clean tab");
        self.active = i;
        self.execute(Action::CloseTab);
        assert!(self.confirm.is_none(), "clean close must not prompt");
    }

    fn cycle_tab_for_test(&mut self, delta: i64) {
        if delta > 0 {
            self.execute(Action::NextTab);
        } else {
            self.execute(Action::PrevTab);
        }
    }
}
