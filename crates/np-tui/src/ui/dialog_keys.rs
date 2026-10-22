//! Keyboard handling for every dialog, on the S14 framework (FR-U4).
//!
//! # Why this replaced the per-dialog matches
//!
//! S5, S8, S10 and S12 each grew their own key handler -- `handle_find_key`,
//! `handle_goto_key`, `handle_picker_key`, `handle_document_list_key`. Four handlers means
//! four ideas of what `Tab` does (two of them nothing), four ideas of what an unbound key
//! does, and no shared focus concept at all. This module gives every dialog the same
//! skeleton -- `Tab`/`Shift+Tab` cycle focus, `Enter` runs the default child, `Esc` pops one
//! level upstream -- and lets a dialog add specific behavior only where it genuinely differs.
//!
//! # The universal rules
//!
//! Handled here before any dialog-specific branch, so no dialog can accidentally break them:
//! - `Tab` / `Shift+Tab` move focus, wrapping at both ends.
//! - A character with `Ctrl` or `Alt` held is ignored rather than typed, matching the editor
//!   rule that a mistyped chord must not scribble in a field.
//! - Nothing returns `None`: an unhandled key inside a dialog is consumed.
//!
//! # Where the old handlers went
//!
//! The Find dialog's behavior is reproduced here rather than deleted, because its keys are
//! the subject of the S10 test suite and a refactor has to keep those green. Go To keeps
//! `find::go_to` as the commit path. The pickers keep `picker_accept`/`picker_move`. What is
//! gone is the duplicated focus and escape bookkeeping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::actions::Action;
use crate::app::{App, DialogKind, FilePurpose, Mode};
use crate::ui::framework::FocusId;

/// Handle one key for the dialog `kind`. Always returns `Some`.
pub fn dialog_key(app: &mut App, kind: DialogKind, ev: KeyEvent) -> Option<Action> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);

    // --- universal: focus cycling ---
    match ev.code {
        KeyCode::Tab => {
            app.cycle_dialog_focus(1);
            return Some(Action::Nop);
        }
        KeyCode::BackTab => {
            app.cycle_dialog_focus(-1);
            return Some(Action::Nop);
        }
        _ => {}
    }

    // A chord that is not bound must not type. Done before the per-dialog match so the rule
    // holds everywhere.
    if (ctrl || alt) && matches!(ev.code, KeyCode::Char(_)) {
        return Some(Action::Nop);
    }

    match kind {
        DialogKind::Open | DialogKind::SaveAs => file_key(app, ev),
        DialogKind::Find | DialogKind::Replace => find_key(app, ev, shift),
        DialogKind::GoTo => goto_key(app, ev),
        DialogKind::Preferences => prefs_key(app, ev, shift),
        DialogKind::About => simple_close_key(app, ev),
        DialogKind::Confirm | DialogKind::BinaryConfirm | DialogKind::CloseAllConfirm => {
            confirm_key(app, ev)
        }
        DialogKind::LanguagePicker | DialogKind::ThemePicker => picker_key(app, kind, ev),
        DialogKind::DocumentList => document_list_key(app, ev),
        DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
            choice_key(app, ev)
        }
    }
}

// ------------------------------------------------- shared list-cursor movement
//
// Every list dialog used to grow its own arrow-key arm, and they were not the same arm. The
// pickers moved a stored cursor, the choice lists moved `dialog_focus`, the file picker moved
// `file_selected` and also wrapped through the field and buttons, and Preferences grew `j`/`k`
// aliases nobody else had. The user's summary of the result was blunt and accurate: each
// dialog has its own control code, each implemented a different way.
//
// These two helpers are the single implementation. A dialog's key handler now says *what its
// rows mean* (open a file, accept a theme) and delegates *where the cursor goes*.

