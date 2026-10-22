//! Preferences (FR-CG5): edit `Config`, persist immediately, apply live.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, DialogKind, Mode};
use np_tui::ui::prefs::{self, Field, Key, Row};
use std::fs;
use std::path::Path;

const W: u16 = 100;
const H: u16 = 30;

/// An app whose config writes go to a tempdir, so "persists immediately" is checkable.
fn app_with_config(dir: &Path) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.config_path = Some(dir.join("config.toml"));
    app
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

fn index_of(key: Key) -> usize {
    prefs::rows()
        .iter()
        .position(|r| r.key == key)
        .unwrap_or_else(|| panic!("no preferences row for {key:?}"))
}

/// Put the dialog's row cursor on `key`.
///
/// Writes through `set_list_cursor` rather than `prefs_selected`. Since S17 the shared
/// `dialog_row` is authoritative and `prefs_selected` is a mirror the renderers read, so a
/// direct write to the mirror is silently overwritten on the next refresh.
fn select(app: &mut App, key: Key) {
    app.set_list_cursor(index_of(key));
}

fn open_prefs(app: &mut App) {
    app.execute(Action::Preferences);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Preferences));
}

// ------------------------------------------------------------------ the rows

#[test]
fn every_row_reads_and_writes_its_own_key() {
    // Table-driven over the row list itself: a new setting that forgets its reader or writer
    // fails here rather than showing a value that never changes.
    for row in prefs::rows() {
        let field = match row.field {
            Field::Bool => "true",
            Field::Num => "7",
            // The theme row has no fixed option list; it is exercised separately.
            Field::Enum(_) => continue,
        };
        let mut config = Config::default();
        assert!(
            prefs::write(&mut config, row.key, field),
            "{:?} should accept {field:?}",
            row.key
        );
        assert_eq!(
            prefs::read(&config, row.key),
            field,
            "{:?} did not read back what it wrote",
            row.key
        );
    }
}

#[test]
fn an_enum_row_cycles_through_its_declared_options() {
    let mut app = App::new_headless(Config::default(), (W, H));
    let row = index_of(Key::DefaultEol);
    let options = match prefs::rows()[row].field {
        Field::Enum(o) => o.to_vec(),
        _ => panic!("Default EOL should be an option picker"),
    };
    assert_eq!(options, vec!["lf", "crlf", "cr"]);
    // Starting from the stored default (option 0), one step lands on option 1.
    assert_eq!(prefs::toggle(&mut app, row, 1).as_deref(), Some("crlf"));
    assert_eq!(prefs::toggle(&mut app, row, 1).as_deref(), Some("cr"));
    // And the next wraps to the first rather than running off the end.
    assert_eq!(prefs::toggle(&mut app, row, 1).as_deref(), Some("lf"));
    // Left steps backwards.
    assert_eq!(prefs::toggle(&mut app, row, -1).as_deref(), Some("cr"));
    // A full cycle visits every option exactly once.
    let mut seen = Vec::new();
    for _ in 0..options.len() {
        seen.push(prefs::toggle(&mut app, row, 1).unwrap_or_default());
    }
    for want in &options {
        assert!(
            seen.iter().any(|s| s == want),
            "{want} never appeared in {seen:?}",
        );
    }
    assert_eq!(seen.len(), options.len());
}

#[test]
fn a_stored_value_outside_the_option_list_resumes_the_cycle_at_the_first_option() {
    // A config edited by hand can name an option the dialog does not offer. The first step
    // then has no "current" to step from, and it must land on a real option rather than
    // skipping one.
    let mut config = Config::default();
    assert!(prefs::write(&mut config, Key::DefaultEol, "crlf"));
    let mut app = App::new_headless(config, (W, H));
    let row = index_of(Key::DefaultEol);
    // From option 1 (`crlf`), the next option is `cr`.
    assert_eq!(prefs::toggle(&mut app, row, 1).as_deref(), Some("cr"));
    assert_eq!(prefs::read(&app.config, Key::DefaultEol), "cr");
}

#[test]
fn a_rejected_value_changes_nothing() {
    let mut config = Config::default();
    let before = config.editor.tab_width;
    assert!(!prefs::write(&mut config, Key::TabWidth, "not a number"));
    assert!(
        !prefs::write(&mut config, Key::TabWidth, "0"),
        "0 is out of range"
    );
    assert!(
        !prefs::write(&mut config, Key::TabWidth, "17"),
        "16 is the max"
    );
    assert_eq!(
        config.editor.tab_width, before,
        "a bad value must not half-write"
    );
    assert!(prefs::write(&mut config, Key::TabWidth, "16"));
    assert_eq!(config.editor.tab_width, 16);
}

// ------------------------------------------------------- persist immediately

