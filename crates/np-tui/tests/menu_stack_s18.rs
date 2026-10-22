//! S18: `Esc` walked back through every menu the user had visited before closing.
//!
//! Reported as: "in menu when I visit one submenu after another submenu and then press esc menu
//! return to previous visited submenu until the first visited and only then closes."
//!
//! # The mechanism
//!
//! `open_menu` pushed a `Mode::Menu` onto the mode stack, guarded only by an equality test
//! against `Mode::Menu { open: menu, item: 0 }`. Opening the *same* menu again was therefore a
//! no-op, but opening a *different* one failed the test and pushed a second level. Clicking File,
//! Edit, Search, View stacked four menu levels, and one `Esc` per level walked View, Search, Edit,
//! File before the bar finally closed.
//!
//! A menu bar is a peer set, not a hierarchy. One level holds it and `Esc` closes it in one press.
//! Submenus are the only genuine nesting in the bar -- Encoding's Convert To / Reload As, File's
//! Recent Files -- and they still push, so `Esc` peels the submenu first and the menu second. That
//! two-level behaviour is what the bar actually has, and it is what these tests pin from both
//! sides: switching must not accumulate, nesting must still nest.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::actions::{Action, MenuId};
use np_tui::app::{App, Mode};

const W: u16 = 120;
const H: u16 = 30;

fn app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    // Geometry only exists after a draw, and the menu bar's hit targets come from it.
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn open_menu(app: &mut App, menu: MenuId) {
    app.execute(Action::MenuBar(menu));
}

/// The open menu, or `None` once the bar is closed.
fn open(app: &App) -> Option<MenuId> {
    match app.mode() {
        Mode::Menu { open, .. } => Some(open),
        _ => None,
    }
}

/// Rows of the menu bar are row 0; click the title of `menu` by measuring its label.
fn click_menu_title(app: &mut App, menu: MenuId) {
    let label = menu.title();
    // Titles start at column 1 with a two-space gap; find the cell that resolves to this menu.
    for x in 0..W {
        let ev = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        if let np_tui::mouse::Hit::MenuBar(Some((m, _))) = np_tui::mouse::hit_test(&ev, app) {
            if m == menu {
                for kind in [
                    crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                ] {
                    app.handle_mouse(&crossterm::event::MouseEvent {
                        kind,
                        column: x,
                        row: 0,
                        modifiers: KeyModifiers::NONE,
                    });
                }
                return;
            }
        }
    }
    panic!("no clickable title found for {label}");
}

#[test]
fn visiting_menus_in_sequence_leaves_one_menu_level_on_the_stack() {
    // The reported setup: explore the bar by opening one menu after another.
    let mut app = app();
    for menu in [MenuId::File, MenuId::Edit, MenuId::Search, MenuId::View] {
        click_menu_title(&mut app, menu);
        assert_eq!(
            open(&app),
            Some(menu),
            "the clicked menu must be the open one"
        );
        assert_eq!(
            app.mode_stack.len(),
            2,
            "one menu level plus Editing, however many menus were visited (after {menu:?})",
        );
    }
}

#[test]
fn one_press_of_escape_closes_the_bar_after_visiting_several_menus() {
    let mut app = app();
    for menu in [MenuId::File, MenuId::Edit, MenuId::Search, MenuId::View] {
        click_menu_title(&mut app, menu);
    }
    key(&mut app, KeyCode::Esc);
    assert_eq!(
        open(&app),
        None,
        "the bar must close in one press, not walk back through File, Edit, Search, View",
    );
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn keyboard_menu_switching_also_leaves_one_level() {
    // Left/Right cycle the bar, and typing another menu's accelerator switches it too. Both go
    // through the same level, so neither may accumulate.
    let mut app = app();
    open_menu(&mut app, MenuId::File);
    for _ in 0..4 {
        key(&mut app, KeyCode::Right);
        assert_eq!(app.mode_stack.len(), 2, "Right must not stack a menu level");
    }
    for _ in 0..4 {
        key(&mut app, KeyCode::Left);
        assert_eq!(app.mode_stack.len(), 2, "Left must not stack a menu level");
    }
}

#[test]
fn a_submenu_still_nests_and_escape_peels_it_first() {
    // The other half of the contract. File > Recent Files is a real nested list, and `Esc` closes
    // the submenu, then the bar -- two presses, two levels. Collapsing everything to one level
    // would break this.
    let mut app = app();
    for i in 0..3 {
        app.recent
            .push(std::path::PathBuf::from(format!("/tmp/recent-{i}.txt")));
    }
    click_menu_title(&mut app, MenuId::File);
    let items = np_tui::ui::menus::menu_items(&app, MenuId::File);
    let recent = items
        .iter()
        .position(|it| it.label.contains("Recent"))
        .expect("Recent Files is in the File menu");
    for _ in 0..recent {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode_stack.len(),
        3,
        "a submenu is a genuine nested level",
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(
        open(&app),
        Some(MenuId::File),
        "the first Esc peels the submenu back to the menu",
    );
    assert_eq!(app.mode_stack.len(), 2);
    key(&mut app, KeyCode::Esc);
    assert_eq!(open(&app), None, "the second Esc closes the bar");
}

#[test]
fn switching_top_level_menu_while_a_submenu_is_open_drops_the_submenu() {
    // Opening another menu is a fresh start, not a deeper level: the open submenu must not survive
    // underneath it, or the next Esc reveals a menu the user already left.
    let mut app = app();
    for i in 0..3 {
        app.recent
            .push(std::path::PathBuf::from(format!("/tmp/recent-{i}.txt")));
    }
    click_menu_title(&mut app, MenuId::File);
    let items = np_tui::ui::menus::menu_items(&app, MenuId::File);
    let recent = items
        .iter()
        .position(|it| it.label.contains("Recent"))
        .expect("Recent Files");
    for _ in 0..recent {
        key(&mut app, KeyCode::Down);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode_stack.len(), 3, "submenu open");
    click_menu_title(&mut app, MenuId::Edit);
    assert_eq!(open(&app), Some(MenuId::Edit));
    assert_eq!(
        app.mode_stack.len(),
        2,
        "the File submenu must not survive under the newly opened Edit menu",
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(open(&app), None, "so one Esc closes the bar");
}

#[test]
fn opening_the_same_menu_twice_is_a_no_op() {
    let mut app = app();
    click_menu_title(&mut app, MenuId::File);
    click_menu_title(&mut app, MenuId::File);
    assert_eq!(app.mode_stack.len(), 2);
    assert_eq!(open(&app), Some(MenuId::File));
}
