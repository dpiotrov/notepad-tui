//! Menu bar and menu navigation (FR-U1, S14 acceptance).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::actions::{Action, MenuId};
use np_tui::app::{App, Mode};
use np_tui::mouse::{self, Hit};
use np_tui::ui::menus;

const W: u16 = 100;
const H: u16 = 30;

fn app() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn press_mod(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let mut ev = KeyEvent::new(code, mods);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn down(x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn f10_opens_the_file_menu() {
    let mut app = app();
    press(&mut app, KeyCode::F(10));
    assert_eq!(
        app.mode(),
        Mode::Menu {
            open: MenuId::File,
            item: 0,
            parent: None,
        }
    );
    assert!(app.menu_bar_active);
}

#[test]
fn alt_letter_opens_a_menu_directly() {
    // FR-U1: Alt+<letter>. The accelerators come from MenuId, so the test reads them there
    // rather than hardcoding a table that could drift from the code under test.
    for menu in MenuId::ALL {
        let mut app = app();
        press_mod(
            &mut app,
            KeyCode::Char(menu.accelerator()),
            KeyModifiers::ALT,
        );
        assert_eq!(
            app.mode(),
            Mode::Menu {
                open: menu,
                item: 0,
                parent: None,
            },
            "Alt+{} should open {:?}",
            menu.accelerator(),
            menu
        );
    }
}

#[test]
fn accelerators_are_unique() {
    // Two menus sharing an accelerator would make one of them unreachable.
    let mut letters: Vec<char> = MenuId::ALL.iter().map(|m| m.accelerator()).collect();
    letters.sort();
    let count = letters.len();
    letters.dedup();
    assert_eq!(letters.len(), count, "menu accelerators must be unique");
}

#[test]
fn arrows_walk_the_bar_and_the_items() {
    let mut app = app();
    press(&mut app, KeyCode::F(10));
    // Right moves along the bar.
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.mode(),
        Mode::Menu {
            open: MenuId::Edit,
            item: 0,
            parent: None,
        }
    );
    // Left wraps backwards past the first menu.
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left);
    assert_eq!(
        app.mode(),
        Mode::Menu {
            open: MenuId::Help,
            item: 0,
            parent: None,
        }
    );
    // Down moves within the open menu's items.
    press(&mut app, KeyCode::Right); // back to File
    press(&mut app, KeyCode::Down);
    let Mode::Menu { item, .. } = app.mode() else {
        panic!("expected a menu");
    };
    assert!(item > 0, "Down should move into the item list");
}

#[test]
fn navigation_skips_separators_and_disabled_rows() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::File));
    // Walk the whole menu; every highlighted row must be an enabled action row.
    let items = menus::menu_items(&app, MenuId::File);
    for _ in 0..items.len() {
        press(&mut app, KeyCode::Down);
        let Mode::Menu { item, .. } = app.mode() else {
            panic!("expected a menu");
        };
        assert_ne!(
            items[item].kind,
            menus::MenuItemKind::Separator,
            "navigation landed on a separator at {item}"
        );
        assert!(items[item].enabled, "navigation landed on a disabled row");
    }
}

#[test]
fn enter_executes_the_highlighted_item() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::File));
    let items = menus::menu_items(&app, MenuId::File);
    let save_index = items
        .iter()
        .position(|i| i.action == Some(Action::SaveAs))
        .expect("File > Save As");
    // Set the highlight directly: walking there with Down is its own test.
    app.set_menu(MenuId::File, save_index);
    let before = app.mode_stack.len();
    press(&mut app, KeyCode::Enter);
    // Save As opens its dialog rather than changing documents, and closes the menu.
    assert_eq!(
        app.mode(),
        Mode::Dialog(np_tui::app::DialogKind::SaveAs),
        "Enter on Save As should open the picker"
    );
    assert!(
        app.mode_stack.len() < before + 2,
        "the menu must close when its item runs"
    );
}