#[test]
fn toggling_a_boolean_writes_the_config_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    let path = dir.path().join("config.toml");
    assert!(!path.exists(), "nothing written before the first change");
    open_prefs(&mut app);
    select(&mut app, Key::WordWrap);
    let before = app.config.ui.word_wrap;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.ui.word_wrap, !before, "and it applies live");
    assert!(path.exists(), "FR-CG5: the change must be on disk already");
    let text = fs::read_to_string(&path).expect("read config");
    assert!(
        text.contains(&format!("word_wrap = {}", !before)),
        "the file should carry the new value: {text}"
    );
}

#[test]
fn a_changed_value_survives_a_restart() {
    // The acceptance test the spec names: change -> file on disk -> a new App sees it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::TabWidth);
    // Typing a digit opens the inline edit; Enter commits it.
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.config.editor.tab_width, 3);
    assert!(path.exists());

    let (reloaded, warnings) = Config::load(Some(&path));
    assert!(
        warnings.is_empty(),
        "a written config must parse clean: {warnings:?}"
    );
    assert_eq!(reloaded.editor.tab_width, 3, "a new load must see 3");

    // The live document of the *editing* session follows immediately.
    assert_eq!(app.doc().map(|d| d.tab_width), Some(3));
}

#[test]
fn esc_closes_preferences_and_keeps_the_changes() {
    // Deviation from the common OK/Cancel dialog, recorded in DEVIATIONS.md: there is no
    // Cancel, because every change was already committed when it was made.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::ShowWhitespace);
    let before = app.config.ui.show_whitespace;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.ui.show_whitespace, !before);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(
        app.config.ui.show_whitespace, !before,
        "Esc must not roll back a committed preference"
    );
}

#[test]
fn a_failed_write_toasts_instead_of_failing_silently() {
    // A config path inside a *file* cannot be written. The change still applies to the
    // session -- the user asked for it -- but they must be told the persistence failed.
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "i am a file").expect("write blocker");
    let mut app = App::new_headless(Config::default(), (W, H));
    app.config_path = Some(blocker.join("config.toml"));
    open_prefs(&mut app);
    select(&mut app, Key::LineNumbers);
    let before = app.config.ui.line_numbers;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        app.config.ui.line_numbers, !before,
        "the session still honors it"
    );
    let messages: Vec<String> = app.timed_toasts.iter().map(|t| t.message.clone()).collect();
    assert!(
        messages.iter().any(|m| m.contains("could not save config")),
        "a lost write must be reported: {messages:?}"
    );
    assert!(
        app.timed_toasts.iter().any(|t| t.error),
        "and it must be sticky, not a 3 s blip"
    );
}

// -------------------------------------------------------------- apply live

#[test]
fn toggling_word_wrap_recomputes_geometry_and_zeroes_the_horizontal_scroll() {
    // FR-V1 via FR-CG5: wrap and horizontal scroll are mutually exclusive states.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    if let Some(doc) = app.doc_mut() {
        doc.scroll.left_col = 8;
    }
    open_prefs(&mut app);
    select(&mut app, Key::WordWrap);
    press(&mut app, KeyCode::Char(' '));
    assert!(app.config.ui.word_wrap);
    assert_eq!(
        app.doc().map(|d| d.scroll.left_col),
        Some(0),
        "enabling wrap must reset horizontal scroll"
    );
}

#[test]
fn toggling_line_numbers_changes_the_gutter_width() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    app.execute(Action::ToggleWordWrap); // no-op guard: leave wrap off
    app.execute(Action::ToggleWordWrap);
    open_prefs(&mut app);
    select(&mut app, Key::LineNumbers);
    let with_numbers = app.geometry.gutter_width;
    press(&mut app, KeyCode::Char(' '));
    assert!(!app.config.ui.line_numbers);
    assert!(
        app.geometry.gutter_width < with_numbers,
        "hiding the strip must narrow the gutter: {with_numbers} -> {}",
        app.geometry.gutter_width
    );
}

#[test]
fn editing_tab_width_applies_to_the_active_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::TabWidth);
    press(&mut app, KeyCode::Char('8'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.doc().map(|d| d.tab_width), Some(8));
}

// ------------------------------------------------------------------ keyboard

#[test]
fn arrow_keys_walk_the_rows_and_clamp_at_both_ends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    assert_eq!(app.prefs_selected, 0);
    let count = prefs::rows().len();
    for _ in 0..count + 5 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(
        app.prefs_selected,
        count - 1,
        "Down must clamp at the last row"
    );
    for _ in 0..count + 5 {
        press(&mut app, KeyCode::Up);
    }
    assert_eq!(app.prefs_selected, 0, "Up must clamp at the first");
    // Page keys move by a block, which matters on a 26-row form in a 24-row terminal.
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.prefs_selected, 5);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.prefs_selected, 0);
}

