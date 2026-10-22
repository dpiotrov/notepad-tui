//! S23: two state-leak bugs from real use.
//!
//! # The highlights that followed you to another tab
//!
//! `find.marks` is a `Vec<Match>` -- line and column pairs with no document id attached. It was
//! computed against whichever buffer was active when the search ran and painted over whatever
//! buffer was active when the frame was drawn, so a search in file A highlighted arbitrary
//! characters in file B. `find.current` had the same problem in a smaller package: a stale match
//! is still painted as "the current one" by `match_candidates`.
//!
//! The fix re-derives the overlay in `load_active_view`, the one function every tab switch goes
//! through. Recomputing rather than clearing is deliberate: the needle survives the switch, so
//! `F3` in the new file keeps working, which is what Notepad++ does and what a user mid-search
//! expects.
//!
//! # The closed tab that came back
//!
//! `close_index` removed the tab and never raised `session_dirty`. A clean tab sets no `modified`
//! bit, so nothing else in the path queued a write, the exit flush saw a clean flag and wrote
//! nothing, and the previous session -- which still listed the closed file -- survived on disk.
//! Closing is a session-relevant change; so is opening, which is why both directions set the flag
//! now.
//!
//! # Why these tests look at the screen
//!
//! The symptom is pixels. Asserting on `find.marks` alone would have passed while the renderer
//! painted a stale list, so the leak tests read cell backgrounds out of a rendered buffer.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol, LineCol};
use np_tui::app::App;

const W: u16 = 100;
const H: u16 = 30;

