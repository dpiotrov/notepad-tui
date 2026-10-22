//! S21: the strips showed no sign of which field had the keyboard.
//!
//! Reported as: "In Find in Files still no cursor in search string", "the `[ ]` of match case,
//! word, gitignore don't react to mouse click and can't be reached any other way", and "same
//! cursor and focus evidence problem in Search and in Replace".
//!
//! # The cursor was fixed in S20 and still did not work
//!
//! S20 added `event::terminal_cursor`, which decides which widget gets the terminal's one real
//! cursor, and tested it thoroughly. `draw_if_dirty` never called it. It still asked
//! `editor_view::caret_position` directly, so the strips carried on showing no caret and every one
//! of those tests passed.
//!
//! The lesson is about what a test is allowed to assert. Mine asserted that a helper *returns* the
//! right cell. The user's complaint was about what the screen *does*, and the only test that could
//! have caught this drives the frame that the terminal actually sees.
//! `draw_if_dirty_points_the_real_cursor_at_the_strip` does that now.
//!
//! # Focus needs a ground, not only a cursor
//!
//! Even with a working cursor, one cell is weak evidence: it vanishes when the terminal loses
//! focus, some terminals draw it under the cell, and on an empty field it is a block the size of
//! nothing. The user asked for "a little different background on the focused field" and correctly
//! pointed out that with no cursor there is otherwise "no any possibility to know what field is in
//! focus". Focused fields now get a ground as well.
//!
//! # The toggles had no address
//!
//! The three boxes were one opaque format string. Nothing could be clicked, nothing could be tabbed
//! to, and the only bindings were `Ctrl+C` / `Ctrl+W` / `Ctrl+G` printed nowhere. `FifToggle` is
//! now an address shared by the painter, the hit-tester and the keyboard, and the boxes sit in the
//! Tab cycle with Space flipping whichever one is highlighted.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::Action;
use np_tui::app::App;
use np_tui::fif::{FifField, FifToggle};
use np_tui::ui::{find_bar, find_in_files};
use ratatui::style::Color;

const W: u16 = 100;
const H: u16 = 24;

// Both strip colors are read from the app's palette rather than hardcoded.
//
// These used to be literals -- `(34, 38, 45)` for the chrome band and `(52, 58, 68)` for a focused
// field -- which is a fair test only while the renderer also hardcoded them. Now the band and the
// focus ground are derived from the active theme, so a literal would fail on any theme other than
// the default and would pass for the wrong reason if the derivation changed shape. The property
// under test is the *relationship*: a focused field sits on a ground the surrounding chrome does
// not have, and that is asserted against the palette the app is actually running with.
fn chrome(app: &App) -> Color {
    let (r, g, b) = app.palette().chrome_bg;
    Color::Rgb(r, g, b)
}

fn focus(app: &App) -> Color {
    let (r, g, b) = app.palette().field_focus_bg;
    Color::Rgb(r, g, b)
}

fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn char(app: &mut App, c: char) {
    key(app, KeyCode::Char(c));
}

fn base_app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/s21.txt".into(),
        "hello needle\nworld\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn fif_app() -> App {
    let mut app = base_app();
    app.execute(Action::FindInFiles);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

/// Where `draw_if_dirty` put the terminal's real cursor.
///
/// Read off the backend, not the frame: `Frame::cursor_position` is write-only in ratatui 0.30,
/// and the question this test asks is where the cursor ended up, which is a backend fact.
fn cursor(terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>) -> (u16, u16) {
    use ratatui::backend::Backend;
    let pos = terminal
        .backend_mut()
        .get_cursor_position()
        .expect("cursor position");
    (pos.x, pos.y)
}

/// The background of every cell on a screen row.
fn row_backgrounds(app: &mut App, y: u16) -> Vec<Option<Color>> {
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    (0..W).map(|x| buf[(x, y)].style().bg).collect()
}

/// The columns whose background is exactly `color`.
fn columns_with_bg(app: &mut App, y: u16, color: Color) -> Vec<u16> {
    row_backgrounds(app, y)
        .into_iter()
        .enumerate()
        .filter(|(_, bg)| *bg == Some(color))
        .map(|(x, _)| x as u16)
        .collect()
}

// ------------------------------------------------------------ the real cursor

