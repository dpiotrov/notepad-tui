//! Dialogs. Today: the Document List only (FR-U4).
//!
//! # Why this file is small on purpose
//!
//! S14 builds the generic dialog framework -- focus, text fields, button rows, tab order --
//! and migrates everything into it. Writing a throwaway framework here would either be
//! rewritten in one stage or constrain what S14 could build, so this file renders exactly
//! the one dialog S8 needs, over state that already lives on `App`. That keeps the
//! migration a rendering swap rather than a state-machine change.
//!
//! # Geometry is computed, not stored
//!
//! The list rectangle derives from the frame each call, so a resize during the dialog needs
//! no handling: the next frame is simply laid out again. `mouse` and tests ask
//! [`document_list_rect`] rather than measuring drawn cells.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use crate::actions::Action;
use crate::app::App;
use crate::ui::menu_bar::draw_str;

/// One row of the Document List.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentListRow {
    /// Index into `App::documents`.
    pub index: usize,
    /// What the row reads, including markers.
    pub text: String,
    pub modified: bool,
}

/// The rows, in tab order.
///
/// Split from drawing so a test can assert the content without a buffer, and so S14 can
/// feed the same rows into whatever widget replaces this one.
pub fn document_list_rows(app: &App) -> Vec<DocumentListRow> {
    app.documents
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let mut text = doc.display_name();
            if doc.modified {
                text.push_str(" \u{25cf}");
            }
            if doc.readonly {
                text.push_str(" \u{25ae}");
            }
            DocumentListRow {
                index: i,
                text,
                modified: doc.modified,
            }
        })
        .collect()
}

/// The dialog's rectangle: centered, sized to the document count.
///
/// Height is capped at half the screen so a hundred open files cannot cover the editor
/// completely; the visible window then scrolls around the selection, which is why
/// [`document_list_window`] exists separately from the row list.
pub fn document_list_rect(app: &App) -> np_core::RectCore {
    let screen = np_core::RectCore::new(0, 0, app.regions.menu.w, app.regions.status.bottom());
    let count = app.documents.len();
    // Title row + list + a blank hint row.
    let wanted = count.saturating_add(2) as u16;
    let max_h = (screen.h / 2).max(3).min(screen.h);
    let h = wanted.min(max_h).max(1);
    let w = 40u16.min(screen.w.saturating_sub(2).max(1));
    let x = screen.x + screen.w.saturating_sub(w) / 2;
    let y = screen.y + screen.h.saturating_sub(h) / 2;
    np_core::RectCore::new(x, y, w, h)
}

/// The picker rectangle: centered, sized to the filtered candidate count (FR-H2, FR-H3).
///
/// Height is capped at two thirds of the screen so a 75-language list still leaves the editor
/// visible behind it -- picking a language is a decision made while looking at the code.
pub fn picker_rect(app: &App, count: usize) -> np_core::RectCore {
    let screen = np_core::RectCore::new(0, 0, app.regions.menu.w, app.regions.status.bottom());
    // Filter line + list + hint row.
    let wanted = count.saturating_add(2) as u16;
    let max_h = ((screen.h * 2) / 3).max(4).min(screen.h);
    let h = wanted.min(max_h).max(1);
    let w = 44u16.min(screen.w.saturating_sub(2).max(1));
    let x = screen.x + screen.w.saturating_sub(w) / 2;
    let y = screen.y + screen.h.saturating_sub(h) / 2;
    np_core::RectCore::new(x, y, w, h)
}

/// Pad or truncate to exactly `cells`, so a selected row is a clean bar.
fn fit(s: &str, cells: usize) -> String {
    let count = s.chars().count();
    if count >= cells {
        return s.chars().take(cells).collect();
    }
    let mut out = s.to_string();
    for _ in 0..(cells - count) {
        out.push(' ');
    }
    out
}

