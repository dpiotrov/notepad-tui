//! The dialog framework primitives: TextInput, focus cycling, layout (FR-U4).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use np_core::Config;
use np_core::RectCore;
use np_tui::app::{App, Mode};
use np_tui::ui::framework::{
    button_row, default_index, dim_region, focus_at, focus_cycle, list_window, DialogLayout,
    FocusId, FocusItem,
};
use np_tui::ui::text_input::TextInput;

// ------------------------------------------------------------- TextInput

#[test]
fn textinput_inserts_and_reads_back() {
    let mut f = TextInput::empty();
    f.insert_char('a');
    f.insert_char('b');
    assert_eq!(f.text(), "ab");
    assert_eq!(f.caret(), 2);
    assert!(f.selection().is_none());
}

#[test]
fn textinput_inserts_a_multichar_string() {
    let mut f = TextInput::new("xy");
    f.set_caret(1, false);
    f.insert("--");
    assert_eq!(f.text(), "x--y");
    assert_eq!(f.caret(), 3);
}

#[test]
fn textinput_backspace_and_delete() {
    let mut f = TextInput::new("abc");
    assert_eq!(f.backspace(), np_tui::ui::text_input::FieldKey::Changed);
    assert_eq!(f.text(), "ab");
    f.set_caret(0, false);
    f.delete_forward();
    assert_eq!(f.text(), "b");
    // Backspace at the start changes nothing rather than wrapping.
    let untouched = f.backspace();
    assert_eq!(untouched, np_tui::ui::text_input::FieldKey::Untouched);
    assert_eq!(f.text(), "b");
}

#[test]
fn textinput_arrows_home_end() {
    let mut f = TextInput::new("hello");
    f.home();
    assert_eq!(f.caret(), 0);
    f.end();
    assert_eq!(f.caret(), 5);
    f.move_left();
    assert_eq!(f.caret(), 4);
    f.move_right();
    assert_eq!(f.caret(), 5);
    // Moving right at the end is inert.
    assert_eq!(f.move_right(), np_tui::ui::text_input::FieldKey::Untouched);
}

#[test]
fn textinput_selection_spans_and_text() {
    let mut f = TextInput::new("hello world");
    f.set_caret(0, false);
    f.set_caret(5, true);
    assert_eq!(f.selection(), Some((0, 5)));
    assert_eq!(f.selected_text().as_deref(), Some("hello"));
    // Typing replaces the selection.
    f.insert_char('X');
    assert_eq!(f.text(), "X world");
    assert_eq!(f.selection(), None);
}

#[test]
fn textinput_arrow_collapses_selection_before_moving() {
    // Every real text field: Left with a selection collapses to the selection's left edge
    // rather than stepping one further.
    let mut f = TextInput::new("abcdef");
    f.set_caret(1, false);
    f.set_caret(4, true);
    f.move_left();
    assert_eq!(f.caret(), 1);
    assert_eq!(f.selection(), None);
    // Now a selection from 1 to 2: Right collapses to the right edge, 2.
    f.set_caret(2, true);
    f.move_right();
    assert_eq!(f.caret(), 2);
}

#[test]
fn textinput_word_navigation_and_ctrl_backspace() {
    let mut f = TextInput::new("one two three");
    f.end();
    f.word_left();
    assert_eq!(f.caret(), 8, "should land at the start of 'three'");
    f.word_left();
    assert_eq!(f.caret(), 4);
    // Ctrl+Right lands on the *next word's start*, not the current word's end.
    f.word_right();
    assert_eq!(f.caret(), 8, "from 'two' should land on 'three'");
    f.set_caret(0, false);
    f.word_right();
    assert_eq!(f.caret(), 4, "from 'one' should land on 'two'");
    f.end();
    f.delete_word_back();
    assert_eq!(f.text(), "one two ", "Ctrl+Backspace removes 'three'");
}

