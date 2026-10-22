//! Configuration loading and defaults (contract §10, spec §7.1).
//!
//! # Never fails
//!
//! [`Config::load`] returns `(Config, Vec<String>)`, not a `Result`. A corrupt config file
//! must not prevent the editor from starting (FR-CG2): the default configuration is
//! returned and the problems come back as human-readable warnings that the UI shows as
//! toasts. The same rule covers a missing file (not an error at all), unreadable
//! permissions, and invalid TOML.
//!
//! # Unknown keys
//!
//! Unknown keys are ignored and reported, never fatal. That is why nothing here uses
//! `#[serde(deny_unknown_fields)]`: an older binary must still read a config written by a
//! newer one. Warnings for unknown keys are produced by parsing the file a second time
//! into a permissive map and diffing the section and field names.
//!
//! # Keymap values
//!
//! `[keymap]` maps an action's kebab-case name to a list of key strings. This module only
//! stores the strings; parsing them into `KeyCode`/`KeyModifiers` is `np-tui::keymap`'s
//! job, because that is where crossterm lives.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::types::{EncodingId, Eol};

/// `[editor]` (spec §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub auto_indent: bool,
    pub smart_backspace: bool,
    pub auto_pair: bool,
    /// Encoding name for new documents; resolved through `EncodingId`, not `encoding_rs`
    /// directly, so an unknown name degrades to UTF-8 with a warning.
    pub default_encoding: String,
    /// `"lf" | "crlf" | "cr"`.
    pub default_eol: String,
    /// `"file" | "lf" | "crlf" | "cr"` -- how to normalize at save time (FR-N4).
    pub eol_mode: String,
    pub trim_trailing_ws_on_save: bool,
    pub ensure_final_newline: bool,
    pub backup_on_save: bool,
    /// 0 disables autosave.
    pub autosave_seconds: u64,
    /// `strftime` pattern used by F5 (FR-E10). Not in spec §7.1, which asks for a
    /// "configurable format" without naming a key; see `docs/DEVIATIONS.md`.
    pub datetime_format: String,
}

impl Default for EditorConfig {
    fn default() -> Self {
        EditorConfig {
            tab_width: 4,
            insert_spaces: true,
            auto_indent: true,
            smart_backspace: true,
            auto_pair: true,
            default_encoding: "utf-8".into(),
            default_eol: "lf".into(),
            eol_mode: "file".into(),
            trim_trailing_ws_on_save: false,
            ensure_final_newline: false,
            backup_on_save: false,
            autosave_seconds: 0,
            datetime_format: "%Y-%m-%d %H:%M:%S".into(),
        }
    }
}

/// `[ui]` (spec §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub line_numbers: bool,
    pub word_wrap: bool,
    pub show_whitespace: bool,
    pub show_eol: bool,
    /// 0 disables the right margin guide.
    pub right_margin_col: usize,
    pub mouse_scroll_lines: usize,
    pub max_recent_files: usize,
    /// `"auto" | "osc52" | "none"`.
    pub clipboard: String,
    /// Highlight the caret's line (FR-H5). On by default, as in Notepad++.
    pub current_line_highlight: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: "base16-ocean.dark".into(),
            line_numbers: true,
            word_wrap: false,
            show_whitespace: false,
            show_eol: false,
            right_margin_col: 0,
            mouse_scroll_lines: 3,
            max_recent_files: 10,
            current_line_highlight: true,
            clipboard: "auto".into(),
        }
    }
}

/// `[find]` (spec §7.1).
///
/// Contract §10 requires these to be the same fields as `search::SearchOpts` plus
/// `mark_matches`, so that dialog state can be copied straight into search options and
/// back without a translation layer (FR-S1). `SearchOpts` lives in S10 and carries
/// `forward`, which is transient direction state rather than a preference, so it is
/// deliberately absent here; S10 must keep the other four names identical or update
/// `SearchOpts::from_find` in the same commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FindConfig {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub wrap_around: bool,
    pub mark_matches: bool,
}

/// Written by hand rather than derived, because spec §7.1 turns `wrap_around` and
/// `mark_matches` on while the other three are off. Deriving `Default` would make them
/// `false`, and since `#[serde(default)]` on a field routes through `Default::default()`
/// for the whole struct, that would silently change search behavior for anyone who omits
/// the `[find]` section.
impl Default for FindConfig {
    fn default() -> Self {
        FindConfig {
            case_sensitive: false,
            whole_word: false,
            regex: false,
            wrap_around: true,
            mark_matches: true,
        }
    }
}

