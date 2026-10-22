//! S20: the docked service strips were not operable by pointer.
//!
//! Reported as: "no cursor in the search string field", "Find in files has no start button, but
//! by pressing enter find was started -- that is not obvious", "after find is done the results
//! can't be scrolled or selected by mouse", and "all Search and Find tool areas should have a set
//! of buttons including Close".
//!
//! # The bug behind three of the four Find-in-Files complaints
//!
//! `hit_test` opened with `if !app.regions.contains(x, y) { return Hit::Outside }`. Since S19
//! `regions.editor` is the band *minus* the docked panel -- that is how the panel gets its rows --
//! so the guard rejected every panel row below `editor`'s bottom edge. The `Hit::FindPanel` arm
//! below it was unreachable dead code: clicks on result rows, on the field rows, and on the
//! Search and Close words all resolved to `Outside` and did nothing at all.
//!
//! The panel is now claimed before the bounds guard. It is still not modal, which is why the arm
//! claims only its own rectangle and lets every other row fall through unchanged.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::Action;
use np_tui::app::{App, Mode};
use np_tui::fif::{self, FifField};
use np_tui::ui::{find_bar, find_in_files};

const W: u16 = 90;
const H: u16 = 24;

fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn wheel(app: &mut App, x: u16, y: u16, up: bool) {
    app.handle_mouse(&MouseEvent {
        kind: if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        },
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn char(app: &mut App, c: char) {
    let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

/// A document whose lines all contain `needle`, so an in-document search always has a hit.
fn doc_app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    let text: String = (0..50)
        .map(|i| format!("line {i} needle"))
        .collect::<Vec<_>>()
        .join("\n");
    app.open_document(Document::from_decoded(
        "/tmp/s20.txt".into(),
        &text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

/// A directory holding one file with more hits than the results area can show at once, so
/// scrolling has somewhere to go.
fn search_dir() -> std::path::PathBuf {
    // The name is unique per *call*, not per process. Tests in this binary run in parallel and
    // several search this tree while asserting an exact hit count; rewriting one shared file let a
    // walker read it half-written and report fewer hits than expected -- a failure that appeared
    // only under load and passed when the test ran alone. A fresh directory per search makes each
    // search independent of every other, which is what a fixture has to be.
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(format!("/tmp/notepad-tui-s20-{}-{n}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return dir;
    }
    let text: String = (0..40)
        .map(|i| format!("needle {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(dir.join("hits.txt"), text);
    dir
}

/// A Find-in-Files panel with a query typed and a search finished.
///
/// `App::new_headless` sets `sync_search`, so `fif::start` runs the walk on the calling thread;
/// the messages still have to be drained before the panel can show them.
fn fif_app() -> App {
    let mut app = doc_app();
    app.execute(Action::FindInFiles);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    for c in "needle".chars() {
        char(&mut app, c);
    }
    app.fif.dir = search_dir().to_string_lossy().into_owned();
    assert!(fif::start(&mut app).is_some(), "the search should start");
    fif::drain(&mut app, 100_000);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    // `fif::start` recomputes `find_panel_height` from `regions.band`, which only refreshes on a
    // layout pass. Asserting here rather than downstream means a panel that came up too short to
    // show any result row fails at the fixture instead of turning a later arithmetic assertion
    // into a mystery.
    assert!(
        find_in_files::result_rows(app.find_panel_height) > 0,
        "the panel should come up tall enough to show results (height {})",
        app.find_panel_height
    );
    app
}

/// The column of a command word on the toggles row.
fn command_x(app: &App, needle: &str) -> u16 {
    let panel = find_in_files::panel_rect(app);
    let (spans, _) = find_in_files::command_spans(app);
    let (col, _) = spans
        .iter()
        .find(|(_, _, cmd)| cmd.contains(needle))
        .map(|(col, w, _)| (*col, *w))
        .unwrap();
    panel.x + col + 1
}

/// The column of the Close word on the Find strip's options row.
fn find_close_x(app: &App) -> u16 {
    let strip = find_bar::strip_rect(app);
    let (_, spans) = find_bar::options_spans(app);
    let (col, _) = spans
        .iter()
        .find(|(_, _, hit)| *hit == find_bar::OptionHit::Button(find_bar::CLOSE_INDEX))
        .map(|(col, w, _)| (*col, *w))
        .unwrap();
    strip.x + col + 1
}

// ---------------------------------------------------------------- cursor

#[test]
fn the_find_strip_shows_a_terminal_cursor_in_the_query_field() {
    let mut app = doc_app();
    // No strip, no cursor: the terminal cursor belongs to the document caret.
    assert!(!find_bar::is_open(&app));
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(find_bar::is_open(&app));

    let empty = find_bar::cursor_position(&app).expect("an empty query still has a caret");
    for c in "abc".chars() {
        char(&mut app, c);
    }
    let typed = find_bar::cursor_position(&app).expect("a typed query has a caret");
    // Three characters typed moves the caret three columns right, on the same row.
    assert_eq!(typed.0, empty.0 + 3);
    assert_eq!(typed.1, empty.1);
}

#[test]
fn the_cursor_follows_tab_into_the_replacement_field() {
    let mut app = doc_app();
    app.execute(Action::Replace);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let query = find_bar::cursor_position(&app).expect("query caret");
    key(&mut app, KeyCode::Tab);
    let repl = find_bar::cursor_position(&app).expect("replacement caret");
    assert_eq!(repl.1, query.1 + 1, "the replacement field is one row down");
}

#[test]
fn a_focused_button_shows_no_cursor_because_it_has_no_text_position() {
    let mut app = doc_app();
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    // Walk focus off the fields and onto a checkbox or button.
    for _ in 0..8 {
        key(&mut app, KeyCode::Tab);
    }
    assert!(
        !matches!(
            app.focused_id(),
            Some(np_tui::ui::framework::FocusId::Field(_))
        ),
        "focus should have left the text fields"
    );
    assert_eq!(find_bar::cursor_position(&app), None);
}

#[test]
fn the_find_in_files_panel_shows_a_cursor_only_while_a_field_is_being_edited() {
    let mut app = fif_app();
    // After a finished search the keyboard drives the results list, so there is no caret to show.
    app.fif.editing = false;
    assert_eq!(find_in_files::cursor_position(&app), None);

    app.fif.editing = true;
    app.fif.editing_field = FifField::Dir;
    let dir = find_in_files::cursor_position(&app).expect("the dir field has a caret");
    app.fif.editing_field = FifField::Query;
    let query = find_in_files::cursor_position(&app).expect("the query field has a caret");
    // Dir is the second field row, Query the first.
    assert_eq!(dir.1, query.1 + 1);
}

#[test]
fn the_terminal_cursor_is_pointed_at_the_strip_not_the_document_caret() {
    let mut app = doc_app();
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let strip = find_bar::cursor_position(&app).expect("strip caret");
    assert_eq!(
        np_tui::event::terminal_cursor(&app),
        Some(strip),
        "the one real terminal cursor must follow the strip that owns the keyboard"
    );
}

// ---------------------------------------------------------------- Find strip Close

#[test]
fn clicking_close_on_the_find_strip_closes_it() {
    let mut app = doc_app();
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(find_bar::is_open(&app));
    let x = find_close_x(&app);
    let y = find_bar::options_row(&app);
    click(&mut app, x, y);
    assert!(!find_bar::is_open(&app), "Close should take the strip away");
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn close_is_not_reachable_by_tab_because_it_is_not_a_search_control() {
    let mut app = doc_app();
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let items = app.dialog_focus_items();
    assert!(
        !items
            .iter()
            .any(|it| it.id == np_tui::ui::framework::FocusId::CloseStrip),
        "Tab must never park the caret on Close in the middle of a query"
    );
    // And the keyboard can still reach every real control.
    assert!(!items.is_empty());
}

// ---------------------------------------------------------------- Find in Files commands

#[test]
fn every_row_of_the_panel_is_hit_tested_as_the_panel() {
    let app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    // Rows below `regions.editor`'s bottom edge used to answer `Outside`, which is what made the
    // whole panel dead to the pointer.
    assert!(
        panel.bottom() > app.regions.editor.bottom(),
        "this test only means something if the panel extends below the editor region"
    );
    for row in panel.y..panel.bottom() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: panel.x + 5,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            np_tui::mouse::hit_test(&ev, &app),
            np_tui::mouse::Hit::FindPanel,
            "row {row} of the panel must belong to the panel"
        );
    }
}

#[test]
fn clicking_search_starts_the_walk() {
    let mut app = doc_app();
    app.execute(Action::FindInFiles);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    for c in "needle".chars() {
        char(&mut app, c);
    }
    app.fif.dir = search_dir().to_string_lossy().into_owned();
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(app.fif.hits.is_empty(), "nothing searched yet");

    let panel = find_in_files::panel_rect(&app);
    let x = command_x(&app, "Search");
    click(&mut app, x, panel.y + 3);
    fif::drain(&mut app, 100_000);
    assert_eq!(app.fif.hits.len(), 40, "the search should have run");
    // Enter already ran the query, so the keyboard moves to the results rather than re-running.
    assert!(!app.fif.editing);
}

#[test]
fn clicking_close_on_the_panel_takes_the_panel_away() {
    let mut app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    let x = command_x(&app, "Close");
    click(&mut app, x, panel.y + 3);
    assert_eq!(app.find_panel_height, 0);
    assert_ne!(app.mode(), Mode::FindPanel);
}

#[test]
fn clicking_a_field_row_points_the_keyboard_at_that_field() {
    let mut app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    app.fif.editing_field = FifField::Query;
    click(&mut app, panel.x + 3, panel.y + 2);
    assert_eq!(app.fif.editing_field, FifField::Dir);
    assert!(app.fif.editing, "a clicked field should accept typing");
}

#[test]
fn the_command_words_win_over_the_glob_row_they_share() {
    let app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    // The commands live on the glob field's row. A click on a command must run the command, not
    // focus the glob field.
    let x = command_x(&app, "Search");
    assert_eq!(
        find_in_files::hit_command(&app, x, panel.y + 3),
        Some("> Search")
    );
    assert_eq!(
        find_in_files::field_at_row(&app, panel.y + 3),
        Some(FifField::Glob),
        "the row is also a field row, which is what made the ordering matter"
    );
}

#[test]
fn clicking_a_result_row_selects_that_row() {
    let mut app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    let row = panel.y + 4 + 2;
    let want = find_in_files::row_index_at(&app, row).expect("a result row");
    click(&mut app, panel.x + 10, row);
    assert_eq!(
        app.fif.selected,
        want,
        "hits={} scroll={} count={:?} cell={:?} mode={:?}",
        app.fif.hits.len(),
        app.fif.scroll,
        app.click.count,
        app.click.cell,
        app.mode()
    );
}

#[test]
fn the_wheel_scrolls_the_results_list() {
    let mut app = fif_app();
    assert_eq!(app.fif.hits.len(), 40, "need enough hits to scroll");
    let panel = find_in_files::panel_rect(&app);
    let row = panel.y + 5;
    wheel(&mut app, panel.x + 10, row, false);
    assert!(app.fif.scroll > 0, "a wheel down should move the view");
    let scrolled = app.fif.scroll;
    wheel(&mut app, panel.x + 10, row, true);
    assert!(app.fif.scroll < scrolled, "a wheel up should come back");
}

#[test]
fn scrolling_the_view_does_not_move_the_selection() {
    let mut app = fif_app();
    app.fif.selected = 3;
    let panel = find_in_files::panel_rect(&app);
    wheel(&mut app, panel.x + 10, panel.y + 5, false);
    assert_eq!(app.fif.selected, 3, "looking around is not choosing");
}

#[test]
fn the_results_stop_scrolling_at_the_end() {
    let mut app = fif_app();
    let panel = find_in_files::panel_rect(&app);
    for _ in 0..500 {
        wheel(&mut app, panel.x + 10, panel.y + 5, false);
    }
    let max = app.fif.scroll;
    let visible = find_in_files::result_rows(app.find_panel_height);
    // Pinned before the sum: `result_rows` answers 0 for a panel squeezed below `HEADER_ROWS`,
    // and this assertion once passed a degenerate panel by reading `max + 0 == hits.len()`. A
    // scroll test with no visible rows is testing nothing, so say so explicitly.
    assert!(
        visible > 0,
        "the panel must show result rows to scroll them (height {})",
        app.find_panel_height
    );
    assert_eq!(max + visible, app.fif.hits.len());
    wheel(&mut app, panel.x + 10, panel.y + 5, false);
    assert_eq!(app.fif.scroll, max, "no scrolling past the last hit");
}

// ---------------------------------------------------------------- menu dismissal

#[test]
fn a_click_on_the_text_closes_an_open_submenu_and_still_moves_the_caret() {
    let mut app = doc_app();
    app.execute(Action::MenuBar(np_tui::actions::MenuId::View));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert!(matches!(app.mode(), Mode::Menu { .. }));
    let text = app.geometry.text_area;
    click(&mut app, text.x + 3, text.y + 1);
    assert_eq!(app.mode(), Mode::Editing, "the menu should be gone");
}

#[test]
fn hovering_and_scrolling_over_the_text_leave_an_open_menu_alone() {
    let mut app = doc_app();
    app.execute(Action::MenuBar(np_tui::actions::MenuId::View));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let text = app.geometry.text_area;
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Moved,
        column: text.x + 3,
        row: text.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    wheel(&mut app, text.x + 3, text.y + 1, false);
    assert!(
        matches!(app.mode(), Mode::Menu { .. }),
        "only a press dismisses a menu"
    );
}
