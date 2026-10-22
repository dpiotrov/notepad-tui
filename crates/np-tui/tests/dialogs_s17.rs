//! S17: the four defects found by using `npt` again.
//!
//! 1. **Scrollbars fight the pointer.** A press near the bottom of the vertical bar threw the
//!    text to the first line and back; a horizontal drag could not reach either end. The bar
//!    was scaled against `content - viewport` while the offsets the scroller accepts are *line
//!    starts*, and the divisor was the track length rather than the last cell's index, so the
//!    final cell fell short of the maximum and the end-nudge ran off the end.
//! 2. **A one-entry directory could not be used.** With only `../` in the listing, the cursor
//!    wrapped out of the list, through the path field and the buttons, and back -- which on a
//!    one-row list looks like it is spinning and makes the only row impossible to stop on.
//! 3. **The Themes dialog had no keyboard.** Its handler moved a stored cursor and never moved
//!    focus, so `Down` from the query field went nowhere and the list was reachable by mouse
//!    alone. Theme was also listed under two menus, which asks the user a question they cannot
//!    answer without trying both.
//! 4. **Every dialog grew its own control code.** Settings had no mouse; Themes had no
//!    keyboard; Find's checkbox toggled on and straight back off. The shared cursor and the
//!    shared routing table are the fix, and the tests below are written against the shared
//!    surface rather than per-dialog behaviour, so a dialog that grows a private opinion about
//!    its cursor fails here.
//!
//! # Why these assert on consequences
//!
//! Same lesson as S15: a stored index is "correct" long after the screen has stopped agreeing
//! with it. So each test drives real key or mouse events and asserts on the resulting document
//! state, mode, or scroll offset -- not on the field that merely remembers a selection.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::actions::{Action, MenuId};
use np_tui::app::{App, DialogKind, Mode};

const W: u16 = 100;
const H: u16 = 30;

fn key(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
    ev.kind = KeyEventKind::Press;
    app.handle_key(ev);
}

