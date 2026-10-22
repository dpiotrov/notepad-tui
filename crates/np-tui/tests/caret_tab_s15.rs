//! S15: the caret must sit where the terminal draws the text (spec FR-E1, FR-E2, §6.7).
//!
//! Reported from a real terminal: "the char I print appear 4 position left from cursor".
//! The buffer stores tabs, and the row writer used to hand that tab straight to ratatui.
//! Ratatui's cell model counts one column per cell, but the crossterm backend emits the
//! symbol with `Print`, so the *terminal* jumped to its own tab stop. Every column the
//! editor computed was then a few cells right of the glyphs the user could see -- by exactly
//! the tab's width, which is why the offset looked like 4.
//!
//! `TestBackend` measures a tab as one cell wide, exactly as ratatui intends, so no
//! headless test could ever have caught this by looking at rendered positions. The guard
//! here is the invariant that makes the terminal's behavior irrelevant: no buffer cell may
//! hold a tab glyph in the first place.

use np_core::{Config, Document, EncodingId, Eol};
use np_tui::app::App;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn open(text: &str, w: u16, h: u16) -> App {
    let mut app = App::new_headless(Config::default(), (w, h));
    app.open_document(Document::from_decoded(
        "/tmp/tab.txt".into(),
        text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    app
}

fn frame(app: &mut App, w: u16, h: u16) -> Buffer {
    app.recompute_geometry(w, h);
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    np_tui::ui::draw(app, &mut buf);
    buf
}

#[test]
fn no_buffer_cell_holds_a_tab_glyph() {
    let text = "\tlead\n\t\ttwo tabs\nafter\tmid\ttabs\nplain\n";
    for (w, h) in [(80u16, 24u16), (40, 12), (200, 30)] {
        for show_ws in [false, true] {
            let mut app = open(text, w, h);
            app.config.ui.show_whitespace = show_ws;
            let buf = frame(&mut app, w, h);
            for y in 0..h {
                for x in 0..w {
                    let sym = buf.cell((x, y)).unwrap().symbol();
                    assert!(
                        !sym.contains('\t'),
                        "tab glyph at ({x},{y}) with show_whitespace={show_ws}: the terminal \
                         jumps to a tab stop while every column calculation counts one, so \
                         the caret drifts right of the text"
                    );
                }
            }
        }
    }
}

#[test]
fn caret_after_a_tab_sits_at_the_tab_stop() {
    // tab_width 4: the leading tab occupies cells 0..4, then 8 characters, so the caret is
    // at text_x(2) + 4 + 8 = 14. Under the old one-cell tab it would have been 13, one cell
    // per character -- and the terminal would have drawn the text 3 cells further left.
    let mut app = open("\tindented\n", 80, 24);
    for _ in 0..9 {
        app.execute(np_tui::actions::Action::MoveRight);
    }
    let pos = np_tui::ui::editor_view::caret_position(&app).expect("caret");
    assert_eq!(pos.col, 14, "caret after a tab stop, not after one cell");
}

#[test]
fn caret_and_drawn_text_agree_on_a_line_full_of_tabs() {
    // For each caret position, the cell the caret points at must be the cell the character
    // under the caret was drawn into. That is the property the user actually perceives.
    let text = "x\ty\tzz\tq\n";
    let mut app = open(text, 80, 24);
    let chars: Vec<char> = text.chars().collect();
    for step in 0..chars.len() {
        let pos = np_tui::ui::editor_view::caret_position(&app).expect("caret");
        let buf = frame(&mut app, 80, 24);
        if step < chars.len() && chars[step] != '\n' {
            let drawn = buf
                .cell((pos.col as u16, pos.line as u16))
                .unwrap()
                .symbol()
                .to_string();
            // Either the character itself, or blank when the caret is on a tab's run.
            assert!(
                drawn == chars[step].to_string() || drawn.trim().is_empty(),
                "caret {step} at col {} points at {drawn:?}, expected {:?}",
                pos.col,
                chars[step]
            );
        }
        app.execute(np_tui::actions::Action::MoveRight);
    }
}

#[test]
fn arrows_move_the_caret_and_typing_lands_at_it() {
    // Bug 1: no key was bound to movement at all, so the caret could not be moved by
    // keyboard in any direction.
    let mut app = open("hello world\nsecond line\n", 80, 24);
    let start = app.doc().expect("doc").cursor.pos;
    app.execute(np_tui::actions::Action::MoveRight);
    assert_eq!(
        app.doc().expect("doc").cursor.pos.col,
        start.col + 1,
        "MoveRight must advance the caret"
    );
    app.execute(np_tui::actions::Action::MoveDown);
    assert_eq!(app.doc().expect("doc").cursor.pos.line, start.line + 1);
    app.execute(np_tui::actions::Action::MoveLeft);
    app.execute(np_tui::actions::Action::MoveUp);
    assert_eq!(
        app.doc().expect("doc").cursor.pos,
        start,
        "a round trip returns home"
    );
}

#[test]
fn the_default_keymap_binds_every_navigation_key() {
    // The regression guard for bug 1: these eight keys resolved to nothing, and the whole
    // test suite still passed because nothing exercised them.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use np_tui::keymap::Keymap;
    let km = Keymap::with_defaults();
    let expect: &[(&str, KeyCode, KeyModifiers, &str)] = &[
        ("Left", KeyCode::Left, KeyModifiers::NONE, "move-left"),
        ("Right", KeyCode::Right, KeyModifiers::NONE, "move-right"),
        ("Up", KeyCode::Up, KeyModifiers::NONE, "move-up"),
        ("Down", KeyCode::Down, KeyModifiers::NONE, "move-down"),
        (
            "S-Left",
            KeyCode::Left,
            KeyModifiers::SHIFT,
            "move-left-select",
        ),
        (
            "S-Right",
            KeyCode::Right,
            KeyModifiers::SHIFT,
            "move-right-select",
        ),
        ("S-Up", KeyCode::Up, KeyModifiers::SHIFT, "move-up-select"),
        (
            "S-Down",
            KeyCode::Down,
            KeyModifiers::SHIFT,
            "move-down-select",
        ),
        ("Home", KeyCode::Home, KeyModifiers::NONE, "line-start"),
        ("End", KeyCode::End, KeyModifiers::NONE, "line-end"),
        ("C-Left", KeyCode::Left, KeyModifiers::CONTROL, "word-left"),
        (
            "C-Right",
            KeyCode::Right,
            KeyModifiers::CONTROL,
            "word-right",
        ),
        (
            "PgUp",
            KeyCode::PageUp,
            KeyModifiers::NONE,
            "scroll-page-up",
        ),
        (
            "PgDn",
            KeyCode::PageDown,
            KeyModifiers::NONE,
            "scroll-page-down",
        ),
    ];
    for (label, code, mods, want) in expect {
        let got = km.resolve(KeyEvent::new(*code, *mods));
        let kebab = got.map(|a| a.kebab().to_string());
        assert_eq!(
            kebab.as_deref(),
            Some(*want),
            "{label} must be bound to {want} (FR-E2/FR-E3)"
        );
    }
}

#[test]
fn pressing_an_arrow_through_handle_key_moves_the_caret() {
    // End to end through the real input path, not just the keymap table: bug 1 lived in the
    // binding, which only `handle_key` exercises.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = open("hello\nworld\n", 80, 24);
    let before = app.doc().expect("doc").cursor.pos;
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    let after = app.doc().expect("doc").cursor.pos;
    assert_ne!(before, after, "a bare Right keystroke must move the caret");
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.doc().expect("doc").cursor.pos.line, before.line + 1);
}

#[test]
fn shift_arrow_extends_the_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = open("hello world\n", 80, 24);
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let doc = app.doc().expect("doc");
    let sel = doc.selection.expect("Shift+Right must extend a selection");
    assert_eq!(sel.bounds().1.col, 2, "two columns selected");
    assert_eq!(doc.selected_text().unwrap_or_default(), "he");
}