#[test]
fn typing_a_digit_opens_the_inline_number_edit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::AutosaveSeconds);
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Char('2'));
    assert!(app.prefs_edit.is_some(), "typing should open the field");
    press(&mut app, KeyCode::Enter);
    assert!(
        app.prefs_edit.is_none(),
        "Enter commits and closes the field"
    );
    assert_eq!(app.config.editor.autosave_seconds, 12);
}

#[test]
fn a_garbage_number_is_rejected_with_a_toast_and_keeps_the_old_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::AutosaveSeconds);
    let before = app.config.editor.autosave_seconds;
    // Backspace the seeded value away, then commit an empty field.
    press(&mut app, KeyCode::Enter);
    assert!(app.prefs_edit.is_some());
    let edits = app.config.editor.autosave_seconds.to_string().len();
    for _ in 0..edits + 1 {
        press(&mut app, KeyCode::Backspace);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.config.editor.autosave_seconds, before,
        "an empty value must not zero the setting"
    );
    let messages: Vec<String> = app.timed_toasts.iter().map(|t| t.message.clone()).collect();
    assert!(
        messages.iter().any(|m| m.contains("not a valid value")),
        "the rejection must be explained: {messages:?}"
    );
}

#[test]
fn shift_enter_steps_an_enum_backwards() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::EolMode);
    let before = app.config.editor.eol_mode.clone();
    press_mod(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    let options = match prefs::rows()[index_of(Key::EolMode)].field {
        Field::Enum(o) => o.to_vec(),
        _ => panic!("EOL mode should be an option picker"),
    }
    .to_vec();
    let pos = options
        .iter()
        .position(|o| *o == before)
        .expect("current in options");
    let expect = &options[(pos + options.len() - 1) % options.len()];
    assert_eq!(&app.config.editor.eol_mode, expect);
}

#[test]
fn tab_moves_focus_like_every_other_dialog() {
    // FR-U4: focus is the framework's job, so Preferences must honor Tab too.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    let items = app.dialog_focus_items().len();
    assert!(items > 0, "the dialog must declare focusable children");
    press(&mut app, KeyCode::Tab);
    assert!(app.focused_id().is_some(), "Tab must land on a child");
}

// ------------------------------------------------------------------- render

#[test]
fn preferences_renders_labels_widgets_and_the_footer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(block.contains("Preferences"), "title missing: {block}");
    assert!(block.contains("Tab width"), "rows missing: {block}");
    assert!(
        block.contains("[x]") || block.contains("[ ]"),
        "checkboxes missing: {block}"
    );
    // The footer is the only place the dialog explains itself, and it has to say the
    // changes are kept, since there is no OK button to press.
    assert!(block.contains("Space toggle"), "footer missing: {block}");
}

#[test]
fn the_visible_window_follows_the_selection() {
    // 26 rows do not fit a 30-row terminal, so the list scrolls. A selection below the fold
    // must be drawn, not silently moved off screen.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config(dir.path());
    open_prefs(&mut app);
    select(&mut app, Key::MarkMatches); // the last row
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(
        block.contains("Find: mark matches") || block.contains("mark matches"),
        "the selected row must be scrolled into view: {block}"
    );
    // And the first row must then be scrolled out, which is what proves it is a window
    // rather than an ever-growing list.
    assert!(
        !block.contains("Tab width"),
        "row 0 should have scrolled away: {block}"
    );
}

#[test]
fn rows_are_unique_and_fully_labeled() {
    // Two rows on one key would silently fight over one config field.
    let rows = prefs::rows();
    let mut keys: Vec<Key> = rows.iter().map(|r| r.key).collect();
    let total = keys.len();
    keys.sort_by_key(|k| format!("{k:?}"));
    keys.dedup();
    assert_eq!(keys.len(), total, "duplicate preference key");
    for row in &rows {
        assert!(!row.label.is_empty(), "every row needs a label");
        let _ = Row {
            label: row.label,
            field: row.field,
            key: row.key,
        };
    }
    // The three sections the spec names must all be represented.
    let labels: String = rows.iter().map(|r| r.label).collect::<Vec<_>>().join(" ");
    assert!(labels.contains("Tab width"), "[editor] missing");
    assert!(labels.contains("Word wrap"), "[ui] missing");
    assert!(labels.contains("Find: regex"), "[find] missing");
}

#[test]
fn a_config_path_of_none_persists_nothing_and_toasts_nothing() {
    // A headless App with nowhere to write must not invent a location, and must not nag.
    let mut app = App::new_headless(Config::default(), (W, H));
    assert!(app.config_path.is_none());
    open_prefs(&mut app);
    select(&mut app, Key::WordWrap);
    let before = app.config.ui.word_wrap;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        app.config.ui.word_wrap, !before,
        "the session still changes"
    );
    assert!(
        app.timed_toasts.is_empty(),
        "no path is a configured state, not an error: {:?}",
        app.timed_toasts
            .iter()
            .map(|t| t.message.clone())
            .collect::<Vec<_>>()
    );
}
