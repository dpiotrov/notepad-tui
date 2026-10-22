//! Help overlay and About (FR-U9).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, Mode};
use np_tui::keymap::default_bindings;
use np_tui::ui::help::{about_lines, help_lines, Line};

const W: u16 = 100;
const H: u16 = 30;

fn fresh() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn press_mod(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let mut ev = KeyEvent::new(code, mods);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn open_help(app: &mut App) {
    press_mod(app, KeyCode::F(1), KeyModifiers::SHIFT);
    assert_eq!(app.mode(), Mode::HelpOverlay);
}

/// Every rendered chord, headers excluded.
fn rendered_keys(app: &App) -> Vec<String> {
    help_lines(app)
        .iter()
        .filter(|l| !l.header)
        .map(|l| l.key.clone())
        .collect()
}

// ------------------------------------------------------------- the overlay

#[test]
fn shift_f1_opens_the_overlay_and_esc_closes_it() {
    let mut app = fresh();
    open_help(&mut app);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn every_default_binding_appears_in_the_help() {
    // FR-U9 acceptance, stated the way the spec asks: assert against `default_bindings()` so
    // the reference and the UI cannot diverge. A chord that stops resolving -- because it was
    // shadowed by a later binding -- is exempt, since showing a key that does not do the
    // thing would be its own kind of stale.
    let app = fresh();
    let shown = rendered_keys(&app);
    let mut missing = Vec::new();
    for (kebab, chord) in default_bindings() {
        let action = match np_tui::keymap::action_from_kebab(kebab) {
            Some(a) => a,
            None => {
                missing.push(format!("{kebab} is not a known action name"));
                continue;
            }
        };
        // A chord the keymap no longer resolves to this action is not the action's binding
        // any more, and `keys_for` already filtered it out of the menus.
        let resolves = match np_tui::keymap::parse_key(chord) {
            Some((code, mods)) => {
                let mut ev = KeyEvent::new(code, mods);
                ev.kind = KeyEventKind::Press;
                app.keymap.resolve(ev) == Some(action)
            }
            None => true,
        };
        if !resolves {
            continue;
        }
        if !shown.iter().any(|k| k == chord) {
            missing.push(format!("{chord} ({kebab})"));
        }
    }
    assert!(
        missing.is_empty(),
        "the cheatsheet is missing {} binding(s): {missing:?}",
        missing.len()
    );
}

#[test]
fn the_overlay_names_every_action_that_has_a_binding() {
    // The other half of no-drift: not just chords on screen, but the command next to each.
    let app = fresh();
    let all = help_lines(&app);
    let lines: Vec<&Line> = all.iter().filter(|l| !l.header).collect();
    assert!(lines.len() > 40, "a cheatsheet this small is missing rows");
    for (key, action) in app.keymap.table() {
        let label = action.label();
        let hit = lines.iter().any(|l| l.key == key && l.text == label);
        assert!(
            hit,
            "{key} -> {label} is bound but not listed: {:?}",
            lines
                .iter()
                .map(|l| format!("{}={}", l.key, l.text))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_remapped_key_shows_the_new_chord_not_the_default() {
    // The overlay reads the live keymap, so a remap is reflected rather than cached.
    let mut app = fresh();
    let (code, mods) = np_tui::keymap::parse_key("ctrl+y").expect("parse ctrl+y");
    app.keymap.bind((code, mods), Action::Undo);
    open_help(&mut app);
    let shown = rendered_keys(&app);
    assert!(
        shown.iter().any(|k| k == "ctrl+y"),
        "the remap should be on screen: {shown:?}"
    );
    let lines = help_lines(&app);
    let undo: Vec<&Line> = lines.iter().filter(|l| l.text == "Undo").collect();
    assert!(
        undo.iter().any(|l| l.key == "ctrl+y"),
        "Undo should list its new chord: {:?}",
        undo.iter().map(|l| l.key.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn an_unbound_action_does_not_appear_with_an_empty_row() {
    // An action with no chord and no menu hint has nothing to say in a *key* reference.
    let app = fresh();
    for line in help_lines(&app) {
        if line.header {
            continue;
        }
        assert!(
            !line.key.is_empty(),
            "a non-header row with no chord is dead weight: {:?}",
            line.text
        );
        assert!(!line.text.is_empty(), "a row with no label: {:?}", line.key);
    }
}

#[test]
fn codec_indices_never_look_like_chords() {
    // The Encoding lists stash a payload index in `hint`; it must not reach the cheatsheet.
    let app = fresh();
    for line in help_lines(&app) {
        if line.header {
            continue;
        }
        assert!(
            !line.key.chars().all(|c| c.is_ascii_digit()),
            "a bare number is a codec index, not a key: {:?}",
            line.key
        );
    }
}

#[test]
fn sections_are_grouped_by_menu() {
    let app = fresh();
    let lines = help_lines(&app);
    let headers: Vec<&str> = lines
        .iter()
        .filter(|l| l.header)
        .map(|l| l.text.as_str())
        .collect();
    // The menus that have bound items must show up as sections.
    for want in ["File", "Edit", "Search", "View"] {
        assert!(headers.contains(&want), "no {want} section in {headers:?}");
    }
    // A header carries no chord.
    for line in lines.iter().filter(|l| l.header) {
        assert!(line.key.is_empty(), "a header should not render a key");
    }
}

// ---------------------------------------------------------------- scrolling

#[test]
fn the_overlay_scrolls_and_clamps_at_both_ends() {
    let mut app = fresh();
    open_help(&mut app);
    let total = help_lines(&app).len();
    assert!(total > 20, "expected a long reference, got {total} rows");
    assert_eq!(app.help_scroll, 0);
    for _ in 0..total + 10 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(
        app.help_scroll,
        total - 1,
        "Down must clamp at the last row"
    );
    for _ in 0..total + 10 {
        press(&mut app, KeyCode::Up);
    }
    assert_eq!(app.help_scroll, 0, "Up must clamp at the first");
    press(&mut app, KeyCode::End);
    assert_eq!(app.help_scroll, total - 1);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn page_keys_move_by_a_page() {
    let mut app = fresh();
    open_help(&mut app);
    press(&mut app, KeyCode::PageDown);
    assert!(app.help_scroll > 1, "a page must move more than one row");
    assert!(app.help_scroll > 1);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.help_scroll, 0, "and come back");
}

#[test]
fn j_and_k_scroll_too() {
    // The overlay is a reader, not a text field, so the vim keys are free.
    let mut app = fresh();
    open_help(&mut app);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.help_scroll, 2);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.help_scroll, 1);
}

#[test]
fn enter_closes_the_overlay() {
    let mut app = fresh();
    open_help(&mut app);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing);
}

// ------------------------------------------------------------------ render

#[test]
fn the_overlay_renders_its_title_rows_and_scroll_hint() {
    let mut app = fresh();
    open_help(&mut app);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(block.contains("Keyboard Help"), "title missing: {block}");
    assert!(block.contains("ctrl+s"), "chords missing: {block}");
    assert!(block.contains("Save"), "labels missing: {block}");
    // The reference is longer than the box, so the hint must say so.
    assert!(
        block.contains("arrows scroll"),
        "a scrollable overlay must say it scrolls: {block}"
    );
}

// ------------------------------------------------------------------- About

#[test]
fn about_shows_the_version_and_terminal_size() {
    // FR-U9 acceptance, at a size that is not the default so the numbers are load-bearing.
    let mut app = App::new_headless(Config::default(), (123, 41));
    app.execute(Action::About);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::About));
    let lines = about_lines(&app);
    let joined = lines.join("\n");
    assert!(
        joined.contains(env!("CARGO_PKG_VERSION")),
        "version missing: {joined}"
    );
    assert!(
        joined.contains("123 x 41"),
        "terminal size missing: {joined}"
    );
    // The project name as shown to the user. Was "npt" until S34 renamed the product in the
    // dialog to Notepad-TUI; the binary is still `npt`, which the About dialog no longer claims.
    assert!(
        joined.contains("Notepad-TUI"),
        "the name should be there: {joined}"
    );
}

#[test]
fn about_reports_the_open_document_count_and_theme() {
    let mut app = fresh();
    app.execute(Action::NewFile);
    app.execute(Action::NewFile);
    let lines = about_lines(&app).join("\n");
    assert!(
        lines.contains(&format!("Documents open: {}", app.documents.len())),
        "document count wrong: {lines}"
    );
    assert!(
        lines.contains(&app.config.ui.theme),
        "theme missing: {lines}"
    );
}

#[test]
fn about_closes_on_enter_or_esc() {
    for code in [KeyCode::Enter, KeyCode::Esc] {
        let mut app = fresh();
        app.execute(Action::About);
        press(&mut app, code);
        assert_eq!(app.mode(), Mode::Editing, "{code:?} must close About");
    }
}

#[test]
fn about_renders_inside_the_frame() {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.execute(Action::About);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(block.contains("About"), "title missing: {block}");
    assert!(
        block.contains(env!("CARGO_PKG_VERSION")),
        "version missing from the render: {block}"
    );
    assert!(block.contains("Terminal:"), "size row missing: {block}");
}

#[test]
fn the_help_overlay_and_about_are_different_surfaces() {
    // One is a key reference, the other a version card. Sharing a mode would make Shift+F1
    // and F1 the same thing.
    let mut app = fresh();
    open_help(&mut app);
    let help = app.mode();
    let mut second = fresh();
    second.execute(Action::About);
    assert_ne!(help, second.mode());
}

// ---------------------------------------------------------------- About fit (S34)

#[test]
fn the_ok_button_never_sits_on_a_line_of_text() {
    // The bug this asserts away: `about_lines` grew to six lines while the frame still reserved
    // five body rows, so the last line ("Theme: ...") was drawn straight through the OK button.
    // `about_renders_inside_the_frame` could not see it -- that test asks whether each string
    // appears *somewhere* on the screen, and both strings did. Only a row-level check notices two
    // things sharing one row, so this asserts on rendered rows rather than on a joined blob.
    for (w, h) in [(100u16, 30u16), (80, 24), (60, 20), (50, 16), (44, 14)] {
        let mut app = App::new_headless(Config::default(), (w, h));
        app.execute(Action::About);
        let rows: Vec<String> = (0..h)
            .map(|y| {
                let buf = np_tui::ui::render_to_buffer(&mut app, w, h);
                np_tui::ui::row_text(&buf, y, 0, w)
            })
            .collect();
        let btn = rows.iter().position(|r| r.contains("[OK]"));
        let btn =
            btn.unwrap_or_else(|| panic!("no OK button rendered at {w}x{h}:\n{}", rows.join("\n")));
        let text_rows: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.contains("Notepad-TUI") || r.contains("Terminal:"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !text_rows.contains(&btn),
            "OK button shares row {btn} with text at {w}x{h}:\n{}",
            rows.join("\n")
        );
        // Every body line must sit above the button, not merely somewhere on screen.
        for i in text_rows {
            assert!(
                i < btn,
                "body row {i} is not above the OK button ({btn}) at {w}x{h}:\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn about_geometry_is_derived_from_the_lines_it_draws() {
    // The frame height used to be a literal, which is how it drifted from the line count. Adding a
    // line to `about_lines` must move the button rather than bury it, so the geometry is asserted
    // against the content directly: body rows + blank + button row + two borders.
    let mut app = fresh();
    app.execute(Action::About);
    let lines = about_lines(&app);
    let layout = np_tui::ui::dialogs::about_layout(&app);
    let want_h = (lines.len() + 4) as u16; // body, blank, button, two borders
    assert_eq!(
        layout.frame.layout.area.h,
        want_h,
        "frame height must follow the line count ({} lines)",
        lines.len()
    );
    let btn = layout.button_row(0);
    assert!(
        btn.y < layout.frame.layout.area.bottom(),
        "button row {} is on or past the bottom border {}",
        btn.y,
        layout.frame.layout.area.bottom()
    );
    assert!(
        btn.y as usize >= lines.len() + layout.frame.layout.inner.y as usize,
        "button row overlaps the text block"
    );
}

#[test]
fn about_names_the_project_and_its_authors() {
    // Content contract: identify ourselves, and say nothing about any other editor.
    let mut app = fresh();
    app.execute(Action::About);
    let joined = about_lines(&app).join("\n");
    assert!(joined.contains("Notepad-TUI"), "name missing: {joined}");
    assert!(
        joined.contains("Dpiotrov and Inorganic Beings"),
        "authorship missing: {joined}"
    );
    assert!(joined.contains("MIT License"), "license missing: {joined}");
    assert!(
        !joined.to_lowercase().contains("notepad++"),
        "About must not mention another editor: {joined}"
    );
    // The banner is the stable part; the dynamic rows must not change the count the layout trusts.
    assert_eq!(
        np_tui::ui::help::about_row_count() as usize,
        about_lines(&app).len(),
        "row count must equal the lines actually drawn"
    );
}