#[test]
fn draw_if_dirty_points_the_real_cursor_at_the_strip() {
    // The regression test for the S20 miss. This drives `draw_if_dirty` -- the frame the terminal
    // actually sees -- rather than asking `terminal_cursor` what it thinks. Reverting the one-line
    // wiring inside `draw_if_dirty` fails this and nothing else.
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).expect("terminal");
    let mut app = base_app();
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let want = find_bar::cursor_position(&app).expect("the query field has a caret");

    app.dirty = true;
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
    assert_eq!(
        cursor(&mut terminal),
        want,
        "the terminal's cursor must be where the strip says, not at the document caret"
    );
}

#[test]
fn the_document_caret_returns_when_no_strip_is_open() {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).expect("terminal");
    let mut app = base_app();
    let doc = np_tui::ui::editor_view::caret_position(&app).expect("document caret");
    app.dirty = true;
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
    assert_eq!(cursor(&mut terminal), (doc.col as u16, doc.line as u16));

    // And giving it back when the strip closes.
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    np_tui::find::close_find(&mut app);
    app.dirty = true;
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
    assert_eq!(
        cursor(&mut terminal),
        (doc.col as u16, doc.line as u16),
        "closing the strip must hand the cursor back to the document"
    );
}

// ------------------------------------------------------------ focus ground: Find

#[test]
fn the_focused_find_field_gets_a_ground_the_chrome_does_not_have() {
    let mut app = base_app();
    app.execute(Action::Find);
    let strip = find_bar::strip_rect(&app);
    let want = focus(&app);
    let focused = columns_with_bg(&mut app, strip.y, want);
    assert!(
        focused.len() > W as usize / 2,
        "the focused query row should be grounded across its width, got {}",
        focused.len()
    );
    // The options row is chrome, not a field, and must stay distinguishable.
    let opts_y = find_bar::options_row(&app);
    let want = focus(&app);
    let opts = columns_with_bg(&mut app, opts_y, want);
    assert!(opts.is_empty(), "the options row must not look focused");
}

#[test]
fn only_one_find_field_is_grounded_at_a_time() {
    let mut app = base_app();
    app.execute(Action::Replace);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let strip = find_bar::strip_rect(&app);
    let want = focus(&app);
    let query = columns_with_bg(&mut app, strip.y, want);
    let want = focus(&app);
    let repl = columns_with_bg(&mut app, strip.y + 1, want);
    assert!(!query.is_empty() && repl.is_empty());

    key(&mut app, KeyCode::Tab);
    let want = focus(&app);
    let query = columns_with_bg(&mut app, strip.y, want);
    let want = focus(&app);
    let repl = columns_with_bg(&mut app, strip.y + 1, want);
    assert!(query.is_empty(), "the query must lose its ground");
    assert!(!repl.is_empty(), "the replacement must gain one");
}

// ------------------------------------------------------------ focus ground: FiF

#[test]
fn the_fif_panel_grounds_only_the_field_being_edited() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    for (row, field) in [
        (1u16, FifField::Query),
        (2, FifField::Dir),
        (3, FifField::Glob),
    ] {
        app.fif.editing = true;
        app.fif.editing_field = field;
        app.fif.focused_toggle = None;
        let want = focus(&app);
        let grounded = columns_with_bg(&mut app, p.y + row, want);
        assert!(!grounded.is_empty(), "{field:?} should be grounded");
        // The other two field rows must not be.
        for other in [1u16, 2, 3] {
            if other == row {
                continue;
            }
            let want = focus(&app);
            let wrong = columns_with_bg(&mut app, p.y + other, want);
            assert!(
                wrong.is_empty(),
                "row {other} must not look focused while {field:?} is"
            );
        }
    }
}

#[test]
fn a_focused_glob_field_does_not_claim_the_toggle_boxes() {
    // The glob shares its row with the three boxes, and the first implementation painted the whole
    // row -- boxes included -- so a focused `Files` field appeared to own the checkboxes.
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    app.fif.editing = true;
    app.fif.editing_field = FifField::Glob;
    let want = focus(&app);
    let grounded = columns_with_bg(&mut app, p.y + 3, want);
    let first_box = first_toggle_column(&app);
    assert!(!grounded.is_empty(), "the glob field should be grounded");
    assert!(
        grounded.iter().all(|x| *x < first_box),
        "the ground must stop before the first box at col {first_box}, reached col {:?}",
        grounded.iter().max()
    );
}

