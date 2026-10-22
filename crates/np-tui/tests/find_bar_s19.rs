//! S19: Find and Replace dock as service lines instead of floating over the text.
//!
//! Reported as: "please use the same form of appearing service lines like in Find in Files also
//! in Search and Search and Replace functions. That is better for usability in text terminal."
//!
//! # What moved and what deliberately did not
//!
//! Only the presentation moved. `Mode::Dialog(Find | Replace)` is still the mode,
//! `dialog_keys::find_key` still owns every keystroke, and focus is still the framework's
//! `FocusId::Field/Checkbox/Button`. The dialog's keyboard model was already field-centric and
//! correct; rewriting it into a second copy of the panel's input handling would have been the
//! same mistake S17 was filed against -- several dialogs, several near-identical implementations.
//!
//! What changed is that the strip's rows are reserved out of the editor band, so the text and
//! the service lines cannot claim the same cells, and the strip's own geometry is what the
//! focus list and the hit-test measure. Both of those are the parts that could silently drift,
//! so both are pinned here.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, Mode};
use np_tui::ui::find_bar;
use np_tui::ui::framework::FocusId;

const W: u16 = 90;
const H: u16 = 22;

fn app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    let text: String = (0..40)
        .map(|i| format!("text line {i} with hello in it"))
        .collect::<Vec<_>>()
        .join("\n");
    app.open_document(Document::from_decoded(
        "/tmp/bar.txt".into(),
        &text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

/// Open Find and type into it, so the query is live before any assertion.
fn open_find(app: &mut App, query: &str) {
    app.execute(Action::Find);
    // Regions are computed by the layout pass that a draw performs, so a test that inspects
    // them has to render after opening -- exactly what the run loop does every frame.
    let _ = np_tui::ui::render_to_buffer(app, W, H);
    type_text(app, query);
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn click(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

/// The options row, resolved without holding a borrow across another call.
fn opts_row(app: &App) -> u16 {
    find_bar::options_row(app)
}

fn row(app: &mut App, y: u16) -> String {
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    (0..W)
        .map(|c| buf[(c, y)].symbol().chars().next().unwrap_or(' '))
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn all_text(app: &mut App) -> String {
    (0..H).map(|y| row(app, y)).collect::<Vec<_>>().join("\n")
}

// ----------------------------------------------------------- the reservation

#[test]
fn the_strip_takes_its_rows_out_of_the_text_area() {
    let mut plain = app();
    let plain_h = plain.regions.editor.h;
    plain.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut plain, W, H);
    let find_h = plain.regions.editor.h;
    let need = find_bar::height(&plain);
    assert!(need > 0, "Find must reserve rows");
    assert_eq!(
        plain_h - find_h,
        need,
        "the text area must shrink by exactly the strip's height"
    );
    let strip = find_bar::strip_rect(&plain);
    assert!(
        strip.y >= plain.regions.editor.bottom(),
        "the strip must start below the last text row: editor={:?} strip={:?}",
        plain.regions.editor,
        strip
    );
}

#[test]
fn the_searched_text_stays_on_screen_while_find_is_open() {
    let mut app = app();
    open_find(&mut app, "hello");
    let body = all_text(&mut app);
    assert!(
        body.contains("text line 0 with hello in it"),
        "opening Find must not hide the file it is searching: {body}"
    );
    // The rows the strip took are not also painted with text.
    let strip = find_bar::strip_rect(&app);
    for y in strip.y..strip.bottom() {
        let line = row(&mut app, y);
        assert!(
            !line.contains("with hello in it"),
            "row {y} is inside the strip and must not carry buffer text: {line:?}"
        );
    }
}

#[test]
fn the_strip_sits_where_find_in_files_sits() {
    // The point of the change is one familiar place for service lines, so the two must dock to
    // the same bottom edge rather than one floating and one docking.
    let mut app = app();
    app.execute(Action::Find);
    let find_bottom = find_bar::strip_rect(&app).bottom();
    assert_eq!(
        find_bottom,
        app.regions.band.bottom(),
        "the strip is flush with the bottom of the editor band"
    );
}

// --------------------------------------------------------------- the content

#[test]
fn the_query_and_the_match_count_share_the_first_row() {
    let mut app = app();
    open_find(&mut app, "hello");
    let qy = find_bar::strip_rect(&app).y;
    let line = row(&mut app, qy);
    assert!(line.contains("Find : hello"), "query missing: {line:?}");
    assert!(
        line.contains("match(es)"),
        "the match count belongs on the query row: {line:?}"
    );
}

#[test]
fn replace_mode_adds_the_replacement_row_and_its_actions() {
    let mut app = app();
    app.execute(Action::Find);
    let find_rows = find_bar::height(&app);
    app.execute(Action::Replace);
    let replace_rows = find_bar::height(&app);
    assert_eq!(
        replace_rows,
        find_rows + 1,
        "Replace adds exactly the replacement row"
    );
    let strip = find_bar::strip_rect(&app);
    assert!(
        row(&mut app, strip.y + 1).starts_with("Repl :"),
        "the replacement field is the second row"
    );
    let oy = opts_row(&app);
    let options = row(&mut app, oy);
    assert!(
        options.contains("Replace") && options.contains("Replace All"),
        "the Replace actions appear only in Replace mode: {options:?}"
    );
}

#[test]
fn find_mode_offers_no_replace_actions() {
    let mut app = app();
    app.execute(Action::Find);
    let oy = opts_row(&app);
    let options = row(&mut app, oy);
    assert!(
        !options.contains("Replace"),
        "Find mode must not offer actions it would refuse: {options:?}"
    );
}

#[test]
fn a_bad_pattern_gets_its_own_row_in_full() {
    let mut app = app();
    open_find(&mut app, "");
    app.find.opts.regex = true;
    type_text(&mut app, "[");
    let body = all_text(&mut app);
    assert!(
        body.contains("invalid regular expression"),
        "the diagnostic must arrive untruncated: {body}"
    );
    assert_eq!(
        find_bar::height(&app),
        find_bar::FIND_ROWS + find_bar::ERROR_ROWS,
        "the strip grows by one row while the error stands"
    );
}

// ------------------------------------------------------------- the keyboard

#[test]
fn enter_in_the_field_finds_and_the_strip_stays_open() {
    let mut app = app();
    open_find(&mut app, "hello");
    key(&mut app, KeyCode::Enter);
    let caret = app.doc().map(|d| d.cursor.pos.line);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Find));
    assert_eq!(caret, Some(0), "the first match is on line 0");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.doc().map(|d| d.cursor.pos.line),
        Some(1),
        "Enter again walks to the next match"
    );
}

#[test]
fn typing_in_the_strip_never_reaches_the_document() {
    let mut app = app();
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    app.execute(Action::Find);
    type_text(&mut app, "zzz quoted text");
    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    assert_eq!(before, after, "the strip owns the keystrokes");
}

#[test]
fn escape_closes_the_strip_and_gives_the_rows_back() {
    let mut app = app();
    let plain_h = app.regions.editor.h;
    open_find(&mut app, "");
    assert!(app.regions.editor.h < plain_h);
    key(&mut app, KeyCode::Esc);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(
        app.regions.editor.h, plain_h,
        "closing the strip must return its rows to the text"
    );
}

// ------------------------------------------------------------------ the mouse

#[test]
fn clicking_an_action_word_runs_it() {
    let mut app = app();
    open_find(&mut app, "hello");
    key(&mut app, KeyCode::Enter); // line 0
    let strip = find_bar::strip_rect(&app);
    let (_, spans) = find_bar::options_spans(&app);
    let (col, _, _) = spans
        .iter()
        .find(|(_, _, h)| matches!(h, find_bar::OptionHit::Button(1)))
        .expect("Prev is on the options row");
    let oy = opts_row(&app);
    click(&mut app, strip.x + col + 1, oy);
    assert_eq!(
        app.doc().map(|d| d.cursor.pos.line),
        Some(39),
        "Prev from the first match wraps to the last"
    );
}

#[test]
fn clicking_a_toggle_flips_it_once() {
    let mut app = app();
    app.execute(Action::Find);
    let strip = find_bar::strip_rect(&app);
    let (_, spans) = find_bar::options_spans(&app);
    let (col, _, _) = spans
        .iter()
        .find(|(_, _, h)| matches!(h, find_bar::OptionHit::Checkbox(0)))
        .expect("the case toggle is on the options row");
    let before = app.find.opts.case_sensitive;
    let oy = opts_row(&app);
    click(&mut app, strip.x + col + 1, oy);
    assert_eq!(app.find.opts.case_sensitive, !before);
}

#[test]
fn every_focus_item_sits_on_the_row_that_is_drawn_there() {
    // The focus list, the painter and the hit-test all describe the same strip. If one of them
    // keeps its own arithmetic this is the assertion that notices.
    let mut app = app();
    open_find(&mut app, "hello");
    let strip = find_bar::strip_rect(&app);
    let items = app.dialog_focus_items();
    assert!(!items.is_empty());
    for item in &items {
        assert!(
            strip.contains(item.area.x, item.area.y),
            "{:?} is outside the strip: {:?}",
            item.id,
            item.area
        );
        let drawn = row(&mut app, item.area.y);
        match item.id {
            FocusId::Field(0) => assert!(drawn.starts_with("Find :"), "{:?} -> {drawn:?}", item.id),
            FocusId::Checkbox(i) => {
                let (_, spans) = find_bar::options_spans(&app);
                let (col, width, _) = spans
                    .iter()
                    .find(|(_, _, h)| matches!(h, find_bar::OptionHit::Checkbox(j) if *j == i))
                    .expect("span for this checkbox");
                let chars: Vec<char> = drawn.chars().collect();
                let start = (strip.x + col) as usize;
                let end = ((strip.x + col + width) as usize).min(chars.len());
                let word: String = chars[start..end].iter().collect();
                assert!(
                    word.starts_with('['),
                    "checkbox {i} hit-rect covers {word:?}, which is not a toggle"
                );
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ coexistence

#[test]
fn find_docked_over_find_in_files_does_not_overlap_it() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    let editor_with_panel = app.regions.editor;
    app.execute(Action::Find);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let strip = find_bar::strip_rect(&app);
    assert!(
        strip.y >= editor_with_panel.bottom() && panel.y >= editor_with_panel.bottom(),
        "both strips live below the text: editor={editor_with_panel:?} panel={panel:?} strip={strip:?}"
    );
    // The text area never grew to cover either of them.
    assert!(
        app.regions.editor.h <= editor_with_panel.h,
        "opening Find on top of the panel must not hand back rows the panel owns"
    );
    let body = all_text(&mut app);
    assert!(
        !body.contains("Find  : ") || body.matches("Find").count() < 4,
        "the two service strips must not interleave: {body}"
    );
}
