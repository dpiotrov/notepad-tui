//! Context menus (FR-U7): editor right-click and tab right-click.
//!
//! # Items come from the same registry as the menus
//!
//! Every entry names an `Action`, and enabled state is resolved against `App` the same way
//! [`crate::ui::menus::menu_items`] does it, so "Paste is grey with an empty clipboard" is
//! one rule with two callers rather than two rules that can disagree.
//!
//! # The caret does not move
//!
//! §6.9 row 18 forbids moving the caret to show a menu, and `App::context_menu_target`
//! records what was right-clicked for exactly that reason. The menu must not "helpfully"
//! place the caret before Copy: a user who right-clicks a word to copy it and then types at
//! their original cursor position would have their text land somewhere else entirely.
//! Editor menu actions therefore operate on the existing selection, not on the click point,
//! which matches Notepad++: right-clicking outside a selection with text selected keeps the
//! selection.
//!
//! # Clamping
//!
//! A menu opened near the right or bottom edge flips to the other side of the pointer
//! before it clamps to the screen, so it is never partially off screen. Clamping alone
//! would produce a menu whose left edge is at column 0 and whose items are still reachable,
//! but a menu that opens *leftward* of the pointer when there is no room rightward is what
//! people expect from every other application they use.

use np_core::RectCore;

use crate::actions::Action;
use crate::app::{App, ContextKind};

/// Preferred menu width in cells.
///
/// Sized for the widest FR-U7 label plus its chord: `Toggle Bookmark  Ctrl+F2` needs 28
/// before the borders, and a menu that clips its own labels is worse than one that covers a
/// few more columns of text.
pub const MENU_W: u16 = 32;

/// One context menu row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    pub action: Action,
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
    pub hint: String,
    pub separator_after: bool,
}

impl ContextItem {
    fn new(action: Action) -> Self {
        ContextItem {
            action,
            label: action.label().to_string(),
            enabled: true,
            checked: false,
            hint: String::new(),
            separator_after: false,
        }
    }