#[test]
fn textinput_deletes_whole_grapheme_clusters() {
    // The reason this type exists. `e` + U+0301 is one visible character made of two scalar
    // values; `String::pop` would remove the accent and leave a bare `e` glued to whatever
    // came next.
    let mut f = TextInput::new("re\u{0301}sume");
    let clusters = f.len();
    assert_eq!(clusters, 6, "six visible characters");
    f.end();
    f.backspace(); // 'e'
    f.backspace(); // 'm'
    f.backspace(); // 'u'
    f.backspace(); // the combined 'e' + accent as ONE delete, not two
                   // Six clusters, four deletes, two remain. The point is what did NOT happen: a char-wise
                   // pop would have removed the accent first and left a bare 'e' plus 'r', i.e. "ree".
    assert_eq!(f.text(), "re\u{301}");
    assert_eq!(f.len(), 2);
    // One more delete takes the whole accented cluster, not just its base letter.
    f.backspace();
    assert_eq!(f.text(), "r");
    // An emoji ZWJ sequence is a single cluster too.
    let mut g = TextInput::new("a\u{1F468}\u{200D}\u{1F469}");
    assert_eq!(g.len(), 2);
    g.backspace();
    assert_eq!(g.text(), "a");
}

#[test]
fn textinput_wide_characters_count_as_two_cells() {
    // CJK occupies two cells; the caret column must be in display cells or the caret is drawn
    // one cell left of where the user thinks it is.
    let mut f = TextInput::new("\u{4F60}\u{597D}x");
    f.set_caret(1, false);
    assert_eq!(f.caret_display_col(20), 2);
    f.set_caret(2, false);
    assert_eq!(
        f.caret_display_col(20),
        4,
        "two full-width chars occupy four cells"
    );
}

#[test]
fn textinput_window_keeps_the_caret_visible() {
    let mut f = TextInput::new("0123456789abcdefghij");
    f.end();
    f.scroll_into_view(8);
    let (start, end) = f.window(8);
    assert!(end - start <= 8, "window must fit the field");
    // `end` is exclusive and a caret at `total` sits after the last cluster, so the caret may
    // legitimately equal `end`.
    assert!(
        start <= f.caret() && f.caret() <= end,
        "caret {} outside window {}..{}",
        f.caret(),
        start,
        end
    );
    // Scrolling back to the front works too.
    f.home();
    f.scroll_into_view(8);
    assert_eq!(f.window(8), (0, 8));
}

#[test]
fn textinput_word_boundaries_survive_wide_text() {
    // A field that mixes CJK and ASCII must never split a cluster, and Ctrl+Left must land on
    // a boundary the slice functions agree with.
    let mut f = TextInput::new("\u{4F60}\u{597D} world");
    f.end();
    f.delete_word_back();
    assert_eq!(f.text(), "\u{4F60}\u{597D} ");
}

// ------------------------------------------------------------ focus model

#[test]
fn focus_cycle_wraps_at_both_ends() {
    // FR-U4 acceptance: Tab forward wraps, Shift+Tab backward wraps.
    assert_eq!(focus_cycle(0, 1, 3), 1);
    assert_eq!(focus_cycle(2, 1, 3), 0, "forward wraps past the last");
    assert_eq!(focus_cycle(0, -1, 3), 2, "backward wraps past the first");
    assert_eq!(focus_cycle(1, -1, 3), 0);
    // Multi-step deltas.
    assert_eq!(focus_cycle(0, 4, 3), 1);
    assert_eq!(focus_cycle(0, -4, 3), 2);
}

#[test]
fn focus_cycle_handles_degenerate_counts() {
    assert_eq!(focus_cycle(0, 1, 0), 0, "no children must not panic");
    // A stale index is clamped into range first, so 5 becomes 2 and the step wraps to 0.
    assert_eq!(focus_cycle(5, 1, 3), 0);
    assert_eq!(focus_cycle(0, 0, 1), 0);
}

