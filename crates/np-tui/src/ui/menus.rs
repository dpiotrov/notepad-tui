//! The menu registry: one source of truth for menus, the palette and the help overlay.
//!
//! # Why a registry rather than per-menu drawing code
//!
//! Spec §5.3 and FR-U8 both insist that menus, the palette and the keymap cannot drift.
//! The only way to actually guarantee that is for all three to read one structure, so this
//! module answers "what is in menu X" and "what does item Y look like right now", and
//! `menu_bar.rs`, `palette.rs` and `help.rs` all consume it. A hardcoded menu drawn beside
//! a separately-maintained palette is how you get a palette entry that executes a command
//! the menu no longer offers.
//!
//! # Items are resolved against `App`, not declared statically
//!
//! An item's label, enabled state, checkmark and key hint are all functions of current
//! state: Paste is grey with an empty clipboard, Toggle Word Wrap is checked when
//! `config.ui.word_wrap` is set, and the hint must be the user's *actual* binding rather
//! than the default one. So [`menu_items`] builds owned [`MenuItem`]s per query. It is
//! cheap -- a handful of small strings -- and building fresh is what makes a stale
//! checkmark impossible.
//!
//! # Submenus
//!
//! [`MenuItemKind::Submenu`] names a child list resolved by [`submenu_items`]. Encoding and
//! Language are the real ones (Convert to / Reload as, and the language list); the rest of
//! the bar is flat. A submenu indicator is drawn whether or not the child list is currently
//! non-empty, so an empty language list does not make the arrow flicker.

use np_core::Eol;

use crate::actions::{Action, MenuId, SubmenuId};
use crate::app::App;

/// One resolved menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// What it runs. `None` for a separator or a submenu header.
    pub action: Option<Action>,
    /// Display text, already state-dependent (e.g. a recent-file path).
    pub label: String,
    pub kind: MenuItemKind,
    pub enabled: bool,
    pub checked: bool,
    /// Right-aligned key hint, from the live keymap, e.g. `Ctrl+S`.
    pub hint: String,
}

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItemKind {
    Action,
    Separator,
    Submenu(MenuId),
}

impl MenuItem {
    fn action(a: Action) -> MenuItem {
        MenuItem {
            action: Some(a),
            label: a.label().to_string(),
            kind: MenuItemKind::Action,
            enabled: true,
            checked: false,
            hint: String::new(),
        }
    }

    fn submenu(menu: MenuId, label: &str) -> MenuItem {
        MenuItem {
            action: Some(Action::MenuBar(menu)),
            label: label.to_string(),
            kind: MenuItemKind::Submenu(menu),
            enabled: true,
            checked: false,
            hint: String::new(),
        }
    }

    fn separator() -> MenuItem {
        MenuItem {
            action: None,
            label: String::new(),
            kind: MenuItemKind::Separator,
            enabled: false,
            checked: false,
            hint: String::new(),
        }
    }
}