/// The match-highlight background the default theme paints.
const MATCH_BG: (u8, u8, u8) = (67, 76, 94);

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let mut ev = KeyEvent::new(code, mods);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn type_str(app: &mut App, text: &str) {
    for ch in text.chars() {
        key(app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
}

fn click(app: &mut App, x: u16, y: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_mouse(&MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
}

/// Click the middle of tab `i`'s label, the way the user does.
fn click_tab(app: &mut App, i: usize) {
    let span = np_tui::ui::tab_bar::spans(app)
        .into_iter()
        .find(|s| s.index == i)
        .expect("tab span");
    click(app, span.x + span.width / 2, app.regions.tabs.y);
}

fn named(dir: &Path, name: &str, text: &str) -> Document {
    Document::from_decoded(dir.join(name), text, EncodingId::Utf8, Eol::Lf, false)
}

/// A two-tab app: A holds `zulu` twice, B holds it nowhere.
///
/// `new_headless` starts with an untitled tab, which is removed here so the tab indices in the
/// assertions mean what they say.
fn two_tabs(dir: &Path) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.documents.clear();
    app.active = 0;
    app.open_document(named(dir, "aaa.txt", "alpha zulu one\nsecond zulu line\n"));
    app.open_document(named(
        dir,
        "bbb.txt",
        "nothing interesting here\nat all at all\n",
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn bg(buf: &ratatui::buffer::Buffer, x: u16, y: u16) -> Option<(u8, u8, u8)> {
    match buf[(x, y)].style().bg {
        Some(ratatui::style::Color::Rgb(r, g, b)) => Some((r, g, b)),
        _ => None,
    }
}

/// Every cell on screen painted with the match background.
fn lit_cells(app: &mut App) -> Vec<(u16, u16)> {
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            if bg(&buf, x, y) == Some(MATCH_BG) {
                out.push((x, y));
            }
        }
    }
    out
}

fn row_text(app: &mut App, y: u16) -> String {
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    (0..W)
        .map(|c| buf[(c, y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

// -------------------------------------------------- bug 1: the overlay follows the document

#[test]
fn searching_in_one_tab_lights_up_that_tab() {
    // Control case: the fixture and the pixel probe really can see a highlight. Without this
    // the leak test below could pass for the wrong reason.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = two_tabs(dir.path());
    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "zulu");

    assert_eq!(app.find.marks.len(), 2, "two occurrences in tab A");
    assert!(
        !lit_cells(&mut app).is_empty(),
        "the search must actually paint something"
    );
}

#[test]
fn a_search_in_one_tab_leaves_the_next_tab_unhighlighted() {
    // The reported bug: search in A, close the strip, click tab B, A's highlights are still
    // painted -- over text that matches nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = two_tabs(dir.path());
    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "zulu");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    click_tab(&mut app, 1);
    assert_eq!(app.active, 1, "setup: must be on tab B");
    assert!(
        row_text(&mut app, 2).contains("nothing interesting"),
        "tab B's own text is on screen: {:?}",
        row_text(&mut app, 2)
    );
    assert!(
        lit_cells(&mut app).is_empty(),
        "tab A's highlights must not paint on tab B"
    );
}

#[test]
fn marks_are_recomputed_for_the_new_document_not_cleared() {
    // The other half of the rule: B is not blanked, it gets *its own* marks. The needle "all"
    // occurs in B's text and nowhere in A's.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(Config::default(), (W, H));
    app.documents.clear();
    app.active = 0;
    app.open_document(named(dir.path(), "aaa.txt", "first line here\n"));
    app.open_document(named(dir.path(), "bbb.txt", "all of it\nall again\n"));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);

    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "all");
    assert_eq!(app.find.marks.len(), 0, "no occurrence in tab A");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    click_tab(&mut app, 1);
    assert_eq!(
        app.find.marks.len(),
        2,
        "the new document's own occurrences are marked"
    );
    assert_eq!(app.find.needle, "all", "the needle survived the switch");
    assert!(
        !lit_cells(&mut app).is_empty(),
        "and they are painted on the new tab"
    );
}

#[test]
fn a_stale_current_match_is_not_painted_after_a_switch() {
    // `find.current` is a Match too, and `match_candidates` adds it even when marking is off,
    // so refreshing only `marks` would still leak one highlighted span onto the next tab.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = two_tabs(dir.path());
    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "zulu");
    // The caret starts inside the first `zulu`, so the first FindNext answers with it and the
    // second moves on. Either way `current` is set -- all this test needs.
    app.execute(np_tui::actions::Action::FindNext);
    app.execute(np_tui::actions::Action::FindNext);
    assert!(
        app.find.current.is_some(),
        "setup: a current match exists in tab A"
    );
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    click_tab(&mut app, 1);
    assert_eq!(app.find.current, None, "a match from A is not current in B");
    assert!(lit_cells(&mut app).is_empty(), "and it is not painted");
}

#[test]
fn find_in_files_opening_a_hit_highlights_the_right_document() {
    // `fif` adopts a loaded tab through `load_active_view`, so the same rule has to hold when
    // the switch comes from a background load rather than a click.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = two_tabs(dir.path());
    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "zulu");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    // `fif`'s background load adopts its placeholder by assigning the tab and calling
    // `load_active_view`, so drive that exact sequence.
    app.active = 1;
    app.load_active_view();
    assert!(
        app.find.marks.is_empty(),
        "B has no occurrence of the needle from A"
    );
    assert!(lit_cells(&mut app).is_empty(), "and nothing is painted");

    // Re-opening an already-open path activates its tab, which is the other route into
    // `load_active_view` that does not come from a tab-bar click.
    app.open_path(&dir.path().join("aaa.txt"));
    assert_eq!(app.active, 0, "the already-open path activates its tab");
    assert_eq!(
        app.find.marks.len(),
        2,
        "A's own occurrences are marked again"
    );
    app.open_path(&dir.path().join("bbb.txt"));
    assert_eq!(app.active, 1, "and back to B");
    assert!(lit_cells(&mut app).is_empty(), "B stays unhighlighted");
}

// ------------------------------------------------- bug 2: the tab list reaches the session

fn session_text(dir: &Path) -> String {
    fs::read_to_string(dir.join("session.toml")).unwrap_or_default()
}

#[test]
fn closing_a_clean_tab_is_written_to_the_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "alpha\n").expect("write");
    fs::write(&b, "beta\n").expect("write");

    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(dir.path().join("session.toml"));
    app.documents.clear();
    app.active = 0;
    for path in [&a, &b] {
        let loaded = np_core::load(path, None).expect("load");
        app.open_document(Document::from_loaded(&loaded));
    }
    app.run_timers(std::time::Instant::now());
    assert!(
        session_text(dir.path()).contains("b.txt"),
        "both tabs saved"
    );

    // Tab B is active and clean, so this closes without a prompt.
    app.execute(np_tui::actions::Action::CloseTab);
    assert_eq!(app.documents.len(), 1, "one tab closed");
    assert!(
        app.session_dirty,
        "closing a tab owes a session write, dirty or not"
    );

    app.flush_session();
    let saved = session_text(dir.path());
    assert!(saved.contains("a.txt"), "the kept tab is there: {saved}");
    assert!(!saved.contains("b.txt"), "the closed tab is gone: {saved}");
}

