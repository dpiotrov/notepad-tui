//! The Preferences dialog (FR-CG5): edit `Config`, persist immediately, apply live.
//!
//! # Three rules that shape the design
//!
//! **Every keystroke persists.** FR-CG5 says "persists immediately", which is stronger than
//! "has an OK button": there is no OK button here, and no Cancel. A change is a change the
//! moment it is made. That is a deliberate reading of the requirement, and it is recorded in
//! `DEVIATIONS.md` because an editor that silently discards preferences on `Esc` is the
//! common alternative and someone will expect it.
//!
//! **A change applies to the running editor, not only after restart.** So each write goes
//! through [`apply`], which updates `app.config`, pushes the value into the live engine and
//! geometry, and *then* writes the file. Writing first and applying second would mean a
//! failed write left the session half-configured in the other direction.
//!
//! **One row per setting, described by data.** The row list below is the dialog. Adding a
//! config field means adding one `Row`, and the widgets, focus order, click targets, and
//! persistence all follow from it. That is the same no-drift argument the menu registry
//! makes: a hand-written widget per setting is a setting that can forget to save.
//!
//! # Value widgets
//!
//! `Bool` renders a checkbox, `Enum` a cycling option picker, `Num` an inline text field.
//! A number gets a field rather than a stepper because a tab width of 42 is legitimate and
//! clicking forty times is not how anyone wants to enter it.

use np_core::Config;

use crate::app::App;
use crate::ui::framework::FocusItem;
use crate::ui::layout::Frame;

/// What a row edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// A boolean checkbox.
    Bool,
    /// One of a fixed set of strings; Space/Enter/Left/Right cycles.
    Enum(&'static [&'static str]),
    /// A whole number, edited as text.
    Num,
}

/// One editable setting.
#[derive(Debug, Clone)]
pub struct Row {
    /// Label shown left of the widget.
    pub label: &'static str,
    pub field: Field,
    /// Where the value lives. Kept as a path rather than a closure so a row is `Clone` and
    /// comparable, and so reading and writing cannot drift apart into two lambdas.
    pub key: Key,
}

/// Every config field the dialog exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    // [editor]
    TabWidth,
    InsertSpaces,
    AutoIndent,
    SmartBackspace,
    AutoPair,
    DefaultEol,
    EolMode,
    TrimTrailingWsOnSave,
    EnsureFinalNewline,
    BackupOnSave,
    AutosaveSeconds,
    // [ui]
    Theme,
    LineNumbers,
    WordWrap,
    ShowWhitespace,
    ShowEol,
    RightMarginCol,
    MouseScrollLines,
    MaxRecentFiles,
    Clipboard,
    CurrentLineHighlight,
    // [find]
    CaseSensitive,
    WholeWord,
    Regex,
    WrapAround,
    MarkMatches,
}

/// The dialog's rows, in display order, grouped by section.
///
/// Sections are separated by a header row that is not itself focusable, which is why
/// [`rows`] and [`layout_rows`] differ: the latter includes headers so the renderer and the
/// focus indices agree on row numbers.
pub fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "Tab width",
            field: Field::Num,
            key: Key::TabWidth,
        },
        Row {
            label: "Insert spaces",
            field: Field::Bool,
            key: Key::InsertSpaces,
        },
        Row {
            label: "Auto indent",
            field: Field::Bool,
            key: Key::AutoIndent,
        },
        Row {
            label: "Smart backspace",
            field: Field::Bool,
            key: Key::SmartBackspace,
        },
        Row {
            label: "Auto pair brackets",
            field: Field::Bool,
            key: Key::AutoPair,
        },
        Row {
            label: "Default EOL",
            field: Field::Enum(&["lf", "crlf", "cr"]),
            key: Key::DefaultEol,
        },
        Row {
            label: "EOL mode on save",
            field: Field::Enum(&["file", "lf", "crlf", "cr"]),
            key: Key::EolMode,
        },
        Row {
            label: "Trim trailing WS on save",
            field: Field::Bool,
            key: Key::TrimTrailingWsOnSave,
        },
        Row {
            label: "Ensure final newline",
            field: Field::Bool,
            key: Key::EnsureFinalNewline,
        },
        Row {
            label: "Backup on save",
            field: Field::Bool,
            key: Key::BackupOnSave,
        },
        Row {
            label: "Autosave seconds",
            field: Field::Num,
            key: Key::AutosaveSeconds,
        },
        Row {
            label: "Theme",
            field: Field::Enum(&[]),
            key: Key::Theme,
        },
        Row {
            label: "Line numbers",
            field: Field::Bool,
            key: Key::LineNumbers,
        },
        Row {
            label: "Word wrap",
            field: Field::Bool,
            key: Key::WordWrap,
        },
        Row {
            label: "Show whitespace",
            field: Field::Bool,
            key: Key::ShowWhitespace,
        },
        Row {
            label: "Show EOL marks",
            field: Field::Bool,
            key: Key::ShowEol,
        },
        Row {
            label: "Right margin column",
            field: Field::Num,
            key: Key::RightMarginCol,
        },
        Row {
            label: "Wheel scroll lines",
            field: Field::Num,
            key: Key::MouseScrollLines,
        },
        Row {
            label: "Max recent files",
            field: Field::Num,
            key: Key::MaxRecentFiles,
        },
        Row {
            label: "Clipboard",
            field: Field::Enum(&["auto", "osc52", "none"]),
            key: Key::Clipboard,
        },
        Row {
            label: "Current line highlight",
            field: Field::Bool,
            key: Key::CurrentLineHighlight,
        },
        Row {
            label: "Find: match case",
            field: Field::Bool,
            key: Key::CaseSensitive,
        },
        Row {
            label: "Find: whole word",
            field: Field::Bool,
            key: Key::WholeWord,
        },
        Row {
            label: "Find: regex",
            field: Field::Bool,
            key: Key::Regex,
        },
        Row {
            label: "Find: wrap around",
            field: Field::Bool,
            key: Key::WrapAround,
        },
        Row {
            label: "Find: mark matches",
            field: Field::Bool,
            key: Key::MarkMatches,
        },
    ]
}