// ================================================================ Find/Replace
//
// The Find and Replace dialogs (FR-S1). Both draw the same state, `App::find`, and Replace
// adds the Replace field plus the Replace/Replace All buttons -- which is why one render
// function serves both and `replace_mode` is a flag rather than a second widget.
//
// # Keyboard-first, mouse later
//
// Keys are routed here from `App::handle_key`; clicks land in S14's dialog framework. That
// asymmetry is deliberate: FR-S1's acceptance criteria are about fields, options and
// persistence, all of which are keyboard-reachable, and a half-built click target would be a
// second layout to keep in sync for no added coverage.
//
// # Geometry is computed, not stored
//
// As with the Document List, the rectangle derives from the frame every call, so a resize
// needs no handling. Tests ask [`find_rect`] rather than measuring drawn cells.

/// The checkbox rows, in display order, with the config field each one owns.
pub const CHECKBOXES: [(&str, &str); 5] = [
    ("Match case", "case_sensitive"),
    ("Whole word", "whole_word"),
    ("Regular expression", "regex"),
    ("Wrap around", "wrap_around"),
    ("In selection", "in_selection"),
];

/// The button rows, in display order.
pub const BUTTONS: [&str; 5] = ["Find Next", "Find Prev", "Replace", "Replace All", "Count"];

/// The Find/Replace dialog's rectangle.
pub fn find_rect(app: &App) -> np_core::RectCore {
    let screen = np_core::RectCore::new(0, 0, app.regions.menu.w, app.regions.status.bottom());
    // Title, Find field, Replace field, checkboxes, buttons, error line.
    let h = 10u16.min(screen.h.max(1));
    let w = 56u16.min(screen.w.max(1));
    let x = screen.x + screen.w.saturating_sub(w) / 2;
    let y = screen.y + screen.h.saturating_sub(h) / 2;
    np_core::RectCore::new(x, y, w, h)
}

/// The checkbox keys that are currently on. `in_selection` is dialog state rather than an
/// option flag, so it rides along here rather than living in `SearchOpts`.
fn find_flags(app: &App) -> Vec<&'static str> {
    let opts = &app.find.opts;
    let mut out = Vec::new();
    if opts.case_sensitive {
        out.push("case_sensitive");
    }
    if opts.whole_word {
        out.push("whole_word");
    }
    if opts.regex {
        out.push("regex");
    }
    if opts.wrap_around {
        out.push("wrap_around");
    }
    if app.find.in_selection {
        out.push("in_selection");
    }
    out
}

/// Set a checkbox by its key. Keypad-free path for the keyboard toggles and for S14's clicks.
pub fn set_find_flag(app: &mut App, key: &str, on: bool) {
    match key {
        "case_sensitive" => app.find.opts.case_sensitive = on,
        "whole_word" => app.find.opts.whole_word = on,
        "regex" => app.find.opts.regex = on,
        "wrap_around" => app.find.opts.wrap_around = on,
        "in_selection" => app.find.in_selection = on,
        _ => {}
    }
    // An option change can invalidate both the pattern and the marks: `regex` decides whether
    // the needle is a pattern at all.
    app.find.validate_now();
    app.refresh_marks();
    app.dirty = true;
}

/// Run one of the dialog's buttons by index.
pub fn press_find_button(app: &mut App, index: usize) {
    let outcome = match index {
        0 => {
            app.find.forward = true;
            crate::find::find_next(app)
        }
        1 => {
            app.find.forward = false;
            let out = crate::find::find_next(app);
            app.find.forward = true;
            out
        }
        2 => crate::find::replace_current(app),
        3 => crate::find::replace_all(app),
        4 => crate::find::count_matches(app),
        _ => crate::find::FindOutcome::Idle,
    };
    app.run_find(outcome);
}