/// The top-level contents of one menu, before key hints and enabled state are filled in.
///
/// Order is Notepad++'s where the spec does not dictate one, because a person arriving from
/// Notepad++ reaches for a position as much as a name.
fn menu_layout(menu: MenuId) -> Vec<MenuItem> {
    match menu {
        MenuId::File => vec![
            MenuItem::action(Action::NewFile),
            MenuItem::action(Action::OpenFile),
            MenuItem::submenu(MenuId::File, "Recent Files"),
            MenuItem::separator(),
            MenuItem::action(Action::Save),
            MenuItem::action(Action::SaveAs),
            MenuItem::action(Action::ReloadFile),
            MenuItem::separator(),
            MenuItem::action(Action::CloseTab),
            MenuItem::action(Action::CloseOtherTabs),
            MenuItem::action(Action::CloseAllTabs),
            MenuItem::action(Action::ReopenClosedTab),
            MenuItem::separator(),
            MenuItem::action(Action::Exit),
        ],
        MenuId::Edit => vec![
            MenuItem::action(Action::Undo),
            MenuItem::action(Action::Redo),
            MenuItem::separator(),
            MenuItem::action(Action::Cut),
            MenuItem::action(Action::Copy),
            MenuItem::action(Action::Paste),
            MenuItem::action(Action::Delete),
            MenuItem::separator(),
            MenuItem::action(Action::SelectAll),
            MenuItem::action(Action::ToggleOvertype),
            MenuItem::separator(),
            MenuItem::action(Action::DuplicateLine),
            MenuItem::action(Action::DeleteLine),
            MenuItem::action(Action::MoveLineUp),
            MenuItem::action(Action::MoveLineDown),
            MenuItem::action(Action::SortLinesAsc),
            MenuItem::action(Action::SortLinesDesc),
            MenuItem::separator(),
            MenuItem::action(Action::UpperCase),
            MenuItem::action(Action::LowerCase),
            MenuItem::action(Action::TitleCase),
            MenuItem::action(Action::ToggleComment),
            MenuItem::action(Action::Indent),
            MenuItem::action(Action::Outdent),
            MenuItem::action(Action::InsertDateTime),
        ],
        MenuId::Search => vec![
            MenuItem::action(Action::Find),
            MenuItem::action(Action::FindNext),
            MenuItem::action(Action::FindPrev),
            MenuItem::action(Action::FindWordNext),
            MenuItem::action(Action::FindWordPrev),
            MenuItem::separator(),
            MenuItem::action(Action::Replace),
            MenuItem::action(Action::ReplaceAll),
            MenuItem::action(Action::FindInFiles),
            MenuItem::separator(),
            MenuItem::action(Action::GoToLine),
            MenuItem::separator(),
            MenuItem::action(Action::ToggleBookmark),
            MenuItem::action(Action::NextBookmark),
            MenuItem::action(Action::PrevBookmark),
            MenuItem::action(Action::ClearBookmarks),
        ],
        MenuId::View => vec![
            MenuItem::action(Action::ToggleWordWrap),
            MenuItem::action(Action::ToggleLineNumbers),
            MenuItem::action(Action::ToggleWhitespace),
            MenuItem::action(Action::ToggleEolMarks),
            MenuItem::separator(),
            MenuItem::action(Action::FoldAll),
            MenuItem::action(Action::UnfoldAll),
            MenuItem::action(Action::ToggleFold),
            MenuItem::separator(),
            MenuItem::action(Action::CommandPalette),
        ],
        MenuId::Encoding => vec![
            MenuItem::submenu(MenuId::Encoding, "Convert to"),
            MenuItem::submenu(MenuId::Encoding, "Reload as"),
            MenuItem::separator(),
            MenuItem::action(Action::ToggleReadonly),
        ],
        // Syntax highlighting, per document. The Theme lives here too because a colour scheme is
        // a property of how the text is rendered, which is what this menu is about.
        MenuId::Language => vec![
            MenuItem::action(Action::LanguagePicker),
            MenuItem::action(Action::ThemePicker),
        ],
        // Application settings only. Theme and Language used to be repeated here as well as in
        // the Language menu, and two entries that open the same dialog is not a shortcut, it is
        // a question -- "are these the same setting or two different ones?" -- that the user has
        // no way to answer without trying both. One command, one place in the bar.
        MenuId::Settings => vec![MenuItem::action(Action::Preferences)],
        MenuId::Window => vec![
            MenuItem::action(Action::DocumentList),
            MenuItem::action(Action::NextTab),
            MenuItem::action(Action::PrevTab),
        ],
        MenuId::Help => vec![
            MenuItem::action(Action::HelpOverlay),
            MenuItem::action(Action::About),
        ],
    }
}

/// The items of a submenu, resolved against `app`.
///
/// The two nested Encoding lists are the interesting ones: they enumerate the real codec
/// list rather than a hardcoded handful, so a codec added to `np-core` shows up in the
/// menu without a menu edit.
pub fn submenu_items(app: &App, parent: MenuId, label: &str) -> Vec<MenuItem> {
    match (parent, label) {
        (MenuId::File, "Recent Files") => recent_items(app),
        (MenuId::Encoding, "Convert to") => encoding_items(app, false),
        (MenuId::Encoding, "Reload as") => encoding_items(app, true),
        // Language's second level is the syntax list itself, opened by the picker; the
        // flat entry above is the reachable path, so this returns nothing extra.
        _ => Vec::new(),
    }
}