/// Read a row's current value from `config`.
pub fn read(config: &Config, key: Key) -> String {
    match key {
        Key::TabWidth => config.editor.tab_width.to_string(),
        Key::InsertSpaces => bool_text(config.editor.insert_spaces),
        Key::AutoIndent => bool_text(config.editor.auto_indent),
        Key::SmartBackspace => bool_text(config.editor.smart_backspace),
        Key::AutoPair => bool_text(config.editor.auto_pair),
        Key::DefaultEol => config.editor.default_eol.clone(),
        Key::EolMode => config.editor.eol_mode.clone(),
        Key::TrimTrailingWsOnSave => bool_text(config.editor.trim_trailing_ws_on_save),
        Key::EnsureFinalNewline => bool_text(config.editor.ensure_final_newline),
        Key::BackupOnSave => bool_text(config.editor.backup_on_save),
        Key::AutosaveSeconds => config.editor.autosave_seconds.to_string(),
        Key::Theme => config.ui.theme.clone(),
        Key::LineNumbers => bool_text(config.ui.line_numbers),
        Key::WordWrap => bool_text(config.ui.word_wrap),
        Key::ShowWhitespace => bool_text(config.ui.show_whitespace),
        Key::ShowEol => bool_text(config.ui.show_eol),
        Key::RightMarginCol => config.ui.right_margin_col.to_string(),
        Key::MouseScrollLines => config.ui.mouse_scroll_lines.to_string(),
        Key::MaxRecentFiles => config.ui.max_recent_files.to_string(),
        Key::Clipboard => config.ui.clipboard.clone(),
        Key::CurrentLineHighlight => bool_text(config.ui.current_line_highlight),
        Key::CaseSensitive => bool_text(config.find.case_sensitive),
        Key::WholeWord => bool_text(config.find.whole_word),
        Key::Regex => bool_text(config.find.regex),
        Key::WrapAround => bool_text(config.find.wrap_around),
        Key::MarkMatches => bool_text(config.find.mark_matches),
    }
}