#[test]
fn no_ground_appears_while_the_results_list_has_the_keyboard() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    app.fif.editing = false;
    app.fif.focused_toggle = None;
    for row in [1u16, 2, 3] {
        let focused = focus(&app);
        let band = chrome(&app);
        assert!(
            columns_with_bg(&mut app, p.y + row, focused).is_empty(),
            "no field may look focused while arrows drive the results"
        );
        // Not merely "not the focus colour" -- the row must read as ordinary chrome, so a third
        // half-lit state cannot slip in and leave the user guessing again.
        assert!(
            columns_with_bg(&mut app, p.y + row, band).len() > (W / 2) as usize,
            "row {row} should read as ordinary chrome"
        );
    }
}

// ------------------------------------------------------------ toggles: reachable

#[test]
fn tab_walks_the_fields_and_then_the_toggles() {
    let mut app = fif_app();
    let mut seen = Vec::new();
    for _ in 0..6 {
        seen.push((app.fif.editing_field, app.fif.focused_toggle));
        key(&mut app, KeyCode::Tab);
    }
    let fields: Vec<FifField> = seen.iter().map(|(f, _)| *f).collect();
    let toggles: Vec<Option<FifToggle>> = seen.iter().map(|(_, t)| *t).collect();
    assert_eq!(
        fields,
        vec![
            FifField::Query,
            FifField::Dir,
            FifField::Glob,
            FifField::Glob,
            FifField::Glob,
            FifField::Glob
        ]
    );
    assert_eq!(
        toggles,
        vec![
            None,
            None,
            None,
            Some(FifToggle::Case),
            Some(FifToggle::Word),
            Some(FifToggle::Gitignore)
        ],
        "all three boxes must be Tab stops -- they used to have no binding anyone could find"
    );
    // Six stops, six presses: the walk comes back round to the first stop rather than running
    // off the end.
    assert_eq!(
        app.fif.editing_field,
        FifField::Query,
        "the cycle must wrap from the last toggle to the first field"
    );
    assert_eq!(app.fif.focused_toggle, None);
    assert!(app.fif.editing, "landing on a field turns typing back on");
}

#[test]
fn back_tab_walks_the_same_stops_backwards() {
    let mut app = fif_app();
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Gitignore));
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Word));
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Case));
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.fif.editing_field, FifField::Glob);
    assert_eq!(app.fif.focused_toggle, None);
}

#[test]
fn space_flips_the_highlighted_toggle_and_types_in_a_field() {
    let mut app = fif_app();
    // In a field, space is a character: a search term with spaces is completely normal.
    app.fif.editing = true;
    app.fif.focused_toggle = None;
    char(&mut app, ' ');
    assert_eq!(app.fif.query, " ");

    // On a toggle, it flips.
    app.fif.focused_toggle = Some(FifToggle::Word);
    app.fif.editing = false;
    let before = app.fif.opts.whole_word;
    char(&mut app, ' ');
    assert_eq!(app.fif.opts.whole_word, !before);
    assert_eq!(app.fif.query, " ", "and must not type");
}

#[test]
fn left_and_right_move_along_the_toggles_once_one_is_highlighted() {
    let mut app = fif_app();
    app.fif.focused_toggle = Some(FifToggle::Case);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Word));
    key(&mut app, KeyCode::Left);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Case));
    key(&mut app, KeyCode::Left);
    assert_eq!(
        app.fif.focused_toggle,
        Some(FifToggle::Gitignore),
        "and wrap"
    );
}

#[test]
fn a_focused_toggle_is_grounded() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    app.fif.focused_toggle = Some(FifToggle::Word);
    app.fif.editing = false;
    let want = focus(&app);
    let grounded = columns_with_bg(&mut app, p.y + 3, want);
    let (start, end) = toggle_span(&app, FifToggle::Word);
    assert!(
        !grounded.is_empty()
            && grounded.iter().all(|x| *x >= start && x < &end)
            && grounded.len() == (end - start) as usize,
        "the ground should cover exactly the `word` box {start}..{end}, got {grounded:?}"
    );
}

// ------------------------------------------------------------ toggles: clickable

