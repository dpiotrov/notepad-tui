//! The S14 chrome wiring: opening dialogs, routing keys to them, and menu navigation.
//!
//! # Why a separate module rather than more methods on `App`
//!
//! `app.rs` was already the largest file in the tree before S14, and what S14 adds is a
//! large but very uniform body of code: "for this mode, these keys do these things". Keeping
//! it here means `App` keeps its role -- state plus `execute` -- and this module is the
//! input adapter between crossterm events and that state. It is all `impl App` because it
//! needs the same private access, but it is a separate file so a reader looking for the menu
//! system finds one file rather than a section 2000 lines into a 3500-line one.
//!
//! # The routing contract
//!
//! `App::handle_key` asks [`dialog_key`] / [`menu_key`] / [`palette_key`] /
//! [`context_menu_key`] in mode order. Each returns:
//! - `Some(action)` -- handled, and the action (usually `Nop`) is what the caller reports;
//! - `None` -- not handled, fall through.
//!
//! A dialog must return `Some` for every printable character. Returning `None` would let the
//! character reach the editor and type into the document, which is the single worst failure
//! mode of this whole area and the reason every pre-S14 dialog ended its match with a catch-all.
//!
//! # `Esc` never reaches this module
//!
//! `App::handle_escape` runs before any of it, because `Esc` is structural (pop one `Mode`
//! level) and must work even when every binding has been remapped away.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use np_core::RectCore;

use crate::actions::{Action, MenuId, SubmenuId};
use crate::app::{App, DialogKind, FilePurpose, Mode};
use crate::ui::framework::{focus_cycle, FocusId, FocusItem};
use crate::ui::menus::{self, MenuItemKind};

// --------------------------------------------------------------- openers

impl App {
    /// Open the file picker (FR-F2, FR-U5).
    pub fn open_file_picker(&mut self, purpose: FilePurpose) {
        self.file_purpose = purpose;
        let active = self.doc().and_then(|d| d.path.clone());
        let start =
            crate::ui::file_picker::start_dir(None, self.last_dir.as_deref(), active.as_deref());
        self.last_dir = Some(start.clone());
        self.file_input
            .set_text(crate::ui::file_picker::dir_text(&start));
        self.file_selected = 0;
        self.file_error = None;
        self.refresh_file_entries();
        let kind = match purpose {
            FilePurpose::Open => DialogKind::Open,
            FilePurpose::SaveAs => DialogKind::SaveAs,
        };
        self.push_mode(Mode::Dialog(kind));
        self.reset_dialog_focus();
    }

    /// Re-read the listing for whatever the path field currently says.
    ///
    /// Drops any pointer row claim. This is the one place the listing is replaced, and a
    /// claim is a row *number* into the previous contents: after `..` or a descend, row 2 of
    /// the old listing has nothing to do with row 2 of the new one. Leaving it set made the
    /// next interaction resolve against the directory the user had just left -- reported as
    /// "after clicking `../`, clicking anything at the next level acts like `../`, and it
    /// takes some cursor movement to come good".
    pub fn refresh_file_entries(&mut self) {
        let (dir, filter) = crate::ui::file_picker::split_query(self.file_input.text());
        let (entries, err) = crate::ui::file_picker::list_dir(&dir, self.show_hidden);
        self.file_error = err;
        self.file_entries = crate::ui::file_picker::filter_entries(&entries, &filter);
        if self.file_selected >= self.file_entries.len() {
            self.file_selected = self.file_entries.len().saturating_sub(1);
        }
        // These are new rows, so a pointer choice made against the previous listing means
        // nothing here. Reported as "after clicking `../`, the next click acts like `../`".
        // The wheel's held choice is in the same category: it names a row of a listing that no
        // longer exists, so keeping it would resurrect a choice against unrelated entries.
        self.pointer_row = None;
        self.wheel_choice = None;
        self.dirty = true;
    }

    /// Open the command palette (FR-U8).
    pub fn open_palette(&mut self) {
        self.palette_query.clear();
        self.palette_selected = 0;
        self.push_mode(Mode::Palette);
    }