fn bool_text(on: bool) -> String {
    if on {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

/// Write a value into `config`. Returns `false` for a value the field cannot hold, which the
/// caller surfaces as an inline error rather than a partial write.
pub fn write(config: &mut Config, key: Key, value: &str) -> bool {
    match key {
        Key::TabWidth => match parse_num(value) {
            Some(n) if (1..=16).contains(&n) => {
                config.editor.tab_width = n;
                true
            }
            _ => false,
        },
        Key::InsertSpaces => set_bool(&mut config.editor.insert_spaces, value),
        Key::AutoIndent => set_bool(&mut config.editor.auto_indent, value),
        Key::SmartBackspace => set_bool(&mut config.editor.smart_backspace, value),
        Key::AutoPair => set_bool(&mut config.editor.auto_pair, value),
        Key::DefaultEol if matches!(value, "lf" | "crlf" | "cr") => {
            config.editor.default_eol = value.to_string();
            true
        }
        Key::EolMode if matches!(value, "file" | "lf" | "crlf" | "cr") => {
            config.editor.eol_mode = value.to_string();
            true
        }
        Key::TrimTrailingWsOnSave => set_bool(&mut config.editor.trim_trailing_ws_on_save, value),
        Key::EnsureFinalNewline => set_bool(&mut config.editor.ensure_final_newline, value),
        Key::BackupOnSave => set_bool(&mut config.editor.backup_on_save, value),
        Key::AutosaveSeconds => match parse_num(value) {
            Some(n) => {
                config.editor.autosave_seconds = n as u64;
                true
            }
            _ => false,
        },
        Key::Theme => {
            if value.is_empty() {
                return false;
            }
            config.ui.theme = value.to_string();
            true
        }
        Key::LineNumbers => set_bool(&mut config.ui.line_numbers, value),
        Key::WordWrap => set_bool(&mut config.ui.word_wrap, value),
        Key::ShowWhitespace => set_bool(&mut config.ui.show_whitespace, value),
        Key::ShowEol => set_bool(&mut config.ui.show_eol, value),
        Key::RightMarginCol => match parse_num(value) {
            Some(n) => {
                config.ui.right_margin_col = n;
                true
            }
            _ => false,
        },
        Key::MouseScrollLines => match parse_num(value) {
            Some(n) if n >= 1 => {
                config.ui.mouse_scroll_lines = n;
                true
            }
            _ => false,
        },
        Key::MaxRecentFiles => match parse_num(value) {
            Some(n) => {
                config.ui.max_recent_files = n;
                true
            }
            _ => false,
        },
        Key::Clipboard if matches!(value, "auto" | "osc52" | "none") => {
            config.ui.clipboard = value.to_string();
            true
        }
        Key::CurrentLineHighlight => set_bool(&mut config.ui.current_line_highlight, value),
        Key::CaseSensitive => set_bool(&mut config.find.case_sensitive, value),
        Key::WholeWord => set_bool(&mut config.find.whole_word, value),
        Key::Regex => set_bool(&mut config.find.regex, value),
        Key::WrapAround => set_bool(&mut config.find.wrap_around, value),
        Key::MarkMatches => set_bool(&mut config.find.mark_matches, value),
        _ => false,
    }
}

fn set_bool(slot: &mut bool, value: &str) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => {
            *slot = true;
            true
        }
        "false" | "no" | "off" | "0" => {
            *slot = false;
            true
        }
        _ => false,
    }
}

fn parse_num(value: &str) -> Option<usize> {
    value.trim().parse::<usize>().ok()
}

/// Apply a change live and persist it (FR-CG5).
///
/// Order matters: `config` is updated first so a geometry or theme refresh sees the new
/// value, then the file is written. A write failure is a toast, not a rollback -- the user
/// asked for the behavior and the running editor already honors it; what they lost is only
/// the persistence, which the toast says plainly.
pub fn apply(app: &mut App, key: Key, value: &str) -> bool {
    if !write(&mut app.config, key, value) {
        return false;
    }
    apply_live(app, key);
    app.persist_config();
    app.dirty = true;
    true
}

/// Push a config change into the live session, so it takes effect without a restart.
fn apply_live(app: &mut App, key: Key) {
    match key {
        Key::Theme => {
            if !app.highlight.set_theme(&app.config.ui.theme) {
                app.push_toast(format!("unknown theme '{}'", app.config.ui.theme), true);
            }
        }
        Key::LineNumbers | Key::RightMarginCol => {
            let (w, h) = app.frame_size();
            app.recompute_geometry(w, h);
        }
        Key::WordWrap => {
            // FR-V1: wrap and horizontal scroll are mutually exclusive. `view::toggle` does
            // this for the Alt+Z path; a preference edit is the same state change arriving by
            // a different door, and it has to leave the same state behind.
            if app.config.ui.word_wrap {
                if let Some(doc) = app.doc_mut() {
                    doc.scroll.left_col = 0;
                }
            }
            let (w, h) = app.frame_size();
            app.recompute_geometry(w, h);
        }
        Key::Clipboard => {
            let mode = crate::clipboard::ClipboardMode::parse(&app.config.ui.clipboard);
            let sink = app.osc_out.clone();
            app.clipboard = crate::clipboard::ClipboardProvider::new(mode)
                .with_osc_sink(crate::clipboard::capture_sink(&sink));
        }
        Key::TabWidth => {
            // Read the width before borrowing the document mutably.
            let width = app.config.editor.tab_width.max(1);
            if let Some(doc) = app.doc_mut() {
                doc.tab_width = width;
            }
            let (w, h) = app.frame_size();
            app.recompute_geometry(w, h);
        }
        _ => {}
    }
}

/// The dialog's geometry: a scrolling list, tall enough for the visible rows.
pub fn frame(app: &App) -> Frame {
    let visible = app.editor_height().saturating_sub(6).max(6);
    let h = (visible + 2) as u16;
    Frame::new(app, 48, h)
}

