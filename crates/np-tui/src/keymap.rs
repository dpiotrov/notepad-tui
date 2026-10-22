//! Key parsing, the default keymap, and `[keymap]` overrides (contract §10, spec §6.10).
//!
//! # Canonical form
//!
//! `format_key` emits lowercase with modifiers in the fixed order
//! `ctrl+alt+shift+super`, and `parse_key` accepts any order and any case. The two are
//! exact inverses on canonical strings, which the round-trip test enforces over every
//! default binding; that is what makes a config written by a human and a config written by
//! the Preferences dialog behave identically.
//!
//! # Conflicts
//!
//! Two bindings for one action are fine and both resolve. One key bound to two actions is a
//! conflict: the last definition wins and `log::warn!` names both actions (FR-CG4).
//! Silently dropping a binding is the worse failure because the user has no way to
//! discover which of their keys stopped working.
//!
//! # Press filtering
//!
//! `resolve` returns `None` unless `kind == Press`. With the kitty keyboard protocol on,
//! crossterm delivers `Repeat` and `Release` as well, so an unfiltered match doubles every
//! held key (see `docs/CONTEXT.md`).

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::actions::{Action, MenuId};

/// A parsed key chord: the code plus its modifiers.
pub type Chord = (KeyCode, KeyModifiers);

/// Parse a key string such as `"ctrl+shift+s"`, `"f3"`, `"alt+z"`, `"ctrl+pgdn"`,
/// `"ctrl+up"` or `"ctrl+↑"`.
///
/// Returns `None` for anything malformed rather than guessing: an unparseable config entry
/// is reported and skipped, whereas a mis-parse would bind a key the user did not ask for.
/// Case-insensitive, and modifier order is not significant.
pub fn parse_key(s: &str) -> Option<Chord> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    // '+' is both the separator and a key name. A string ending in "++" carries a literal
    // plus as its final token ("ctrl++" is Ctrl-Plus) and is rewritten before splitting. A
    // *bare* "+" is deliberately rejected: it is ambiguous between the separator and the
    // key, `format_key` never emits it (it emits "plus"), so accepting it would break the
    // parse/format inverse. Write "plus" for an unmodified plus key. A single trailing '+'
    // after a modifier names no key at all ("ctrl+", "ctrl+shift+") and is malformed, as is
    // a leading one ("+x"). Deciding all this here keeps the modifier loop free of cases.
    let body = match trimmed.strip_suffix("++") {
        Some(head) => format!("{head}+plus"),
        None => trimmed.to_string(),
    };
    let parts: Vec<&str> = body.split('+').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return None;
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut super_ = false;
    let mut code: Option<KeyCode> = None;

    let last_index = parts.len() - 1;
    for (i, raw) in parts.iter().enumerate() {
        let p = raw.trim().to_lowercase();
        let is_last = i == last_index;
        match p.as_str() {
            "ctrl" | "control" if !is_last => ctrl = true,
            "alt" | "meta" if !is_last => alt = true,
            "shift" if !is_last => shift = true,
            "super" | "win" | "cmd" | "logo" if !is_last => super_ = true,
            _ => {
                if code.is_some() {
                    // Two non-modifier tokens, e.g. "a+b".
                    return None;
                }
                code = Some(parse_key_code(&p)?);
            }
        }
    }

    let code = code?;
    let mut mods = KeyModifiers::empty();
    if ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if alt {
        mods |= KeyModifiers::ALT;
    }
    if shift {
        mods |= KeyModifiers::SHIFT;
    }
    if super_ {
        mods |= KeyModifiers::SUPER;
    }
    Some((code, mods))
}

