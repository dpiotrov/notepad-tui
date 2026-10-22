//! Menu bar: the nine titles from FR-U1, and the dropdown that opens under one of them.
//!
//! The title row is unchanged from S6: the spans come from `mouse::menu_bar_spans`, which is
//! also what hit-tests clicks on the row, and sharing that computation is the only way to
//! guarantee a click lands on the title the user saw. S14 adds [`draw_dropdown`], which
//! renders a resolved [`crate::ui::menus::MenuItem`] list -- labels, checkmarks, disabled
//! rows, submenu arrows and right-aligned key hints -- and nothing else draws a menu.

use ratatui::buffer::Buffer;
use ratatui::style::Style;

use np_core::RectCore;
use ratatui::style::Modifier as _Mod;

use crate::actions::{MenuId, SubmenuId};
use crate::app::{App, Mode};
use crate::ui::menus::{MenuItem, MenuItemKind};

/// Draw the menu titles into `App::regions.menu`.
pub fn draw(app: &App, buf: &mut Buffer) {
    let area = rect(app.regions.menu);
    crate::ui::framework::draw_chrome_band(buf, app.regions.menu, &app.highlight.palette());
    // A leading space keeps the first title off the screen corner, matching how desktop
    // menu bars read.
    let open = match app.mode() {
        Mode::Menu { open, .. } => Some(open),
        _ => None,
    };
    let palette = app.highlight.palette();
    // The spans come from `mouse::menu_bar_spans`, which is also what hit-tests clicks on
    // this row. Sharing the computation is the only way to guarantee a click lands on the
    // title the user actually sees.
    for (_menu_id, _i, col, width) in crate::mouse::menu_bar_spans(app.regions.menu) {
        let title = _menu_id.title();
        let hovered = matches!(app.hover, Some(crate::app::HoverTarget::Menu(i)) if i == _i);
        // A menu reads as open when it is the open one, or when the bar is armed and the
        // pointer is over this title -- the "hovering switches open menus" rule of FR-U1.
        let active = open == Some(_menu_id)
            || (app.menu_bar_active && open.is_none())
            || (open.is_some() && hovered);
        // The active title swaps the band's two colors rather than using `Modifier::REVERSED`.
        // REVERSED asks the terminal to swap *its* defaults, which on a light terminal produces
        // light-on-dark text on a dark band -- readable by accident, and wrong on any theme where
        // the band is not the terminal's background. Swapping the palette's own pair gives the
        // same visual emphasis and is correct everywhere.
        let style = if active {
            crate::style::ink(palette.chrome_bg, palette.chrome_fg)
        } else {
            crate::style::chrome(&palette)
        };
        debug_assert_eq!(width as usize, title.chars().count());
        draw_str(buf, col, area.y, title, style);
    }
}

/// Write text into the buffer, clipping at the right edge.
pub(crate) fn draw_str(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let mut col = x;
    for ch in text.chars() {
        let Some(cell) = buf.cell_mut(ratatui::layout::Position { x: col, y }) else {
            break;
        };
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
        col = col.saturating_add(1);
    }
    col
}

/// `RectCore` to a ratatui `Rect`.
pub(crate) fn rect(r: np_core::RectCore) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.w,
        height: r.h,
    }
}

/// The bar's own height, which is one row everywhere.
const BAR_H: u16 = 1;

/// Draw the open menu's dropdown (FR-U1).
///
/// Called from `ui::draw` after the base frame so the box covers the editor. The item list
/// comes from the registry, already resolved against `App`, so a checkmark here is the same
/// boolean the action will read when it runs.
pub fn draw_dropdown(app: &App, buf: &mut Buffer) {
    let Mode::Menu { open, item, parent } = app.mode() else {
        return;
    };
    let screen_w = app.regions.menu.w;
    let screen_h = app.regions.status.bottom();
    match parent {
        // A submenu is open. Its parent's dropdown is still on screen and still has to be
        // drawn -- it was popped off the mode stack by nothing, so the level below is live --
        // and then the child box beside the row it came from.
        Some(submenu) => {
            let highlighted = parent_row(app, open, submenu).unwrap_or(item);
            draw_top_level(app, buf, open, highlighted, screen_w, screen_h);
            draw_submenu(app, buf, submenu, item, screen_w, screen_h);
        }
        None => draw_top_level(app, buf, open, item, screen_w, screen_h),
    }
}

/// Which top-level row a submenu label came from, so the parent box can keep it highlighted.
///
/// Falls back to no highlight rather than guessing at row 0: a wrong highlight in the parent
/// reads as "this row is selected" while the child box shows something else entirely.
fn parent_row(app: &App, menu: MenuId, submenu: SubmenuId) -> Option<usize> {
    crate::ui::menus::menu_items(app, menu)
        .iter()
        .position(|it| it.kind == MenuItemKind::Submenu(menu) && it.label == submenu.label())
}

/// The dropdown for one of the nine menu titles, under its title in the bar.
fn draw_top_level(
    app: &App,
    buf: &mut Buffer,
    menu: MenuId,
    item: usize,
    screen_w: u16,
    screen_h: u16,
) {
    let items = crate::ui::menus::menu_items(app, menu);
    if items.is_empty() {
        return;
    }
    let box_w = crate::ui::menus::dropdown_width(&items)
        .min(screen_w.max(1))
        .max(3);
    let box_h = crate::ui::menus::dropdown_height(items.len(), screen_h);
    let (title_x, title_w) = title_span(app.regions.menu, menu);
    let area = crate::ui::menus::dropdown_rect(
        RectCore::new(0, app.regions.menu.y, screen_w, BAR_H),
        title_x,
        title_w,
        box_w,
        box_h,
        screen_w,
    );
    let layout = crate::ui::framework::DialogLayout::centered(area, box_w, box_h);
    // A dropdown is not modal over a backdrop: the point of a menu is that you can still see
    // the text you are about to act on, so no dimming here (unlike a dialog).
    layout.draw_box(buf, menu.title(), &app.palette());
    draw_rows(buf, &layout, &items, item, &app.palette());
}