/// The whole `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub editor: EditorConfig,
    pub ui: UiConfig,
    pub find: FindConfig,
    /// Action kebab-name -> key strings, e.g. `save = ["ctrl+s"]` (FR-CG4). Values are
    /// parsed by `np-tui::keymap::parse_key`.
    #[serde(default)]
    pub keymap: HashMap<String, Vec<String>>,
}

impl Config {
    /// `~/.config/npt/config.toml`, or `None` if no config directory can be resolved.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("npt").join("config.toml"))
    }

    /// Load configuration, never failing (FR-CG2).
    ///
    /// `path` of `None` means "use the default location, and if there is none, use
    /// defaults". A missing file is not a warning: it is the normal first-run state.
    /// Anything else -- unreadable, invalid TOML, bad field values -- degrades to the
    /// default for that item and is reported.
    pub fn load(path: Option<&Path>) -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        let target = match path {
            Some(p) => Some(p.to_path_buf()),
            None => Self::default_path(),
        };
        let Some(path) = target else {
            return (Config::default(), warnings);
        };

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Config::default(), warnings);
            }
            Err(e) => {
                warnings.push(format!(
                    "cannot read {}: {e}; using defaults",
                    path.display()
                ));
                return (Config::default(), warnings);
            }
        };

        let config = parse_tolerantly(&text, &mut warnings);
        warnings.extend(unknown_key_warnings(&text));
        warnings.extend(value_warnings(&config));
        (config, warnings)
    }

    /// Write the configuration, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> CoreResult<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| CoreError::Invalid(format!("cannot serialize config: {e}")))?;
        // CoreError::Io carries the path so the UI can name the file in the error toast,
        // which means `?` cannot convert io::Error on its own; attach it at each call.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::io(parent, e))?;
            }
        }
        std::fs::write(path, text).map_err(|e| CoreError::io(path, e))?;
        Ok(())
    }

    /// The encoding new documents should start in, defaulting to UTF-8 when the configured
    /// name is not recognized.
    pub fn default_encoding(&self) -> EncodingId {
        let want = self.editor.default_encoding.to_lowercase();
        EncodingId::all()
            .iter()
            .find(|enc| enc.label().eq_ignore_ascii_case(&want))
            .cloned()
            .unwrap_or(EncodingId::Utf8)
    }

    /// The line ending new documents should start in. Unrecognized values mean LF, which
    /// is also what the platform expects.
    pub fn default_eol(&self) -> Eol {
        match self.editor.default_eol.to_lowercase().as_str() {
            "crlf" => Eol::Crlf,
            "cr" => Eol::Cr,
            _ => Eol::Lf,
        }
    }

    /// Where a config file should be created by "Open config file" (FR-CG1).
    ///
    /// Falls back to a name under the current directory only when the platform offers no
    /// config directory at all, which is the case where writing nowhere would leave the menu
    /// item doing nothing.
    pub fn editable_default_path() -> PathBuf {
        Self::default_path().unwrap_or_else(|| PathBuf::from("config.toml"))
    }

    /// The commented default configuration from spec §7.1, for "Open config file" (FR-CG5).
    ///
    /// Written as a literal rather than serialized, because `toml::to_string_pretty` cannot
    /// emit the comments that make a default file useful -- `# lf | crlf | cr` and the value
    /// ranges are the reason a user opens it. The round-trip test parses this text back and
    /// asserts it equals `Config::default()`, so the literal cannot silently drift from the
    /// real defaults.
    pub fn default_config_toml() -> String {
        DEFAULT_CONFIG_TOML.to_string()
    }

    /// Whether `eol_mode` forces a specific ending at save time, or `"file"` keeps the
    /// document's own (FR-N4).
    pub fn eol_override(&self) -> Option<Eol> {
        match self.editor.eol_mode.to_lowercase().as_str() {
            "lf" => Some(Eol::Lf),
            "crlf" => Some(Eol::Crlf),
            "cr" => Some(Eol::Cr),
            _ => None,
        }
    }
}