fn down(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn drag(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn up(app: &mut App, x: u16, y: u16) {
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
}

fn open(text: &str) -> App {
    let mut app = App::new_headless(Config::default(), (W, H));
    app.open_document(Document::from_decoded(
        "/tmp/s17.txt".into(),
        text,
        EncodingId::Utf8,
        Eol::Lf,
        false,
    ));
    // A headless App has no geometry until something draws it, and every bar rect below is
    // derived from that geometry.
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app
}

fn lines(n: usize) -> String {
    (0..n).map(|i| format!("line {i}\n")).collect()
}

/// Screen-absolute rect of one scrollbar track, taken from the same geometry the bar is drawn from.
fn track(app: &App, vertical: bool) -> (u16, u16, u16) {
    let detail = if vertical {
        app.vertical_scrollbar_detail()
    } else {
        app.horizontal_scrollbar_detail()
    };
    let (bar, _content, _viewport) = detail.expect("the bar under test must be drawn");
    if vertical {
        (
            app.regions.editor.right() - 1,
            bar.track_start,
            bar.track_len,
        )
    } else {
        (
            bar.track_start,
            app.regions.editor.bottom() - 1,
            bar.track_len,
        )
    }
}

/// The scroll offset one bar currently holds, **in that bar's own unit**.
///
/// The two bars speak different units and mixing them is precisely the defect under test, so
/// this helper is explicit about it:
///   * the vertical bar is scaled against *row* offsets, and `scroll.top_line` names a *line*.
///     Under wrap the two diverge -- a wrapped line occupies several rows -- so the vertical
///     reading converts through `row_offset_of_top`, the same function the thumb position uses.
///   * the horizontal bar is scaled against columns, which is what `scroll.left_col` holds.
fn offset(app: &App, vertical: bool) -> usize {
    let doc = app.doc().expect("document");
    if vertical {
        np_tui::app::row_offset_of_top(doc, &app.geometry)
    } else {
        doc.scroll.left_col
    }
}

// ------------------------------------------------- 1. vertical bar: monotone, both ends

#[test]
fn pressing_each_cell_of_the_vertical_bar_walks_the_document_without_jumping() {
    // The reported symptom was "jumps under the pointer near the bottom, text goes to the first
    // line and returns". That signature is a scaling mismatch: cells whose requested offset the
    // scroller refuses, so the view snaps back. Asserting monotone non-decreasing travel across
    // every cell catches it, and catches the off-by-one divisor too (a wrapping last cell is a
    // decrease).
    let mut app = open(&lines(200));
    let (x, start, len) = track(&app, true);
    let mut previous = 0;
    for cell in 0..len {
        down(&mut app, x, start + cell);
        up(&mut app, x, start + cell);
        let now = offset(&app, true);
        assert!(
            now >= previous,
            "cell {cell} of {len} went backwards: {now} < {previous} \
             (a wrapping or snapping bar)"
        );
        previous = now;
    }
}

#[test]
fn the_last_cell_of_the_vertical_bar_reaches_the_last_page() {
    // The end of the bar must be the end of the document. Before the fix the last cell landed
    // one increment short of the maximum and the end-nudge then ran past it, so the very bottom
    // of the trough was dead -- "very difficult to reach the end".
    let mut app = open(&lines(200));
    let max = app.max_viewport_row_offset();
    let (x, start, len) = track(&app, true);
    down(&mut app, x, start + len - 1);
    up(&mut app, x, start + len - 1);
    assert_eq!(
        offset(&app, true),
        max,
        "the bar's last cell must land on the deepest page the painter honours"
    );
}

#[test]
fn the_first_cell_of_the_vertical_bar_returns_to_the_top() {
    let mut app = open(&lines(200));
    let (x, start, _len) = track(&app, true);
    // Leave the top first, otherwise the assertion passes without doing anything.
    {
        let doc = app.doc_mut().expect("document");
        doc.scroll.top_line = 150;
    }
    app.dirty = true;
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    assert_ne!(offset(&app, true), 0);
    down(&mut app, x, start);
    up(&mut app, x, start);
    assert_eq!(
        offset(&app, true),
        0,
        "the bar's first cell is the document's first row"
    );
}

#[test]
fn dragging_the_vertical_bar_from_top_to_bottom_is_monotone_and_reaches_the_end() {
    // A press-then-drag is the gesture the user actually performs. Under the old
    // delta-from-grab model the reachable range was capped by how far the pointer travelled
    // from where it happened to land on the thumb, so a mid-grab could never reach the end no
    // matter how far it was dragged.
    let mut app = open(&lines(200));
    let max = app.max_viewport_row_offset();
    let (x, start, len) = track(&app, true);
    down(&mut app, x, start + len / 2);
    let mut previous = offset(&app, true);
    for cell in (len / 2)..len {
        drag(&mut app, x, start + cell);
        let now = offset(&app, true);
        assert!(
            now >= previous,
            "drag to cell {cell} moved backwards: {now} < {previous}"
        );
        previous = now;
    }
    up(&mut app, x, start + len - 1);
    assert_eq!(
        offset(&app, true),
        max,
        "a drag to the bottom must reach the last page"
    );
}

#[test]
fn dragging_the_vertical_bar_back_to_the_top_returns_to_row_zero() {
    let mut app = open(&lines(200));
    let (x, start, len) = track(&app, true);
    down(&mut app, x, start + len - 1);
    for cell in (0..len).rev() {
        drag(&mut app, x, start + cell);
    }
    up(&mut app, x, start);
    assert_eq!(
        offset(&app, true),
        0,
        "a drag back to the top must land on the first row"
    );
}

#[test]
fn the_vertical_bar_scales_against_reachable_offsets_on_wrapped_text() {
    // The unit bug only shows with wrap on. `scroll.top_line` names a *line*, so the positions
    // the view can take are line starts, and on wrapped text the deepest line start sits well
    // short of `total_rows - viewport_rows`. Scaling against rows produced offsets the scroller
    // refused -- exactly the "text jumps to the first line and returns" report.
    let wrapped = (0..40)
        .map(|i| format!("line {i} {}", "w".repeat(300)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = open(&wrapped);
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let max = app.max_viewport_row_offset();
    let (x, start, len) = track(&app, true);
    let mut previous = 0;
    for cell in 0..len {
        down(&mut app, x, start + cell);
        up(&mut app, x, start + cell);
        let now = offset(&app, true);
        assert!(
            now >= previous,
            "wrapped cell {cell} went backwards: {now} < {previous}"
        );
        assert!(
            now <= max,
            "wrapped cell {cell} asked for {now}, past the reachable {max}"
        );
        previous = now;
    }
    assert_eq!(
        previous, max,
        "the last cell on wrapped text must still reach the last page"
    );
}

#[test]
fn the_vertical_bar_is_scaled_against_the_pages_the_painter_will_show() {
    // The unit the bar is scaled against is the root cause behind "the bar jumps and the text
    // jumps to the first line and returns", and it is only observable under wrap.
    //
    // Three numbers look interchangeable and are not:
    //   * the document's row count,
    //   * row count minus viewport height -- how far there appears to be to scroll, and
    //   * the row offset of the deepest page the painter will actually show.
    // A wrapped page shows whole lines only, so it is usually *shorter* than the viewport, and
    // the third number is larger than the second. Scaling against either of the first two makes
    // the bottom of the trough resolve to a page that stops short of the document's end, and
    // because every request is clamped there, the last rows become unreachable by any gesture.
    let long = (0..60)
        .map(|i| format!("line {i} {}", "w".repeat(400)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = open(&long);
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (_bar, _content, viewport) = app
        .vertical_scrollbar_detail()
        .expect("a wrapped document taller than the viewport needs a bar");
    let max = app.max_viewport_row_offset();
    let doc = app.doc().unwrap();
    let rows = np_core::total_row_count(&doc.buffer, &app.geometry, &doc.folds);
    // The premise: the naive ceiling is *not* the reachable one here, so an assertion against the
    // reachable one can actually fail.
    assert!(
        max > rows.saturating_sub(viewport),
        "premise: the deepest paintable page ({max}) should sit past rows-minus-viewport ({})",
        rows.saturating_sub(viewport),
    );

    // The bar's last cell must land on a page that shows the document's final line.
    let x = app.regions.editor.right() - 1;
    down(&mut app, x, _bar.track_start + _bar.track_len - 1);
    up(&mut app, x, _bar.track_start + _bar.track_len - 1);
    let doc = app.doc().unwrap();
    let painted =
        np_core::layout_visible_rows(&doc.buffer, &app.geometry, doc.scroll.top_line, &doc.folds);
    assert_eq!(
        painted.last().map(|r| r.line),
        Some(doc.buffer.line_count() - 1),
        "the bar's last cell must show the last line; top_line={} stops at {:?}",
        doc.scroll.top_line,
        painted.last().map(|r| r.line),
    );

    // No cell may put the view past the ceiling.
    for cell in [
        0u16,
        1,
        _bar.track_len / 2,
        _bar.track_len - 2,
        _bar.track_len - 1,
    ] {
        down(&mut app, x, _bar.track_start + cell);
        up(&mut app, x, _bar.track_start + cell);
        let now = offset(&app, true);
        assert!(
            now <= max,
            "cell {cell} put the view at row offset {now}, past the ceiling {max}",
        );
    }
}

#[test]
fn going_to_the_end_of_a_wrapped_document_shows_the_last_line() {
    // The "go to the end" path is a separate intent from an ordinary aim, and it is where the
    // bar's unit and the scroll unit collide: the bar is scaled in *row* offsets while
    // `scroll.top_line` names a *line*. Applying a row offset straight to `top_line` fails
    // quietly on a long wrapped file -- it scrolls to somewhere near the middle and the thumb
    // still reads "bottom" -- so the assertion has to be about what ends up on screen.
    //
    // Written as "the document's last row is inside the painted window" rather than as an
    // expected line number, because a wrapped page is allowed to be short at the bottom: a
    // window starting at line 57 of 60 holds 18 rows of a 27-row viewport, since the "never
    // paint half a wrapped line" rule stops it mid-line. That shortness is correct and is not
    // what this test is about.
    let long = (0..60)
        .map(|i| format!("line {i} {}", "w".repeat(400)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = open(&long);
    app.config.ui.word_wrap = true;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (bar, _total, _viewport) = app.vertical_scrollbar_detail().expect("bar");
    let x = app.regions.editor.right() - 1;
    down(&mut app, x, bar.track_start + bar.track_len - 1);
    up(&mut app, x, bar.track_start + bar.track_len - 1);

    let doc = app.doc().unwrap();
    let last_line = doc.buffer.line_count() - 1;
    let painted =
        np_core::layout_visible_rows(&doc.buffer, &app.geometry, doc.scroll.top_line, &doc.folds);
    let shown_last = painted.last().map(|r| r.line).unwrap_or(usize::MAX);
    assert_eq!(
        shown_last, last_line,
        "after going to the end the document's last line must be on screen:          top_line={} paints lines up to {shown_last}, last is {last_line}",
        doc.scroll.top_line,
    );

    // The bar's own position reading must say "at the end" too. This is the property the
    // reported "the text jumps to the first line and returns" turned on: the thumb was drawn
    // against one range while clicks were resolved against another, so the two disagreed and the
    // view snapped back. Asserting the reading rather than the thumb's pixel position keeps the
    // test honest about the unit without depending on how a minimum-size thumb is centred.
    let at_end = np_tui::app::row_offset_of_top(app.doc().unwrap(), &app.geometry);
    assert_eq!(
        at_end,
        app.max_viewport_row_offset(),
        "once the end is reached the scroll position must read as the ceiling the bar is scaled to",
    );
}

#[test]
fn going_to_the_end_of_a_plain_document_reaches_the_last_line() {
    // The same gesture without wrap, where the arithmetic is simple enough to assert exactly.
    // This is the case the old `NudgeThumbEnd` got wrong in the other direction: it added one
    // to a line index while the surrounding code spoke rows, so under wrap it moved backwards.
    let mut app = open(&lines(200));
    let (bar, _c, _v) = app.vertical_scrollbar_detail().expect("bar");
    let x = app.regions.editor.right() - 1;
    down(&mut app, x, bar.track_start + bar.track_len - 1);
    up(&mut app, x, bar.track_start + bar.track_len - 1);
    let doc = app.doc().unwrap();
    let painted =
        np_core::layout_visible_rows(&doc.buffer, &app.geometry, doc.scroll.top_line, &doc.folds);
    assert_eq!(
        painted.last().map(|r| r.line),
        Some(doc.buffer.line_count() - 1),
        "no-wrap: the last page must end on the document's last line",
    );
}

// --------------------------------------------- 1b. horizontal bar: both ends reachable

#[test]
fn the_horizontal_bar_reaches_both_ends_of_a_very_long_line() {
    // Reported on a file with an 8000-column line. Two defects met here: the bar was offered
    // only after the text arm had claimed the cell, so its left end placed the caret instead of
    // scrolling, and the drag model capped reach at the trough's length -- the user's words
    // were "very difficult to reach left or right end of the text".
    let mut app = open(&format!("{}\n{}\n", "x".repeat(8000), "tail"));
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (start, y, len) = track(&app, false);
    let max = np_tui::mouse::scroll_scope(&app, np_tui::mouse::VerticalOrHorizontal::Horizontal)
        .expect("horizontal bar")
        .max_offset();
    assert!(
        max > 1000,
        "a wide document should have a long horizontal reach, got {max}"
    );

    down(&mut app, start + len - 1, y);
    up(&mut app, start + len - 1, y);
    assert_eq!(
        offset(&app, false),
        max,
        "the right end of the bar must reach the last column"
    );

    down(&mut app, start, y);
    up(&mut app, start, y);
    assert_eq!(
        offset(&app, false),
        0,
        "the left end of the bar must return to column 0"
    );
}

#[test]
fn the_left_end_of_the_horizontal_bar_scrolls_and_does_not_place_the_caret() {
    // The bar's first cell sits left of the text area, in the gutter column. `hit_test` used to
    // bail to `Outside` before consulting the bar, so that cell was unreachable and a drag
    // starting there stalled.
    let mut app = open(&format!("{}\n", "y".repeat(8000)));
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (start, y, _len) = track(&app, false);
    let hit = np_tui::mouse::scrollbar_hit(&app, start, y);
    assert!(
        matches!(
            hit,
            Some(np_tui::mouse::Hit::ScrollbarThumb(
                np_tui::mouse::VerticalOrHorizontal::Horizontal,
                _
            )) | Some(np_tui::mouse::Hit::ScrollbarTrack(
                np_tui::mouse::VerticalOrHorizontal::Horizontal,
                _
            ))
        ),
        "the horizontal bar's first cell must resolve to the bar, got {hit:?}",
    );
}

#[test]
fn horizontal_presses_walk_the_line_monotonically() {
    let mut app = open(&format!("{}\n", "z".repeat(4000)));
    app.config.ui.word_wrap = false;
    app.recompute_geometry(W, H);
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let (start, y, len) = track(&app, false);
    let mut previous = 0;
    for cell in 0..len {
        down(&mut app, start + cell, y);
        up(&mut app, start + cell, y);
        let now = offset(&app, false);
        assert!(
            now >= previous,
            "horizontal cell {cell} went backwards: {now} < {previous}"
        );
        previous = now;
    }
}

// ------------------------------------------------ 2. a one-entry directory stops, not spins

#[test]
fn a_listing_with_only_the_parent_row_clamps_instead_of_rotating() {
    // The report: "when only `../` item in list it can't be selected. Item list rotating
    // infinitely". The file picker's own `move_file` walked the path field and both buttons
    // between the list ends, so on a one-row list the cursor was almost never on the row.
    let dir = std::path::Path::new("/tmp/npt-empty-dir-s17");
    let _ = std::fs::create_dir_all(dir);
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::OpenFile);
    app.file_input.set_text(format!("{}/", dir.display()));
    app.refresh_file_entries();
    assert_eq!(
        app.file_entries.len(),
        1,
        "an empty directory should leave only the parent row: {:?}",
        app.file_entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    );

    // Down past the end, repeatedly: the cursor stays on the only row there is.
    for _ in 0..8 {
        key(&mut app, KeyCode::Down);
        assert_eq!(
            app.list_cursor(),
            0,
            "Down must stop at the last row, not leave the list"
        );
    }
    for _ in 0..8 {
        key(&mut app, KeyCode::Up);
        assert_eq!(
            app.list_cursor(),
            0,
            "Up must stop at the first row, not leave the list"
        );
    }
    // And the row is actually actionable: Enter on it descends to the parent, which is what
    // the only row in the list means. The old wrap-around made this key press land on the path
    // field or a button instead, so the row could be highlighted and still not be openable.
    let parent = app.file_entries[0].path.clone();
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode(),
        Mode::Dialog(DialogKind::Open),
        "descending keeps the dialog open",
    );
    assert_eq!(
        app.file_input.text(),
        format!("{}/", parent.display()),
        "Enter on the parent row must descend to that row's directory",
    );
}

#[test]
fn the_row_cursor_clamps_at_both_ends_of_a_long_listing() {
    // Clamping is now a framework rule for every list, not a per-dialog choice.
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::ThemePicker);
    let count = app.picker_candidates(DialogKind::ThemePicker).len();
    assert!(count > 2, "need more than one theme to test the ends");
    for _ in 0..count + 6 {
        key(&mut app, KeyCode::Down);
    }
    assert_eq!(
        app.list_cursor(),
        count - 1,
        "Down must clamp at the last theme"
    );
    for _ in 0..count + 6 {
        key(&mut app, KeyCode::Up);
    }
    assert_eq!(app.list_cursor(), 0, "Up must clamp at the first theme");
}

// ------------------------------------------- 3. the Themes dialog answers the keyboard

#[test]
fn down_from_the_theme_query_field_reaches_the_list() {
    // The report was that the Themes dialog could not be controlled by keyboard at all. Its
    // handler moved `theme_selected` and never moved focus, so `Down` from the field had no
    // effect the user could see or act on.
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::ThemePicker);
    assert!(
        !app.list_focused(),
        "a freshly opened picker starts in its query field",
    );
    key(&mut app, KeyCode::Down);
    assert!(
        app.list_focused(),
        "Down must carry focus out of the field and into the list",
    );
}

#[test]
fn enter_in_the_theme_list_applies_the_theme_the_cursor_is_on() {
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    let before = app.config.ui.theme.clone();
    app.execute(Action::ThemePicker);
    let names = app.picker_candidates(DialogKind::ThemePicker);
    let target = names
        .iter()
        .position(|n| n != &before)
        .expect("a theme that differs from the default");
    app.set_list_cursor(target);
    // One `Down` reaches the list *and* steps it, so the row under the cursor after it is the
    // row Enter must act on. Deriving it rather than hard-coding it keeps the test about which
    // row is applied, not about how many keys it took to get there.
    key(&mut app, KeyCode::Down);
    let selected = app.list_cursor();
    assert!(app.list_focused(), "focus must be in the list before Enter");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode(), Mode::Editing, "Enter accepts the picker");
    assert_eq!(
        app.config.ui.theme, names[selected],
        "Enter must apply the row the cursor was on, not row 0",
    );
    assert_ne!(
        app.config.ui.theme, before,
        "and that row must be a real change"
    );
}

#[test]
fn theme_is_offered_by_exactly_one_menu() {
    // "Themes item present in more that one submenu - both places open same Themes dialog."
    // Two entries for one command is a question the user cannot answer without trying both.
    let app = App::new_headless(Config::default(), (W, H));
    // Grouping is by `Action`, not by label: matching on the substring "Theme" would also
    // catch unrelated rows, and the question being asked is "how many rows run this command".
    let mut occurrences: Vec<(MenuId, String, Action)> = Vec::new();
    for menu in MenuId::ALL {
        for item in np_tui::ui::menus::menu_items(&app, menu) {
            if matches!(
                item.action,
                Some(Action::ThemePicker) | Some(Action::LanguagePicker)
            ) {
                occurrences.push((menu, item.label.clone(), item.action.unwrap()));
            }
        }
    }
    for command in [Action::ThemePicker, Action::LanguagePicker] {
        let hits: Vec<&(MenuId, String, Action)> = occurrences
            .iter()
            .filter(|(_, _, a)| *a == command)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "{command:?} should be offered by exactly one menu, got {:?}",
            hits.iter()
                .map(|(m, l, _)| format!("{m:?}/{l}"))
                .collect::<Vec<_>>(),
        );
    }
}

// ------------------------------- 4. one dialog object: the same gestures everywhere

#[test]
fn the_preferences_dialog_answers_the_mouse() {
    // Settings had no mouse control at all: its row count answered 0, so the shared cursor
    // clamped to row 0 no matter where the pointer went.
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::Preferences);
    let rows = np_tui::ui::prefs::rows().len();
    assert_eq!(
        app.dialog_row_count(),
        rows,
        "the framework must know the form has rows"
    );
    let items = app.dialog_focus_items();
    let target = items
        .iter()
        .position(|it| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(n) if n > 0))
        .expect("a visible row below the first");
    let np_tui::ui::framework::FocusId::ListRow(row) = items[target].id else {
        unreachable!();
    };
    let rect = items[target].area;
    down(&mut app, rect.x + 1, rect.y);
    up(&mut app, rect.x + 1, rect.y);
    assert_eq!(
        app.list_cursor(),
        row,
        "a click must select the row under the pointer"
    );
}