/// Parse the non-modifier part of a chord.
fn parse_key_code(p: &str) -> Option<KeyCode> {
    let code = match p {
        // Whitespace and editing keys.
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "esc" | "escape" => KeyCode::Esc,
        "space" | "spacebar" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "print" => KeyCode::PrintScreen,
        "menu" => KeyCode::Menu,

        // Arrows, both spellings (spec accepts "ctrl+up" and "ctrl+↑").
        "up" | "↑" => KeyCode::Up,
        "down" | "↓" => KeyCode::Down,
        "left" | "←" => KeyCode::Left,
        "right" | "→" => KeyCode::Right,

        // Punctuation, spelled out or literal.
        "plus" => KeyCode::Char('+'),
        "+" => KeyCode::Char('+'),
        "minus" | "dash" => KeyCode::Char('-'),
        "-" => KeyCode::Char('-'),
        "comma" => KeyCode::Char(','),
        "," => KeyCode::Char(','),
        "period" | "dot" => KeyCode::Char('.'),
        "." => KeyCode::Char('.'),
        "slash" => KeyCode::Char('/'),
        "/" => KeyCode::Char('/'),
        "semicolon" => KeyCode::Char(';'),
        ";" => KeyCode::Char(';'),

        // Function keys.
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),

        other => {
            let mut it = other.chars();
            let c = it.next()?;
            // Exactly one scalar: "ab" and "" are both invalid.
            if it.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(code)
}

/// Render a chord in canonical form: lowercase, `ctrl+alt+shift+super`.
pub fn format_key(code: KeyCode, mods: KeyModifiers) -> String {
    let mut out = String::new();
    if mods.contains(KeyModifiers::CONTROL) {
        out.push_str("ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        out.push_str("alt+");
    }
    if mods.contains(KeyModifiers::SHIFT) {
        out.push_str("shift+");
    }
    if mods.contains(KeyModifiers::SUPER) {
        out.push_str("super+");
    }
    out.push_str(&code_name(code));
    out
}

/// The key portion of a canonical chord name.
fn code_name(code: KeyCode) -> String {
    match code {
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_lowercase().collect(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Menu => "menu".into(),
        _ => "unknown".into(),
    }
}

/// Key chord -> `Action`, with the §6.10 defaults plus config overrides.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    /// Canonical chord name -> action. Last binding wins on a conflict.
    bindings: HashMap<String, Action>,
    /// action kebab -> its chords, in definition order, for the help overlay (FR-U9).
    by_action: HashMap<Action, Vec<String>>,
}

/// The complete §6.10 default table as `(action kebab, key)` pairs.
///
/// Kept as strings rather than constructed chords so the table is readable next to the
/// spec and so a typo surfaces as a `parse_key` failure in tests rather than a silently
/// wrong `KeyCode`.
pub fn default_bindings() -> Vec<(&'static str, &'static str)> {
    vec![
        // File
        ("new-file", "ctrl+n"),
        ("open-file", "ctrl+o"),
        ("save", "ctrl+s"),
        ("save-as", "ctrl+shift+s"),
        ("close-tab", "ctrl+w"),
        // App exit: File > Exit plus Ctrl+Q when nothing is modified (spec §6.10 final
        // decision). The confirm-if-unsaved logic lives in App::execute.
        ("exit", "ctrl+q"),
        ("reload-file", "ctrl+shift+r"),
        // Edit
        ("undo", "ctrl+z"),
        ("redo", "ctrl+y"),
        ("redo", "ctrl+shift+z"),
        ("cut", "ctrl+x"),
        ("copy", "ctrl+c"),
        ("paste", "ctrl+v"),
        ("select-all", "ctrl+a"),
        ("delete", "delete"),
        // Search
        ("find", "ctrl+f"),
        ("find-next", "f3"),
        ("find-prev", "shift+f3"),
        ("replace", "ctrl+h"),
        ("find-word-next", "ctrl+f3"),
        ("find-word-prev", "ctrl+shift+f3"),
        ("find-in-files", "ctrl+shift+f"),
        ("go-to-line", "ctrl+g"),
        ("replace-all", "ctrl+shift+a"),
        // Bookmarks
        ("toggle-bookmark", "ctrl+f2"),
        ("next-bookmark", "f2"),
        ("prev-bookmark", "shift+f2"),
        ("clear-bookmarks", "ctrl+shift+f2"),
        // Line operations
        ("duplicate-line", "ctrl+d"),
        ("delete-line", "ctrl+l"),
        ("move-line-up", "ctrl+shift+up"),
        ("move-line-down", "ctrl+shift+down"),
        ("sort-lines-asc", "alt+shift+s"),
        // The spec §6.10 table stops at "Alt+Shift+arrows" and leaves the remaining
        // Alt+Shift chords to us. Alt+Shift+R is sort-lines-desc because that pair is
        // already listed as a set (Asc = Alt+Shift+S); reload-as-encoding (FR-N3, wired in
        // S12) is menu-only, which is also how Notepad++ exposes it. Binding both to one
        // chord would silently drop one of them.
        ("sort-lines-desc", "alt+shift+r"),
        // Case / comment / date
        ("upper-case", "ctrl+shift+u"),
        ("lower-case", "ctrl+u"),
        ("title-case", "ctrl+alt+u"),
        // Notepad++ parity: Ctrl+Q is toggle-comment, and app exit is also Ctrl+Q. The
        // spec resolves this in App::execute (exit only when nothing is modified), so the
        // keymap binds Ctrl+Q to exit and the Edit menu carries toggle-comment.
        ("toggle-comment", "ctrl+shift+q"),
        ("insert-date-time", "f5"),
        // Movement (FR-E2) and its selection forms (FR-E3).
        //
        // These were implemented in `App::execute` from S3 onward but never bound, so no
        // key could reach them: arrows, Home/End, word-wise movement and paging all
        // resolved to nothing and the caret could not be moved by keyboard at all. The
        // §6.10 table lists only the chord commands and leaves the plain navigation keys
        // implied -- FR-E2 names them explicitly ("arrows, Home/End, Ctrl+left/right
        // word-wise, Ctrl+Home/End, PgUp/PgDn"), so the omission is a bug rather than a
        // spec-sanctioned gap.
        ("move-left", "left"),
        ("move-right", "right"),
        ("move-up", "up"),
        ("move-down", "down"),
        ("move-left-select", "shift+left"),
        ("move-right-select", "shift+right"),
        ("move-up-select", "shift+up"),
        ("move-down-select", "shift+down"),
        ("line-start", "home"),
        ("line-end", "end"),
        ("line-start-select", "shift+home"),
        ("line-end-select", "shift+end"),
        ("word-left", "ctrl+left"),
        ("word-right", "ctrl+right"),
        ("word-left-select", "ctrl+shift+left"),
        ("word-right-select", "ctrl+shift+right"),
        ("scroll-page-up", "pageup"),
        ("scroll-page-down", "pagedown"),
        // Indentation
        ("indent", "tab"),
        ("outdent", "shift+tab"),
        // Keyboard column selection (spec §6.10 "Alt+Shift+arrows"): these are not
        // commands but continuous selection, so S6/S7 read them directly from the event.
        // They are listed here so the help overlay can show them.
        // View toggles
        ("toggle-word-wrap", "alt+z"),
        ("toggle-line-numbers", "alt+shift+n"),
        ("toggle-whitespace", "alt+shift+w"),
        ("toggle-eol-marks", "alt+shift+e"),
        ("fold-all", "alt+0"),
        ("unfold-all", "alt+9"),
        ("toggle-fold", "alt+1"),
        // Tabs
        ("next-tab", "ctrl+tab"),
        ("prev-tab", "ctrl+shift+tab"),
        ("next-tab", "ctrl+pagedown"),
        ("prev-tab", "ctrl+pageup"),
        ("close-other-tabs", "ctrl+alt+w"),
        ("close-all-tabs", "ctrl+shift+w"),
        ("reopen-closed-tab", "ctrl+t"),
        // Pickers and menus
        // Ctrl+Shift+Tab belongs to prev-tab (spec §6.10 lists it there explicitly), so
        // the document list opens from the View menu only. Two actions on one chord would
        // silently cost the user their prev-tab key.
        ("recent-files", "ctrl+e"),
        ("language-picker", "ctrl+shift+l"),
        ("theme-picker", "ctrl+shift+t"),
        ("encoding-menu", "alt+shift+u"),
        ("eol-menu", "alt+shift+d"),
        ("convert-encoding", "alt+shift+c"),
        ("toggle-readonly", "alt+shift+o"),
        ("copy-path", "ctrl+shift+c"),
        ("copy-filename", "ctrl+shift+f12"),
        // Modes
        ("preferences", "ctrl+,"),
        ("command-palette", "ctrl+shift+p"),
        ("help-overlay", "shift+f1"),
        ("about", "f1"),
        ("toggle-overtype", "insert"),
        ("scroll-page-up", "ctrl+home"),
        ("scroll-page-down", "ctrl+end"),
        // Menu bar activation (spec §6.10 "F10 or Alt").
        ("menu-file", "f10"),
    ]
}

impl Keymap {
    /// The §6.10 defaults plus the generated `Alt+<letter>` menu accelerators.
    ///
    /// The accelerators are registered here rather than listed in `default_bindings()`
    /// because they are derived from `MenuId::accelerator()`, so adding a menu cannot
    /// forget one. They come last: a listed binding and a generated accelerator that
    /// collide resolve in favor of the accelerator, which is the behavior spec §6.10 asks
    /// for ("Alt+F/E/S/V/... direct").
    pub fn with_defaults() -> Self {
        let mut km = Keymap::empty();
        for (kebab, key) in default_bindings() {
            km.bind_default(kebab, key);
        }
        for (chord, action) in menu_accelerator_bindings() {
            km.bind(chord, action);
        }
        km
    }

    /// An empty keymap, used by tests and by `--keymap-none` style debugging.
    pub fn empty() -> Self {
        Keymap {
            bindings: HashMap::new(),
            by_action: HashMap::new(),
        }
    }

    /// Defaults plus `[keymap]` overrides from config (FR-CG4).
    ///
    /// An action named in the config has its defaults for that action **replaced**, not
    /// merged, so `save = ["ctrl+f12"]` really does move Save off `Ctrl+S`. Keys freed that
    /// way become unbound.
    pub fn from_config_overrides(overrides: &HashMap<String, Vec<String>>) -> Self {
        let mut km = Keymap::with_defaults();
        for kebab in overrides.keys() {
            km.clear_action(kebab);
        }
        for (kebab, keys) in overrides {
            let Some(action) = action_from_kebab(kebab) else {
                log::warn!("[keymap] unknown action \"{kebab}\" is ignored");
                continue;
            };
            for key in keys {
                match parse_key(key) {
                    Some(chord) => km.bind(chord, action),
                    None => {
                        log::warn!("[keymap] cannot parse key \"{key}\" for {kebab}; skipped")
                    }
                }
            }
        }
        km
    }

    fn bind_default(&mut self, kebab: &str, key: &str) {
        let Some(action) = action_from_kebab(kebab) else {
            // A typo in the default table is a programming error: surface it loudly in
            // tests via the resolve assertions rather than panicking in a user session.
            log::error!("[keymap] default table names unknown action \"{kebab}\"");
            return;
        };
        let Some(chord) = parse_key(key) else {
            log::error!("[keymap] default table has unparseable key \"{key}\"");
            return;
        };
        self.bind(chord, action);
    }

    /// Bind one chord. A previous binding for the same chord is replaced with a warning.
    pub fn bind(&mut self, chord: Chord, action: Action) {
        let name = canonical(chord);
        if let Some(prev) = self.bindings.get(&name) {
            if *prev != action {
                log::warn!(
                    "[keymap] {name} bound to both {} and {}; last one wins",
                    prev.kebab(),
                    action.kebab()
                );
                // The chord is moving to a different action, so it must leave the old one's
                // list. Without this, `keys_for(old)` still reports a chord that no longer
                // resolves to it -- which S14 renders as a menu hint for a key that does
                // something else.
                if let Some(list) = self.by_action.get_mut(prev) {
                    list.retain(|c| c != &name);
                    if list.is_empty() {
                        self.by_action.remove(prev);
                    }
                }
            }
        }
        // Re-binding the same chord to the same action must not duplicate the entry, or
        // `keys_for` grows one copy per config reload.
        let list = self.by_action.entry(action).or_default();
        if !list.contains(&name) {
            list.push(name.clone());
        }
        self.bindings.insert(name, action);
    }

    /// Remove every chord currently resolving to the action with this kebab name.
    fn clear_action(&mut self, kebab: &str) {
        let Some(action) = action_from_kebab(kebab) else {
            return;
        };
        if let Some(chords) = self.by_action.remove(&action) {
            for c in chords {
                self.bindings.remove(&c);
            }
        }
    }

    /// Resolve a key event to an action. `None` for anything that is not a press, or for an
    /// unbound key.
    pub fn resolve(&self, ev: KeyEvent) -> Option<Action> {
        if ev.kind != KeyEventKind::Press {
            return None;
        }
        // crossterm reports Shift differently for characters versus control chords: a
        // shifted letter arrives as KeyCode::Char('S') *and* SHIFT, while "shift+f3"
        // arrives as F(3) with SHIFT. Normalize the letter case so both spellings find the
        // same binding.
        let code = match ev.code {
            KeyCode::Char(c) => KeyCode::Char(c.to_lowercase().next().unwrap_or(c)),
            other => other,
        };
        self.bindings.get(&canonical((code, ev.modifiers))).copied()
    }

    /// The chord bound to an action, if any. Used by the help overlay (FR-U9).
    pub fn key_for(&self, action: Action) -> Option<KeyEvent> {
        self.keys_for(action)
            .first()
            .and_then(|name| parse_key(name))
            .map(|(code, mods)| KeyEvent::new(code, mods))
    }

    /// Every chord bound to an action, in definition order.
    ///
    /// Filtered through `resolve`: a chord can be in an action's list and still not reach it,
    /// because a later `bind` took the key away and left a stale entry behind. Reporting a
    /// chord that does not do what it says is worse than reporting none, and S14 renders this
    /// list as the hint beside a menu item.
    pub fn keys_for(&self, action: Action) -> Vec<String> {
        self.by_action
            .get(&action)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|name| {
                parse_key(name)
                    .map(|(code, mods)| {
                        let mut ev = KeyEvent::new(code, mods);
                        ev.kind = KeyEventKind::Press;
                        self.resolve(ev) == Some(action)
                    })
                    .unwrap_or(false)
            })
            .collect()
    }

    /// `(canonical key, action)` sorted by key, for the help overlay.
    /// `(canonical key, action)` sorted by key name, for the help overlay (FR-U9).
    ///
    /// Sorted by the key string only: `Action` is `Eq + Hash` but not `Ord`, and deriving
    /// `Ord` on it would mean committing to a variant ordering that has no meaning outside
    /// this one display.
    pub fn table(&self) -> Vec<(String, Action)> {
        let mut rows: Vec<(String, Action)> =
            self.bindings.iter().map(|(k, a)| (k.clone(), *a)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// The canonical name of a chord, as produced by `format_key`.
fn canonical(chord: Chord) -> String {
    format_key(chord.0, chord.1)
}

/// Look up an action by its kebab name. Used for `[keymap]` keys and the default table.
pub fn action_from_kebab(kebab: &str) -> Option<Action> {
    Action::ALL.iter().copied().find(|a| a.kebab() == kebab)
}

/// The `Alt+<letter>` accelerators for the nine menus (spec §6.10 "Alt+F/E/S/V/...").
///
/// Built separately from `default_bindings()` because these are generated from
/// `MenuId::accelerator()` rather than listed, so adding a menu cannot forget its
/// accelerator.
pub fn menu_accelerator_bindings() -> Vec<(Chord, Action)> {
    MenuId::ALL
        .iter()
        .map(|m| {
            (
                (
                    KeyCode::Char(m.accelerator().to_lowercase().next().unwrap_or('x')),
                    KeyModifiers::ALT,
                ),
                Action::MenuBar(*m),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    // ------------------------------------------------------------- parse_key

    #[test]
    fn parses_the_shapes_the_spec_lists() {
        let cases: Vec<(&str, Chord)> = vec![
            (
                "ctrl+shift+s",
                (
                    KeyCode::Char('s'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ),
            ("f3", (KeyCode::F(3), KeyModifiers::NONE)),
            ("shift+f1", (KeyCode::F(1), KeyModifiers::SHIFT)),
            ("alt+z", (KeyCode::Char('z'), KeyModifiers::ALT)),
            ("ctrl+pgdn", (KeyCode::PageDown, KeyModifiers::CONTROL)),
            ("ctrl+pgup", (KeyCode::PageUp, KeyModifiers::CONTROL)),
            ("home", (KeyCode::Home, KeyModifiers::NONE)),
            ("end", (KeyCode::End, KeyModifiers::NONE)),
            ("enter", (KeyCode::Enter, KeyModifiers::NONE)),
            ("tab", (KeyCode::Tab, KeyModifiers::NONE)),
            ("esc", (KeyCode::Esc, KeyModifiers::NONE)),
            ("insert", (KeyCode::Insert, KeyModifiers::NONE)),
            ("del", (KeyCode::Delete, KeyModifiers::NONE)),
            ("space", (KeyCode::Char(' '), KeyModifiers::NONE)),
            ("ctrl+↑", (KeyCode::Up, KeyModifiers::CONTROL)),
            ("ctrl+up", (KeyCode::Up, KeyModifiers::CONTROL)),
            ("ctrl+↓", (KeyCode::Down, KeyModifiers::CONTROL)),
            (
                "alt+shift+←",
                (KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(parse_key(input), Some(want), "parse_key({input:?})");
        }
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(parse_key("Ctrl+Shift+S"), parse_key("ctrl+shift+s"));
        assert_eq!(parse_key("F3"), parse_key("f3"));
        assert_eq!(parse_key("  ctrl+s  "), parse_key("ctrl+s"));
    }

    #[test]
    fn modifier_order_is_not_significant() {
        let a = parse_key("ctrl+alt+shift+x").unwrap();
        let b = parse_key("shift+ctrl+alt+x").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn malformed_strings_return_none_and_never_panic() {
        for bad in [
            "",
            " ",
            "ctrl+",
            "+",
            "++",
            "ctrl++x",
            "a+b",
            "ctrl+alt",
            "qqq",
            "ctrl+shift",
            "f99",
            "ctrl+shift+",
            "up+down",
            "ctrl+f13",
            "  +  ",
            "ctrl+  ",
            "ctrl+++",
        ] {
            let r = std::panic::catch_unwind(|| parse_key(bad));
            let outcome = match r {
                Ok(v) => v,
                Err(_) => panic!("parse_key({bad:?}) panicked"),
            };
            assert!(
                outcome.is_none(),
                "parse_key({bad:?}) should be None, got {outcome:?}"
            );
        }
        // Single printable keys are valid, and are the only way to name an unmodified
        // punctuation key: "plus" and "-" both parse, bare "+" does not (see parse_key).
        assert_eq!(
            parse_key("plus"),
            Some((KeyCode::Char('+'), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key("-"),
            Some((KeyCode::Char('-'), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key("f"),
            Some((KeyCode::Char('f'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn literal_plus_is_accepted_after_a_modifier() {
        assert_eq!(
            parse_key("ctrl++"),
            Some((KeyCode::Char('+'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn named_punctuation_and_synonyms_agree() {
        assert_eq!(parse_key("delete"), parse_key("del"));
        assert_eq!(parse_key("pageup"), parse_key("pgup"));
        assert_eq!(parse_key("escape"), parse_key("esc"));
        assert_eq!(parse_key("return"), parse_key("enter"));
        assert_eq!(parse_key("spacebar"), parse_key("space"));
        assert_eq!(
            parse_key("plus"),
            parse_key("ctrl++").map(|(c, _)| (c, KeyModifiers::NONE))
        );
    }

    // ----------------------------------------------------------- format_key

    #[test]
    fn formats_modifiers_in_canonical_order_lowercase() {
        let mods =
            KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        assert_eq!(
            format_key(KeyCode::Char('x'), mods),
            "ctrl+alt+shift+super+x"
        );
    }

    #[test]
    fn round_trips_over_every_default_binding() {
        let mut checked = 0usize;
        for (kebab, key) in default_bindings() {
            let chord = parse_key(key)
                .unwrap_or_else(|| panic!("default binding {kebab} = {key:?} does not parse"));
            let canon = format_key(chord.0, chord.1);
            assert_eq!(
                parse_key(&canon),
                Some(chord),
                "{kebab}: {key:?} -> {canon:?} does not round-trip"
            );
            assert_eq!(
                canon,
                key.to_lowercase(),
                "{kebab}: default key {key:?} is not in canonical form (got {canon:?})"
            );
            checked += 1;
        }
        assert!(
            checked >= 60,
            "expected the full §6.10 table, got {checked}"
        );
    }

    // --------------------------------------------------------- default table

    #[test]
    fn every_default_binding_names_a_real_action() {
        for (kebab, key) in default_bindings() {
            assert!(
                action_from_kebab(kebab).is_some(),
                "default table references unknown action {kebab:?}"
            );
            assert!(parse_key(key).is_some(), "unparseable default key {key:?}");
        }
    }

    #[test]
    fn the_spec_rows_resolve_to_the_expected_actions() {
        let km = Keymap::with_defaults();
        let cases: Vec<(&str, Action)> = vec![
            ("ctrl+n", Action::NewFile),
            ("ctrl+o", Action::OpenFile),
            ("ctrl+s", Action::Save),
            ("ctrl+shift+s", Action::SaveAs),
            ("ctrl+w", Action::CloseTab),
            ("ctrl+z", Action::Undo),
            ("ctrl+y", Action::Redo),
            ("ctrl+shift+z", Action::Redo),
            ("ctrl+x", Action::Cut),
            ("ctrl+c", Action::Copy),
            ("ctrl+v", Action::Paste),
            ("ctrl+a", Action::SelectAll),
            ("ctrl+f", Action::Find),
            ("f3", Action::FindNext),
            ("shift+f3", Action::FindPrev),
            ("ctrl+h", Action::Replace),
            ("ctrl+f3", Action::FindWordNext),
            ("ctrl+shift+f3", Action::FindWordPrev),
            ("ctrl+shift+f", Action::FindInFiles),
            ("ctrl+g", Action::GoToLine),
            ("ctrl+f2", Action::ToggleBookmark),
            ("f2", Action::NextBookmark),
            ("shift+f2", Action::PrevBookmark),
            ("ctrl+d", Action::DuplicateLine),
            ("ctrl+l", Action::DeleteLine),
            ("ctrl+shift+up", Action::MoveLineUp),
            ("ctrl+shift+down", Action::MoveLineDown),
            ("ctrl+shift+u", Action::UpperCase),
            ("ctrl+u", Action::LowerCase),
            ("tab", Action::Indent),
            ("shift+tab", Action::Outdent),
            ("ctrl+pgdn", Action::NextTab),
            ("ctrl+pgup", Action::PrevTab),
            ("alt+z", Action::ToggleWordWrap),
            ("f5", Action::InsertDateTime),
            ("insert", Action::ToggleOvertype),
            ("f10", Action::MenuBar(MenuId::File)),
            ("ctrl+shift+p", Action::CommandPalette),
            ("shift+f1", Action::HelpOverlay),
            ("f1", Action::About),
        ];
        for (key, want) in cases {
            let (code, mods) = parse_key(key).expect("parses");
            let got = km.resolve(press(code, mods));
            assert_eq!(got, Some(want), "{key} should resolve to {}", want.kebab());
        }
    }

    #[test]
    fn two_bindings_for_one_action_both_resolve() {
        let km = Keymap::with_defaults();
        // The default table gives Redo both Ctrl+Y and Ctrl+Shift+Z.
        for key in ["ctrl+y", "ctrl+shift+z"] {
            let (code, mods) = parse_key(key).unwrap();
            assert_eq!(km.resolve(press(code, mods)), Some(Action::Redo), "{key}");
        }
        assert_eq!(km.keys_for(Action::Redo).len(), 2);
        assert_eq!(
            km.keys_for(Action::NextTab).len(),
            2,
            "Ctrl+Tab and Ctrl+PgDn"
        );
    }

    #[test]
    fn no_key_is_bound_to_two_different_actions() {
        let km = Keymap::with_defaults();
        // Walk every chord and confirm the table is internally consistent: the number of
        // distinct chords must equal the number of bindings, which is only true if no
        // chord was silently overwritten with a different action.
        let mut chords: Vec<String> = Vec::new();
        for (kebab, key) in default_bindings() {
            let (code, mods) = parse_key(key).unwrap();
            let canon = format_key(code, mods);
            let resolved = km.resolve(press(code, mods)).expect("resolves");
            assert_eq!(
                resolved.kebab(),
                kebab,
                "{canon} was defined for {kebab} but resolves to {}",
                resolved.kebab()
            );
            chords.push(canon);
        }
        let unique: std::collections::HashSet<&String> = chords.iter().collect();
        assert_eq!(
            unique.len(),
            chords.len(),
            "the default table binds the same chord twice"
        );
    }

    // -------------------------------------------------------------- overrides

    #[test]
    fn config_override_replaces_the_default_for_that_action() {
        let mut overrides = HashMap::new();
        overrides.insert("save".to_string(), vec!["ctrl+f12".to_string()]);
        let km = Keymap::from_config_overrides(&overrides);

        let (code, mods) = parse_key("ctrl+f12").unwrap();
        assert_eq!(km.resolve(press(code, mods)), Some(Action::Save));
        // The old binding is gone, not shadowed.
        let (code, mods) = parse_key("ctrl+s").unwrap();
        assert_eq!(km.resolve(press(code, mods)), None);
    }

    #[test]
    fn override_can_give_one_action_several_keys() {
        let mut overrides = HashMap::new();
        overrides.insert("find".to_string(), vec!["ctrl+f".into(), "f6".into()]);
        let km = Keymap::from_config_overrides(&overrides);
        for key in ["ctrl+f", "f6"] {
            let (code, mods) = parse_key(key).unwrap();
            assert_eq!(km.resolve(press(code, mods)), Some(Action::Find), "{key}");
        }
    }

    #[test]
    fn conflicting_bindings_take_the_last_and_warn() {
        // Two actions claiming the same chord: last definition wins (FR-CG4).
        let mut km = Keymap::empty();
        km.bind(parse_key("ctrl+g").unwrap(), Action::GoToLine);
        km.bind(parse_key("ctrl+g").unwrap(), Action::Find);
        let (code, mods) = parse_key("ctrl+g").unwrap();
        assert_eq!(
            km.resolve(press(code, mods)),
            Some(Action::Find),
            "last binding must win"
        );
    }

    #[test]
    fn unparseable_override_is_skipped_without_touching_the_defaults() {
        let mut overrides = HashMap::new();
        overrides.insert("save".to_string(), vec!["totally-bogus-key".into()]);
        let km = Keymap::from_config_overrides(&overrides);
        // Save's defaults were cleared and the replacement failed to parse, so Save ends up
        // unbound rather than half-rebound -- and nothing else broke.
        assert!(km.keys_for(Action::Save).is_empty());
        let (code, mods) = parse_key("ctrl+z").unwrap();
        assert_eq!(km.resolve(press(code, mods)), Some(Action::Undo));
    }

    #[test]
    fn unknown_action_name_in_config_is_ignored() {
        let mut overrides = HashMap::new();
        overrides.insert("no-such-action".to_string(), vec!["ctrl+1".into()]);
        let km = Keymap::from_config_overrides(&overrides);
        assert!(km
            .resolve(press(KeyCode::Char('1'), KeyModifiers::CONTROL))
            .is_none());
    }

    // ---------------------------------------------------------- press filter

    #[test]
    fn only_press_events_resolve() {
        let km = Keymap::with_defaults();
        let (code, mods) = parse_key("ctrl+s").unwrap();
        assert_eq!(km.resolve(press(code, mods)), Some(Action::Save));
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            let mut ev = press(code, mods);
            ev.kind = kind;
            assert_eq!(km.resolve(ev), None, "{kind:?} must not resolve");
        }
    }

    #[test]
    fn shifted_letters_match_regardless_of_char_case() {
        // crossterm may hand us 'S' with SHIFT, or 's' with SHIFT, depending on terminal.
        let km = Keymap::with_defaults();
        let upper = press(
            KeyCode::Char('S'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        let lower = press(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(km.resolve(upper), Some(Action::SaveAs));
        assert_eq!(km.resolve(lower), Some(Action::SaveAs));
    }

    // -------------------------------------------------------------- reverse

    #[test]
    fn key_for_returns_a_chord_that_resolves_back_to_the_action() {
        let km = Keymap::with_defaults();
        for &a in Action::ALL {
            let bound = km.keys_for(a);
            if bound.is_empty() {
                continue; // not every action has a default key
            }
            let ev = km
                .key_for(a)
                .unwrap_or_else(|| panic!("{} has no reverse lookup", a.kebab()));
            assert_eq!(
                km.resolve(ev),
                Some(a),
                "{}: key_for gave {} which resolves elsewhere",
                a.kebab(),
                format_key(ev.code, ev.modifiers)
            );
        }
    }

    #[test]
    fn menu_accelerators_resolve_to_their_menus() {
        let mut km = Keymap::with_defaults();
        for (chord, action) in menu_accelerator_bindings() {
            km.bind(chord, action);
        }
        for m in MenuId::ALL {
            let letter = m.accelerator().to_lowercase().next().unwrap();
            let ev = press(KeyCode::Char(letter), KeyModifiers::ALT);
            assert_eq!(
                km.resolve(ev),
                Some(Action::MenuBar(m)),
                "Alt+{} should open {:?}",
                letter,
                m
            );
        }
    }

    #[test]
    fn table_is_sorted_and_complete() {
        let km = Keymap::with_defaults();
        let rows = km.table();
        assert_eq!(rows.len(), km.len());
        let mut sorted = rows.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows, sorted);
    }
}
