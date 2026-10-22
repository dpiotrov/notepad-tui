//! Stage S9 acceptance for configuration: every §7.1 key, tolerant loading, the commented
//! default file, and XDG paths (FR-CG1, FR-CG2, FR-CG4).

use np_core::{Config, EncodingId, Eol};

fn load(text: &str) -> (Config, Vec<String>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).expect("write");
    let (cfg, warns) = Config::load(Some(&path));
    // `dir` is dropped here, after the read.
    (cfg, warns)
}

// ------------------------------------------------------------- every default

#[test]
fn every_editor_default_matches_spec_7_1() {
    let c = Config::default();
    assert_eq!(c.editor.tab_width, 4);
    assert!(c.editor.insert_spaces);
    assert!(c.editor.auto_indent);
    assert!(c.editor.smart_backspace);
    assert!(c.editor.auto_pair);
    assert_eq!(c.editor.default_encoding, "utf-8");
    assert_eq!(c.editor.default_eol, "lf");
    assert_eq!(c.editor.eol_mode, "file");
    assert!(!c.editor.trim_trailing_ws_on_save);
    assert!(!c.editor.ensure_final_newline);
    assert!(!c.editor.backup_on_save);
    assert_eq!(c.editor.autosave_seconds, 0);
    // The documented addition to §7.1 (FR-E10).
    assert_eq!(c.editor.datetime_format, "%Y-%m-%d %H:%M:%S");
}

#[test]
fn every_ui_default_matches_spec_7_1() {
    let c = Config::default();
    assert_eq!(c.ui.theme, "base16-ocean.dark");
    assert!(c.ui.line_numbers);
    assert!(!c.ui.word_wrap);
    assert!(!c.ui.show_whitespace);
    assert!(!c.ui.show_eol);
    assert_eq!(c.ui.right_margin_col, 0);
    assert_eq!(c.ui.mouse_scroll_lines, 3);
    assert_eq!(c.ui.max_recent_files, 10);
    assert_eq!(c.ui.clipboard, "auto");
}

#[test]
fn every_find_default_matches_spec_7_1() {
    let c = Config::default();
    assert!(!c.find.case_sensitive);
    assert!(!c.find.whole_word);
    assert!(!c.find.regex);
    assert!(c.find.wrap_around);
    assert!(c.find.mark_matches);
}

// --------------------------------------------------------------- partial files

#[test]
fn a_partial_file_keeps_everything_else_default() {
    let (cfg, warns) = load("[ui]\ntheme = \"Solarized (dark)\"\n");
    assert_eq!(cfg.ui.theme, "Solarized (dark)");
    assert_eq!(cfg.ui.max_recent_files, 10);
    assert_eq!(cfg.editor.tab_width, 4);
    assert!(cfg.find.wrap_around);
    assert!(
        warns.is_empty(),
        "a valid partial file must be silent: {warns:?}"
    );
}

#[test]
fn a_single_keymap_entry_leaves_the_rest_default() {
    let (cfg, warns) = load("[keymap]\nsave = [\"ctrl+f12\"]\n");
    assert_eq!(cfg.keymap.get("save"), Some(&vec!["ctrl+f12".to_string()]));
    assert!(cfg.ui.line_numbers);
    assert!(warns.is_empty(), "{warns:?}");
}

// ----------------------------------------------------------------- unknown keys

#[test]
fn an_unknown_key_is_ignored_with_a_warning_and_the_load_succeeds() {
    let (cfg, warns) = load("[editor]\ntab_width = 2\nquantum_mode = true\n");
    assert_eq!(cfg.editor.tab_width, 2, "the known key still applied");
    assert!(
        warns.iter().any(|w| w.contains("editor.quantum_mode")),
        "{warns:?}"
    );
}

#[test]
fn an_unknown_top_level_table_is_reported() {
    let (cfg, warns) = load("[editor]\ntab_width = 2\n\n[gleaming_new_thing]\nx = 1\n");
    assert_eq!(cfg.editor.tab_width, 2);
    assert!(
        warns.iter().any(|w| w.contains("gleaming_new_thing")),
        "the unknown table must be named: {warns:?}"
    );
}