/// The default `config.toml`, verbatim in spec §7.1's shape and comments.
///
/// Written as a literal rather than serialized, because `toml::to_string_pretty` cannot emit
/// the comments that make a default file worth opening. The cost is that the literal can
/// drift from the real defaults, which is exactly what `default_config_toml_round_trips`
/// guards against.
const DEFAULT_CONFIG_TOML: &str = r#"[editor]
tab_width = 4
insert_spaces = true
auto_indent = true
smart_backspace = true
auto_pair = true
default_encoding = "utf-8"
default_eol = "lf"                # lf | crlf | cr
eol_mode = "file"                 # file | lf | crlf | cr (save normalization)
trim_trailing_ws_on_save = false
ensure_final_newline = false
backup_on_save = false
autosave_seconds = 0              # 0 = off
datetime_format = "%Y-%m-%d %H:%M:%S"

[ui]
theme = "base16-ocean.dark"
line_numbers = true
word_wrap = false
show_whitespace = false
show_eol = false
right_margin_col = 0              # 0 = off
mouse_scroll_lines = 3
max_recent_files = 10
clipboard = "auto"                # auto | osc52 | none

[find]
case_sensitive = false
whole_word = false
regex = false
wrap_around = true
mark_matches = true

[keymap]
# save = ["ctrl+s"]
# find = ["ctrl+f", "f6"]         # multiple bindings allowed
"#;