#[test]
fn clicking_a_preferences_row_then_pressing_space_toggles_that_setting_once() {
    // One gesture, one consequence. The Find dialog used to toggle a checkbox on press and then
    // again on release, which read as "nothing happened".
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::Preferences);
    let index = np_tui::ui::prefs::rows()
        .iter()
        .position(|r| r.key == np_tui::ui::prefs::Key::WordWrap)
        .expect("word wrap row");
    let items = app.dialog_focus_items();
    let slot = items
        .iter()
        .position(|it| it.id == np_tui::ui::framework::FocusId::ListRow(index))
        .expect("the word wrap row is on screen");
    let rect = items[slot].area;
    let before = app.config.ui.word_wrap;
    down(&mut app, rect.x + 1, rect.y);
    up(&mut app, rect.x + 1, rect.y);
    assert_eq!(
        app.config.ui.word_wrap, before,
        "a click selects; it does not toggle"
    );
    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.ui.word_wrap, !before, "Space toggles it once");
}

#[test]
fn a_find_checkbox_toggles_once_per_click() {
    let mut app = open(&lines(5));
    app.execute(Action::Find);
    let items = app.dialog_focus_items();
    let slot = items
        .iter()
        .position(|it| matches!(it.id, np_tui::ui::framework::FocusId::Checkbox(_)))
        .expect("Find has checkboxes");
    let np_tui::ui::framework::FocusId::Checkbox(which) = items[slot].id else {
        unreachable!();
    };
    let before = app.find.opts.case_sensitive;
    let rect = items[slot].area;
    down(&mut app, rect.x + 1, rect.y);
    up(&mut app, rect.x + 1, rect.y);
    assert_eq!(
        app.find.opts.case_sensitive, !before,
        "one click must flip the flag exactly once, not press and release each toggling it",
    );
    let _ = which;
}