// ------------------------------------------------------------- wrong types

#[test]
fn a_wrongly_typed_value_falls_back_and_the_rest_of_the_file_survives() {
    // The point of the two-pass parse: one bad value must not take the user's theme,
    // keymap and search settings down with it.
    let (cfg, warns) = load(
        r#"
[editor]
tab_width = "wide"
insert_spaces = false

[ui]
theme = "Solarized"
line_numbers = false

[find]
regex = true
"#,
    );
    assert_eq!(
        cfg.editor.tab_width, 4,
        "the bad value fell back to default"
    );
    assert!(
        !cfg.editor.insert_spaces,
        "the good value in the same section survived"
    );
    assert_eq!(cfg.ui.theme, "Solarized", "the next section survived");
    assert!(!cfg.ui.line_numbers);
    assert!(cfg.find.regex, "and the one after that too");
    assert!(
        warns
            .iter()
            .any(|w| w.contains("wrong type") || w.contains("defaults")),
        "the user must be told something fell back: {warns:?}"
    );
}

#[test]
fn a_bad_keymap_value_does_not_lose_the_good_ones() {
    let (cfg, _warns) = load(
        r#"
[editor]
tab_width = "wide"

[keymap]
save = ["ctrl+s"]
find = ["ctrl+f", "f6"]
"#,
    );
    assert_eq!(cfg.editor.tab_width, 4);
    assert_eq!(cfg.keymap.get("save"), Some(&vec!["ctrl+s".to_string()]));
    assert_eq!(
        cfg.keymap.get("find"),
        Some(&vec!["ctrl+f".into(), "f6".into()])
    );
}

#[test]
fn invalid_toml_falls_back_to_defaults_with_a_warning() {
    let (cfg, warns) = load("this is not = = toml");
    assert_eq!(cfg, Config::default());
    assert!(!warns.is_empty(), "silence would hide a broken file");
}

#[test]
fn a_keymap_binding_that_does_not_parse_is_rejected_by_the_keymap_not_by_config() {
    // Config stores strings; validity is `np-tui::keymap`'s judgment. A nonsense binding
    // must still load here, or a typo would disable the whole config file.
    let (cfg, _warns) = load("[keymap]\nsave = [\"ctrl+notakey\"]\n");
    assert_eq!(
        cfg.keymap.get("save"),
        Some(&vec!["ctrl+notakey".to_string()])
    );
}

// ------------------------------------------------------------------ round trip

#[test]
fn default_config_toml_round_trips_to_the_default_config() {
    // This is what keeps the hand-written commented literal honest: if a default changes in
    // the struct and not in the literal, this test fails.
    let text = Config::default_config_toml();
    let parsed: Config = toml::from_str(&text).expect("the default file must parse");
    assert_eq!(parsed, Config::default());
}

#[test]
fn default_config_toml_is_commented_and_readable() {
    let text = Config::default_config_toml();
    for section in ["[editor]", "[ui]", "[find]", "[keymap]"] {
        assert!(text.contains(section), "{section} missing");
    }
    assert!(text.contains('#'), "the comments are the point of the file");
    // The value ranges spec §7.1 documents must be visible.
    assert!(text.contains("lf | crlf | cr"), "eol options missing");
    assert!(
        text.contains("auto | osc52 | none"),
        "clipboard options missing"
    );
}

#[test]
fn default_config_toml_names_every_section_key() {
    // A user opening the file should find every knob, not a subset.
    let text = Config::default_config_toml();
    for key in [
        "tab_width",
        "insert_spaces",
        "auto_indent",
        "smart_backspace",
        "auto_pair",
        "default_encoding",
        "default_eol",
        "eol_mode",
        "trim_trailing_ws_on_save",
        "ensure_final_newline",
        "backup_on_save",
        "autosave_seconds",
        "datetime_format",
        "theme",
        "line_numbers",
        "word_wrap",
        "show_whitespace",
        "show_eol",
        "right_margin_col",
        "mouse_scroll_lines",
        "max_recent_files",
        "clipboard",
        "case_sensitive",
        "whole_word",
        "regex",
        "wrap_around",
        "mark_matches",
    ] {
        assert!(
            text.contains(&format!("{key} =")),
            "{key} missing from the default file"
        );
    }
}