/// Keys for the Find/Replace dialog.
///
/// Everything is eaten while the dialog is up, including plain characters, which belong to the
/// field rather than to the document. Returning `None` for nothing is not an option here: the
/// caller would insert the character into the buffer.
pub fn handle_find_key(app: &mut App, ev: crossterm::event::KeyEvent) -> Option<Action> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    match ev.code {
        KeyCode::Char(c) if ctrl && (c == 'a' || c == 'A') => {
            // Ctrl+Alt+Enter is the documented Replace All chord; the terminal does not
            // deliver it reliably, so Ctrl+A in the dialog is the portable second binding.
            press_find_button(app, 3);
            Some(Action::Nop)
        }
        KeyCode::Char(c) => {
            match app.find.focus {
                crate::find::FindField::Find => app.find.needle.push(c),
                crate::find::FindField::Replace => app.find.replacement.push(c),
            }
            // Validate on keystroke (FR-S4), and keep the previous matches when it fails.
            app.find.validate_now();
            if app.find.error.is_none() {
                app.refresh_marks();
            }
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Backspace => {
            match app.find.focus {
                crate::find::FindField::Find => {
                    app.find.needle.pop();
                }
                crate::find::FindField::Replace => {
                    app.find.replacement.pop();
                }
            }
            app.find.validate_now();
            if app.find.error.is_none() {
                app.refresh_marks();
            }
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Enter => {
            // Enter runs the default button: Find in the Find dialog, Replace in Replace
            // (FR-S1). Shift+Enter goes the other direction, which is how a person walks
            // backwards without leaving the keyboard.
            if app.find.replace_mode && !shift {
                press_find_button(app, 2);
            } else if shift {
                press_find_button(app, 1);
            } else {
                press_find_button(app, 0);
            }
            Some(Action::Nop)
        }
        KeyCode::Tab => {
            app.find.focus = match app.find.focus {
                crate::find::FindField::Find => crate::find::FindField::Replace,
                crate::find::FindField::Replace => crate::find::FindField::Find,
            };
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Down | KeyCode::Up => {
            // The direction control (FR-S1). Arrow keys are the ↑/↓ the spec's dialog shows.
            app.find.forward = ev.code == KeyCode::Down;
            app.find.opts.forward = app.find.forward;
            app.dirty = true;
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

// ==================================================================== Go To

/// The Go To Line dialog's rectangle (FR-S6).
pub fn goto_rect(app: &App) -> np_core::RectCore {
    let screen = np_core::RectCore::new(0, 0, app.regions.menu.w, app.regions.status.bottom());
    let h = 5u16.min(screen.h.max(1));
    let w = 40u16.min(screen.w.max(1));
    let x = screen.x + screen.w.saturating_sub(w) / 2;
    let y = screen.y + screen.h.saturating_sub(h) / 2;
    np_core::RectCore::new(x, y, w, h)
}

// ================================================================ S14 layouts
//
// The S14 dialog framework asks each dialog for its geometry rather than measuring drawn
// cells, so the click targets and the focus list come from the same source as the pixels.
// These constructors are the whole adapter: they build the `layout` types from `App`, and
// `chrome.rs` plus the renderers consume them. They are free functions rather than methods
// so `dialogs.rs` keeps its existing shape and a reader can see the old and new paths side
// by side during the migration.

use super::layout::{About, Choice, Confirm, FilePicker, FindDialog, GoTo, Picker};

/// The Open / Save As picker's geometry (FR-F2).
pub fn file_picker_layout(app: &App) -> FilePicker {
    FilePicker::new(app)
}

/// Find / Replace geometry (FR-S1).
pub fn find_layout(app: &App) -> FindDialog {
    FindDialog::new(app)
}

/// Go To geometry (FR-S6).
pub fn goto_layout(app: &App) -> GoTo {
    GoTo::new(app)
}

/// About geometry (FR-U9).
pub fn about_layout(app: &App) -> About {
    About::new(app)
}

/// Confirmation geometry (FR-F5, FR-N5).
pub fn confirm_layout(app: &App) -> Confirm {
    Confirm::new(app)
}

/// The Language / Theme picker geometry (FR-H2, FR-H3).
pub fn picker_layout(app: &App) -> Picker {
    let kind = match app.mode() {
        crate::app::Mode::Dialog(k) => k,
        _ => return Picker::sized(app, 0, 44),
    };
    let count = app.picker_candidates(kind).len();
    Picker::sized(app, count, 44)
}

/// The Document List geometry (FR-U4).
pub fn document_list_layout(app: &App) -> Picker {
    Picker::sized(app, app.documents.len(), 40)
}

/// A flat choice list: Encoding, EOL, Recent Files.
pub fn choice_layout(app: &App) -> Choice {
    Choice::new(app, choice_rows(app).len())
}

/// The rows of whichever choice list is open, as `(label, action)` pairs.
///
/// Built here rather than in `menus.rs` because it is the *dialog* form of the same data:
/// a flat list instead of a dropdown, for a choice summoned from the status bar where a
/// full menu bar is not involved.
pub fn choice_rows(app: &App) -> Vec<(String, Action)> {
    let Some(crate::app::Mode::Dialog(kind)) = Some(app.mode()) else {
        return Vec::new();
    };
    match kind {
        crate::app::DialogKind::EncodingPicker => menus::encoding_convert_items(app)
            .into_iter()
            .map(|i| (i.label, i.action.unwrap_or(Action::Nop)))
            .collect(),
        crate::app::DialogKind::EolMenu => menus::eol_items(app)
            .into_iter()
            .map(|i| (i.label, i.action.unwrap_or(Action::Nop)))
            .collect(),
        crate::app::DialogKind::RecentFiles => menus::recent_menu_items(app)
            .into_iter()
            .map(|i| (i.label, i.action.unwrap_or(Action::Nop)))
            .collect(),
        _ => Vec::new(),
    }
}

use crate::ui::menus;

// ============================================================ S14 renderers
//
// The overlay stack calls [`draw_dialog`] for any `DialogKind`, and each dialog's box comes
// from the `layout` types so the renderer and `chrome.rs::dialog_focus_items` agree on every
// rectangle. The pre-S14 renderers above stay in place for the dialogs whose drawing the
// migration has not replaced yet; where a new renderer exists it is the one called.

use crate::app::FilePurpose;
use crate::ui::framework::{
    dim_frame, draw_button, draw_checkbox, draw_field, draw_hint, ButtonState, FocusId,
};

/// Draw whichever dialog is open.
pub fn draw_dialog(app: &App, buf: &mut Buffer, kind: crate::app::DialogKind) {
    match kind {
        crate::app::DialogKind::Open | crate::app::DialogKind::SaveAs => draw_file_picker(app, buf),
        crate::app::DialogKind::Find | crate::app::DialogKind::Replace => draw_find_s14(app, buf),
        crate::app::DialogKind::GoTo => draw_goto_s14(app, buf),
        crate::app::DialogKind::Preferences => crate::ui::prefs::draw(app, buf),
        crate::app::DialogKind::About => crate::ui::help::draw_about(app, buf),
        crate::app::DialogKind::Confirm
        | crate::app::DialogKind::BinaryConfirm
        | crate::app::DialogKind::CloseAllConfirm => draw_confirm(app, buf),
        crate::app::DialogKind::LanguagePicker | crate::app::DialogKind::ThemePicker => {
            draw_picker_s14(app, buf, kind)
        }
        crate::app::DialogKind::DocumentList => draw_document_list_s14(app, buf),
        crate::app::DialogKind::EncodingPicker
        | crate::app::DialogKind::EolMenu
        | crate::app::DialogKind::RecentFiles => draw_choice(app, buf),
    }
}

/// The Open / Save As picker (FR-F2, FR-U5).
fn draw_file_picker(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = file_picker_layout(app);
    layout.frame.layout.draw_box(
        buf,
        if app.file_purpose == FilePurpose::SaveAs {
            "Save As"
        } else {
            "Open"
        },
        &pal,
    );
    let focused = matches!(app.focused_id(), Some(FocusId::Field(0)));
    draw_field(
        buf,
        layout.field_row(),
        None,
        &app.file_input,
        focused,
        app.file_error.is_some(),
        &pal,
    );
    let names: Vec<String> = app
        .file_entries
        .iter()
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.name.trim_end_matches('/'))
            } else {
                e.name.clone()
            }
        })
        .collect();
    crate::ui::framework::draw_list_scrolled(
        buf,
        layout.list_area(),
        &names,
        app.file_selected,
        app.dialog_scroll,
        app.list_focused(),
        &pal,
    );
    let open_label = if app.file_purpose == FilePurpose::SaveAs {
        "Save"
    } else {
        "Open"
    };
    let buttons = [open_label, "Hidden"];
    for (i, label) in buttons.iter().enumerate() {
        let area = layout.button_row(i as u16);
        if area.w == 0 {
            continue;
        }
        let is_focus = app.focused_id() == Some(FocusId::Button(i));
        let state = if is_focus {
            ButtonState::Focused
        } else if i == 0 {
            ButtonState::Default
        } else {
            ButtonState::Normal
        };
        draw_button(buf, area, label, state, &pal);
    }
    if let Some(msg) = &app.file_error {
        draw_hint(
            buf,
            layout.error_row(),
            &format!(
                "! {msg}  (hidden: {})",
                if app.show_hidden { "on" } else { "off" }
            ),
            &pal,
        );
    }
}

/// Find / Replace on the framework (FR-S1).
fn draw_find_s14(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = find_layout(app);
    let title = if app.find.replace_mode {
        "Replace"
    } else {
        "Find"
    };
    layout.frame.layout.draw_box(buf, title, &pal);
    let find_focused = app.focused_id() == Some(FocusId::Field(0));
    let needle = crate::ui::text_input::TextInput::new(app.find.needle.clone());
    draw_field(
        buf,
        layout.find_field(),
        Some("Find:"),
        &needle,
        find_focused,
        app.find.error.is_some(),
        &pal,
    );
    if app.find.replace_mode {
        let rep_focused = app.focused_id() == Some(FocusId::Field(1));
        let replacement = crate::ui::text_input::TextInput::new(app.find.replacement.clone());
        draw_field(
            buf,
            layout.replace_field(),
            Some("Repl:"),
            &replacement,
            rep_focused,
            false,
            &pal,
        );
    }
    let flags = find_flags(app);
    for (i, label) in crate::ui::layout::CHECK_LABELS.iter().enumerate() {
        let area = layout.checkboxes[i];
        if area.w == 0 {
            continue;
        }
        let on = flags.iter().any(|f| *f == checkbox_key(i));
        draw_checkbox(
            buf,
            area,
            label,
            on,
            app.focused_id() == Some(FocusId::Checkbox(i)),
            &pal,
        );
    }
    let default = layout.default_button();
    for (i, label) in crate::ui::layout::BUTTON_LABELS.iter().enumerate() {
        let Some(area) = layout.buttons.get(i).copied() else {
            continue;
        };
        if area.w == 0 {
            continue;
        }
        // Replace-only buttons are dead in Find mode; greying them tells the user why Enter
        // does not do what the highlighted button says.
        let live = app.find.replace_mode || !matches!(i, 2 | 3);
        let state = if app.focused_id() == Some(FocusId::Button(i)) {
            ButtonState::Focused
        } else if !live {
            ButtonState::Disabled
        } else if i == default {
            ButtonState::Default
        } else {
            ButtonState::Normal
        };
        draw_button(buf, area, label, state, &pal);
    }
    let status = match &app.find.error {
        Some(msg) => format!("! {msg}"),
        None if !app.find.needle.is_empty() => format!("{} match(es)", app.find.marks.len()),
        None => String::new(),
    };
    if !status.is_empty() {
        draw_hint(buf, layout.status_row(), &status, &pal);
    }
}

/// The checkbox key for display index `i`, matching `set_find_flag`'s vocabulary.
fn checkbox_key(i: usize) -> &'static str {
    match i {
        0 => "case_sensitive",
        1 => "whole_word",
        2 => "regex",
        3 => "wrap_around",
        _ => "in_selection",
    }
}