/// Move the open dialog's row cursor by `delta`, clamped at both ends.
///
/// Clamping rather than wrapping is deliberate and is a reported behaviour change: a list must
/// stop at its first and last row. The file picker used to leave the list at the bottom and
/// walk through the path field and the buttons before coming back, which on a one-entry
/// directory looked like the list spinning and made `../` effectively impossible to stop on.
pub fn move_list_cursor(app: &mut App, delta: i32) -> Option<Action> {
    let count = app.dialog_row_count();
    if count == 0 {
        return Some(Action::Nop);
    }
    let next = crate::ui::framework::clamp_step(app.list_cursor(), delta, count);
    app.set_list_cursor(next);
    // An arrow key is the keyboard taking the selection back. Without this the row a wheel step
    // was holding keeps outranking the cursor in `focused_id`, so the arrow appears to do nothing:
    // the cursor moves, the highlight does not.
    app.wheel_choice = None;
    Some(Action::Nop)
}

/// Move focus to the list, taking the row cursor with it.
///
/// This is the step that was missing from the pickers: focus could reach the listing by mouse
/// and by nothing else, because their handlers only moved the stored cursor and never brought
/// focus along. Landing on a row makes that row the cursor, so the highlight, the focus ring,
/// and what `Enter` acts on are one value.
pub fn focus_the_list(app: &mut App, delta: i32) -> Option<Action> {
    let items = app.dialog_focus_items();
    let Some(idx) = items
        .iter()
        .position(|it| it.id == crate::ui::framework::FocusId::ListRow(app.list_cursor()))
    else {
        // No row is on screen at the current cursor (a filtered list scrolled away, say).
        // Climb to the first row that is.
        let Some(idx) = items
            .iter()
            .position(|it| matches!(it.id, crate::ui::framework::FocusId::ListRow(_)))
        else {
            return Some(Action::Nop);
        };
        app.dialog_focus = idx;
        app.pointer_row = None;
        // The keyboard now owns row movement, so the wheel's held choice is released.
        app.wheel_choice = None;
        if let Some(crate::ui::framework::FocusId::ListRow(row)) = items.get(idx).map(|i| i.id) {
            app.set_list_cursor(row);
        }
        app.dirty = true;
        return Some(Action::Nop);
    };
    app.dialog_focus = idx;
    app.pointer_row = None;
    app.wheel_choice = None;
    app.dirty = true;
    // Then apply the requested step, so `Down` from a field lands on the first row and another
    // `Down` moves on, rather than the first key press being wasted highlighting where the
    // cursor already was.
    if delta != 0 {
        move_list_cursor(app, delta);
    }
    Some(Action::Nop)
}

