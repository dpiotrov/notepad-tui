//! S19: submenu rows drew an arrow and did nothing.
//!
//! Reported as: "submenus contains some items with arrow, that do nothing or doesnt react to
//! selection -- what it is?"
//!
//! # The mechanism
//!
//! A submenu shares its parent's `MenuId`. `Mode::Menu` carried only `{ open, item }`, so
//! "Encoding open" and "Encoding's Convert To open" were literally the same value. Three
//! consumers resolved the item list from that value alone:
//!
//! * the painter resolved the parent's list for both levels and drew the same box twice on the
//!   same rows, so the child box never appeared;
//! * the keyboard router moved a cursor through the *parent's* rows while the user looked at a
//!   box that was not there;
//! * the mouse hit-test measured the parent's rectangle, so a click inside the child box, once
//!   it was drawn, selected an unrelated parent row.
//!
//! A fourth bug hid behind the first: `decorate` stamped the live key chord over the codec index
//! that `submenu_items` parks in `hint`, and `activate_menu_item` parses that index back to pick
//! the encoding. Convert To / Reload As closed the menu and reported success while leaving the
//! document's encoding untouched.
//!
//! `Mode::Menu` now carries `parent: Option<SubmenuId>`, and `menus::level_items` is the one
//! place a level's rows are resolved, so all three consumers read the same list.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::{Action, MenuId, SubmenuId};
use np_tui::app::{App, Mode};
use np_tui::ui::menus;

const W: u16 = 120;
const H: u16 = 30;

fn app() -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn doc_app() -> App {
    let mut app = app();
    app.open_document(Document::from_decoded(
        "/tmp/sub.txt".into(),
        "hello world\n",
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

/// Text of one rendered screen row.
fn row(app: &mut App, y: u16) -> String {
    let buf = np_tui::ui::render_to_buffer(app, W, H);
    (0..W)
        .map(|c| buf[(c, y)].symbol().chars().next().unwrap_or(' '))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Everything the screen shows to the right of the parent dropdown.
fn child_column_text(app: &mut App, from_x: u16) -> String {
    let mut out = String::new();
    for y in 0..H {
        let line = row(app, y);
        let chars: Vec<char> = line.chars().collect();
        if chars.len() > from_x as usize {
            out.extend(&chars[from_x as usize..]);
        }
    }
    out
}

fn parent_right_edge(app: &App) -> u16 {
    np_tui::mouse::top_dropdown_rect(app, MenuId::Encoding)
        .map(|r| r.right())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ drawing

#[test]
fn opening_a_submenu_draws_a_second_box_the_parent_does_not_cover() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    let edge = parent_right_edge(&app);
    let before = child_column_text(&mut app, edge);
    key(&mut app, KeyCode::Enter);

    let child = child_column_text(&mut app, edge);
    assert!(
        !child.is_empty(),
        "the Convert To box must be drawn beside the parent; parent side reads {before:?}"
    );
    assert!(
        child.contains("UTF-8"),
        "the child box must show the codec list, got {child:?}"
    );
    assert!(
        child.contains("ISO-8859-1"),
        "the whole codec list belongs in the child box, got {child:?}"
    );
}

#[test]
fn the_parent_stays_visible_and_keeps_its_submenu_row_highlighted() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Enter);
    let parent = np_tui::mouse::top_dropdown_rect(&app, MenuId::Encoding)
        .expect("the parent dropdown is still open");
    let text = row(&mut app, parent.y + 1);
    assert!(
        text.contains("Convert to"),
        "the parent dropdown must still be drawn, got {text:?}"
    );
}

// ----------------------------------------------------------------- keyboard

#[test]
fn enter_on_a_submenu_row_runs_that_codec() {
    let mut app = doc_app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Enter); // descend into Convert To
    key(&mut app, KeyCode::Down); // UTF-8 BOM
    key(&mut app, KeyCode::Enter);

    assert_eq!(
        app.doc().map(|d| d.encoding.label()),
        Some("UTF-8 BOM"),
        "Enter on a codec row must convert the document"
    );
    assert_eq!(app.mode(), Mode::Editing, "the bar closes once a leaf ran");
}

#[test]
fn down_inside_a_submenu_walks_the_child_rows_not_the_parents() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Enter);
    let start = app.mode();
    key(&mut app, KeyCode::Down);
    let after = app.mode();

    assert!(
        matches!(after, Mode::Menu {
            parent: Some(SubmenuId::EncodingConvertTo),
            item: 1,
            ..
        }),
        "Down must move inside the open submenu, not drop back to the parent: {start:?} -> {after:?}"
    );
}

