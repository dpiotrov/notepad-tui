//! A theme picked in the dialog must survive a restart (S31).
//!
//! # The bug
//!
//! Picking a theme from the Theme dialog set `config.ui.theme` and repainted the editor. It did not
//! write the config file. So the change was completely visible for the rest of the session, and the
//! only evidence of the failure appeared on the next start, where the old theme came back.
//!
//! # Why manual testing could not find it
//!
//! Two entry points set the same field, and only one of them persisted: Preferences wrote the file,
//! the Theme picker did not. Testing through Preferences -- the dialog a reviewer reaches for when
//! asked "do settings stick?" -- gave a correct answer. The setting *was* persisted, by the other
//! path, so a check aimed at the field rather than at the gesture that sets it also passed. The
//! defect lived in the wiring between one specific gesture and one specific file.
//!
//! The general shape: a session-visible setting is not a persisted setting, and the two are only
//! the same thing if something writes. Assert the bytes on disk, reached through the same UI path a
//! person uses.

use np_core::Config;
use np_tui::app::{DialogKind, Mode};
use np_tui::Action;
use np_tui::App;

/// An app whose config lives in `dir`, with a document so the editor is in a normal state.
fn app_with_config_path(dir: &std::path::Path) -> App {
    let mut app = App::new_headless(Config::default(), (100, 30));
    app.config_path = Some(dir.join("config.toml"));
    app
}

/// Open the Theme dialog and pick `wanted` the way a person does: find the row, click it.
///
/// Driven through the public dialog API rather than by setting `config.ui.theme`, because the
/// assignment is not the thing under test. The gesture is.
fn pick_theme(app: &mut App, wanted: &str) {
    app.execute(Action::ThemePicker);
    assert!(
        matches!(app.mode(), Mode::Dialog(DialogKind::ThemePicker)),
        "the Theme dialog did not open",
    );
    let names = app.picker_candidates(DialogKind::ThemePicker);
    let row = names
        .iter()
        .position(|n| n == wanted)
        .unwrap_or_else(|| panic!("{wanted} is not in the theme list: {names:?}"));
    // `activate: true` is the release-half of a click, the decision to apply.
    app.click_dialog_row(row, true);
}

#[test]
fn a_theme_picked_in_the_dialog_is_written_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app_with_config_path(dir.path());

    pick_theme(&mut app, "Dracula");

    // In-session state first: the engine actually changed, so the test is about persistence and
    // not about a picker that silently did nothing at all.
    assert_eq!(app.highlight.theme_name(), "Dracula");
    assert_eq!(app.config.ui.theme, "Dracula");

    let path = dir.path().join("config.toml");
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no config was written to {}: {e}", path.display()));
    assert!(
        on_disk.contains("Dracula"),
        "the picked theme never reached the config file. Contents:\n{on_disk}"
    );
}

#[test]
fn a_picked_theme_survives_a_restart() {
    // The user-visible claim, asserted end to end: choose it, close, open again, still chosen.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");

    let mut app = app_with_config_path(dir.path());
    pick_theme(&mut app, "Nord");
    drop(app);

    let (reloaded, warnings) = Config::load(Some(&path));
    assert!(
        warnings.is_empty(),
        "reloading the config warned: {warnings:?}"
    );
    assert_eq!(
        reloaded.ui.theme, "Nord",
        "the theme did not survive the restart"
    );

    // And a fresh app reading that file renders with it, not merely stores it.
    let reopened = App::new_headless(reloaded, (100, 30));
    assert_eq!(
        reopened.highlight.theme_name(),
        "Nord",
        "config recorded Nord but the editor opened on {}",
        reopened.highlight.theme_name()
    );
}