#[test]
fn clicking_a_toggle_box_flips_that_box_and_only_that_box() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    for (toggle, index) in [
        (FifToggle::Case, 0usize),
        (FifToggle::Word, 1),
        (FifToggle::Gitignore, 2),
    ] {
        let (start, _) = toggle_span(&app, toggle);
        let before_each = [
            app.fif.opts.case_sensitive,
            app.fif.opts.whole_word,
            app.fif.use_gitignore,
        ];
        click(&mut app, p.x + start + 1, p.y + 3);
        let now = [
            app.fif.opts.case_sensitive,
            app.fif.opts.whole_word,
            app.fif.use_gitignore,
        ];
        for (i, t) in FifToggle::ALL.iter().enumerate() {
            if i == index {
                assert_ne!(now[i], before_each[i], "clicking {t:?} must flip it");
            } else {
                assert_eq!(now[i], before_each[i], "{t:?} must not move");
            }
        }
        assert_ne!(
            toggle.get(&app),
            before_each[index],
            "{toggle:?} should have flipped from {}",
            before_each[index]
        );
    }
}

#[test]
fn clicking_a_toggle_leaves_the_other_two_alone() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    app.fif.opts.case_sensitive = false;
    app.fif.opts.whole_word = false;
    app.fif.use_gitignore = true;
    let (start, _) = toggle_span(&app, FifToggle::Word);
    click(&mut app, p.x + start + 1, p.y + 3);
    assert!(app.fif.opts.whole_word);
    assert!(!app.fif.opts.case_sensitive, "match case must not move");
    assert!(app.fif.use_gitignore, "gitignore must not move");
}

#[test]
fn clicking_a_toggle_highlights_it_so_pointer_and_keyboard_agree() {
    let mut app = fif_app();
    let p = find_in_files::panel_rect(&app);
    let (start, _) = toggle_span(&app, FifToggle::Gitignore);
    click(&mut app, p.x + start + 1, p.y + 3);
    assert_eq!(app.fif.focused_toggle, Some(FifToggle::Gitignore));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (from, to) = toggle_span(&app, FifToggle::Gitignore);
    let want = focus(&app);
    let grounded = columns_with_bg(&mut app, p.y + 3, want);
    assert!(
        grounded.contains(&from) && grounded.contains(&to.saturating_sub(1)),
        "the box just clicked should read as the highlighted one"
    );
}

#[test]
fn the_command_words_still_beat_the_toggles_they_share_a_row_with() {
    // Three hit-tests now live on one row: command, toggle, field. The order is load-bearing.
    let app = fif_app();
    let p = find_in_files::panel_rect(&app);
    let (spans, _) = find_in_files::command_spans(&app);
    let (col, _, cmd) = spans
        .iter()
        .find(|(_, _, c)| c.contains("Search"))
        .expect("a Search word");
    let x = p.x + col + 1;
    assert_eq!(find_in_files::hit_command(&app, x, p.y + 3), Some(*cmd));
    assert_eq!(
        find_in_files::hit_toggle(&app, x, p.y + 3),
        None,
        "the Search word must not read as a toggle"
    );
}

#[test]
fn a_toggle_hit_does_not_fall_through_to_the_glob_field() {
    let app = fif_app();
    let p = find_in_files::panel_rect(&app);
    let (start, _) = toggle_span(&app, FifToggle::Case);
    let x = p.x + start + 1;
    assert_eq!(
        find_in_files::hit_toggle(&app, x, p.y + 3),
        Some(FifToggle::Case)
    );
    // The row is also the glob field's row, which is why the mouse layer tries the toggle first.
    assert_eq!(
        find_in_files::field_at_row(&app, p.y + 3),
        Some(FifField::Glob)
    );
}

// ------------------------------------------------------------ the old chords

#[test]
fn the_ctrl_chords_still_work() {
    let mut app = fif_app();
    let press = |app: &mut App, c: char| {
        let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    };
    let before = app.fif.opts.case_sensitive;
    press(&mut app, 'c');
    assert_eq!(app.fif.opts.case_sensitive, !before);
    let before = app.fif.use_gitignore;
    press(&mut app, 'g');
    assert_eq!(app.fif.use_gitignore, !before);
}

// ------------------------------------------------------------ helpers

/// The `(start, end)` columns of a toggle box on the toggles row.
fn toggle_span(app: &App, want: FifToggle) -> (u16, u16) {
    let (_, spans) = find_in_files::toggle_spans(app);
    let (start, w, _) = spans
        .into_iter()
        .find(|(_, _, t)| *t == want)
        .expect("the toggle is on the row");
    (start, start + w)
}

fn first_toggle_column(app: &App) -> u16 {
    let (_, spans) = find_in_files::toggle_spans(app);
    spans.first().map(|(s, _, _)| *s).expect("a toggle")
}