/// A submenu box, hanging to the right of the parent row and flipped left when it would run
/// off the screen edge -- the same clamping rule the top-level box uses.
fn draw_submenu(
    app: &App,
    buf: &mut Buffer,
    submenu: SubmenuId,
    item: usize,
    screen_w: u16,
    screen_h: u16,
) {
    let menu = submenu.parent();
    let items = crate::ui::menus::level_items(app, menu, Some(submenu));
    if items.is_empty() {
        return;
    }
    let Some(parent_area) = crate::mouse::top_dropdown_rect(app, menu) else {
        return;
    };
    // Nudge the box down to the row the arrow actually sits on, then let the shared rect
    // function do the width, height and left-flip.
    let offset = parent_row(app, menu, submenu).unwrap_or(0) as u16;
    let anchored = RectCore {
        y: parent_area.y.saturating_add(offset),
        ..parent_area
    };
    let area = crate::ui::menus::submenu_rect(anchored, &items, screen_w, screen_h);
    if area.h < 3 {
        return;
    }
    let layout = crate::ui::framework::DialogLayout::centered(area, area.w, area.h);
    layout.draw_box(buf, submenu.label(), &app.palette());
    draw_rows(buf, &layout, &items, item, &app.palette());
}

/// Paint a resolved item list into a laid-out box, honouring the scroll window.
fn draw_rows(
    buf: &mut Buffer,
    layout: &crate::ui::framework::DialogLayout,
    items: &[MenuItem],
    item: usize,
    palette: &np_core::ThemePalette,
) {
    let visible = layout.inner.h as usize;
    let start = crate::ui::framework::list_window(items.len(), item, visible);
    // Two `enumerate()`s: the outer index is the on-screen row, the inner one the item's
    // position in the full list. Conflating them is how a scrolled menu highlights the wrong
    // row.
    for (row, (i, it)) in items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let line = layout.row(row as u16);
        draw_item(buf, line, it, i == item, palette);
    }
}

/// One dropdown row: check column, label, submenu arrow, right-aligned hint.
fn draw_item(
    buf: &mut Buffer,
    line: RectCore,
    it: &MenuItem,
    selected: bool,
    palette: &np_core::ThemePalette,
) {
    if it.kind == MenuItemKind::Separator {
        // A separator is a rule across the interior, not a blank row: without the glyph the
        // grouping it exists to create is invisible.
        let rule: String = "\u{2500}".repeat(line.w as usize);
        draw_str(
            buf,
            line.x,
            line.y,
            &rule,
            crate::ui::framework::hint_style(palette),
        );
        return;
    }
    let check = if it.checked { "\u{2713} " } else { "  " };
    let label = match it.kind {
        MenuItemKind::Submenu(_) => format!("{} \u{25b8}", it.label),
        _ => it.label.clone(),
    };
    let text = format!("{check}{label}");
    // A dropdown is a dialog box, so it wears dialog colors. The selected row takes the
    // theme's highlight pair rather than `Modifier::REVERSED`, which on a light terminal
    // reversed into a pair nobody chose and frequently could not read.
    let style = if !it.enabled {
        crate::ui::framework::hint_style(palette)
    } else if selected {
        crate::style::highlight(palette).add_modifier(_Mod::BOLD)
    } else {
        crate::style::dialog(palette)
    };
    // The selection bar spans the full interior width, so a highlighted row reads as a row.
    let padded = crate::ui::framework::fit(&text, line.w as usize);
    draw_str(buf, line.x, line.y, &padded, style);
    if !it.hint.is_empty() && !is_encoding_index_hint(it) {
        let hint = format!(" {}", it.hint);
        let hw = hint.chars().count() as u16;
        if hw + 1 < line.w {
            let hx = line.right().saturating_sub(hw + 1);
            let chord = if !it.enabled {
                crate::ui::framework::hint_style(palette)
            } else if selected {
                // The chord sits on the highlight bar, so its ink is derived from that ground.
                let ink = np_core::dim_ink(
                    palette.highlight_bg,
                    palette.highlight_fg,
                    np_core::MIN_UI_CONTRAST,
                );
                crate::style::ink(ink, palette.highlight_bg)
            } else {
                crate::ui::framework::hint_style(palette)
            };
            draw_str(buf, hx, line.y, &hint, chord);
        }
    }
}

/// The Encoding lists stash a codec index in `hint` for the action payload. It is data, not
/// a key chord, so it must not be rendered as one.
fn is_encoding_index_hint(it: &MenuItem) -> bool {
    matches!(
        it.action,
        Some(crate::actions::Action::ConvertEncoding)
            | Some(crate::actions::Action::ReloadAsEncoding)
    )
}

/// `(x, width)` of one menu title in the bar, from the same spans the hit-test walks.
pub fn title_span(menu: RectCore, which: MenuId) -> (u16, u16) {
    for (id, _, col, width) in crate::mouse::menu_bar_spans(menu) {
        if id == which {
            return (col, width);
        }
    }
    (menu.x, 0)
}

/// Which menu title the pointer is over, or `None`. Shares `menu_bar_spans` with the draw.
pub fn title_at(menu: RectCore, x: u16) -> Option<MenuId> {
    crate::mouse::menu_bar_spans(menu)
        .into_iter()
        .find(|(_, _, col, width)| x >= *col && x < col.saturating_add(*width))
        .map(|(id, _, _, _)| id)
}