#[test]
fn a_word_wrap_toggle_writes_to_the_same_file_the_app_reads() {
    // The second half of the reported class. `persist_ui` resolved its own path via
    // `Config::default_path()` instead of using `app.config_path`, so under `--config <path>` the
    // toggle applied for the session, wrote to `~/.config/npt/config.toml` -- a file that run is
    // not reading -- and the next launch came back with the old value. With a config file in play,
    // exactly one file may be touched.
    let dir = tempfile::tempdir().expect("tempdir");

    let mut app = app_with_config_path(dir.path());
    let before = app.config.ui.word_wrap;
    app.execute(Action::ToggleWordWrap);
    assert_ne!(
        app.config.ui.word_wrap, before,
        "precondition: the toggle did not change anything in memory"
    );

    let written = dir.path().join("config.toml");
    let text = std::fs::read_to_string(&written)
        .unwrap_or_else(|e| panic!("toggle did not write {}: {e}", written.display()));
    assert!(
        text.contains("word_wrap"),
        "the toggle's file does not mention word_wrap:\n{text}"
    );

    let (reloaded, _) = Config::load(Some(&written));
    assert_eq!(
        reloaded.ui.word_wrap, app.config.ui.word_wrap,
        "word wrap did not survive the restart"
    );
}

#[test]
fn a_failed_config_write_reports_an_error_toast() {
    // Persistence can fail -- read-only directory, a config path that is actually a directory, a
    // full disk. The setting still applies for the session, which is the right call, but the user
    // has to learn that it will not be there next time. Losing that message is how "it remembered
    // for an hour and then forgot" becomes a bug report nobody can reproduce.
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory in place of a file: every write to it fails, and fails the same way every time.
    let blocked = dir.path().join("is_a_directory");
    std::fs::create_dir_all(&blocked).expect("create blocking directory");

    let mut app = App::new_headless(Config::default(), (100, 30));
    app.config_path = Some(blocked.clone());
    app.persist_config();

    let mut found = None;
    while let Some(toast) = app.toasts.pop() {
        if toast.error {
            found = Some(toast.message);
        }
    }
    let message = found.expect("a failed config write produced no error toast");
    assert!(
        message.contains("config"),
        "the toast did not say what failed: {message}"
    );
}

#[test]
fn no_config_path_means_no_write_and_no_complaint() {
    // A session with nowhere to persist is a configured state, not a failure: writing a default
    // path nobody asked for would be worse, and a toast on every Alt+Z would be noise.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::new_headless(Config::default(), (100, 30));
    app.config_path = None;
    app.persist_config();

    // Drained into a Vec rather than `while let ... { panic! }`, which clippy correctly rejects as
    // a loop that can never iterate, and which would also report only the first complaint.
    let quiet: Vec<String> = (0..8)
        .filter_map(|_| app.toasts.pop())
        .map(|t| t.message)
        .collect();
    assert!(
        quiet.is_empty(),
        "a session with no config path complained: {quiet:?}"
    );
    // And nothing was created behind the app's back.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        leftovers.is_empty(),
        "files appeared in an untouched directory: {leftovers:?}"
    );
}

#[test]
fn both_theme_entry_points_persist() {
    // The bug was two paths to one setting with one writer. This asserts both write, so a future
    // edit that makes either one memory-only fails here instead of in someone's dotfiles.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");

    // Path 1: the Theme dialog.
    let mut app = app_with_config_path(dir.path());
    pick_theme(&mut app, "Dracula");
    let (after_picker, _) = Config::load(Some(&path));
    assert_eq!(
        after_picker.ui.theme, "Dracula",
        "the dialog did not persist"
    );

    // Path 2: Preferences, on an app that starts from what the dialog wrote.
    let mut app2 = App::new_headless(after_picker, (100, 30));
    app2.config_path = Some(path.clone());
    assert!(
        np_tui::ui::prefs::apply(&mut app2, np_tui::ui::prefs::Key::Theme, "GitHub Light"),
        "Preferences rejected the theme key"
    );
    let (after_prefs, _) = Config::load(Some(&path));
    assert_eq!(
        after_prefs.ui.theme, "GitHub Light",
        "Preferences did not persist"
    );
}