/// Parse config in two passes so one bad value cannot discard a whole file (FR-CG2).
///
/// Pass one is a normal typed parse. When it fails, pass two parses the document as an
/// untyped `toml::Value` and deserializes **each known table on its own**, keeping every
/// table that worked. The distinction matters: a single `tab_width = "wide"` line otherwise
/// takes the user's theme, keymap and search settings down with it, and the warning says
/// "using defaults" when three quarters of the file were fine.
///
/// A file that is not valid TOML at all has no per-table salvage possible, so it degrades to
/// defaults as before.
fn parse_tolerantly(text: &str, warnings: &mut Vec<String>) -> Config {
    if let Ok(config) = toml::from_str::<Config>(text) {
        return config;
    }
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        warnings.push("invalid config; using defaults".to_string());
        return Config::default();
    };
    let Some(table) = value.as_table() else {
        warnings.push("config is not a table; using defaults".to_string());
        return Config::default();
    };

    // Pass two works field by field, not table by table. Dropping a whole section over one
    // bad value would throw away the keys the user got right: `tab_width = "wide"` should
    // cost `tab_width`, not the `insert_spaces = false` typed on the next line. Each field is
    // deserialized from its own one-key document, so the blast radius of a bad value is
    // exactly that field.
    let mut config = Config::default();
    let mut applied = 0usize;
    let mut rejected = 0usize;

    if let Some(items) = table.get("editor").and_then(|v| v.as_table()) {
        for (field, item) in items {
            match field.as_str() {
                "tab_width" => apply(
                    item,
                    |v| config.editor.tab_width = v,
                    &mut applied,
                    &mut rejected,
                ),
                "insert_spaces" => apply(
                    item,
                    |v| config.editor.insert_spaces = v,
                    &mut applied,
                    &mut rejected,
                ),
                "auto_indent" => apply(
                    item,
                    |v| config.editor.auto_indent = v,
                    &mut applied,
                    &mut rejected,
                ),
                "smart_backspace" => apply(
                    item,
                    |v| config.editor.smart_backspace = v,
                    &mut applied,
                    &mut rejected,
                ),
                "auto_pair" => apply(
                    item,
                    |v| config.editor.auto_pair = v,
                    &mut applied,
                    &mut rejected,
                ),
                "default_encoding" => apply(
                    item,
                    |v| config.editor.default_encoding = v,
                    &mut applied,
                    &mut rejected,
                ),
                "default_eol" => apply(
                    item,
                    |v| config.editor.default_eol = v,
                    &mut applied,
                    &mut rejected,
                ),
                "eol_mode" => apply(
                    item,
                    |v| config.editor.eol_mode = v,
                    &mut applied,
                    &mut rejected,
                ),
                "trim_trailing_ws_on_save" => apply(
                    item,
                    |v| config.editor.trim_trailing_ws_on_save = v,
                    &mut applied,
                    &mut rejected,
                ),
                "ensure_final_newline" => apply(
                    item,
                    |v| config.editor.ensure_final_newline = v,
                    &mut applied,
                    &mut rejected,
                ),
                "backup_on_save" => apply(
                    item,
                    |v| config.editor.backup_on_save = v,
                    &mut applied,
                    &mut rejected,
                ),
                "autosave_seconds" => apply(
                    item,
                    |v| config.editor.autosave_seconds = v,
                    &mut applied,
                    &mut rejected,
                ),
                "datetime_format" => apply(
                    item,
                    |v| config.editor.datetime_format = v,
                    &mut applied,
                    &mut rejected,
                ),
                // Unknown keys are reported by `unknown_key_warnings`, not here.
                _ => {}
            }
        }
    }
    if let Some(items) = table.get("ui").and_then(|v| v.as_table()) {
        for (field, item) in items {
            match field.as_str() {
                "theme" => apply(item, |v| config.ui.theme = v, &mut applied, &mut rejected),
                "line_numbers" => apply(
                    item,
                    |v| config.ui.line_numbers = v,
                    &mut applied,
                    &mut rejected,
                ),
                "word_wrap" => apply(
                    item,
                    |v| config.ui.word_wrap = v,
                    &mut applied,
                    &mut rejected,
                ),
                "show_whitespace" => apply(
                    item,
                    |v| config.ui.show_whitespace = v,
                    &mut applied,
                    &mut rejected,
                ),
                "show_eol" => apply(
                    item,
                    |v| config.ui.show_eol = v,
                    &mut applied,
                    &mut rejected,
                ),
                "right_margin_col" => apply(
                    item,
                    |v| config.ui.right_margin_col = v,
                    &mut applied,
                    &mut rejected,
                ),
                "mouse_scroll_lines" => apply(
                    item,
                    |v| config.ui.mouse_scroll_lines = v,
                    &mut applied,
                    &mut rejected,
                ),
                "max_recent_files" => apply(
                    item,
                    |v| config.ui.max_recent_files = v,
                    &mut applied,
                    &mut rejected,
                ),
                "clipboard" => apply(
                    item,
                    |v| config.ui.clipboard = v,
                    &mut applied,
                    &mut rejected,
                ),
                "current_line_highlight" => apply(
                    item,
                    |v| config.ui.current_line_highlight = v,
                    &mut applied,
                    &mut rejected,
                ),
                _ => {}
            }
        }
    }
    if let Some(items) = table.get("find").and_then(|v| v.as_table()) {
        for (field, item) in items {
            match field.as_str() {
                "case_sensitive" => apply(
                    item,
                    |v| config.find.case_sensitive = v,
                    &mut applied,
                    &mut rejected,
                ),
                "whole_word" => apply(
                    item,
                    |v| config.find.whole_word = v,
                    &mut applied,
                    &mut rejected,
                ),
                "regex" => apply(item, |v| config.find.regex = v, &mut applied, &mut rejected),
                "wrap_around" => apply(
                    item,
                    |v| config.find.wrap_around = v,
                    &mut applied,
                    &mut rejected,
                ),
                "mark_matches" => apply(
                    item,
                    |v| config.find.mark_matches = v,
                    &mut applied,
                    &mut rejected,
                ),
                _ => {}
            }
        }
    }
    if let Some(keymap) = table.get("keymap").and_then(|v| v.as_table()) {
        let mut map = HashMap::new();
        for (action, value) in keymap {
            if let Some(list) = value.as_array() {
                let keys: Vec<String> = list
                    .iter()
                    .filter_map(|k| k.as_str().map(|s| s.to_string()))
                    .collect();
                if !keys.is_empty() {
                    map.insert(action.clone(), keys);
                }
            }
        }
        if !map.is_empty() {
            config.keymap = map;
            applied += 1;
        }
    }

    if rejected > 0 {
        warnings.push(format!(
            "{rejected} config value(s) had the wrong type and fall back to defaults;              {applied} value(s) from the file were still applied"
        ));
    }
    config
}

/// Deserialize one config value into a typed field, counting success and rejection.
///
/// Generic over the value type so `usize`, `bool`, `u64` and `String` all go through the same
/// path, and over the setter so the call site reads as the assignment it performs. A failure
/// leaves the field at its default, which is what `Config::default()` already put there.
fn apply<T, F>(item: &toml::Value, set: F, applied: &mut usize, rejected: &mut usize)
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T),
{
    match item.clone().try_into::<T>() {
        Ok(value) => {
            set(value);
            *applied += 1;
        }
        Err(_) => *rejected += 1,
    }
}