/// Go To Line on the framework (FR-S6).
fn draw_goto_s14(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = goto_layout(app);
    layout.frame.layout.draw_box(buf, "Go to Line", &pal);
    draw_hint(buf, layout.frame.row(0), "Line (or line:col)", &pal);
    let input = crate::ui::text_input::TextInput::new(app.goto_input.clone());
    draw_field(
        buf,
        layout.field_row(),
        Some("Line:"),
        &input,
        matches!(app.focused_id(), Some(FocusId::Field(_))),
        app.goto_error.is_some(),
        &pal,
    );
    let focused = matches!(app.focused_id(), Some(FocusId::Button(0)));
    draw_button(
        buf,
        layout.button_row(0),
        "Go",
        if focused {
            ButtonState::Focused
        } else {
            ButtonState::Default
        },
        &pal,
    );
    if let Some(msg) = &app.goto_error {
        draw_hint(buf, layout.error_row(), &format!("! {msg}"), &pal);
    }
}

/// A confirmation dialog (FR-F5, FR-N5).
fn draw_confirm(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = confirm_layout(app);
    layout.frame.layout.draw_box(buf, "Confirm", &pal);
    let prompt = app
        .confirm
        .as_ref()
        .map(|c| c.prompt.clone())
        .unwrap_or_else(|| "Are you sure?".to_string());
    draw_hint(buf, layout.prompt_row(), &prompt, &pal);
    let labels: Vec<&str> = app
        .confirm
        .as_ref()
        .map(|c| c.buttons.iter().map(|b| b.label()).collect())
        .unwrap_or_default();
    for (i, label) in labels.iter().enumerate() {
        let Some(area) = layout.button_rects().get(i).copied() else {
            continue;
        };
        let state = if app.focused_id() == Some(FocusId::Button(i)) {
            ButtonState::Focused
        } else if i == 0 {
            ButtonState::Default
        } else {
            ButtonState::Normal
        };
        draw_button(buf, area, label, state, &pal);
    }
}