    /// Open the Preferences dialog (FR-CG5).
    pub fn open_preferences(&mut self) {
        self.prefs_selected = 0;
        self.prefs_edit = None;
        self.push_mode(Mode::Dialog(DialogKind::Preferences));
        self.reset_dialog_focus();
    }

    /// Open About (FR-U9).
    pub fn open_about(&mut self) {
        self.push_mode(Mode::Dialog(DialogKind::About));
        self.reset_dialog_focus();
    }

    /// Open the scrollable keymap help (FR-U9).
    pub fn open_help(&mut self) {
        self.help_scroll = 0;
        self.push_mode(Mode::HelpOverlay);
    }

    /// Open the Encoding menu (FR-N3).
    pub fn open_encoding_menu(&mut self) {
        self.push_mode(Mode::Dialog(DialogKind::EncodingPicker));
        self.reset_dialog_focus();
    }

    /// Open the EOL menu (FR-N4).
    pub fn open_eol_menu(&mut self) {
        self.push_mode(Mode::Dialog(DialogKind::EolMenu));
        self.reset_dialog_focus();
    }

    /// Open Recent Files (FR-F8).
    pub fn open_recent_files(&mut self) {
        self.push_mode(Mode::Dialog(DialogKind::RecentFiles));
        self.reset_dialog_focus();
    }
}

impl App {
    /// Reset child focus when a dialog or menu opens.
    ///
    /// One function because the reset has to cover every piece of focus state, and a dialog
    /// that inherits an index from the dialog beneath it starts with focus on an arbitrary
    /// different in the next dialog or the next directory.
    /// Put a freshly opened dialog's focus at its default child.
    ///
    /// The one hook every dialog open goes through, which is why the shared row cursor is
    /// loaded here too: a picker that opened with the previous dialog's row number still in
    /// `dialog_row` would highlight a row nobody chose, and -- worse -- a list shorter than
    /// that number would clamp to its last row and look selected.
    pub fn reset_dialog_focus(&mut self) {
        self.dialog_focus = 0;
        self.pointer_row = None;
        self.wheel_choice = None;
        self.sync_list_cursor_from_kind();
    }
}

// ---------------------------------------------------------- focus helpers