/// One encoding list. `reload` picks `ReloadAsEncoding` over `ConvertEncoding`.
///
/// Both actions are unparameterized in the contract (`Action` is `Copy` and its kebab name
/// is a `[keymap]` key, so adding a codec payload would mean N new kebab names for a list
/// that is not keyboard-addressable anyway). The chosen codec therefore goes into
/// `App::pending_encoding` when the item fires, and `App::execute` reads it. `pending`
/// marks which one so the checkmark can show the document's current codec.
pub fn encoding_convert_items(app: &App) -> Vec<MenuItem> {
    encoding_items(app, false)
}

pub fn encoding_reload_items(app: &App) -> Vec<MenuItem> {
    encoding_items(app, true)
}

fn encoding_items(app: &App, reload: bool) -> Vec<MenuItem> {
    let current = app.doc().map(|d| d.encoding.clone());
    np_core::EncodingId::all()
        .iter()
        .enumerate()
        .map(|(i, enc)| {
            let mut item = MenuItem::action(if reload {
                Action::ReloadAsEncoding
            } else {
                Action::ConvertEncoding
            });
            item.label = enc.label().to_string();
            // The list index rides in `hint`, which is the one free string slot on a
            // resolved item; `App` reads it back when the item fires. Keeping it out of the
            // label is what stops a codec name from being rendered twice.
            item.hint = i.to_string();
            item.checked = !reload && current.as_ref() == Some(enc);
            item
        })
        .collect()
}

/// The Recent Files list, oldest-trimmed and deduplicated upstream in `App::note_recent`.
pub fn recent_menu_items(app: &App) -> Vec<MenuItem> {
    recent_items(app)
}

fn recent_items(app: &App) -> Vec<MenuItem> {
    app.recent
        .iter()
        .map(|path| {
            let mut item = MenuItem::action(Action::RecentFiles);
            item.label = path.display().to_string();
            item
        })
        .collect()
}

/// EOL conversion items (FR-N4), shared by the EOL menu and the status-bar click.
pub fn eol_items(app: &App) -> Vec<MenuItem> {
    let current = app.doc().map(|d| d.eol);
    [Eol::Lf, Eol::Crlf, Eol::Cr]
        .iter()
        .map(|eol| {
            let mut item = MenuItem::action(Action::SetEol(*eol));
            item.checked = current == Some(*eol);
            item
        })
        .collect()
}

/// The full contents of one menu, resolved and decorated for `app`.
///
/// Decoration is where state enters: key hints from the live keymap (so a remapped action
/// shows its new chord), checkmarks for the four view toggles and overtype, and enabled
/// flags for the actions that genuinely cannot run right now.
pub fn menu_items(app: &App, menu: MenuId) -> Vec<MenuItem> {
    let mut items = menu_layout(menu);
    // A submenu's label is the only thing that varies by parent for the nested lists, so
    // resolve them in place rather than making every caller ask.
    for item in items.iter_mut() {
        if let MenuItemKind::Submenu(parent) = item.kind {
            let children = submenu_items(app, parent, &item.label);
            if matches!(parent, MenuId::File) && item.label == "Recent Files" {
                // Recent Files is a real list, not a jump to an empty submenu: inline it so
                // one click reaches a file. With nothing recent it reads as a disabled row.
                item.kind = MenuItemKind::Action;
                item.action = None;
                item.enabled = false;
                item.label = if children.is_empty() {
                    "(no recent files)".to_string()
                } else {
                    item.label.clone()
                };
                if !children.is_empty() {
                    // Keep it a submenu so the paths are individually clickable.
                    item.kind = MenuItemKind::Submenu(parent);
                    item.enabled = true;
                }
            }
        }
    }
    for item in items.iter_mut() {
        decorate(app, item);
    }
    items
}