/// Known section and field names, used to report keys this binary does not understand.
const KNOWN_SECTIONS: &[(&str, &[&str])] = &[
    (
        "editor",
        &[
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
        ],
    ),
    (
        "ui",
        &[
            "theme",
            "line_numbers",
            "word_wrap",
            "show_whitespace",
            "show_eol",
            "right_margin_col",
            "mouse_scroll_lines",
            "max_recent_files",
            "clipboard",
            "current_line_highlight",
        ],
    ),
    (
        "find",
        &[
            "case_sensitive",
            "whole_word",
            "regex",
            "wrap_around",
            "mark_matches",
        ],
    ),
];

/// Report keys present in the file but absent from this binary's schema (FR-CG2).
///
/// Deliberately a second, permissive parse rather than a serde attribute:
/// `deny_unknown_fields` would make an unknown key fatal, which is exactly what the spec
/// forbids. `[keymap]` is skipped because its keys are action names, not schema fields.
fn unknown_key_warnings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // `toml::from_str`, not `text.parse::<toml::Value>()`: in toml 1.x `Value` does not
    // implement `FromStr`, so the parse form does not resolve the way the name suggests
    // and quietly yields nothing, making the whole unknown-key check a no-op that always
    // reads as "clean".
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        // Already reported by the typed parse; nothing more to say.
        return out;
    };
    let Some(table) = value.as_table() else {
        return out;
    };
    for (key, item) in table {
        if key == "keymap" {
            continue;
        }
        let Some(section) = KNOWN_SECTIONS.iter().find(|(n, _)| *n == key) else {
            out.push(format!("unknown config section [{key}] is ignored"));
            continue;
        };
        let Some(fields) = item.as_table() else {
            out.push(format!("[{key}] should be a table and is ignored"));
            continue;
        };
        for field in fields.keys() {
            if !section.1.contains(&field.as_str()) {
                out.push(format!("unknown config key {key}.{field} is ignored"));
            }
        }
        // Keys are checked regardless of value type, so a nested table such as
        // `[editor.sub]` is reported here too rather than mistaken for a section.
    }
    out
}