    fn sep(mut self) -> Self {
        self.separator_after = true;
        self
    }

    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// The editor right-click items (FR-U7).
pub fn editor_items(app: &App) -> Vec<ContextItem> {
    let has_selection = app
        .doc()
        .and_then(|d| d.selection)
        .is_some_and(|s| !s.is_empty());
    let readonly = app.doc().is_some_and(|d| d.readonly);
    let doc_present = app.doc().is_some();
    let mut items = vec![
        ContextItem::new(Action::Cut),
        ContextItem::new(Action::Copy),
        ContextItem::new(Action::Paste),
        ContextItem::new(Action::Delete).sep(),
        ContextItem::new(Action::SelectAll).sep(),
        ContextItem::new(Action::ToggleBookmark),
        ContextItem::new(Action::FoldAll),
        ContextItem::new(Action::UnfoldAll).sep(),
        ContextItem::new(Action::CopyPath),
    ];
    for item in items.iter_mut() {
        item.hint = app
            .keymap
            .keys_for(item.action)
            .first()
            .cloned()
            .unwrap_or_default();
        item.checked = match item.action {
            Action::ToggleBookmark => app
                .doc()
                .is_some_and(|d| d.bookmarks.contains(&d.cursor.pos.line)),
            _ => false,
        };
        item.enabled = match item.action {
            Action::Cut | Action::Copy | Action::Delete => doc_present && has_selection,
            Action::Paste => doc_present && !readonly,
            Action::SelectAll => doc_present,
            a if is_write(a) => doc_present && !readonly,
            _ => doc_present,
        };
    }
    items
}

/// The tab right-click items (FR-U2).
pub fn tab_items(app: &App) -> Vec<ContextItem> {
    let multi = app.documents.len() > 1;
    let mut items = vec![
        ContextItem::new(Action::CloseTab),
        ContextItem::new(Action::CloseOtherTabs),
        ContextItem::new(Action::CloseAllTabs).sep(),
        ContextItem::new(Action::ReopenClosedTab),
        ContextItem::new(Action::CopyPath),
    ];
    for item in items.iter_mut() {
        item.hint = app
            .keymap
            .keys_for(item.action)
            .first()
            .cloned()
            .unwrap_or_default();
        item.enabled = match item.action {
            Action::CloseOtherTabs | Action::CloseAllTabs => multi,
            Action::CopyPath => app
                .doc()
                .and_then(|d| d.path.as_ref())
                .is_some_and(|p| !p.as_os_str().is_empty()),
            _ => true,
        };
    }
    // `Reopen Closed Tab` is dead with an empty history, and showing it enabled invites a
    // keystroke that does nothing.
    if app.closed_tabs.is_empty() {
        if let Some(item) = items
            .iter_mut()
            .find(|i| i.action == Action::ReopenClosedTab)
        {
            *item = item.clone().disabled();
        }
    }
    items
}

/// The items for whichever context menu is open.
pub fn items(app: &App, kind: ContextKind) -> Vec<ContextItem> {
    match kind {
        ContextKind::Editor => editor_items(app),
        ContextKind::Tab => tab_items(app),
    }
}

fn is_write(action: Action) -> bool {
    matches!(
        action,
        Action::ToggleBookmark
            | Action::FoldAll
            | Action::UnfoldAll
            | Action::ToggleComment
            | Action::DuplicateLine
    )
}

/// The menu's rectangle for a pointer at `(x, y)`, flipped and clamped to stay on screen.
///
/// `screen` is the whole frame. The menu's own size is derived from the item count, so the
/// caller does not have to agree with the renderer about row counts.
pub fn menu_rect(x: u16, y: u16, count: usize, screen: RectCore) -> RectCore {
    let w = MENU_W.min(screen.w.max(1)).max(3);
    let h = (count as u16 + 2).min(screen.h.max(1));
    // Prefer opening down-right from the pointer. Flip horizontally when the box would run
    // off the right edge, and vertically when it would run off the bottom.
    let mut left = x;
    if left.saturating_add(w) > screen.right() {
        left = x.saturating_sub(w);
    }
    if left < screen.x {
        left = screen.x;
    }
    if left.saturating_add(w) > screen.right() {
        left = screen.right().saturating_sub(w);
    }
    let mut top = y;
    if top.saturating_add(h) > screen.bottom() {
        top = y.saturating_sub(h);
    }
    if top < screen.y {
        top = screen.y;
    }
    if top.saturating_add(h) > screen.bottom() {
        top = screen.bottom().saturating_sub(h);
    }
    RectCore::new(left, top, w, h)
}

/// Which item a pointer is over, given the menu's rectangle and its rows.
///
/// Returns `None` for the border rows and for a miss, which the caller reads as "dismiss".
pub fn item_at(area: RectCore, x: u16, y: u16, count: usize) -> Option<usize> {
    if !area.contains(x, y) {
        return None;
    }
    let row = y.checked_sub(area.y + 1)? as usize;
    if row < count {
        Some(row)
    } else {
        None
    }
}

/// Draw the context menu.
pub fn draw(app: &App, buf: &mut ratatui::buffer::Buffer) {
    let crate::app::Mode::ContextMenu(kind) = app.mode() else {
        return;
    };
    let items = items(app, kind);
    if items.is_empty() {
        return;
    }
    let screen = app.regions.bounds();
    let anchor = app
        .context_menu_target
        .map(|_| (app.context_menu_cell.0, app.context_menu_cell.1))
        .unwrap_or((app.regions.editor.x, app.regions.editor.y));
    let area = menu_rect(anchor.0, anchor.1, items.len(), screen);
    let layout = crate::ui::framework::DialogLayout::centered(area, area.w, area.h);
    let pal = app.palette();
    layout.draw_box(buf, "", &pal);
    for (i, item) in items.iter().enumerate() {
        let line = layout.row(i as u16);
        let text = if item.checked {
            format!("\u{2713} {}", item.label)
        } else {
            format!("  {}", item.label)
        };
        let style = if !item.enabled {
            crate::ui::framework::hint_style(&pal)
        } else {
            crate::style::dialog(&pal)
        };
        let padded = crate::ui::framework::fit(&text, line.w as usize);
        crate::ui::menu_bar::draw_str(buf, line.x, line.y, &padded, style);
        if !item.hint.is_empty() {
            let hint = format!(" {}", item.hint);
            let hw = hint.chars().count() as u16;
            if hw + 1 < line.w {
                let hx = line.right().saturating_sub(hw + 1);
                // Dim in both states: a chord beside a disabled item must not look more
                // available than the item it belongs to.
                let hint_style = crate::ui::framework::hint_style(&pal);
                crate::ui::menu_bar::draw_str(buf, hx, line.y, &hint, hint_style);
            }
        }
        if item.separator_after && line.bottom() < layout.inner.bottom() {
            let rule: String = "\u{2500}".repeat(line.w as usize);
            crate::ui::menu_bar::draw_str(
                buf,
                line.x,
                line.bottom(),
                &rule,
                crate::ui::framework::hint_style(&app.palette()),
            );
        }
    }
}