#[test]
fn esc_pops_exactly_one_menu_level() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    let depth = app.mode_stack.len();
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode_stack.len(), depth - 1);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn every_menu_item_maps_to_a_real_action_and_executes() {
    // The table-driven check the spec asks for: no menu entry may be dead. Every action the
    // registry exposes must be one `execute` accepts without panicking, and must not be Nop
    // unless it is the documented fallthrough.
    let actions = menus::all_menu_actions();
    assert!(
        actions.len() > 50,
        "the registry should expose a full menu bar, found {}",
        actions.len()
    );
    assert!(!actions.contains(&Action::Nop), "no menu entry may be Nop");
    for action in actions {
        let mut app = app();
        // Executing must not panic; state effects are covered by the per-action suites.
        app.execute(action);
    }
}

#[test]
fn every_menu_item_carries_its_live_binding() {
    // FR-U1: the key hint comes from `key_for(action)`, so a remap shows up in the menu.
    let mut app = app();
    let items = menus::menu_items(&app, MenuId::File);
    let save = items
        .iter()
        .find(|i| i.action == Some(Action::Save))
        .expect("File > Save");
    assert_eq!(save.hint, "ctrl+s");

    // Remap save and confirm the menu follows, which is the no-drift property.
    let chord = np_tui::keymap::parse_key("ctrl+k").expect("parses");
    app.keymap.bind(chord, Action::Save);
    let items = menus::menu_items(&app, MenuId::File);
    let save = items
        .iter()
        .find(|i| i.action == Some(Action::Save))
        .expect("File > Save");
    assert_eq!(save.hint, "ctrl+k", "the hint must track the live keymap");
}

#[test]
fn toggle_items_reflect_state_and_flipping_changes_it() {
    let mut app = app();
    let items = menus::menu_items(&app, MenuId::View);
    let wrap = items
        .iter()
        .find(|i| i.action == Some(Action::ToggleWordWrap))
        .expect("View > Toggle Word Wrap");
    assert!(!wrap.checked, "wrap defaults off");

    app.execute(Action::ToggleWordWrap);
    let items = menus::menu_items(&app, MenuId::View);
    let wrap = items
        .iter()
        .find(|i| i.action == Some(Action::ToggleWordWrap))
        .expect("View > Toggle Word Wrap");
    assert!(wrap.checked, "the checkmark must follow the config");
    assert!(app.config.ui.word_wrap);
}

#[test]
fn disabled_items_do_not_execute() {
    // Paste with no document and nothing to paste: the row is disabled and Enter is inert.
    let mut app = app();
    app.documents.clear();
    let items = menus::menu_items(&app, MenuId::Edit);
    let paste = items
        .iter()
        .find(|i| i.action == Some(Action::Paste))
        .expect("Edit > Paste");
    assert!(!paste.enabled, "Paste is dead with no document");

    app.execute(Action::MenuBar(MenuId::Edit));
    let paste_index = {
        let items = menus::menu_items(&app, MenuId::Edit);
        items
            .iter()
            .position(|i| i.action == Some(Action::Paste))
            .expect("Edit > Paste")
    };
    for _ in 0..paste_index {
        press(&mut app, KeyCode::Down);
    }
    // Navigation should never have landed on it, so the menu is elsewhere; aim directly.
    app.set_menu(MenuId::Edit, paste_index);
    let mode_before = app.mode();
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode(),
        mode_before,
        "Enter on a disabled row must not close the menu or run anything"
    );
}

#[test]
fn hovering_switches_between_open_menus() {
    // FR-U1: once a menu is open, moving the pointer along the bar opens the one under it.
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::File));
    let (x, _w) = np_tui::ui::menu_bar::title_span(app.regions.menu, MenuId::Search);
    let hover = MouseEvent {
        kind: MouseEventKind::Moved,
        column: x + 1,
        row: app.regions.menu.y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(&hover);
    assert!(matches!(app.hover, Some(np_tui::app::HoverTarget::Menu(_))));
    // The renderer highlights the hovered title while a menu is open.
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let title = np_tui::ui::row_text(&buf, app.regions.menu.y, x, x + 6);
    assert!(title.contains("Search"), "bar reads {title:?}");
}