/// Run dialog button `i`. The index means something different per dialog, which is why each
/// dialog's own key handler decides when to call this.
///
/// Public because the mouse layer needs it too. A click on a button resolved to the right
/// focus child and then only moved focus, which left the save-changes prompt unanswerable by
/// mouse and the application impossible to close.
pub fn press_button(app: &mut App, index: usize) {
    let kind = match app.mode() {
        Mode::Dialog(k) => k,
        _ => return,
    };
    match (kind, index) {
        (DialogKind::Open, 0) => commit_file(app),
        (DialogKind::Open, 1) => toggle_hidden(app),
        (DialogKind::SaveAs, 0) => commit_file(app),
        (DialogKind::SaveAs, 1) => toggle_hidden(app),
        (DialogKind::GoTo, 0) => commit_goto(app),
        (DialogKind::About, 0) => {
            app.pop_mode();
        }
        (DialogKind::Find, i) | (DialogKind::Replace, i) => {
            crate::ui::dialogs::press_find_button(app, i);
        }
        (DialogKind::Confirm, i)
        | (DialogKind::BinaryConfirm, i)
        | (DialogKind::CloseAllConfirm, i) => {
            if let Some(request) = app.confirm.clone() {
                if let Some(button) = request.buttons.get(i) {
                    let button = *button;
                    app.resolve_confirm(button);
                }
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------- file picker

/// Keys for the Open / Save As picker (FR-F2), on the shared routing table.
///
/// The picker is a path field, a listing, and two buttons, so the framework's rules decide
/// where the arrows go and what `Enter` runs. What stays here is the picker's own vocabulary:
/// a character or an editing key belongs to the path field and re-reads the directory, and
/// accepting a row means descending into a directory rather than opening a file.
fn file_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let on_list = app.list_focused();
    use crate::ui::framework::DialogInput as D;
    // The field's editing keys, in one place. Each one changes the path text, and the path
    // text *is* the listing's query, so every one of them is followed by a re-read.
    let field_edit = |app: &mut App, input: D| -> Option<Action> {
        match input {
            D::Char(c) => {
                app.file_input.insert_char(c);
            }
            D::Backspace => {
                app.file_input.backspace();
            }
            D::Delete => {
                app.file_input.delete_forward();
            }
            D::FieldHome => {
                app.file_input.home();
            }
            D::FieldEnd => {
                app.file_input.end();
            }
            _ => return None,
        }
        app.file_error = None;
        app.refresh_file_entries();
        Some(Action::Nop)
    };
    match D::route(&ev, true, true, on_list) {
        D::Activate => {
            // A focused row is the row the user can see highlighted, so Enter runs on it.
            match app.focused_id() {
                Some(FocusId::ListRow(row)) => {
                    app.set_list_cursor(row);
                    accept_entry(app);
                }
                Some(FocusId::List) => accept_entry(app),
                Some(FocusId::Button(i)) => press_button(app, i),
                _ => commit_file(app),
            }
            Some(Action::Nop)
        }
        D::EnterList => focus_the_list(app, 1),
        D::Row(d) => move_list_cursor(app, d),
        D::PageRows(d) => move_list_cursor(app, d * 5),
        D::Step(d) => {
            // Left on a directory row means "go to the parent", the same trip Enter on the
            // `..` row takes. Right has no meaning here and is swallowed.
            if d < 0 && on_list {
                go_up(app);
            }
            Some(Action::Nop)
        }
        // `Left`/`Right` inside the path field: the field owns the caret.
        other => field_edit(app, other).or(Some(Action::Nop)),
    }
}

/// Open the highlighted entry, or descend into it.
fn accept_entry(app: &mut App) {
    let Some(entry) = app.file_entries.get(app.file_selected).cloned() else {
        return;
    };
    if entry.is_dir {
        app.file_input
            .set_text(crate::ui::file_picker::dir_text(&entry.path));
        app.last_dir = Some(entry.path.clone());
        app.set_list_cursor(0);
        app.refresh_file_entries();
        return;
    }
    app.file_input.set_text(entry.path.display().to_string());
    commit_file(app);
}

/// Go to the parent directory (`..`).
fn go_up(app: &mut App) {
    let (dir, _) = crate::ui::file_picker::split_query(app.file_input.text());
    if let Some(parent) = dir.parent() {
        let parent = parent.to_path_buf();
        app.file_input
            .set_text(crate::ui::file_picker::dir_text(&parent));
        app.last_dir = Some(parent);
        app.set_list_cursor(0);
        app.refresh_file_entries();
    }
}

/// Flip the hidden-files toggle and re-read the listing (FR-F2).
fn toggle_hidden(app: &mut App) {
    app.show_hidden = !app.show_hidden;
    app.refresh_file_entries();
}

/// Act on the path field: open, navigate, or save (FR-F2, FR-F4).
fn commit_file(app: &mut App) {
    let for_save = app.file_purpose == FilePurpose::SaveAs;
    let text = app.file_input.text().to_string();
    match crate::ui::file_picker::resolve(&text, for_save) {
        crate::ui::file_picker::Resolved::EnterDir(dir) => {
            app.file_input
                .set_text(crate::ui::file_picker::dir_text(&dir));
            app.last_dir = Some(dir);
            app.set_list_cursor(0);
            app.refresh_file_entries();
        }
        crate::ui::file_picker::Resolved::OpenFile(path) => {
            app.pop_mode();
            app.open_path(&path);
        }
        crate::ui::file_picker::Resolved::CreateAt(path) => {
            app.pop_mode();
            app.save_active_as(&path);
        }
        crate::ui::file_picker::Resolved::Invalid(msg) => {
            app.file_error = Some(msg);
            app.dirty = true;
        }
    }
}

// ------------------------------------------------------------ find/replace

/// Keys for Find and Replace (FR-S1), on the shared focus model.
///
/// Behavior is unchanged from S10: the same buttons, the same Enter/Shift+Enter rule, the
/// same live validation. What changed is that `Tab` now walks the checkboxes and buttons too,
/// not just the two text fields.
fn find_key(app: &mut App, ev: KeyEvent, shift: bool) -> Option<Action> {
    let focused = app.focused_id();
    match ev.code {
        KeyCode::Enter => {
            if let Some(FocusId::Button(i)) = focused {
                crate::ui::dialogs::press_find_button(app, i);
                return Some(Action::Nop);
            }
            // Enter in a field runs the default button. Shift+Enter goes backwards, which is
            // how a person walks matches in reverse without leaving the field.
            if shift {
                crate::ui::dialogs::press_find_button(app, 1);
            } else if app.find.replace_mode {
                crate::ui::dialogs::press_find_button(app, 2);
            } else {
                crate::ui::dialogs::press_find_button(app, 0);
            }
            Some(Action::Nop)
        }
        KeyCode::Char(' ') if matches!(focused, Some(FocusId::Checkbox(_))) => {
            let i = match focused {
                Some(FocusId::Checkbox(i)) => i,
                _ => return Some(Action::Nop),
            };
            toggle_find_flag(app, i);
            Some(Action::Nop)
        }
        KeyCode::Down | KeyCode::Up => {
            // Direction, when a field has focus (FR-S1). On a checkbox or button the arrows
            // move focus instead, which is what a user expects from a form.
            if matches!(focused, Some(FocusId::Field(_))) {
                app.find.forward = ev.code == KeyCode::Down;
                app.find.opts.forward = app.find.forward;
                app.dirty = true;
            } else {
                app.cycle_dialog_focus(if ev.code == KeyCode::Down { 1 } else { -1 });
            }
            Some(Action::Nop)
        }
        KeyCode::Backspace => {
            if let Some(FocusId::Field(i)) = focused {
                if i == 0 {
                    app.find.needle.pop();
                } else {
                    app.find.replacement.pop();
                }
                app.find.validate_now();
                if app.find.error.is_none() {
                    app.refresh_marks();
                }
                app.dirty = true;
            }
            Some(Action::Nop)
        }
        KeyCode::Char(c) => {
            // Only a field takes text. With a checkbox or button focused, a letter is a
            // mistaken keystroke and must vanish rather than land in the needle.
            if let Some(FocusId::Field(i)) = focused {
                if i == 0 {
                    app.find.needle.push(c);
                } else {
                    app.find.replacement.push(c);
                }
                app.find.validate_now();
                if app.find.error.is_none() {
                    app.refresh_marks();
                }
                app.dirty = true;
            }
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// Toggle the open dialog's checkbox `i`, whichever dialog owns it.
///
/// The one checkbox entry point for both input devices. It dispatches on the dialog because the
/// flags live in different places -- Find's five options, Preferences' settings rows -- but the
/// *gesture* is one gesture, and a mouse click must not reach a different toggle than Space does.
/// Before this, the mouse arm called the Find-specific toggle directly, so a click on a
/// Preferences row ran Find's flag table against a row number that meant something else.
pub fn toggle_dialog_checkbox(app: &mut App, i: usize) {
    if matches!(app.mode(), Mode::Dialog(DialogKind::Preferences)) {
        crate::ui::prefs::toggle(app, i, 1);
        return;
    }
    toggle_find_flag(app, i);
}

/// Toggle the find dialog's checkbox `i` (FR-S1). Also driven by a mouse click.
pub fn toggle_find_flag(app: &mut App, i: usize) {
    let key = match i {
        0 => "case_sensitive",
        1 => "whole_word",
        2 => "regex",
        3 => "wrap_around",
        4 => "in_selection",
        _ => return,
    };
    let on = match key {
        "case_sensitive" => !app.find.opts.case_sensitive,
        "whole_word" => !app.find.opts.whole_word,
        "regex" => !app.find.opts.regex,
        "wrap_around" => !app.find.opts.wrap_around,
        _ => !app.find.in_selection,
    };
    crate::ui::dialogs::set_find_flag(app, key, on);
}

// -------------------------------------------------------------------- go to

/// Keys for Go To Line (FR-S6).
fn goto_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let on_field = matches!(app.focused_id(), Some(FocusId::Field(_)));
    match ev.code {
        KeyCode::Enter => {
            if matches!(app.focused_id(), Some(FocusId::Button(_))) {
                press_button(app, 0);
            } else {
                commit_goto(app);
            }
            Some(Action::Nop)
        }
        KeyCode::Char(c) if on_field && (c.is_ascii_digit() || c == ':') => {
            app.goto_input.push(c);
            app.goto_error = None;
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Backspace if on_field => {
            app.goto_input.pop();
            app.goto_error = None;
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Up => {
            app.cycle_dialog_focus(-1);
            Some(Action::Nop)
        }
        KeyCode::Down => {
            app.cycle_dialog_focus(1);
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// Commit the Go To field.
fn commit_goto(app: &mut App) {
    let input = app.goto_input.clone();
    match crate::find::go_to(app, &input) {
        crate::find::FindOutcome::Found { .. } => {
            app.goto_input.clear();
            app.goto_error = None;
            app.pop_mode();
        }
        crate::find::FindOutcome::Invalid(msg) => {
            app.goto_error = Some(msg);
            app.dirty = true;
        }
        other => {
            if let Some(msg) = other.toast() {
                app.push_toast(msg, other.is_error());
            }
            app.pop_mode();
        }
    }
}

// -------------------------------------------------------------- preferences

/// Keys for Preferences (FR-CG5). Space toggles, Enter commits an edit, Left/Right cycle an
/// enum, and typing on a numeric row starts an inline edit.
fn prefs_key(app: &mut App, ev: KeyEvent, shift: bool) -> Option<Action> {
    // The shared row cursor, not the dialog's own copy of it: the mouse writes the shared one, so
    // a keyboard action taken after a click has to act on the row the click selected.
    let index = app.list_cursor();
    let rows = crate::ui::prefs::rows();
    let Some(row) = rows.get(index).cloned() else {
        return Some(Action::Nop);
    };
    // An open inline edit owns every keystroke until Enter or Esc closes it.
    if let Some((_, field)) = app.prefs_edit.clone() {
        match ev.code {
            KeyCode::Enter => {
                let text = field.text().to_string();
                app.prefs_edit = None;
                if !crate::ui::prefs::apply(app, row.key, &text) {
                    app.push_toast(format!("`{text}` is not a valid value"), true);
                }
                return Some(Action::Nop);
            }
            KeyCode::Backspace => {
                let mut f = field;
                f.backspace();
                app.prefs_edit = Some((index, f));
                app.dirty = true;
                return Some(Action::Nop);
            }
            KeyCode::Char(c) => {
                let mut f = field;
                f.insert_char(c);
                app.prefs_edit = Some((index, f));
                app.dirty = true;
                return Some(Action::Nop);
            }
            _ => {
                app.prefs_edit = None;
            }
        }
    }
    // Row movement comes from the shared table, so Preferences arrows mean what every other
    // dialog's arrows mean: clamp at both ends, page by five, Home and End to the ends. The `j`
    // and `k` aliases this dialog used to carry are gone -- one gesture per action, and the
    // letters belong to the setting being edited, not to navigation.
    use crate::ui::framework::DialogInput as D;
    match D::route(&ev, false, true, true) {
        D::Row(d) => return move_list_cursor(app, d),
        D::PageRows(d) => return move_list_cursor(app, d * 5),
        _ => {}
    }
    match ev.code {
        KeyCode::Char(' ') => {
            crate::ui::prefs::toggle(app, index, 1);
            Some(Action::Nop)
        }
        KeyCode::Left => {
            crate::ui::prefs::toggle(app, index, -1);
            Some(Action::Nop)
        }
        KeyCode::Right => {
            crate::ui::prefs::toggle(app, index, 1);
            Some(Action::Nop)
        }
        KeyCode::Enter => {
            match row.field {
                crate::ui::prefs::Field::Num => {
                    let current = crate::ui::prefs::value_text(app, index);
                    app.prefs_edit = Some((index, crate::ui::text_input::TextInput::new(current)));
                    app.dirty = true;
                }
                crate::ui::prefs::Field::Bool => {
                    crate::ui::prefs::toggle(app, index, 1);
                }
                crate::ui::prefs::Field::Enum(_) => {
                    crate::ui::prefs::toggle(app, index, if shift { -1 } else { 1 });
                }
            }
            Some(Action::Nop)
        }
        KeyCode::Char(c) if matches!(row.field, crate::ui::prefs::Field::Num) => {
            // Typing a digit on a numeric row starts the edit with that digit, which is what
            // a user expects from every other form.
            app.prefs_edit = Some((index, crate::ui::text_input::TextInput::new(c.to_string())));
            app.dirty = true;
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

// ------------------------------------------------------- simple / confirm / etc

/// A dialog with one OK button.
fn simple_close_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    match ev.code {
        KeyCode::Enter => {
            press_button(app, 0);
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// A confirmation: Enter takes the *focused* button, digits pick by position.
///
/// Enter used to resolve `buttons.first()` unconditionally. The arrows and `Tab` moved focus
/// and the highlight moved with them, so the dialog looked like a normal radio row and
/// behaved like nothing was happening: pressing Enter on `[Cancel]` answered `[Save]`. On
/// a close prompt that meant an unwanted write to disk, which is the opposite of what the
/// user selected.
///
/// Falling back to the default button only applies when focus is not on a button at all --
/// a freshly opened prompt, or one whose focus landed on the prompt text.
fn confirm_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    match ev.code {
        KeyCode::Enter => {
            let focused = match app.focused_id() {
                Some(FocusId::Button(i)) => Some(i),
                _ => None,
            };
            if let Some(request) = app.confirm.clone() {
                let index = focused
                    .unwrap_or(0)
                    .min(request.buttons.len().saturating_sub(1));
                if let Some(button) = request.buttons.get(index) {
                    let button = *button;
                    app.resolve_confirm(button);
                }
            }
            Some(Action::Nop)
        }
        // Up and Down walk the same row as Left and Right. Three buttons on one line make
        // the horizontal pair the obvious chords, but a user with a vertical habit should
        // not get nothing.
        KeyCode::Left | KeyCode::Up => {
            app.cycle_dialog_focus(-1);
            Some(Action::Nop)
        }
        KeyCode::Right | KeyCode::Down => {
            app.cycle_dialog_focus(1);
            Some(Action::Nop)
        }
        KeyCode::Char(c @ ('1'..='9')) => {
            if let Some(request) = app.confirm.clone() {
                let i = (c as u8 - b'1') as usize;
                if let Some(button) = request.buttons.get(i) {
                    let button = *button;
                    app.resolve_confirm(button);
                }
            }
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// The Language / Theme pickers (FR-H2, FR-H3). Typing filters; Enter accepts.
fn picker_key(app: &mut App, kind: DialogKind, ev: KeyEvent) -> Option<Action> {
    // The picker is a query field and a filtered list. That is the whole shape, so the shape
    // decides the keys and nothing here is specific to "theme" or "language" beyond which
    // field a character is typed into.
    let on_list = app.list_focused();
    let has_field = true;
    use crate::ui::framework::DialogInput as D;
    match D::route(&ev, has_field, true, on_list) {
        D::EnterList => focus_the_list(app, 1),
        D::Row(d) => move_list_cursor(app, d),
        D::PageRows(d) => move_list_cursor(app, d * 5),
        D::Activate => {
            // A focused row wins over the stored cursor: after `Tab` into a scrolled list the
            // user is looking at that row, not at wherever the cursor happened to rest.
            if let Some(FocusId::ListRow(row)) = app.focused_id() {
                app.set_list_cursor(row);
            }
            app.picker_accept(kind);
            Some(Action::Nop)
        }
        D::Char(c) => {
            // Typing filters the list rather than selecting row 0 outright: the cursor is kept,
            // clamped, so a user who was on "monokai" and types "d" still lands on a match near
            // where they were instead of the alphabetically-first entry.
            match kind {
                DialogKind::LanguagePicker => app.lang_query.push(c),
                _ => app.theme_query.push(c),
            }
            let count = app.picker_candidates(kind).len();
            app.set_list_cursor(app.list_cursor().min(count.saturating_sub(1)));
            Some(Action::Nop)
        }
        D::Backspace => {
            match kind {
                DialogKind::LanguagePicker => {
                    app.lang_query.pop();
                }
                _ => {
                    app.theme_query.pop();
                }
            }
            let count = app.picker_candidates(kind).len();
            app.set_list_cursor(app.list_cursor().min(count.saturating_sub(1)));
            Some(Action::Nop)
        }
        D::Cancel => {
            app.pop_mode();
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// The Document List (FR-U4), on the shared routing table.
///
/// The whole dialog is one list, so there is no field to leave and no boundary to cross: the
/// framework's row rules apply directly and what is left here is only what a row *means* -- switch
/// to that tab.
fn document_list_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    use crate::ui::framework::DialogInput as D;
    match D::route(&ev, false, true, true) {
        D::Activate => {
            // A row that took focus by Tab is the row the user is looking at, so it wins over
            // wherever the cursor last rested.
            if let Some(FocusId::ListRow(row)) = app.focused_id() {
                app.set_list_cursor(row);
            }
            app.document_list_activate();
            Some(Action::Nop)
        }
        D::Row(d) => move_list_cursor(app, d),
        D::PageRows(d) => move_list_cursor(app, d * 5),
        _ => Some(Action::Nop),
    }
}

/// Accept the highlighted row of a choice list, the same path `Enter` takes (FR-N3, FR-N4).
///
/// Returns the row's action so the key path can run it through the dispatcher; the mouse
/// path, which has no dispatcher return value, runs it itself. Splitting it this way keeps
/// the encoding-hint recovery -- the one tricky part of accepting these rows -- in a single
/// place instead of a copy per input device.
pub fn accept_choice(app: &mut App) -> Option<Action> {
    let rows = crate::ui::dialogs::choice_rows(app);
    let Some(action) = rows.get(app.choice_cursor()).map(|(_, a)| *a) else {
        app.pop_mode();
        return Some(Action::Nop);
    };
    // The Encoding rows carry a codec index in the menu item's hint, which
    // `menu_items` filled in; recover it here the same way the dropdown does.
    if matches!(action, Action::ConvertEncoding | Action::ReloadAsEncoding) {
        let items = if action == Action::ConvertEncoding {
            crate::ui::menus::encoding_convert_items(app)
        } else {
            crate::ui::menus::encoding_reload_items(app)
        };
        if let Some(item) = items.get(app.choice_cursor()) {
            if let Ok(i) = item.hint.parse::<usize>() {
                if let Some(enc) = np_core::EncodingId::all().get(i) {
                    app.pending_encoding = Some(enc.clone());
                }
            }
        }
    }
    app.pop_mode();
    Some(action)
}

/// A flat choice list: Encoding, EOL, Recent Files, on the shared routing table.
///
/// These three dialogs have no field and no buttons, so the list always has focus and the only
/// decisions are where the cursor goes and what a row runs. Both come from the framework now;
/// `accept_choice` stays because recovering the codec index from a menu hint is genuinely specific
/// to the Encoding rows.
fn choice_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    use crate::ui::framework::DialogInput as D;
    match D::route(&ev, false, true, true) {
        D::Activate => {
            // A row that took focus by Tab is the cursor: the choice list draws its bar from the
            // shared cursor, so focus, highlight, and what Enter runs are one value.
            if let Some(FocusId::ListRow(row)) = app.focused_id() {
                app.set_list_cursor(row);
            }
            accept_choice(app)
        }
        D::Row(d) => move_list_cursor(app, d),
        D::PageRows(d) => move_list_cursor(app, d * 5),
        _ => Some(Action::Nop),
    }
}