/// The Language / Theme pickers on the framework (FR-H2, FR-H3).
fn draw_picker_s14(app: &App, buf: &mut Buffer, kind: crate::app::DialogKind) {
    dim_frame(buf, app);
    let pal = app.palette();
    let (title, query, selected) = match kind {
        crate::app::DialogKind::LanguagePicker => {
            ("Language", app.lang_query.as_str(), app.lang_selected)
        }
        _ => ("Theme", app.theme_query.as_str(), app.theme_selected),
    };
    let candidates = app.picker_candidates(kind);
    let layout = picker_layout(app);
    layout.frame.layout.draw_box(buf, title, &pal);
    let field = crate::ui::text_input::TextInput::new(query.to_string());
    draw_field(
        buf,
        layout.field_row(),
        Some("filter:"),
        &field,
        matches!(app.focused_id(), Some(FocusId::Field(_))),
        false,
        &pal,
    );
    crate::ui::framework::draw_list_scrolled(
        buf,
        layout.list_area(),
        &candidates,
        selected,
        app.dialog_scroll,
        app.list_focused(),
        &pal,
    );
}

/// The Document List on the framework (FR-U4).
fn draw_document_list_s14(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let layout = document_list_layout(app);
    layout.frame.layout.draw_box(buf, "Documents", &pal);
    let rows = document_list_rows(app);
    let names: Vec<String> = rows.iter().map(|r| r.text.clone()).collect();
    crate::ui::framework::draw_list_scrolled(
        buf,
        layout.list_area(),
        &names,
        app.document_list_selected,
        app.dialog_scroll,
        true,
        &pal,
    );
}