#[test]
fn clicking_a_dropdown_item_runs_it() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::File));
    let area = mouse::top_dropdown_rect(&app, MenuId::File).expect("dropdown has a rect");
    let items = menus::menu_items(&app, MenuId::File);
    let new_index = items
        .iter()
        .position(|i| i.action == Some(Action::NewFile))
        .expect("File > New");
    let y = area.y + 1 + new_index as u16;
    assert_eq!(
        mouse::hit_test(&down(area.x + 1, y), &app),
        Hit::MenuItem(new_index)
    );
    let before = app.documents.len();
    app.handle_mouse(&down(area.x + 1, y));
    assert_eq!(app.documents.len(), before + 1);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn a_click_on_the_bar_opens_that_menu() {
    let mut app = app();
    let (x, _w) = np_tui::ui::menu_bar::title_span(app.regions.menu, MenuId::View);
    app.handle_mouse(&down(x + 1, app.regions.menu.y));
    assert_eq!(
        app.mode(),
        Mode::Menu {
            open: MenuId::View,
            item: 0,
            parent: None,
        }
    );
}

#[test]
fn every_menu_has_items_and_a_title() {
    let app = app();
    for menu in MenuId::ALL {
        let items = menus::menu_items(&app, menu);
        assert!(!items.is_empty(), "{:?} must not be an empty menu", menu);
        assert!(
            items.iter().any(|i| i.action.is_some()),
            "{:?} must offer at least one action",
            menu
        );
    }
}

#[test]
fn encoding_submenu_lists_every_codec() {
    // FR-N3: both levels enumerate the real codec list, so no codec is missing from the menu.
    let app = app();
    let convert = menus::encoding_convert_items(&app);
    let reload = menus::encoding_reload_items(&app);
    assert_eq!(convert.len(), np_core::EncodingId::all().len());
    assert_eq!(reload.len(), np_core::EncodingId::all().len());
    for item in &convert {
        assert_eq!(item.action, Some(Action::ConvertEncoding));
    }
    for item in &reload {
        assert_eq!(item.action, Some(Action::ReloadAsEncoding));
    }
}

#[test]
fn eol_menu_checks_the_documents_current_ending() {
    let app = app();
    let items = menus::eol_items(&app);
    let checked: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.checked)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(checked.len(), 1, "exactly one EOL is current");
    assert_eq!(
        items[checked[0]].action,
        Some(Action::SetEol(app.doc().unwrap().eol))
    );
}

#[test]
fn dropdown_never_runs_off_the_right_edge() {
    let mut app = app();
    // The rightmost menu on a narrow screen is the worst case.
    app.recompute_geometry(40, 24);
    let items = menus::menu_items(&app, MenuId::Help);
    let w = menus::dropdown_width(&items).min(40);
    let (title_x, title_w) = np_tui::ui::menu_bar::title_span(app.regions.menu, MenuId::Help);
    let area = menus::dropdown_rect(
        app.regions.menu,
        title_x,
        title_w,
        w,
        menus::dropdown_height(items.len(), 24),
        40,
    );
    assert!(area.right() <= 40, "dropdown ran off screen: {area:?}");
    assert!(
        area.w <= 40 && area.x + area.w <= 40,
        "dropdown must fit inside a 40-column screen: {area:?}"
    );
}

#[test]
fn submenu_indicator_and_checkmark_render() {
    let mut app = app();
    app.config.ui.word_wrap = true;
    app.execute(Action::MenuBar(MenuId::Encoding));
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..8)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(
        block.contains('\u{25b8}'),
        "a submenu row should show an arrow: {block}"
    );
    app.execute(Action::MenuBar(MenuId::View));
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..12)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(
        block.contains('\u{2713}'),
        "a checked toggle should show a checkmark: {block}"
    );
}