#[test]
fn arrow_keys_move_the_same_cursor_the_mouse_just_moved() {
    // The point of a single shared cursor: after a click, the keyboard continues from the row
    // the pointer chose rather than from wherever the dialog's private copy last rested.
    //
    // Preferences is the right dialog for this because a click on a row there selects without
    // dismissing. In the Theme picker a click *accepts*, closing the dialog -- so there is no
    // cursor left to continue from, and the arrow keys would be driving the editor.
    let mut app = App::new_headless(Config::default(), (W, H));
    let _ = np_tui::ui::render_to_buffer(&mut app, W, H);
    app.execute(Action::Preferences);
    let items = app.dialog_focus_items();
    let slot = items
        .iter()
        .position(|it| matches!(it.id, np_tui::ui::framework::FocusId::ListRow(n) if n >= 2))
        .expect("a row at index 2 or deeper is visible");
    let np_tui::ui::framework::FocusId::ListRow(row) = items[slot].id else {
        unreachable!();
    };
    let rect = items[slot].area;
    down(&mut app, rect.x + 1, rect.y);
    up(&mut app, rect.x + 1, rect.y);
    assert_eq!(app.mode(), Mode::Dialog(DialogKind::Preferences));
    assert_eq!(app.list_cursor(), row, "the click moved the shared cursor");
    key(&mut app, KeyCode::Down);
    assert_eq!(
        app.list_cursor(),
        row + 1,
        "the arrow continues from the clicked row"
    );
    key(&mut app, KeyCode::Up);
    assert_eq!(app.list_cursor(), row, "and steps back onto it");
}