/// A flat choice list: Encoding, EOL, Recent Files (FR-N3, FR-N4, FR-F8).
fn draw_choice(app: &App, buf: &mut Buffer) {
    dim_frame(buf, app);
    let pal = app.palette();
    let rows = choice_rows(app);
    let title = match app.mode() {
        crate::app::Mode::Dialog(crate::app::DialogKind::EncodingPicker) => "Encoding",
        crate::app::Mode::Dialog(crate::app::DialogKind::EolMenu) => "Convert EOL",
        _ => "Recent Files",
    };
    let layout = choice_layout(app);
    layout.picker.frame.layout.draw_box(buf, title, &pal);
    let labels: Vec<String> = rows.iter().map(|(label, _)| label.clone()).collect();
    // The EOL list checks the document's current ending, so the user can see what they are
    // changing from.
    let checked: Option<usize> = match app.mode() {
        crate::app::Mode::Dialog(crate::app::DialogKind::EolMenu) => {
            menus::eol_items(app).iter().position(|i| i.checked)
        }
        _ => None,
    };
    let display: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if checked == Some(i) {
                format!("{l}  \u{2713}")
            } else {
                l.clone()
            }
        })
        .collect();
    crate::ui::framework::draw_list_scrolled(
        buf,
        layout.list_area(),
        &display,
        app.dialog_focus,
        app.dialog_scroll,
        true,
        &pal,
    );
}