/// Where a submenu box opens: beside the parent dropdown's row, flipped left at the edge.
///
/// `parent_area` is the parent dropdown's own rectangle. Shared by the painter and the mouse
/// layer so a click inside a submenu lands on the row that is actually drawn there.
pub fn submenu_rect(
    parent_area: np_core::RectCore,
    items: &[MenuItem],
    screen_w: u16,
    screen_h: u16,
) -> np_core::RectCore {
    let box_w = dropdown_width(items).min(screen_w.max(1)).max(3);
    let box_h = dropdown_height(items.len(), screen_h);
    let anchor_row = parent_area.y + 1;
    let mut x = parent_area.right();
    if x.saturating_add(box_w) > screen_w {
        x = parent_area.x.saturating_sub(box_w);
    }
    np_core::RectCore::new(
        x,
        anchor_row,
        box_w,
        box_h.min(screen_h.saturating_sub(anchor_row)),
    )
}

/// The rows of one menu *level*: a top-level menu's own items, or a submenu's children.
///
/// Three consumers need this -- the dropdown painter, the keyboard router and the mouse
/// hit-test -- and each used to call [`menu_items`] with just a `MenuId`. A submenu shares its
/// parent's `MenuId`, so all three resolved the parent's top level for the submenu: the box was
/// never painted and a click inside it selected the wrong row. Resolving a level in one place is
/// what keeps the arrow, the highlighted row and the running action in agreement.
///
/// `parent` is the submenu label (`"Convert to"`), or `None` for a top-level menu.
pub fn level_items(app: &App, menu: MenuId, parent: Option<SubmenuId>) -> Vec<MenuItem> {
    let Some(submenu) = parent else {
        return menu_items(app, menu);
    };
    let mut items = submenu_items(app, menu, submenu.label());
    for item in items.iter_mut() {
        decorate(app, item);
    }
    items
}

/// Add the key hint, checkmark and enabled flag for one item.
fn decorate(app: &App, item: &mut MenuItem) {
    if item.kind == MenuItemKind::Separator {
        return;
    }
    if let Some(action) = item.action {
        // The *last* chord the user bound, not the first. `bind` appends, so a remap adds a
        // chord rather than replacing it, and showing the default next to a key the user has
        // since rebound would advertise a binding that no longer runs this command.
        // A codec row's `hint` carries its list index, which `activate_menu_item` parses back
        // into the encoding to apply. Stamping a chord over it made Convert To / Reload As
        // silently keep the document's existing encoding while still reading as if they ran.
        //
        // A recent-file row is the other case: `RecentFiles` is bound to a chord, but that chord
        // opens the picker, not the file on this row. Advertising it on every path implied a
        // per-file binding that does not exist.
        if !is_index_hint(item) && item.action != Some(Action::RecentFiles) {
            item.hint = app
                .keymap
                .keys_for(action)
                .last()
                .cloned()
                .unwrap_or_default();
        }
    }
    let Some(action) = item.action else {
        return;
    };
    let doc_present = app.doc().is_some();
    let has_selection = app
        .doc()
        .and_then(|d| d.selection)
        .is_some_and(|s| !s.is_empty());
    match action {
        // Toggles read their state from config so the checkmark survives a tab switch,
        // which is what a preference means.
        Action::ToggleWordWrap => item.checked = app.config.ui.word_wrap,
        Action::ToggleLineNumbers => item.checked = app.config.ui.line_numbers,
        Action::ToggleWhitespace => item.checked = app.config.ui.show_whitespace,
        Action::ToggleEolMarks => item.checked = app.config.ui.show_eol,
        Action::ToggleOvertype => item.checked = app.overtype,
        Action::ToggleReadonly => item.checked = app.doc().is_some_and(|d| d.readonly),
        Action::SetEol(eol) => item.checked = app.doc().is_some_and(|d| d.eol == eol),
        // Needs a document.
        Action::Save
        | Action::SaveAs
        | Action::ReloadFile
        | Action::CloseTab
        | Action::CopyPath
        | Action::CopyFilename => item.enabled = doc_present,
        // Needs a selection to have anything to act on.
        Action::Cut | Action::Copy | Action::Delete | Action::ReplaceAll => {
            item.enabled = doc_present && has_selection && !is_readonly(app);
        }
        // Needs somewhere to put text.
        Action::Paste => item.enabled = doc_present && !is_readonly(app),
        // Anything that writes to the buffer is dead in a read-only document (FR-F9).
        a if is_mutating(a) => item.enabled = doc_present && !is_readonly(app),
        _ => {}
    }
}

