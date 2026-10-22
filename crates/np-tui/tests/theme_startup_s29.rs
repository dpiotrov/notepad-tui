//! The configured theme must be in force the moment the editor opens.

use np_core::Config;
use np_tui::App;

/// Drain the toast queue through the public API.
///
/// `ToastQueue` keeps `items` private on purpose -- the renderer is the only consumer in
/// production -- so a test reads it the way the queue allows: pop until empty. Returns
/// `(message, is_error)` so a test can also assert a config problem is flagged as an error
/// rather than shown as an informational blip.
fn drain_toasts(app: &mut App) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    while let Some(toast) = app.toasts.pop() {
        out.push((toast.message, toast.error));
    }
    out
}

fn app_with_theme(theme: &str) -> App {
    let mut cfg = Config::default();
    cfg.ui.theme = theme.to_string();
    App::new_headless(cfg, (80, 24))
}

#[test]
fn a_configured_theme_is_applied_at_startup() {
    // `config.ui.theme` is written to config.toml, shown in the About panel, and editable in
    // Preferences -- and was never read at startup. Every session opened on np-core's default and
    // the setting only took effect once Preferences happened to push it, so `theme = "Dracula"`
    // in a config file did nothing until the user opened a dialog. Themes shipped in S29 would
    // have been unusable from config without this.
    for name in ["Dracula", "Monokai", "One Dark", "Nord", "GitHub Light"] {
        let app = app_with_theme(name);
        assert_eq!(
            app.highlight.theme_name(),
            name,
            "{name} was configured but the engine opened on {}",
            app.highlight.theme_name()
        );
    }
}

#[test]
fn an_unknown_configured_theme_keeps_the_default_and_reports() {
    // A typo must not strip highlighting or refuse to start. `set_theme` keeps the previous theme;
    // the user is told which name was wrong rather than left to notice the editor ignored them.
    let mut app = app_with_theme("Drakula");
    assert_eq!(
        app.highlight.theme_name(),
        np_core::highlight::DEFAULT_THEME,
        "an unknown theme changed what the engine renders"
    );
    let toasts = drain_toasts(&mut app);
    assert!(
        toasts.iter().any(|(m, _)| m.contains("Drakula")),
        "no toast named the bad theme: {toasts:?}"
    );
}

#[test]
fn an_empty_configured_theme_is_not_reported_as_an_error() {
    // An empty value means "not set", not "a name that failed". It must fall back quietly.
    let mut app = app_with_theme("");
    assert_eq!(
        app.highlight.theme_name(),
        np_core::highlight::DEFAULT_THEME
    );
    let toasts = drain_toasts(&mut app);
    assert!(
        toasts.is_empty(),
        "an unset theme produced a toast: {toasts:?}"
    );
}

#[test]
fn the_configured_theme_survives_the_theme_picker_dialog() {
    // The picker opens scrolled to the active theme. If startup left the engine on the default
    // while config said otherwise, the dialog would highlight the wrong row.
    let mut app = app_with_theme("Nord");
    app.execute(np_tui::actions::Action::ThemePicker);
    let names = app.highlight.theme_names();
    let idx = names
        .iter()
        .position(|n| n == "Nord")
        .expect("Nord is a bundled theme");
    assert_eq!(
        app.theme_selected, idx,
        "the picker did not open on the configured theme"
    );
}