/// The focusable rows, one per setting (headers are skipped).
/// The focusable rows, one per *visible* preference row.
///
/// A preference row is a row of a list: `FocusId::ListRow`, addressed by its absolute index so a
/// click resolves to the right setting however the list is scrolled. It used to be
/// `FocusId::Checkbox`, which is what made Preferences the odd one out -- the mouse layer routes
/// `ListRow` through the shared row-cursor and `Checkbox` through the Find dialog's flag toggle,
/// so a click in this dialog landed on a handler that knew nothing about preference rows and did
/// nothing at all. Rows are rows everywhere; what a row *does* when activated is the dialog's
/// business, and that stays in `prefs_key`.
///
/// The window is derived from the shared row cursor rather than the dialog's own copy of it, so
/// the drawn highlight, the focused item, and what Enter acts on are one value.
pub fn focus_items(app: &App) -> Vec<FocusItem> {
    let frame = frame(app);
    app.list_row_items(frame.layout.inner, rows().len(), app.list_cursor())
}

/// The value the row currently shows, for rendering.
pub fn value_text(app: &App, index: usize) -> String {
    rows()
        .get(index)
        .map(|r| read(&app.config, r.key))
        .unwrap_or_default()
}

/// Cycle an enum row by `delta`, or flip a bool. Returns the new value.
pub fn toggle(app: &mut App, index: usize, delta: i32) -> Option<String> {
    let row = rows().get(index)?.clone();
    let current = read(&app.config, row.key);
    let next = match row.field {
        Field::Bool => {
            if current == "true" {
                "false".to_string()
            } else {
                "true".to_string()
            }
        }
        Field::Enum(options) if !options.is_empty() => {
            let pos = options.iter().position(|o| *o == current).unwrap_or(0) as i32;
            let n = options.len() as i32;
            options[((pos + delta).rem_euclid(n)) as usize].to_string()
        }
        // A theme has no fixed list here: the Theme picker dialog owns that choice, and the
        // preferences row cycles the loaded theme names instead.
        Field::Enum(_) => {
            let names = app.highlight.theme_names();
            if names.is_empty() {
                return None;
            }
            let pos = names.iter().position(|n| *n == current).unwrap_or(0) as i32;
            let n = names.len() as i32;
            names[((pos + delta).rem_euclid(n)) as usize].clone()
        }
        Field::Num => return None,
    };
    if apply(app, row.key, &next) {
        Some(next)
    } else {
        None
    }
}

/// Draw the Preferences dialog (FR-CG5).
pub fn draw(app: &App, buf: &mut ratatui::buffer::Buffer) {
    use crate::ui::framework::dim_frame;
    dim_frame(buf, app);
    let pal = app.palette();
    let frame = frame(app);
    frame.layout.draw_box(buf, "Preferences", &pal);
    let all = rows();
    let visible = frame.layout.inner.h.saturating_sub(1) as usize;
    let start = crate::ui::framework::list_window(all.len(), app.prefs_selected, visible);
    for (slot, (i, row)) in all.iter().enumerate().skip(start).take(visible).enumerate() {
        let line = frame.row(slot as u16);
        let focused = i == app.prefs_selected;
        let value = match row.field {
            Field::Bool => {
                let on = read(&app.config, row.key) == "true";
                format!("[{}] {}", if on { 'x' } else { ' ' }, label_of(row.label))
            }
            Field::Enum(_) | Field::Num => {
                let shown = if focused {
                    app.prefs_edit
                        .as_ref()
                        .map(|(_, f)| f.text().to_string())
                        .unwrap_or_else(|| read(&app.config, row.key))
                } else {
                    read(&app.config, row.key)
                };
                if focused && matches!(row.field, Field::Num) {
                    format!("{shown}_  {}", row.label)
                } else {
                    format!("{shown:<10}{}", row.label)
                }
            }
        };
        // The focused preference row: the theme's highlight pair, not `Modifier::REVERSED`.
        // Preferences was the one dialog with no mouse support at all, and reversal made its
        // selected row a color pair the theme had never chosen.
        let style = if focused {
            crate::style::highlight(&pal).add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            crate::style::dialog(&pal)
        };
        crate::ui::menu_bar::draw_str(
            buf,
            line.x,
            line.y,
            &crate::ui::framework::fit(&format!(" {value}"), line.w as usize),
            style,
        );
    }
    // A footer that says what the keys do, since the dialog has no OK button and the whole
    // point is that there is nothing to confirm.
    let footer = "Space toggle  <-/-> cycle  Enter edit number  Esc close (changes kept)";
    let footer_row = frame.row(frame.layout.inner.h.saturating_sub(1));
    crate::ui::menu_bar::draw_str(
        buf,
        footer_row.x,
        footer_row.y,
        &crate::ui::framework::fit(footer, footer_row.w as usize),
        crate::ui::framework::hint_style(&pal),
    );
}

/// The label with any leading section prefix trimmed, so the value column lines up.
fn label_of(label: &str) -> &str {
    label.rsplit(": ").next().unwrap_or(label)
}
