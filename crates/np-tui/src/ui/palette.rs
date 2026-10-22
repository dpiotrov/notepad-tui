//! Command palette (FR-U8): a fuzzy-filtered list of every command, `Ctrl+Shift+P`.
//!
//! # It lists the registry, not a copy of it
//!
//! The candidates come from `Action::ALL` filtered by "does this have a menu entry or a key
//! binding", so a command becomes reachable in the palette the moment it is added to the
//! registry. That is the FR-U8 requirement stated in spec §5.3 -- the palette and the menus
//! cannot drift -- and it is satisfied by construction rather than by a test that would only
//! catch drift after someone had already written the second list.
//!
//! # What is excluded
//!
//! Two groups: pure movement and selection chords (thirty variants that would bury the
//! useful commands and are already bound to keys you are pressing anyway), and internal
//! menu-open actions, which the palette would express better as the menu's own commands.
//! The exclusion list is explicit and tested, because an implicit filter is the first thing
//! to silently swallow a new command.
//!
//! # Ranking
//!
//! `SkimMatcherV2`, the same matcher the language picker uses in S12, so the two filters
//! rank identically and a user who learned one understands the other. Ties break by
//! original `Action::ALL` order, which keeps the palette stable while you type.

use crate::actions::Action;
use crate::app::App;
use crate::highlight::fuzzy_filter;

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    /// The command.
    pub action: Action,
    /// `Action::label()`, what the row reads.
    pub label: String,
    /// Current binding, right-aligned in the row.
    pub hint: String,
}

/// Commands deliberately absent from the palette.
///
/// Movement and selection chords: reachable by keys that are already under the hands, and
/// listing thirty of them makes the palette slower to scan than the menu it replaces.
const HIDDEN_FROM_PALETTE: &[Action] = &[
    Action::MoveLeft,
    Action::MoveRight,
    Action::MoveUp,
    Action::MoveDown,
    Action::MoveLeftSelect,
    Action::MoveRightSelect,
    Action::MoveUpSelect,
    Action::MoveDownSelect,
    Action::WordLeft,
    Action::WordRight,
    Action::WordLeftSelect,
    Action::WordRightSelect,
    Action::LineStart,
    Action::LineEnd,
    Action::LineStartSelect,
    Action::LineEndSelect,
    Action::DocStart,
    Action::DocEnd,
    Action::ScrollPageUp,
    Action::ScrollPageDown,
    Action::Nop,
];

/// Every palette candidate, in `Action::ALL` order.
///
/// Public so the test can assert that no *useful* action is missing, and so the help
/// overlay can reuse the same visibility rule.
pub fn palette_actions() -> Vec<Action> {
    Action::ALL
        .iter()
        .copied()
        .filter(|a| !HIDDEN_FROM_PALETTE.contains(a))
        .collect()
}

/// The entries for `query`, best match first.
///
/// An empty query lists everything, which is the documented behavior: opening the palette
/// and reading it is a legitimate way to discover what the editor can do.
pub fn palette_entries(app: &App, query: &str) -> Vec<PaletteEntry> {
    let actions = palette_actions();
    let labels: Vec<String> = actions.iter().map(|a| a.label().to_string()).collect();
    let order = fuzzy_filter(&labels, query);
    order
        .into_iter()
        .map(|i| PaletteEntry {
            action: actions[i],
            label: labels[i].clone(),
            hint: app
                .keymap
                .keys_for(actions[i])
                .first()
                .cloned()
                .unwrap_or_default(),
        })
        .collect()
}

/// The command at the palette's current selection, or `None` when the list is empty.
pub fn selected(app: &App, entries: &[PaletteEntry]) -> Option<Action> {
    entries.get(app.palette_selected).map(|e| e.action)
}

/// Draw the palette (FR-U8).
pub fn draw_palette(app: &App, buf: &mut ratatui::buffer::Buffer) {
    use crate::ui::framework::{dim_frame, draw_field, draw_list};
    dim_frame(buf, app);
    let pal = app.palette();
    let entries = palette_entries(app, &app.palette_query);
    let layout = palette_layout(app);
    layout.draw_box(buf, "Commands", &pal);
    // The query field, echoing what has been typed.
    let field = crate::ui::text_input::TextInput::new(app.palette_query.clone());
    // The palette's field always owns the keyboard, so it always draws focused.
    draw_field(buf, layout.row(0), Some(">"), &field, true, false, &pal);
    let list = list_area(&layout);
    let rows: Vec<String> = entries
        .iter()
        .map(|e| {
            if e.hint.is_empty() {
                e.label.clone()
            } else {
                // Right-align the chord within the row, so a column of bindings lines up.
                let pad = list
                    .w
                    .saturating_sub(
                        e.label.chars().count() as u16 + e.hint.chars().count() as u16 + 1,
                    )
                    .max(1);
                format!("{}{}{}", e.label, " ".repeat(pad as usize), e.hint)
            }
        })
        .collect();
    draw_list(buf, list, &rows, app.palette_selected, true, &pal);
}

/// The palette's box, computed the same way the renderer computes it (FR-U8).
///
/// Shared so a click can be resolved against the rows the user actually saw. Any change to
/// the palette's size has to go through here or the hit-test drifts.
pub fn palette_layout(app: &App) -> crate::ui::framework::DialogLayout {
    let entries = palette_entries(app, &app.palette_query);
    let screen_h = app.regions.status.bottom();
    let screen_w = app.regions.menu.w;
    let wanted = entries.len().saturating_add(3) as u16;
    let h = wanted.min(screen_h.saturating_sub(2)).max(5);
    let w = 52u16.min(screen_w.saturating_sub(2).max(10));
    let area = np_core::RectCore::new(
        screen_w.saturating_sub(w) / 2,
        screen_h.saturating_sub(h) / 2,
        w,
        h,
    );
    crate::ui::framework::DialogLayout::centered(area, w, h)
}

/// The list region inside the palette's box.
pub fn list_area(layout: &crate::ui::framework::DialogLayout) -> np_core::RectCore {
    np_core::RectCore::new(
        layout.inner.x,
        layout.inner.y.saturating_add(1),
        layout.inner.w,
        layout.inner.bottom().saturating_sub(layout.inner.y + 1),
    )
}

/// Which palette row a pointer is over, in *entry* index space.
pub fn row_at(
    app: &App,
    layout: &crate::ui::framework::DialogLayout,
    x: u16,
    y: u16,
) -> Option<usize> {
    let list = list_area(layout);
    if !list.contains(x, y) {
        return None;
    }
    let entries = palette_entries(app, &app.palette_query);
    let visible = list.h as usize;
    if visible == 0 {
        return None;
    }
    let start = crate::ui::framework::list_window(entries.len(), app.palette_selected, visible);
    let index = start + (y - list.y) as usize;
    if index < entries.len() {
        Some(index)
    } else {
        None
    }
}