impl App {
    /// The focusable children of the dialog currently on screen, in tab order (FR-U4).
    ///
    /// Rebuilt every call rather than cached. The rectangles come from the same layout
    /// functions the renderer uses, so a click target and a drawn widget cannot disagree,
    /// and a resize needs no invalidation because nothing was stored.
    pub fn dialog_focus_items(&self) -> Vec<FocusItem> {
        let kind = match self.mode() {
            Mode::Dialog(k) => k,
            _ => return Vec::new(),
        };
        match kind {
            DialogKind::Open | DialogKind::SaveAs => {
                let layout = crate::ui::dialogs::file_picker_layout(self);
                // Tab order follows the screen: field, then the listing, then the buttons.
                // The order matters for `Tab` reachability, and the listing sits between the
                // field and the buttons in the drawn box, so it sits there here too.
                let mut items = vec![FocusItem::new(FocusId::Field(0), layout.field_row())];
                items.extend(self.list_row_items(
                    layout.list_area(),
                    self.file_entries.len(),
                    self.file_selected,
                ));
                items.push(
                    FocusItem::new(FocusId::Button(0), layout.button_row(0)).default_action(),
                );
                items.push(FocusItem::new(FocusId::Button(1), layout.button_row(1)));
                items
            }
            DialogKind::Find | DialogKind::Replace => {
                // The strip's own geometry, not the floating dialog's. Tab has to land on the
                // control the user can see, and the click router maps a `FocusId` back through
                // this same list, so both questions get one answer.
                let strip = crate::ui::find_bar::strip_rect(self);
                let mut items = vec![FocusItem::new(
                    FocusId::Field(0),
                    RectCore::new(strip.x, strip.y, strip.w, 1),
                )];
                if self.find.replace_mode {
                    items.push(FocusItem::new(
                        FocusId::Field(1),
                        RectCore::new(strip.x, strip.y + 1, strip.w, 1),
                    ));
                }
                let (_, spans) = crate::ui::find_bar::options_spans(self);
                let options_y = crate::ui::find_bar::options_row(self);
                for (col, width, hit) in spans {
                    let area = RectCore::new(strip.x + col, options_y, width, 1);
                    match hit {
                        crate::ui::find_bar::OptionHit::Checkbox(i) => {
                            items.push(FocusItem::new(FocusId::Checkbox(i), area));
                        }
                        crate::ui::find_bar::OptionHit::Button(i) => {
                            let item = FocusItem::new(FocusId::Button(i), area);
                            // Enter in a field runs the default action, which is Replace in
                            // Replace mode and Find Next otherwise -- the same rule the floating
                            // dialog used, so the keyboard behaviour did not move with the pixels.
                            let is_default = if self.find.replace_mode {
                                i == 2
                            } else {
                                i == 0
                            };
                            items.push(if is_default {
                                item.default_action()
                            } else {
                                item
                            });
                        }
                    }
                }
                items
            }
            DialogKind::GoTo => {
                let layout = crate::ui::dialogs::goto_layout(self);
                vec![
                    FocusItem::new(FocusId::Field(0), layout.field_row()).default_action(),
                    FocusItem::new(FocusId::Button(0), layout.button_row(0)),
                ]
            }
            DialogKind::Preferences => crate::ui::prefs::focus_items(self),
            DialogKind::About => {
                let layout = crate::ui::dialogs::about_layout(self);
                vec![FocusItem::new(FocusId::Button(0), layout.button_row(0)).default_action()]
            }
            DialogKind::Confirm | DialogKind::BinaryConfirm | DialogKind::CloseAllConfirm => {
                let layout = crate::ui::dialogs::confirm_layout(self);
                layout
                    .button_rects()
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let item = FocusItem::new(FocusId::Button(i), *r);
                        if i == 0 {
                            item.default_action()
                        } else {
                            item
                        }
                    })
                    .collect()
            }
            DialogKind::LanguagePicker | DialogKind::ThemePicker => {
                let layout = crate::ui::dialogs::picker_layout(self);
                let count = self.picker_candidates(kind).len();
                let cursor = match kind {
                    DialogKind::LanguagePicker => self.lang_selected,
                    _ => self.theme_selected,
                };
                let mut items = vec![FocusItem::new(FocusId::Field(0), layout.field_row())];
                items.extend(self.list_row_items(layout.list_area(), count, cursor));
                // The field is the default child: Enter there runs the filter's accept path.
                if let Some(first) = items.first_mut() {
                    *first = first.clone().default_action();
                }
                items
            }
            DialogKind::DocumentList => {
                let layout = crate::ui::dialogs::document_list_layout(self);
                let mut items = self.list_row_items(
                    layout.list_area(),
                    self.documents.len(),
                    self.document_list_selected,
                );
                if let Some(first) = items.first_mut() {
                    *first = first.clone().default_action();
                }
                items
            }
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                let layout = crate::ui::dialogs::choice_layout(self);
                let count = crate::ui::dialogs::choice_rows(self).len();
                let mut items =
                    self.list_row_items(layout.list_area(), count, self.choice_cursor());
                if let Some(first) = items.first_mut() {
                    *first = first.clone().default_action();
                }
                items
            }
        }
    }

    /// The rectangle of the dialog box currently drawn, border included.
    ///
    /// # Why this exists next to the focus list rather than instead of it
    ///
    /// `dialog_focus_items` answers "which control is under the pointer" but deliberately
    /// contains no rectangle for the box itself, and no rectangle for the space around it. That
    /// was enough while a click anywhere near a dialog was simply swallowed. It stopped being
    /// enough the moment an outside click had to mean "close", because deciding that needs the
    /// one rectangle the focus list does not carry: the frame.
    ///
    /// Derived from the same layout objects the renderer draws and the focus list measures, for
    /// the same reason those two agree today: a box computed here and a box painted on screen
    /// cannot disagree, so the dismiss region is exactly the visible box. A hand-written size
    /// here would drift the first time a dialog's row count changed, and the failure would be a
    /// click that closes a dialog the user believes they clicked inside.
    ///
    /// # What has no box
    ///
    /// Find, Replace and the palette are not centered boxes. The first two are docked service
    /// strips whose geometry is `find_bar::strip_rect`, and the palette is a full-width overlay;
    /// both answer their own hit-testing above this function, and both return `None` here so the
    /// caller cannot accidentally apply box logic to a strip. `None` means "no outside region
    /// exists for this overlay", which is the honest answer rather than a rectangle nobody drew.
    pub fn dialog_box_rect(&self) -> Option<RectCore> {
        let kind = match self.mode() {
            Mode::Dialog(k) => k,
            _ => return None,
        };
        let area = match kind {
            DialogKind::Open | DialogKind::SaveAs => {
                crate::ui::dialogs::file_picker_layout(self)
                    .frame
                    .layout
                    .area
            }
            DialogKind::GoTo => crate::ui::dialogs::goto_layout(self).frame.layout.area,
            DialogKind::Preferences => crate::ui::prefs::frame(self).layout.area,
            DialogKind::About => crate::ui::dialogs::about_layout(self).frame.layout.area,
            DialogKind::Confirm | DialogKind::BinaryConfirm | DialogKind::CloseAllConfirm => {
                crate::ui::dialogs::confirm_layout(self).frame.layout.area
            }
            DialogKind::LanguagePicker | DialogKind::ThemePicker => {
                crate::ui::dialogs::picker_layout(self).frame.layout.area
            }
            DialogKind::DocumentList => {
                crate::ui::dialogs::document_list_layout(self)
                    .frame
                    .layout
                    .area
            }
            DialogKind::EncodingPicker | DialogKind::EolMenu | DialogKind::RecentFiles => {
                crate::ui::dialogs::choice_layout(self)
                    .picker
                    .frame
                    .layout
                    .area
            }
            // Docked strips and the palette: no centered box, so no outside.
            DialogKind::Find | DialogKind::Replace => return None,
        };
        Some(area)
    }

    /// One [`FocusItem`] per *visible* list row, addressed by absolute row index.
    ///
    /// A scrolled listing is the reason this exists. The whole list used to be a single
    /// `FocusId::List` rectangle, so every row hit-test answered "the list" and the click
    /// handler fell back to the row already selected -- clicking `../` at the top of a
    /// scrolled picker could not select it, and `Tab` focus landed on a row nobody could see.
    /// Only on-screen rows get an item: the window is derived from the same
    /// [`crate::ui::framework::visible_rows`] call the renderer makes, so item index and
    /// screen row are the same question with the same answer.
    ///
    /// `FocusId::List` is deliberately *not* in the returned list. Tab order wants rows, and
    /// a whole-list item would shadow them in [`crate::ui::framework::focus_at`].
    /// Whether a list row (or a legacy whole-list focus) currently has focus.
    ///
    /// Used by the renderer to decide whether the selection bar draws in the focused style.
    /// Rows carry focus on their own now, so matching only `FocusId::List` would leave every
    /// list looking unfocused.
    pub fn list_focused(&self) -> bool {
        matches!(
            self.focused_id(),
            Some(crate::ui::framework::FocusId::List)
                | Some(crate::ui::framework::FocusId::ListRow(_))
        )
    }

    /// The focus items for a list's visible rows, addressed by absolute row number.
    ///
    /// `pub` because a dialog's focus list is built in that dialog's own module, and every list
    /// dialog needs the same windowing: the rows on screen, each carrying its index in the data
    /// rather than its slot on the screen. A dialog that invents its own version gets a click
    /// target that means one thing to the mouse and another to the renderer.
    pub fn list_row_items(
        &self,
        area: np_core::RectCore,
        count: usize,
        cursor: usize,
    ) -> Vec<FocusItem> {
        let (first, last) = crate::ui::framework::visible_rows(count, cursor, area.h as usize);
        (first..last)
            .map(|row| FocusItem::new(FocusId::ListRow(row), area.row_slice((row - first) as u16)))
            .collect()
    }

    /// Move dialog focus by `delta`, wrapping (FR-U4 `Tab` / `Shift+Tab`).
    pub fn cycle_dialog_focus(&mut self, delta: i32) {
        let count = self.dialog_focus_items().len();
        self.dialog_focus = focus_cycle(self.dialog_focus, delta, count);
        // Tab addresses children by position, so it releases the pointer's hold; a stale row
        // would otherwise outrank the one Tab just landed on. The wheel's held choice too -- the
        // keyboard has taken over row movement.
        self.pointer_row = None;
        self.wheel_choice = None;
        // Landing on a row by Tab is a real choice of that row: make it the list's cursor so
        // the highlighted row, the focused row, and the row Enter acts on are one thing.
        if let Some(FocusId::ListRow(row)) = self.focused_id() {
            self.select_dialog_row_cursor(row);
        }
        self.sync_legacy_field_focus();
        self.dirty = true;
    }

    /// Mirror the framework's focus index into the per-dialog field state S10 and S12 read.
    ///
    /// Those stages predate the framework and kept their own idea of which field was active
    /// (`find.focus`, and the picker's implicit "typing goes to the query"). FR-U4 moves the
    /// cycle into the framework, so the mirror has to be updated in one place rather than
    /// having each dialog advance its own counter, or the two disagree about where the caret
    /// is and typing lands in the wrong field.
    pub fn sync_legacy_field_focus(&mut self) {
        if !matches!(
            self.mode(),
            Mode::Dialog(DialogKind::Find) | Mode::Dialog(DialogKind::Replace)
        ) {
            return;
        }
        if let Some(FocusId::Field(i)) = self.focused_id() {
            self.find.focus = if i == 0 {
                crate::find::FindField::Find
            } else {
                crate::find::FindField::Replace
            };
        }
    }

    /// The id of the focused child, if any.
    /// Which child of the open dialog currently has focus.
    ///
    /// # Why this resolves by identity rather than by index
    ///
    /// `dialog_focus` is an index into the *visible* child list, and for a list dialog that list
    /// is a window: a scrolled picker exposes rows 40..60 as items 0..20. An index recorded
    /// before the window moved therefore comes to mean a different row, which is the mechanism
    /// behind three separate reported bugs -- Enter acting on a row nobody selected, a click
    /// after `../` resolving against the directory just left, and the Themes list being
    /// unreachable by keyboard.
    ///
    /// So a list dialog's *row cursor* is the authority while focus is inside the listing, and
    /// the index is the authority everywhere else. The split is decided by what the index points
    /// at, not by guessing which of the two was written last: if the index lands on a row, the
    /// cursor answers with the row; if it lands on a field or a button, the index answers.
    ///
    /// The pointer's explicit choice outranks both, and is cleared the moment the listing is
    /// replaced -- see `App::pointer_row`.
    pub fn focused_id(&self) -> Option<FocusId> {
        let items = self.dialog_focus_items();
        if items.is_empty() {
            return None;
        }
        // 1. An explicit pointer choice, while it stands. A wheel-held choice outranks even that:
        //    it is the row the user selected and then deliberately scrolled away from, and the
        //    wheel is the only thing that clears it. Both are checked against visibility, so a
        //    row that is off-screen simply does not answer and the highlight follows the window.
        if let Some(row) = self.wheel_choice {
            let id = FocusId::ListRow(row);
            if items.iter().any(|it| it.id == id) {
                return Some(id);
            }
        }
        if let Some(row) = self.pointer_row {
            let id = FocusId::ListRow(row);
            if items.iter().any(|it| it.id == id) {
                return Some(id);
            }
        }
        // 2. The index answers, except when it points into the listing: then the dialog's own
        //    row cursor is what "this row" means, because the index is a window offset and the
        //    cursor is a row number.
        let by_index = items
            .get(self.dialog_focus)
            .or_else(|| items.first())
            .map(|i| i.id);
        if matches!(by_index, Some(FocusId::ListRow(_))) {
            if let Some(id) = self.list_cursor_identity() {
                if items.iter().any(|it| it.id == id) {
                    return Some(id);
                }
            }
        }
        by_index
    }

    /// The open dialog's row cursor, as a focus identity, or `None` if it has no list.
    ///
    /// Reads the shared cursor (`App::list_cursor`), which is why this works for every list
    /// dialog with no per-kind branching. Preferences is included: its rows are focus items
    /// already, and giving the shared cursor an answer for it is what makes the arrow keys and
    /// the mouse drive the same row rather than two.
    fn list_cursor_identity(&self) -> Option<FocusId> {
        if self.dialog_row_count() == 0 {
            return None;
        }
        Some(FocusId::ListRow(self.list_cursor()))
    }
}