#[test]
fn left_closes_the_submenu_and_right_opens_it() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Right); // open the submenu under the cursor
    assert!(
        matches!(
            app.mode(),
            Mode::Menu {
                parent: Some(SubmenuId::EncodingConvertTo),
                ..
            }
        ),
        "Right on a row with an arrow must open that submenu, got {:?}",
        app.mode()
    );
    key(&mut app, KeyCode::Left); // close it, stay in Encoding
    assert_eq!(
        app.mode(),
        Mode::Menu {
            open: MenuId::Encoding,
            item: 0,
            parent: None,
        },
        "Left inside a submenu closes the submenu and leaves the parent open"
    );
}

#[test]
fn escape_peels_the_submenu_then_the_menu() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.mode(), Mode::Menu { .. }),
        "one Esc closes only the submenu"
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing, "the second Esc closes the bar");
}

// ------------------------------------------------------------------- mouse

#[test]
fn a_click_in_the_submenu_selects_the_row_drawn_there() {
    let mut app = doc_app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    key(&mut app, KeyCode::Enter);

    let area = np_tui::mouse::active_dropdown_rect(&app).expect("the submenu has a rect");
    // Row 3 of the child interior is UTF-16BE (index 3).
    let y = area.y + 1 + 3;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x + 2,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(&click);
    assert_eq!(
        app.doc().map(|d| d.encoding.label()),
        Some("UTF-16BE"),
        "clicking a codec row must apply that codec, not one from the parent list"
    );
}

#[test]
fn the_active_rect_is_the_child_box_while_a_submenu_is_open() {
    let mut app = app();
    app.execute(Action::MenuBar(MenuId::Encoding));
    let parent = np_tui::mouse::active_dropdown_rect(&app).expect("parent rect");
    key(&mut app, KeyCode::Enter);
    let child = np_tui::mouse::active_dropdown_rect(&app).expect("child rect");

    assert_ne!(
        parent, child,
        "the active rect must follow the visible level"
    );
    assert!(
        child.x >= parent.right() || child.right() <= parent.x,
        "the child box must sit beside the parent, not on top of it: {parent:?} {child:?}"
    );
}

// -------------------------------------------------------------- registry

#[test]
fn every_submenu_label_resolves_to_a_submenu_id() {
    // A row drawn with an arrow must be openable: that is the whole complaint. Walking the
    // registry and asking `SubmenuId::find` about each arrow row catches a new submenu added
    // to `menu_layout` without an id, which would otherwise reproduce the dead arrow.
    let app = app();
    for menu in MenuId::ALL {
        for item in menus::menu_items(&app, menu) {
            if item.kind == menus::MenuItemKind::Submenu(menu) {
                assert!(
                    SubmenuId::find(menu, &item.label).is_some(),
                    "{menu:?} row {:?} draws an arrow but has no SubmenuId",
                    item.label
                );
                let children = menus::submenu_items(&app, menu, &item.label);
                assert!(
                    !children.is_empty(),
                    "{menu:?} row {:?} draws an arrow over an empty list",
                    item.label
                );
            }
        }
    }
}

#[test]
fn a_recent_files_row_opens_that_file_not_the_recent_list() {
    let dir = std::env::temp_dir().join("npt-s19-recent");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("alpha.txt");
    std::fs::write(&path, "alpha contents\n").expect("write temp file");

    let mut app = app();
    app.recent.push(path.clone());
    app.execute(Action::MenuBar(MenuId::File));
    app.set_menu(MenuId::File, 2); // the Recent Files row
    key(&mut app, KeyCode::Enter);

    assert!(
        matches!(
            app.mode(),
            Mode::Menu {
                parent: Some(SubmenuId::RecentFiles),
                ..
            }
        ),
        "Recent Files must open as a submenu, got {:?}",
        app.mode()
    );
    key(&mut app, KeyCode::Enter); // the one file

    assert_eq!(app.mode(), Mode::Editing, "the bar closes");
    assert!(
        !matches!(app.mode(), Mode::Dialog(_)),
        "choosing a recent file must not open the recent-files dialog again"
    );
    let opened = app.doc().and_then(|d| d.path.clone());
    assert_eq!(
        opened.as_deref(),
        Some(path.as_path()),
        "the chosen file must open"
    );
}

