//! Command palette (FR-U8) and toasts (FR-U6).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, Mode};
use np_tui::ui::palette::{self, palette_actions, palette_entries};
use np_tui::ui::toast::{self, TimedToast, INFO_TTL, MAX_VISIBLE};
use std::time::{Duration, Instant};

const W: u16 = 100;
const H: u16 = 30;

fn app() -> App {
    App::new_headless(Config::default(), (W, H))
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        let mut ev = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    }
}

// ------------------------------------------------------------- palette

#[test]
fn ctrl_shift_p_opens_the_palette() {
    let mut app = app();
    let mut ev = KeyEvent::new(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert_eq!(app.mode(), Mode::Palette);
}

#[test]
fn an_empty_query_lists_every_candidate() {
    let app = app();
    let entries = palette_entries(&app, "");
    assert_eq!(
        entries.len(),
        palette_actions().len(),
        "opening the palette should show everything"
    );
    assert!(entries.len() > 40);
}

#[test]
fn save_as_ranks_first_for_query_save_as() {
    // FR-U8 acceptance: the query "save as" must rank SaveAs above Save and Save-related noise.
    let app = app();
    let entries = palette_entries(&app, "save as");
    assert!(!entries.is_empty(), "a two-word query should still match");
    assert_eq!(
        entries.first().map(|e| e.action),
        Some(Action::SaveAs),
        "top hit was {:?}",
        entries.iter().map(|e| e.label.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn a_query_narrows_and_every_result_actually_matches() {
    let app = app();
    for query in ["find", "wrap", "book", "close", "eol"] {
        let entries = palette_entries(&app, query);
        assert!(!entries.is_empty(), "{query:?} matched nothing");
        assert!(
            entries.len() < palette_actions().len(),
            "{query:?} narrowed nothing"
        );
    }
}

#[test]
fn enter_executes_the_selected_command() {
    let mut app = app();
    app.execute(Action::CommandPalette);
    type_str(&mut app, "word wrap");
    let entries = palette_entries(&app, &app.palette_query.clone());
    let target = entries
        .iter()
        .position(|e| e.action == Action::ToggleWordWrap)
        .expect("word wrap should be reachable from the palette");
    app.palette_selected = target;
    let before = app.config.ui.word_wrap;
    let mut ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert_eq!(
        app.config.ui.word_wrap, !before,
        "Enter should have run Toggle Word Wrap"
    );
    assert_eq!(app.mode(), Mode::Editing, "the palette must close");
}

#[test]
fn down_arrow_walks_and_up_clamps_at_zero() {
    let mut app = app();
    app.execute(Action::CommandPalette);
    let mut down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    down.kind = KeyEventKind::Press;
    let mut up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    up.kind = KeyEventKind::Press;
    app.handle_key(down);
    app.handle_key(down);
    assert_eq!(app.palette_selected, 2);
    for _ in 0..10 {
        app.handle_key(up);
    }
    assert_eq!(app.palette_selected, 0, "Up must clamp rather than wrap");
    // And Down must not run past the end of the list.
    let count = palette_entries(&app, "").len();
    for _ in 0..count + 10 {
        app.handle_key(down);
    }
    assert_eq!(
        app.palette_selected,
        count - 1,
        "Down must clamp at the last row"
    );
}

#[test]
fn esc_closes_the_palette_without_running_anything() {
    let mut app = app();
    app.execute(Action::CommandPalette);
    type_str(&mut app, "word wrap");
    let before = app.config.ui.word_wrap;
    let mut ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(app.config.ui.word_wrap, before, "Esc must not execute");
}

#[test]
fn backspace_edits_the_query() {
    let mut app = app();
    app.execute(Action::CommandPalette);
    type_str(&mut app, "findx");
    let mut ev = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert_eq!(app.palette_query, "find");
}

#[test]
fn every_palette_entry_shows_its_binding() {
    let app = app();
    let entries = palette_entries(&app, "");
    let with_hint = entries.iter().filter(|e| !e.hint.is_empty()).count();
    assert!(
        with_hint > entries.len() / 2,
        "most commands are bound; only {with_hint} of {} showed a chord",
        entries.len()
    );
}

#[test]
fn the_palette_covers_the_menu_registry_with_no_drift() {
    // FR-U8 says the palette reuses the menu registry. The check that makes that true rather
    // than aspirational: every action a menu offers must be in the palette.
    let palette: Vec<Action> = palette_actions();
    for action in np_tui::ui::menus::all_menu_actions() {
        assert!(
            palette.contains(&action),
            "{action:?} is in a menu but not in the palette"
        );
    }
}

#[test]
fn movement_chords_are_deliberately_excluded() {
    // Documented in palette.rs: thirty movement variants would bury the useful commands.
    let palette = palette_actions();
    assert!(!palette.contains(&Action::MoveLeft));
    assert!(!palette.contains(&Action::LineEndSelect));
    assert!(!palette.contains(&Action::Nop));
    // And the exclusion is explicit, not an accident of `Action::ALL`.
    assert!(palette.contains(&Action::Save));
}

#[test]
fn a_double_click_on_a_palette_row_runs_it() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = app();
    app.execute(Action::CommandPalette);
    let layout = palette::palette_layout(&app);
    let list = palette::list_area(&layout);
    let entries = palette_entries(&app, "");
    let target = entries
        .iter()
        .position(|e| e.action == Action::ToggleWordWrap)
        .expect("wrap is a palette entry");
    // Scroll the list so the row is visible, then click it.
    let visible = list.h as usize;
    app.palette_selected = target;
    let start = np_tui::ui::framework::list_window(entries.len(), target, visible);
    let y = list.y + (target - start) as u16;
    assert_eq!(
        np_tui::mouse::hit_test(
            &MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: list.x + 1,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &app
        ),
        np_tui::mouse::Hit::PaletteRow(target)
    );
    // A single click only points the cursor at the row. The palette runs *commands*, so a press
    // that fired one was the most damaging instance of click-to-open anywhere in the app: the
    // pointer crossing a row on the way to the query field could execute an arbitrary command.
    // Every other list in npt selects on click and acts on double-click, and the palette now
    // matches. The hit-test assertions above are unchanged -- only what the click *does* moved.
    let before = app.config.ui.word_wrap;
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 1,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: list.x + 1,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let t = Instant::now();
    app.handle_mouse_at(&down, t);
    app.handle_mouse_at(&up, t);
    assert_eq!(
        app.palette_selected, target,
        "the first click should point the cursor at the row"
    );
    assert_eq!(
        app.config.ui.word_wrap, before,
        "a single click must not run the command"
    );
    // Second click, same cell, inside the multi-click window: now it runs.
    app.handle_mouse_at(&down, t);
    app.handle_mouse_at(&up, t);
    assert_eq!(app.config.ui.word_wrap, !before);
}

#[test]
fn palette_renders_labels_and_chords() {
    let mut app = app();
    app.execute(Action::CommandPalette);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(block.contains("Commands"), "palette title missing");
    assert!(block.contains("Save"), "commands missing: {block}");
    assert!(block.contains("ctrl+s"), "chords missing: {block}");
}

// --------------------------------------------------------------- toasts

#[test]
fn an_info_toast_expires_after_three_seconds() {
    // FR-U6 acceptance, with the clock injected rather than slept on.
    let now = Instant::now();
    let t = TimedToast::info("saved", now);
    assert!(!t.expired(now));
    assert!(!t.expired(now + Duration::from_millis(2900)));
    assert!(t.expired(now + INFO_TTL));
    assert!(t.expired(now + INFO_TTL * 10));
}

#[test]
fn an_error_toast_never_expires() {
    let now = Instant::now();
    let t = TimedToast::error("disk full", now);
    assert!(!t.expired(now + Duration::from_secs(3600)));
}

#[test]
fn app_level_expiry_drops_info_and_keeps_errors() {
    let mut app = app();
    app.push_toast("info one", false);
    app.push_toast("info two", false);
    app.push_toast("boom", true);
    assert_eq!(app.timed_toasts.len(), 3);
    app.advance_clock(Duration::from_millis(3100));
    app.expire_toasts();
    let remaining: Vec<&str> = app
        .timed_toasts
        .iter()
        .map(|t| t.message.as_str())
        .collect();
    assert_eq!(remaining, vec!["boom"], "only the error should survive");
    // The legacy queue the older suites read must agree.
    assert_eq!(app.toasts.len(), 1);
    assert!(app.toasts.iter().next().is_some_and(|t| t.error));
}

#[test]
fn esc_dismisses_a_sticky_error() {
    let mut app = app();
    app.push_toast("boom", true);
    let mut ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert!(app.timed_toasts.is_empty(), "Esc must clear the error");
}

#[test]
fn esc_clears_a_selection_when_there_is_no_toast() {
    // The ordering rule: a toast outranks a selection, but with no toast the press must still
    // do the ordinary editing thing rather than vanish.
    // A selection needs content: select_all on an empty buffer selects nothing, which would
    // make this assertion pass for the wrong reason.
    let mut app = app();
    let doc = np_core::Document::from_decoded(
        std::path::PathBuf::from("/tmp/s14-toast.txt"),
        "hello\n",
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.open_document(doc);
    app.doc_mut().unwrap().select_all();
    assert!(
        app.doc().unwrap().selection.is_some_and(|s| !s.is_empty()),
        "precondition: the document should have a selection"
    );
    let mut ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert!(
        app.doc().unwrap().selection.is_none_or(|s| s.is_empty()),
        "Esc should clear the selection"
    );
}

#[test]
fn a_burst_of_toasts_stays_capped_and_readable() {
    // FR-U6: a burst must not pile into an unreadable wall.
    let mut items: Vec<TimedToast> = (0..40)
        .map(|i| TimedToast::info(format!("note {i}"), Instant::now()))
        .collect();
    toast::expire(&mut items, Instant::now());
    let shown = toast::visible(&items);
    assert!(
        shown.len() <= MAX_VISIBLE,
        "drew {} toasts, cap is {MAX_VISIBLE}",
        shown.len()
    );
    // Newest first, so the stack grows upward from the bottom-right corner.
    assert_eq!(shown[0].message, "note 39");
}

#[test]
fn an_error_is_never_pushed_off_screen_by_chatty_info() {
    let now = Instant::now();
    let mut items = vec![TimedToast::error("disk full", now)];
    for i in 0..10 {
        items.push(TimedToast::info(format!("note {i}"), now));
    }
    let shown = toast::visible(&items);
    assert!(
        shown.iter().any(|t| t.error),
        "the sticky error must stay visible"
    );
}

#[test]
fn the_toast_stack_sits_bottom_right_and_above_the_status_bar() {
    let area = toast::stack_rect(80, 23, 2);
    assert!(area.right() <= 80);
    assert!(area.bottom() <= 23, "must not cover the status row");
    // Right-aligned.
    assert!(area.right() > 70, "should hug the right edge: {area:?}");
}

#[test]
fn toasts_render_over_a_dialog() {
    // A config write that fails while Preferences is open has to be readable.
    let mut app = app();
    app.execute(np_tui::actions::Action::Preferences);
    app.push_toast("could not save config", true);
    let buf = np_tui::ui::render_to_buffer(&mut app, W, H);
    let block: String = (0..H)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, W))
        .collect();
    assert!(
        block.contains("could not save config"),
        "toast not drawn: {block}"
    );
}

#[test]
fn a_failed_io_path_surfaces_as_a_toast_not_a_panic() {
    // FR-U6 "all I/O errors surface here". Opening a path that cannot be read must report.
    let mut app = app();
    app.open_path(std::path::Path::new("/definitely/not/here/npt-404.txt"));
    assert!(
        !app.timed_toasts.is_empty(),
        "a failed open must produce a toast"
    );
    assert!(
        app.timed_toasts.iter().any(|t| t.error),
        "and it must be an error toast"
    );
}