// ------------------------------------------------------------- menu keys

/// Keys while a top-level menu is open (FR-U1).
///
/// Left/Right walk the bar, Up/Down walk the items, Enter runs the highlighted item, and a
/// printable character matches an item's accelerator. `Esc` is handled upstream.
pub fn menu_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let Mode::Menu { open, item, parent } = app.mode() else {
        return None;
    };
    // The list for *this* level. A submenu's items are the parent's children, not the menu's
    // own top level; resolving the top level here is what made a submenu cursor move through
    // rows it could not see.
    let items = menus::level_items(app, open, parent);
    let in_submenu = parent.is_some();
    match ev.code {
        KeyCode::Left => {
            // Inside a submenu, Left closes the submenu and leaves the parent where it was.
            // Only at the top level does it walk to the previous menu title.
            if in_submenu {
                app.pop_mode();
                app.submenu_open = false;
                return Some(Action::Nop);
            }
            let idx = MenuId::ALL.iter().position(|m| *m == open).unwrap_or(0);
            let next = MenuId::ALL[(idx + MenuId::ALL.len() - 1) % MenuId::ALL.len()];
            app.set_top_menu(next, 0);
            Some(Action::Nop)
        }
        KeyCode::Right => {
            // Right opens the submenu under the cursor; at the top level it walks titles.
            if !in_submenu {
                if let Some(entry) = items.get(item) {
                    if let MenuItemKind::Submenu(parent_id) = entry.kind {
                        let children = menus::submenu_items(app, parent_id, &entry.label);
                        if !children.is_empty() {
                            app.push_submenu(parent_id, &entry.label);
                            return Some(Action::Nop);
                        }
                    }
                }
            }
            let idx = MenuId::ALL.iter().position(|m| *m == open).unwrap_or(0);
            let next = MenuId::ALL[(idx + 1) % MenuId::ALL.len()];
            app.set_top_menu(next, 0);
            Some(Action::Nop)
        }
        KeyCode::Down => {
            let next = next_enabled(&items, item, 1);
            app.set_menu(open, next);
            Some(Action::Nop)
        }
        KeyCode::Up => {
            let next = next_enabled(&items, item, -1);
            app.set_menu(open, next);
            Some(Action::Nop)
        }
        KeyCode::PageDown => {
            app.set_menu(open, (item + 5).min(items.len().saturating_sub(1)));
            Some(Action::Nop)
        }
        KeyCode::PageUp => {
            app.set_menu(open, item.saturating_sub(5));
            Some(Action::Nop)
        }
        KeyCode::Enter => activate_menu_item(app, open, parent, item, &items),
        KeyCode::Home => {
            app.set_menu(open, 0);
            Some(Action::Nop)
        }
        KeyCode::End => {
            app.set_menu(open, items.len().saturating_sub(1));
            Some(Action::Nop)
        }
        KeyCode::Char(c) => {
            // An item's own accelerator wins over the bar's, so `x` in the Edit menu cuts.
            if let Some(i) = items.iter().position(|it| {
                it.enabled && item_accelerator(it).is_some_and(|a| a == c.to_ascii_uppercase())
            }) {
                app.set_menu(open, i);
                return activate_menu_item(app, open, parent, i, &items);
            }
            // Otherwise a letter is either another menu's accelerator or noise; either way it
            // must not type into the document.
            let upper = c.to_ascii_uppercase();
            if let Some(m) = MenuId::ALL.iter().find(|m| m.accelerator() == upper) {
                app.set_top_menu(*m, 0);
            }
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

/// Run the highlighted menu item, descending into a submenu when there is one.
fn activate_menu_item(
    app: &mut App,
    _open: MenuId,
    parent: Option<SubmenuId>,
    item: usize,
    items: &[menus::MenuItem],
) -> Option<Action> {
    let Some(entry) = items.get(item) else {
        return Some(Action::Nop);
    };
    if !entry.enabled {
        return Some(Action::Nop);
    }
    // A submenu row only opens a level when it is itself in the top level. Inside a submenu
    // every row is a leaf, and descending again would stack a level with nothing new in it.
    if parent.is_none() {
        if let MenuItemKind::Submenu(parent_id) = entry.kind {
            let children = menus::submenu_items(app, parent_id, &entry.label);
            if !children.is_empty() {
                app.push_submenu(parent_id, &entry.label);
                return Some(Action::Nop);
            }
        }
    }
    // A recent-file row's action is the unparameterized `RecentFiles`, whose job is to open the
    // picker dialog. Coming from the submenu, where each row is already a named file, that read
    // as "clicking a file opens a list of files". The row's label is the path, so open it.
    if entry.action == Some(Action::RecentFiles) {
        let path = std::path::PathBuf::from(&entry.label);
        app.close_menu();
        app.open_path(&path);
        return Some(Action::Nop);
    }
    let Some(action) = entry.action else {
        return Some(Action::Nop);
    };
    // The Encoding lists encode the codec index in `hint`; recover it before closing the
    // menu, because `execute` reads `pending_encoding`.
    if matches!(action, Action::ConvertEncoding | Action::ReloadAsEncoding) {
        if let Ok(i) = entry.hint.parse::<usize>() {
            if let Some(enc) = np_core::EncodingId::all().get(i) {
                app.pending_encoding = Some(enc.clone());
            }
        }
    }
    app.close_menu();
    Some(action)
}

/// The next enabled item after `from` in direction `delta`, skipping separators.
fn next_enabled(items: &[menus::MenuItem], from: usize, delta: i32) -> usize {
    if items.is_empty() {
        return 0;
    }
    let mut i = from.min(items.len() - 1);
    for _ in 0..items.len() {
        i = if delta > 0 {
            (i + 1) % items.len()
        } else {
            (i + items.len() - 1) % items.len()
        };
        if items[i].kind != MenuItemKind::Separator && items[i].enabled {
            return i;
        }
    }
    i
}

/// The first character of a label that is not already taken by another item in the menu.
///
/// Notepad++ underlines one letter per item and the user types it. Choosing "first letter
/// not used by an earlier item" reproduces that without a hand-maintained table, which is
/// the same no-drift argument the rest of the registry rests on.
pub fn item_accelerator(item: &menus::MenuItem) -> Option<char> {
    let first = item.label.chars().next()?.to_ascii_uppercase();
    Some(first)
}

/// The accelerator actually usable for item `index` in `items`, disambiguated.
pub fn item_accelerator_at(items: &[menus::MenuItem], index: usize) -> Option<char> {
    let item = items.get(index)?;
    let label = item.label.trim_start();
    let mut taken: Vec<char> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if i == index {
            continue;
        }
        if let Some(c) = it.label.trim_start().chars().next() {
            taken.push(c.to_ascii_uppercase());
        }
    }
    for c in label.chars() {
        let up = c.to_ascii_uppercase();
        if c.is_alphabetic() && !taken.contains(&up) {
            return Some(up);
        }
    }
    label.chars().next().map(|c| c.to_ascii_uppercase())
}

impl App {
    /// Move the cursor within the dropdown the user is looking at.
    ///
    /// A submenu keeps its `parent` and therefore its own item list, so Down inside
    /// "Convert to" walks the codec rows rather than jumping back to the parent menu's rows.
    /// The top-level `Left`/`Right` title walking uses [`App::set_top_menu`], which is the one
    /// that leaves a submenu.
    pub fn set_menu(&mut self, menu: MenuId, item: usize) {
        self.menu_bar_active = true;
        if let Some(Mode::Menu {
            open,
            item: slot,
            parent,
        }) = self.mode_stack.last_mut()
        {
            *open = menu;
            *slot = item;
            // A submenu's `parent` must match the menu it hangs off. Swapping the menu under a
            // submenu without swapping the label would resolve a submenu that does not exist
            // under the new parent, and the box would vanish.
            if !matches!(parent, Some(sub) if sub.parent() == menu) {
                *parent = None;
            }
        } else {
            self.push_mode(Mode::Menu {
                open: menu,
                item,
                parent: None,
            });
        }
        self.dirty = true;
    }

    /// Point the bar at a top-level menu title, closing any submenu open above it.
    pub fn set_top_menu(&mut self, menu: MenuId, item: usize) {
        self.set_menu(menu, item);
        if let Some(Mode::Menu { parent, .. }) = self.mode_stack.last_mut() {
            *parent = None;
        }
        self.submenu_open = false;
        self.dirty = true;
    }

    /// Open a submenu as a nested `Mode::Menu`, so `Esc` closes only the submenu.
    pub fn push_submenu(&mut self, menu: MenuId, label: &str) {
        self.submenu_open = true;
        self.push_mode(Mode::Menu {
            open: menu,
            item: 0,
            parent: SubmenuId::find(menu, label),
        });
    }

    /// Close the whole menu bar.
    pub fn close_menu(&mut self) {
        while matches!(self.mode_stack.last(), Some(Mode::Menu { .. })) {
            self.pop_mode();
        }
        self.menu_bar_active = false;
        self.submenu_open = false;
        self.dirty = true;
    }
}

// ----------------------------------------------------------- palette keys

/// Keys while the command palette is open (FR-U8).
pub fn palette_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let entries = crate::ui::palette::palette_entries(app, &app.palette_query);
    match ev.code {
        KeyCode::Enter => {
            let action = crate::ui::palette::selected(app, &entries);
            app.pop_mode();
            match action {
                Some(a) => Some(a),
                None => Some(Action::Nop),
            }
        }
        KeyCode::Down => {
            app.palette_selected = (app.palette_selected + 1).min(entries.len().saturating_sub(1));
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Up => {
            app.palette_selected = app.palette_selected.saturating_sub(1);
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Backspace => {
            app.palette_query.pop();
            app.palette_selected = 0;
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Char(c) => {
            if ev.modifiers.contains(KeyModifiers::ALT) {
                return Some(Action::Nop);
            }
            app.palette_query.push(c);
            app.palette_selected = 0;
            app.dirty = true;
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}

// ------------------------------------------------------ context menu keys

/// Keys while a context menu is open (FR-U7).
///
/// The menu opens with nothing highlighted and Up/Down move into it, which is what a menu
/// summoned by a right-click at a pointer should do: the pointer, not a keyboard cursor, was
/// the thing that expressed intent.
pub fn context_menu_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let Mode::ContextMenu(kind) = app.mode() else {
        return None;
    };
    let items = crate::ui::context_menu::items(app, kind);
    let current = app.dialog_focus;
    match ev.code {
        KeyCode::Down => {
            app.dialog_focus = (current + 1).min(items.len().saturating_sub(1));
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Up => {
            app.dialog_focus = current.saturating_sub(1);
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Enter => {
            let Some(entry) = items.get(current) else {
                app.pop_mode();
                return Some(Action::Nop);
            };
            if !entry.enabled {
                return Some(Action::Nop);
            }
            let action = entry.action;
            app.pop_mode();
            Some(action)
        }
        _ => Some(Action::Nop),
    }
}

// --------------------------------------------------------- help overlay keys

/// Keys while the help overlay is open (FR-U9). It is a scrollable read-only view, so the
/// only input it has is scrolling.
pub fn help_key(app: &mut App, ev: KeyEvent) -> Option<Action> {
    let lines = crate::ui::help::help_lines(app).len();
    let page = app.editor_height().saturating_sub(4).max(1);
    match ev.code {
        KeyCode::Down | KeyCode::Char('j') => {
            app.help_scroll = (app.help_scroll + 1).min(lines.saturating_sub(1));
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::PageDown => {
            app.help_scroll = (app.help_scroll + page).min(lines.saturating_sub(1));
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::PageUp => {
            app.help_scroll = app.help_scroll.saturating_sub(page);
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Home => {
            app.help_scroll = 0;
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::End => {
            app.help_scroll = lines.saturating_sub(1);
            app.dirty = true;
            Some(Action::Nop)
        }
        KeyCode::Enter => {
            app.pop_mode();
            Some(Action::Nop)
        }
        _ => Some(Action::Nop),
    }
}