#[test]
fn a_recent_files_row_advertises_no_chord_it_cannot_honour() {
    // `RecentFiles` has a key binding, but that binding opens the picker -- it cannot mean
    // "this row". Showing it on a path implied a per-file shortcut that does not exist.
    let mut app = app();
    app.recent.push(std::path::PathBuf::from("/tmp/seeded.txt"));
    let items = menus::level_items(&app, MenuId::File, Some(SubmenuId::RecentFiles));
    assert_eq!(items.len(), 1, "the seeded entry must resolve");
    for item in items {
        assert!(
            item.hint.is_empty(),
            "recent row {:?} must not show chord {:?}",
            item.label,
            item.hint
        );
    }
}

#[test]
fn a_codec_rows_hint_stays_its_index_after_decoration() {
    // The chord is what `decorate` wants to write; the index is what `activate_menu_item` needs
    // to read. This pins the ordering rule that keeps both.
    let app = app();
    let items = menus::level_items(&app, MenuId::Encoding, Some(SubmenuId::EncodingConvertTo));
    let all = np_core::EncodingId::all();
    for (i, item) in items.iter().enumerate() {
        assert_eq!(
            item.hint,
            i.to_string(),
            "{:?} must carry its codec index, not a chord",
            item.label
        );
        assert_eq!(all[i].label(), item.label, "the index must match the row");
    }
}

// ------------------------------------------------- chrome band (S19 item 3)
//
// Separate concern from submenus, but the same complaint surface: strips that read as text.
// Kept here so one file covers "the chrome looks like chrome".

/// The background of one cell.
fn bg(app: &mut App, x: u16, y: u16) -> Option<ratatui::style::Color> {
    np_tui::ui::render_to_buffer(app, W, H)[(x, y)].style().bg
}

#[test]
fn chrome_strips_are_a_different_ground_from_the_text_area() {
    // Asserted relationally rather than against a `CHROME_BG` constant. The band's color is now
    // derived from the active theme, so a hardcoded RGB would have to be recomputed per theme and
    // would silently pass the moment the derivation changed. What the user actually needs is the
    // relationship -- the strips share one ground and the buffer has a different one -- and that
    // holds for every theme.
    let mut app = doc_app();
    app.execute(Action::FindInFiles);
    let pal = app.palette();
    let want = ratatui::style::Color::Rgb(pal.chrome_bg.0, pal.chrome_bg.1, pal.chrome_bg.2);
    let text_y = app.regions.editor.y + 1;
    let text_bg = bg(&mut app, 40, text_y);
    for region in [app.regions.menu, app.regions.tabs] {
        assert_eq!(
            bg(&mut app, 40, region.y),
            Some(want),
            "the strip at y={} must read as chrome, not as buffer text (text is {text_bg:?})",
            region.y
        );
    }
    let panel = np_tui::ui::find_in_files::panel_rect(&app);
    assert_eq!(
        bg(&mut app, 40, panel.y),
        Some(want),
        "the service lines must not read as lines of the file"
    );
    assert_ne!(
        text_bg,
        Some(want),
        "and the text area must keep its own ground"
    );
}

#[test]
fn the_active_tab_is_still_distinguishable_from_its_band() {
    // The band is a distinct ground, which is exactly what would have made the old
    // REVERSED-active-tab invisible: reversed out of grey it came back looking like the band.
    let mut app = doc_app();
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let span = np_tui::ui::tab_bar::spans(&app)
        .into_iter()
        .find(|s| s.index == app.active)
        .expect("the active tab has a span");
    let active_cell = &buf[(span.x, app.regions.tabs.y)];
    let band_cell = &buf[(
        app.regions.tabs.right().saturating_sub(1),
        app.regions.tabs.y,
    )];
    assert!(
        active_cell
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "the active tab must carry a modifier the bare band does not"
    );
    assert!(
        !band_cell
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "empty tab-bar space must stay bare band"
    );
}