#[test]
fn focus_at_finds_the_child_under_the_pointer() {
    let items = vec![
        FocusItem::new(FocusId::Field(0), RectCore::new(2, 2, 20, 1)),
        FocusItem::new(FocusId::Button(0), RectCore::new(2, 4, 8, 1)),
    ];
    assert_eq!(focus_at(&items, 5, 2), Some(0));
    assert_eq!(focus_at(&items, 5, 4), Some(1));
    assert_eq!(focus_at(&items, 5, 3), None);
    assert_eq!(focus_at(&items, 30, 2), None);
}

#[test]
fn default_index_prefers_the_flagged_child() {
    let mut items = vec![
        FocusItem::new(FocusId::Field(0), RectCore::new(0, 0, 4, 1)),
        FocusItem::new(FocusId::Button(0), RectCore::new(0, 1, 4, 1)).default_action(),
    ];
    assert_eq!(default_index(&items), 1);
    items[1].is_default = false;
    assert_eq!(default_index(&items), 0, "falls back to the first");
    assert_eq!(default_index(&[]), 0);
}

// ---------------------------------------------------------------- layout

#[test]
fn dialog_layout_centers_and_never_exceeds_the_screen() {
    let screen = RectCore::new(0, 0, 80, 24);
    let layout = DialogLayout::centered(screen, 40, 10);
    assert_eq!(layout.area.x, 20);
    assert_eq!(layout.area.y, 7);
    assert_eq!(layout.inner.w, 38);
    assert_eq!(layout.inner.h, 8);
    // A box bigger than the screen shrinks instead of overflowing.
    let big = DialogLayout::centered(screen, 200, 200);
    assert!(big.area.right() <= screen.right());
    assert!(big.area.bottom() <= screen.bottom());
    // A one-column screen still produces a box rather than a zero or negative rect.
    let tiny = DialogLayout::centered(RectCore::new(0, 0, 1, 1), 40, 10);
    assert!(tiny.area.w >= 1 && tiny.area.h >= 1);
}

#[test]
fn layout_rows_are_interior_relative() {
    let layout = DialogLayout::centered(RectCore::new(0, 0, 40, 12), 20, 6);
    // Row 0 is the first interior row, one below the top border.
    assert_eq!(layout.row(0).y, layout.area.y + 1);
    assert_eq!(layout.row(2).y, layout.area.y + 3);
    assert_eq!(layout.row(0).w, layout.inner.w);
}

#[test]
fn button_row_lays_out_left_to_right_without_overlap() {
    let row = RectCore::new(2, 5, 40, 1);
    let rects = button_row(row, &["OK", "Cancel", "Help"]);
    assert_eq!(rects.len(), 3);
    for (area, _) in &rects {
        assert!(area.w >= 4, "a button needs brackets and a label");
    }
    // No overlaps, and all inside the row.
    for i in 1..rects.len() {
        assert!(rects[i].0.x > rects[i - 1].0.right());
    }
    assert!(rects.last().unwrap().0.right() <= row.right());
}

#[test]
fn button_row_stops_when_the_row_runs_out() {
    let rects = button_row(RectCore::new(0, 0, 6, 1), &["Very Long Label", "Another"]);
    assert_eq!(rects.len(), 1, "the second button has nowhere to go");
}

#[test]
fn list_window_keeps_the_selection_visible() {
    assert_eq!(list_window(3, 1, 10), 0, "short list needs no scroll");
    assert_eq!(list_window(100, 0, 10), 0);
    assert_eq!(
        list_window(100, 50, 10),
        41,
        "selection lands on the last visible row"
    );
    assert_eq!(list_window(100, 99, 10), 90);
    assert_eq!(list_window(0, 0, 10), 0);
}