#[test]
fn a_saved_config_reloads_identically_and_silently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("deep").join("nested").join("config.toml");
    let mut cfg = Config::default();
    cfg.editor.tab_width = 8;
    cfg.editor.autosave_seconds = 30;
    cfg.ui.clipboard = "osc52".into();
    cfg.keymap
        .insert("save".into(), vec!["ctrl+s".into(), "ctrl+f12".into()]);
    cfg.save(&path).expect("save");
    let (back, warns) = Config::load(Some(&path));
    assert!(
        warns.is_empty(),
        "a file this program wrote must not warn: {warns:?}"
    );
    assert_eq!(back, cfg);
}

// ---------------------------------------------------------------------- XDG

#[test]
fn xdg_config_home_is_honored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg_home = dir.path().join("cfg");
    std::fs::create_dir_all(&cfg_home).expect("mkdir");

    // `dirs` reads the environment, and the process env is shared with other tests in this
    // binary, so the whole assertion runs serialized against a guard.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
    let found = Config::default_path();
    match previous {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }

    let found = found.expect("a config path must resolve");
    assert_eq!(
        found,
        cfg_home.join("npt").join("config.toml"),
        "XDG_CONFIG_HOME must decide the location"
    );
}

#[test]
fn xdg_data_home_is_honored_for_the_session_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_home = dir.path().join("data");
    std::fs::create_dir_all(&data_home).expect("mkdir");

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("XDG_DATA_HOME", &data_home);
    let found = np_core::Session::default_path();
    match previous {
        Some(v) => std::env::set_var("XDG_DATA_HOME", v),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }

    let found = found.expect("a data path must resolve");
    assert_eq!(found, data_home.join("npt").join("session.toml"));
}

#[test]
fn an_absolute_xdg_path_is_used_verbatim() {
    // `dirs` ignores a relative XDG value; a test that wrote one would silently measure
    // nothing, so assert the documented behavior rather than a hopeful one.
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("XDG_CONFIG_HOME", &abs);
    let found = Config::default_path();
    match previous {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    assert!(
        found.as_ref().map(|p| p.starts_with(&abs)).unwrap_or(false),
        "{found:?}"
    );
}

// ------------------------------------------------------- values the app must obey

#[test]
fn the_resolved_types_agree_with_the_parsed_strings() {
    let (cfg, _warns) = load(
        r#"
[editor]
default_encoding = "windows-1251"
default_eol = "crlf"
eol_mode = "lf"
"#,
    );
    assert_eq!(
        cfg.default_encoding(),
        EncodingId::parse("windows-1251").expect("codec")
    );
    assert_eq!(cfg.default_eol(), Eol::Crlf);
    assert_eq!(cfg.eol_override(), Some(Eol::Lf));
}

#[test]
fn an_unusable_tab_width_warns_but_still_loads() {
    // `tab_width = 0` parses fine as a usize but no column math can use it; the warning is
    // what tells the user why their setting appears to do nothing.
    let (cfg, warns) = load("[editor]\ntab_width = 0\n");
    assert_eq!(cfg.editor.tab_width, 0);
    assert!(
        warns.iter().any(|w| w.contains("editor.tab_width")),
        "{warns:?}"
    );
}

#[test]
fn a_path_that_is_a_directory_reports_a_warning_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (cfg, warns) = Config::load(Some(dir.path()));
    assert_eq!(cfg, Config::default());
    assert!(!warns.is_empty(), "reading a directory is a real problem");
}

/// Serializes the process-wide environment across the XDG tests.
///
/// Env vars are process-global and cargo runs tests in threads, so without this one test's
/// `XDG_CONFIG_HOME` leaks into another's `default_path()` and both fail non-deterministically.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