#[test]
fn the_closed_tab_stays_closed_through_a_restart() {
    // The user-visible symptom, end to end: close, exit, start again.
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, "alpha\n").expect("write");
    fs::write(&b, "beta\n").expect("write");
    let session = dir.path().join("session.toml");

    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(session.clone());
    app.documents.clear();
    app.active = 0;
    for path in [&a, &b] {
        let loaded = np_core::load(path, None).expect("load");
        app.open_document(Document::from_loaded(&loaded));
    }
    // Write the starting state out, the way an earlier session would have.
    app.flush_session();
    assert!(
        session_text(dir.path()).contains("b.txt"),
        "setup: both tabs on disk"
    );

    app.execute(np_tui::actions::Action::CloseTab);
    assert_eq!(app.documents.len(), 1, "one tab closed");
    // The event loop's exit path: autosave, then flush. Nothing else happens in between, so a
    // close that raises no flag writes nothing and the old file survives untouched.
    app.flush_session();

    let loaded = np_core::Session::load(&session).expect("load session");
    let mut fresh = App::new_headless(Config::default(), (W, H));
    fresh.session_path = Some(session);
    fresh.restore_session(&loaded);
    let paths: Vec<String> = fresh
        .documents
        .iter()
        .map(|d| {
            d.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(paths.len(), 1, "one tab restored: {paths:?}");
    assert!(
        paths[0].ends_with("a.txt"),
        "the closed file must not come back: {paths:?}"
    );
}

#[test]
fn closing_every_tab_is_written_too() {
    // `close_many` removes tabs without going through `close_index`. It carries the same
    // bookkeeping, and this pins the behaviour rather than the exact line that sets the flag:
    // the batch's clean path also reaches `close_index` for tabs it removes under a prompt,
    // so a mutation of the batch's own call alone is not observable here. What matters is that
    // no batch-close route can leave a stale tab list on disk.
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.txt");
    fs::write(&a, "alpha\n").expect("write");

    let b = dir.path().join("b.txt");
    fs::write(&b, "beta\n").expect("write");

    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(dir.path().join("session.toml"));
    app.documents.clear();
    app.active = 0;
    for path in [&a, &b] {
        let loaded = np_core::load(path, None).expect("load");
        app.open_document(Document::from_loaded(&loaded));
    }
    // Both tabs on disk first, so the batch has to *remove* something from the file rather
    // than merely fail to add it.
    app.flush_session();
    let both = session_text(dir.path());
    assert!(
        both.contains("a.txt") && both.contains("b.txt"),
        "setup: {both}"
    );

    app.execute(np_tui::actions::Action::CloseAllTabs);
    assert!(app.session_dirty, "Close All owes a session write");
    app.flush_session();
    let saved = session_text(dir.path());
    assert!(
        !saved.contains("a.txt") && !saved.contains("b.txt"),
        "a batch close must empty the recorded tab list: {saved}"
    );
}

#[test]
fn opening_a_tab_is_written_to_the_session() {
    // The symmetric rule: a tab the user opened has to appear, or the two directions disagree
    // about what the tab list is.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(dir.path().join("session.toml"));
    app.documents.clear();
    app.active = 0;
    app.open_document(named(dir.path(), "fresh.txt", "hello\n"));
    assert!(app.session_dirty, "a new tab owes a session write");
    app.flush_session();
    assert!(session_text(dir.path()).contains("fresh.txt"));
}

#[test]
fn constructing_an_app_owes_no_session_write() {
    // The constraint that keeps the above honest: a read-only look at the editor must not
    // create a session file.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(dir.path().join("session.toml"));
    for _ in 0..20 {
        app.run_timers(std::time::Instant::now());
    }
    assert_eq!(app.session_writes, 0, "no change, no write");
    assert!(
        !dir.path().join("session.toml").exists(),
        "an idle session must not create a file"
    );
}

#[test]
fn a_query_survives_the_switch_and_finds_in_the_new_file() {
    // What the user expects after the fix: the needle is still there and F3 works in the file
    // that is now on screen.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = two_tabs(dir.path());
    click_tab(&mut app, 0);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut app, "zulu");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    click_tab(&mut app, 1);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert_eq!(app.find.needle, "zulu", "the needle is still in the field");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    // And in a document that does contain the needle, F3 still lands on a match.
    click_tab(&mut app, 0);
    let before = app.cursor_pos();
    app.execute(np_tui::actions::Action::FindNext);
    assert_ne!(
        app.cursor_pos(),
        before,
        "FindNext moved the caret to a match"
    );
    let _ = LineCol::new(0, 0);
}

#[test]
fn closing_the_other_tabs_is_written_to_the_session() {
    // `close_many` removes tabs without going through `close_index`, so it carries the same
    // bookkeeping. Three tabs with one kept is the route where that call is the only thing
    // raising the flag: the batch removes the others in its own loop rather than delegating.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths: Vec<PathBuf> = ["t1.txt", "t2.txt", "t3.txt"]
        .iter()
        .map(|name| {
            let path = dir.path().join(name);
            fs::write(&path, "x\n").expect("write");
            path
        })
        .collect();

    let mut app = App::new_headless(Config::default(), (W, H));
    app.session_path = Some(dir.path().join("session.toml"));
    app.documents.clear();
    app.active = 0;
    for path in &paths {
        let loaded = np_core::load(path, None).expect("load");
        app.open_document(Document::from_loaded(&loaded));
    }
    app.flush_session();
    let all = session_text(dir.path());
    assert!(
        all.contains("t1.txt") && all.contains("t2.txt") && all.contains("t3.txt"),
        "setup: three tabs on disk: {all}"
    );

    // Active tab is the last, so Close Other Tabs keeps t3 and drops t1 and t2.
    app.execute(np_tui::actions::Action::CloseOtherTabs);
    assert_eq!(app.documents.len(), 1, "one tab kept");
    assert!(app.session_dirty, "closing tabs owes a session write");
    app.flush_session();

    let saved = session_text(dir.path());
    assert!(saved.contains("t3.txt"), "the kept tab is there: {saved}");
    assert!(!saved.contains("t1.txt"), "t1 is gone: {saved}");
    assert!(!saved.contains("t2.txt"), "t2 is gone: {saved}");
}