#[test]
fn dim_region_marks_every_cell_without_erasing_symbols() {
    use ratatui::style::Modifier;
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 6,
        height: 2,
    });
    for (i, ch) in "hello".chars().enumerate() {
        buf.cell_mut(ratatui::layout::Position { x: i as u16, y: 0 })
            .expect("cell exists")
            .set_symbol(&ch.to_string());
    }
    let pal = np_core::ThemePalette::neutral();
    dim_region(&mut buf, RectCore::new(0, 0, 6, 2), &pal);
    let cell = buf
        .cell(ratatui::layout::Position { x: 1, y: 0 })
        .expect("cell exists");
    assert_eq!(cell.symbol(), "e", "dimming must not replace the glyph");
    assert!(
        cell.style().add_modifier.contains(Modifier::DIM),
        "the cell should be dimmed"
    );
}

// ------------------------------------------------- no keystroke leaks

#[test]
fn a_dialog_eats_every_printable_character() {
    // The single worst failure mode of a dialog: a character reaching the document. Every
    // dialog mode is checked over a sample of keys.
    let probes: Vec<KeyCode> = vec![
        KeyCode::Char('a'),
        KeyCode::Char('Z'),
        KeyCode::Char(' '),
        KeyCode::Char('/'),
        KeyCode::Char('~'),
        KeyCode::Enter,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::F(5),
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
    ];
    let openers = [
        np_tui::actions::Action::OpenFile,
        np_tui::actions::Action::SaveAs,
        np_tui::actions::Action::Find,
        np_tui::actions::Action::Replace,
        np_tui::actions::Action::GoToLine,
        np_tui::actions::Action::Preferences,
        np_tui::actions::Action::About,
        np_tui::actions::Action::LanguagePicker,
        np_tui::actions::Action::ThemePicker,
        np_tui::actions::Action::DocumentList,
        np_tui::actions::Action::CommandPalette,
        np_tui::actions::Action::HelpOverlay,
        np_tui::actions::Action::MenuBar(np_tui::actions::MenuId::Edit),
        np_tui::actions::Action::EncodingMenu,
        np_tui::actions::Action::EolMenu,
        np_tui::actions::Action::RecentFiles,
    ];
    for opener in openers {
        let mut app = App::new_headless(Config::default(), (100, 30));
        app.execute(opener);
        let text_before = app.doc().map(|d| d.buffer.text_string());
        for code in &probes {
            // Once the chrome has closed, the editor legitimately takes input again; the
            // invariant is only about keys pressed *while* the dialog was up.
            if app.mode() == Mode::Editing {
                break;
            }
            let mut ev = KeyEvent::new(*code, KeyModifiers::NONE);
            ev.kind = KeyEventKind::Press;
            app.handle_key(ev);
            let now = app.doc().map(|d| d.buffer.text_string());
            assert_eq!(
                text_before, now,
                "key {code:?} leaked into the document while {opener:?} was open"
            );
        }
    }
}

#[test]
fn esc_pops_exactly_one_level_with_a_stacked_dialog() {
    // FR-U4 acceptance: a confirm over a save-as must pop only the confirm.
    let mut app = App::new_headless(Config::default(), (100, 30));
    app.execute(np_tui::actions::Action::SaveAs);
    let depth_after_save_as = app.mode_stack.len();
    assert_eq!(app.mode(), Mode::Dialog(np_tui::app::DialogKind::SaveAs));
    // Stack a confirm on top, the way a close prompt would.
    app.confirm = Some(np_tui::tabs::ConfirmRequest::save_changes(
        "x",
        np_tui::tabs::PendingOp::Discard,
        Some(0),
    ));
    app.push_mode(Mode::Dialog(np_tui::app::DialogKind::Confirm));
    assert_eq!(app.mode_stack.len(), depth_after_save_as + 1);

    let mut ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
    assert_eq!(
        app.mode(),
        Mode::Dialog(np_tui::app::DialogKind::SaveAs),
        "Esc must reveal the save-as, not close both"
    );
    app.handle_key(ev);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn esc_at_the_bottom_of_the_stack_never_empties_it() {
    let mut app = App::new_headless(Config::default(), (100, 30));
    for _ in 0..5 {
        let mut ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        app.handle_key(ev);
    }
    assert_eq!(app.mode(), Mode::Editing);
    assert!(!app.mode_stack.is_empty());
}