/// Whether the active document refuses writes.
fn is_readonly(app: &App) -> bool {
    app.doc().is_some_and(|d| d.readonly)
}

/// The actions that modify the buffer, used only for the read-only grey-out.
fn is_mutating(action: Action) -> bool {
    matches!(
        action,
        Action::Undo
            | Action::Redo
            | Action::SelectAll
            | Action::DuplicateLine
            | Action::DeleteLine
            | Action::MoveLineUp
            | Action::MoveLineDown
            | Action::SortLinesAsc
            | Action::SortLinesDesc
            | Action::UpperCase
            | Action::LowerCase
            | Action::TitleCase
            | Action::ToggleComment
            | Action::Indent
            | Action::Outdent
            | Action::InsertDateTime
            | Action::ToggleBookmark
            | Action::ClearBookmarks
            | Action::SetEol(_)
    )
}

/// Every menu item that carries an action, across all nine menus.
///
/// The table-driven test iterates this to prove no menu entry is dead: each one must have a
/// real `Action` and each action must be reachable.
pub fn all_menu_actions() -> Vec<Action> {
    let mut out = Vec::new();
    for menu in MenuId::ALL {
        for item in menu_layout(menu) {
            if let MenuItemKind::Action = item.kind {
                if let Some(a) = item.action {
                    out.push(a);
                }
            }
        }
    }
    out
}

/// The width a menu's dropdown needs: the widest label plus its hint plus the gutter.
///
/// The check column (2 cells), one space of separation between label and chord, and the two
/// border columns are all counted, because a box one cell too narrow clips the last character
/// of the longest chord and the user reads `ctrl+shift+f` for `ctrl+shift+f3`.
pub fn dropdown_width(items: &[MenuItem]) -> u16 {
    let mut widest = 16u16;
    for item in items {
        let label = match item.kind {
            MenuItemKind::Submenu(_) => item.label.chars().count() + 2,
            _ => item.label.chars().count(),
        };
        let hint = if item.hint.is_empty() || is_index_hint(item) {
            0
        } else {
            item.hint.chars().count() + 3
        };
        widest = widest.max((label + hint + 6) as u16);
    }
    widest
}

/// Whether this item's `hint` carries a codec index rather than a chord.
fn is_index_hint(item: &MenuItem) -> bool {
    matches!(
        item.action,
        Some(Action::ConvertEncoding) | Some(Action::ReloadAsEncoding)
    )
}

/// Where a dropdown opens, given the bar rectangle and which title is open.
///
/// Opens below the title, left-aligned with it, and flips left when the box would run off
/// the right edge -- the same clamping rule as the context menu, so neither can be drawn
/// partially off screen.
pub fn dropdown_rect(
    bar: np_core::RectCore,
    title_x: u16,
    title_w: u16,
    box_w: u16,
    box_h: u16,
    screen_w: u16,
) -> np_core::RectCore {
    let w = box_w.min(screen_w.max(1));
    let mut x = title_x;
    if x + w > screen_w {
        // Prefer aligning to the title's right edge, then clamp to the screen.
        x = title_x.saturating_add(title_w).saturating_sub(w);
    }
    if x + w > screen_w {
        x = screen_w.saturating_sub(w);
    }
    np_core::RectCore::new(x, bar.bottom().min(bar.y + 1), w, box_h)
}

/// The height a dropdown needs for `count` items.
pub fn dropdown_height(count: usize, screen_h: u16) -> u16 {
    // Two rows of chrome (top and bottom border) plus the items, capped so a long menu on
    // a short terminal stays inside the screen instead of running past the status bar.
    let wanted = count.saturating_add(2) as u16;
    wanted.min(screen_h.saturating_sub(2)).max(3)
}