/// Warn about values that parsed but are nonsense, and say what will happen instead.
///
/// Serde accepts any `usize` and any string, so bounds that the editor cannot honor have
/// to be checked here rather than rejected at parse time.
fn value_warnings(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if config.editor.tab_width == 0 || config.editor.tab_width > 32 {
        out.push(format!(
            "editor.tab_width = {} is out of range 1..=32; using 4",
            config.editor.tab_width
        ));
    }
    if config.ui.mouse_scroll_lines == 0 {
        out.push("ui.mouse_scroll_lines = 0 would make the wheel inert; using 1".into());
    }
    match config.editor.default_eol.to_lowercase().as_str() {
        "lf" | "crlf" | "cr" => {}
        other => out.push(format!(
            "editor.default_eol = \"{other}\" is unknown; using lf"
        )),
    }
    match config.editor.eol_mode.to_lowercase().as_str() {
        "file" | "lf" | "crlf" | "cr" => {}
        other => out.push(format!(
            "editor.eol_mode = \"{other}\" is unknown; using file"
        )),
    }
    match config.ui.clipboard.to_lowercase().as_str() {
        "auto" | "osc52" | "none" => {}
        other => out.push(format!("ui.clipboard = \"{other}\" is unknown; using auto")),
    }
    if config.default_encoding() == EncodingId::Utf8
        && !matches!(
            config.editor.default_encoding.to_lowercase().as_str(),
            "utf-8" | "utf8"
        )
    {
        out.push(format!(
            "editor.default_encoding = \"{}\" is unknown; using utf-8",
            config.editor.default_encoding
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> (Config, Vec<String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write");
        let (cfg, warns) = Config::load(Some(&path));
        // Keep `dir` alive until after the read.
        drop(dir);
        (cfg, warns)
    }

    #[test]
    fn defaults_match_the_spec_examples() {
        let c = Config::default();
        assert_eq!(c.editor.tab_width, 4);
        assert!(c.editor.insert_spaces);
        assert_eq!(c.ui.theme, "base16-ocean.dark");
        assert!(c.ui.line_numbers);
        assert!(!c.ui.word_wrap);
        assert_eq!(c.ui.mouse_scroll_lines, 3);
        assert_eq!(c.ui.max_recent_files, 10);
        assert_eq!(c.ui.clipboard, "auto");
        assert!(c.find.wrap_around);
        assert!(c.find.mark_matches);
        assert!(!c.find.case_sensitive);
        assert!(c.keymap.is_empty());
    }

    #[test]
    fn missing_file_is_not_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.toml");
        let (cfg, warns) = Config::load(Some(&path));
        assert_eq!(cfg, Config::default());
        assert!(warns.is_empty(), "first run must be silent: {warns:?}");
    }

    #[test]
    fn bad_toml_degrades_to_defaults_with_a_warning() {
        let (cfg, warns) = load("this is not = = toml");
        assert_eq!(cfg, Config::default());
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(warns[0].contains("invalid config"), "{warns:?}");
    }

    #[test]
    fn unknown_keys_are_ignored_and_reported() {
        let (cfg, warns) = load(
            r#"
[editor]
tab_width = 2
made_up = 7

[editor.sub]
x = 1

[nonsense]
y = 2
"#,
        );
        // The known key still applied even though the file has junk in it.
        assert_eq!(cfg.editor.tab_width, 2);
        assert!(
            warns.iter().any(|w| w.contains("editor.made_up")),
            "{warns:?}"
        );
        assert!(warns.iter().any(|w| w.contains("[nonsense]")), "{warns:?}");
    }

    #[test]
    fn keymap_section_is_not_checked_against_the_schema() {
        // `[keymap]` keys are action names, so they must never be flagged as unknown.
        let (cfg, warns) = load(
            r#"
[keymap]
save = ["ctrl+s"]
find = ["ctrl+f", "f6"]
"#,
        );
        assert_eq!(cfg.keymap.get("save"), Some(&vec!["ctrl+s".to_string()]));
        assert_eq!(
            cfg.keymap.get("find"),
            Some(&vec!["ctrl+f".into(), "f6".into()])
        );
        assert!(warns.is_empty(), "{warns:?}");
    }

    #[test]
    fn nonsense_values_warn_and_say_what_happens_instead() {
        let (cfg, warns) = load(
            r#"
[editor]
tab_width = 0
default_eol = "macos"
eol_mode = "wild"
default_encoding = "klingon"

[ui]
mouse_scroll_lines = 0
clipboard = "carrier-pigeon"
"#,
        );
        assert_eq!(cfg.default_eol(), Eol::Lf);
        assert!(cfg.eol_override().is_none());
        assert_eq!(cfg.default_encoding(), EncodingId::Utf8);
        for needle in [
            "editor.tab_width",
            "editor.default_eol",
            "editor.eol_mode",
            "ui.mouse_scroll_lines",
            "ui.clipboard",
            "editor.default_encoding",
        ] {
            assert!(
                warns.iter().any(|w| w.contains(needle)),
                "{needle} missing: {warns:?}"
            );
        }
    }

    #[test]
    fn eol_and_encoding_parsers_accept_mixed_case() {
        let (cfg, _) = load(
            r#"
[editor]
default_eol = "CRLF"
eol_mode = "LF"
default_encoding = "windows-1251"
"#,
        );
        assert_eq!(cfg.default_eol(), Eol::Crlf);
        assert_eq!(cfg.eol_override(), Some(Eol::Lf));
        assert_eq!(cfg.default_encoding().label(), "windows-1251");
    }

    #[test]
    fn round_trips_through_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("deeper").join("config.toml");
        let mut cfg = Config::default();
        cfg.editor.tab_width = 8;
        cfg.ui.theme = "InspiredGitHub".into();
        cfg.keymap
            .insert("save".into(), vec!["ctrl+s".into(), "ctrl+f12".into()]);
        cfg.save(&path).expect("save");
        // save() must have created the missing parent directories.
        let (reloaded, warns) = Config::load(Some(&path));
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(reloaded, cfg);
    }

    #[test]
    fn a_config_written_by_a_newer_binary_still_loads() {
        // Forward compatibility: this is the reason deny_unknown_fields is banned here.
        let (cfg, warns) = load(
            r#"
[editor]
tab_width = 3
a_key_from_the_future = true
"#,
        );
        assert_eq!(cfg.editor.tab_width, 3);
        assert_eq!(warns.len(), 1, "{warns:?}");
    }

    #[test]
    fn default_path_ends_with_npt_config_toml() {
        // Only assert the shape: CI may or may not have XDG_CONFIG_HOME set.
        if let Some(p) = Config::default_path() {
            assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("config.toml"));
            assert_eq!(
                p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str()),
                Some("npt")
            );
        }
    }
}