/// The disk-change banner strip with its two buttons (FR-F7, FR-U10).
///
/// Drawn in the row S6's `mouse::banner_region` already reserves, directly above the editor.
/// The buttons are real click targets and `[Reload]` is the default, matching Notepad++: the
/// usual reason a file changed underneath you is that a build or a formatter did it, and you
/// want the new content.
pub fn draw_banner(app: &App, buf: &mut Buffer) {
    let Some(banner) = &app.banner else {
        return;
    };
    let area = crate::mouse::banner_region(app);
    if area.h == 0 {
        return;
    }
    // The banner is a strip of chrome laid over the top row of the editor, so it paints its own
    // ground. Previously only a foreground was set -- `Color::LightYellow`, one channel of the
    // emulator's palette -- over whatever text was already in that row. On a light terminal the
    // message was light-yellow-on-white, and even on a dark one document glyphs showed through
    // between the letters.
    let pal = app.palette();
    crate::ui::framework::draw_chrome_band(buf, area, &pal);
    let style = crate::style::ink(pal.accent_fg, pal.chrome_bg).add_modifier(Modifier::BOLD);
    let line = fit(&format!(" {} ", banner.message), area.w as usize);
    draw_str(buf, area.x, area.y, &line, style);
    // The two buttons sit at the right end of the row so the message stays readable from the
    // left, which is where the eye starts.
    let labels = ["Reload", "Ignore"];
    let total: usize = labels.iter().map(|l| l.chars().count() + 3).sum();
    let mut x = area.right().saturating_sub(total as u16 + 1);
    for (i, label) in labels.iter().enumerate() {
        let text = if app.banner_button == i {
            format!("<{label}>")
        } else {
            format!("[{label}]")
        };
        let button_style = if app.banner_button == i {
            // The chosen button swaps the theme's own pair rather than `Modifier::REVERSED`, so
            // the swap lands on two colors that are known to be legible against each other.
            crate::style::ink(pal.chrome_bg, pal.accent_fg).add_modifier(Modifier::BOLD)
        } else {
            crate::style::ink(pal.accent_fg, pal.chrome_bg)
        };
        draw_str(buf, x, area.y, &text, button_style);
        x = x.saturating_add(text.chars().count() as u16 + 1);
    }
}
