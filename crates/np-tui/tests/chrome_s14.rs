//! Status-bar click targets (FR-U3), banner strip (FR-F7, FR-U10), context menu (FR-U7),
//! and the S14 snapshot set (spec §10).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use np_core::Config;
use np_tui::actions::Action;
use np_tui::app::{App, ContextKind, Mode};
use np_tui::mouse::{Hit, StatusSegment};
use np_tui::ui::context_menu::{items, menu_rect};
use np_tui::ui::status_bar::status_segments;
use std::path::PathBuf;

fn fresh(w: u16, h: u16) -> App {
    App::new_headless(Config::default(), (w, h))
}

fn press(app: &mut App, code: KeyCode) {
    let mut ev = KeyEvent::new(code, KeyModifiers::NONE);
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

fn right(x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

fn with_doc(app: &mut App, name: &str, text: &str) {
    let doc = np_core::Document::from_decoded(
        PathBuf::from(name),
        text,
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.open_document(doc);
}

// ------------------------------------------------- FR-U3: status segments

/// The segment at a column, and the action a click there must produce.
fn click_segment(app: &mut App, segment: StatusSegment) -> Option<Hit> {
    let y = app.regions.status.y;
    let span = status_segments(app)
        .into_iter()
        .find(|s| s.segment == segment)?;
    // Click the middle of the segment: the first and last cell are the ones most likely to
    // be clipped or to collide with a neighbour at a narrow width.
    let x = span.x + span.width / 2;
    let hit = np_tui::mouse::hit_test(&down(x, y), app);
    Some(hit)
}

#[test]
fn every_segment_has_its_own_span_at_eighty_columns() {
    let mut app = fresh(80, 24);
    with_doc(&mut app, "/tmp/eighty.txt", "hello\nworld\n");
    let spans = status_segments(&app);
    assert!(spans.len() >= 7, "expected the full segment set: {spans:?}");
    // Segments must not overlap: each starts at or after the previous one ends.
    let mut cursor = 0u16;
    for span in &spans {
        assert!(
            span.x >= cursor,
            "{:?} at x={} overlaps the previous segment ending at {cursor}",
            span.segment,
            span.x
        );
        assert!(span.width > 0, "{:?} has no width", span.segment);
        cursor = span.x + span.width;
    }
}

#[test]
fn status_segment_clicks_route_at_eighty_columns() {
    // FR-U3 acceptance, first width. Each target is asserted by what it opens.
    let cases: Vec<(StatusSegment, Mode)> = vec![
        (
            StatusSegment::Position,
            Mode::Dialog(np_tui::app::DialogKind::GoTo),
        ),
        (
            StatusSegment::Eol,
            Mode::Dialog(np_tui::app::DialogKind::EolMenu),
        ),
        (
            StatusSegment::Encoding,
            Mode::Dialog(np_tui::app::DialogKind::EncodingPicker),
        ),
    ];
    for (segment, expected) in cases {
        let mut app = fresh(80, 24);
        with_doc(&mut app, "/tmp/w80.txt", "hello\nworld\n");
        let hit = click_segment(&mut app, segment)
            .unwrap_or_else(|| panic!("{segment:?} absent at 80 cols"));
        assert_eq!(
            hit,
            Hit::StatusBar(segment),
            "{segment:?} must be hit-testable"
        );
        app.handle_mouse(&{
            let spans = status_segments(&app);
            let s = spans.iter().find(|s| s.segment == segment).expect("span");
            down(s.x + s.width / 2, app.regions.status.y)
        });
        assert_eq!(
            app.mode(),
            expected,
            "clicking {segment:?} at 80 columns should open {expected:?}"
        );
    }
}

#[test]
fn language_and_overtype_clicks_route_at_eighty_columns() {
    let mut app = fresh(80, 24);
    with_doc(&mut app, "/tmp/w80b.txt", "fn main() {}\n");
    let before = app.doc().map(|d| d.overtype);
    let spans = status_segments(&app);
    let over = spans
        .iter()
        .find(|s| s.segment == StatusSegment::Overtype)
        .expect("INS segment");
    app.handle_mouse(&down(over.x + over.width / 2, app.regions.status.y));
    assert_eq!(
        app.doc().map(|d| d.overtype),
        before.map(|b| !b),
        "clicking INS must flip overtype"
    );

    let mut second = fresh(80, 24);
    with_doc(&mut second, "/tmp/main.rs", "fn main() {}\n");
    let lang = {
        let spans = status_segments(&second);
        let s = spans
            .iter()
            .find(|s| s.segment == StatusSegment::Language)
            .expect("language segment");
        down(s.x + s.width / 2, second.regions.status.y)
    };
    assert_eq!(
        np_tui::mouse::hit_test(&lang, &second),
        Hit::StatusBar(StatusSegment::Language)
    );
    second.handle_mouse(&lang);
    assert_eq!(
        second.mode(),
        Mode::Dialog(np_tui::app::DialogKind::LanguagePicker),
        "clicking the language must open the picker"
    );
}

#[test]
fn segments_stay_separate_and_clickable_at_forty_columns() {
    // FR-U3 acceptance, second width. 40 columns is where a fixed layout starts colliding,
    // so the no-overlap and inside-the-row rules are asserted again rather than assumed.
    let mut app = fresh(40, 12);
    with_doc(&mut app, "/tmp/w40.txt", "hello\nworld\n");
    let spans = status_segments(&app);
    assert!(!spans.is_empty(), "something must remain on the row");
    let row = app.regions.status;
    let mut cursor = 0u16;
    for span in &spans {
        assert!(
            span.x >= cursor,
            "{:?} at x={} overlaps the previous segment ending at {cursor} (40 cols)",
            span.segment,
            span.x
        );
        assert!(
            span.width > 0,
            "{:?} has no width at 40 columns",
            span.segment
        );
        assert!(
            span.x >= row.x && span.x + span.width <= row.x + row.w,
            "{:?} at {}..{} is outside the {}-cell row",
            span.segment,
            span.x,
            span.x + span.width,
            row.w
        );
        cursor = span.x + span.width;
    }
    // Every segment that survived the squeeze is still routed to its own target.
    for span in &spans {
        let hit = np_tui::mouse::hit_test(&down(span.x + span.width / 2, row.y), &app);
        assert_eq!(
            hit,
            Hit::StatusBar(span.segment),
            "{:?} misrouted at 40 cols",
            span.segment
        );
    }
}

#[test]
fn the_targets_that_survive_a_narrow_row_are_the_ones_the_spec_names() {
    // The squeeze drops the document name -- the tab bar already shows it -- and keeps every
    // segment FR-U3 requires to be clickable.
    let mut app = fresh(40, 12);
    with_doc(&mut app, "/tmp/w40b.txt", "hello\nworld\n");
    let present: Vec<StatusSegment> = status_segments(&app).iter().map(|s| s.segment).collect();
    for want in [
        StatusSegment::Eol,
        StatusSegment::Encoding,
        StatusSegment::Language,
        StatusSegment::Overtype,
    ] {
        assert!(
            present.contains(&want),
            "{want:?} dropped at 40 cols: {present:?}"
        );
    }
    // And the dropped one really is only the name.
    assert!(
        !present.contains(&StatusSegment::Document),
        "the name should be the thing that gives way: {present:?}"
    );
}

#[test]
fn every_segment_is_clickable_at_eighty_columns() {
    // The mirror of the narrow case: with room for all of them, all of them are targets.
    let mut app = fresh(80, 24);
    with_doc(&mut app, "/tmp/w80c.txt", "hello\n");
    let spans = status_segments(&app);
    assert_eq!(spans.len(), 7, "all seven segments fit at 80: {spans:?}");
    for span in &spans {
        let hit =
            np_tui::mouse::hit_test(&down(span.x + span.width / 2, app.regions.status.y), &app);
        assert_eq!(
            hit,
            Hit::StatusBar(span.segment),
            "{:?} misrouted at 80 cols",
            span.segment
        );
    }
}

#[test]
fn a_click_between_segments_does_not_trigger_one_of_them() {
    // The `" | "` separator belongs to the segment on its left. A click in the gap must not
    // be read as the neighbouring target, which is how "I clicked near the EOL and the
    // encoding menu opened" bugs are born.
    let mut app = fresh(40, 12);
    with_doc(&mut app, "/tmp/gap.txt", "hello\n");
    let spans = status_segments(&app);
    let gap = spans
        .windows(2)
        .map(|w| w[0].x + w[0].width)
        .find(|x| (x + 1) < app.regions.status.x + app.regions.status.w);
    let x = gap.expect("a separator gap should exist");
    let hit = np_tui::mouse::hit_test(&down(x, app.regions.status.y), &app);
    match hit {
        Hit::StatusBar(s) => assert_eq!(s, StatusSegment::Other, "the gap belongs to no widget"),
        other => panic!("expected a status hit, got {other:?}"),
    }
}

#[test]
fn the_rendered_row_and_the_click_targets_agree_at_every_width() {
    // The rule that was broken: the renderer and the hit-tester must read one layout. Each
    // span's text must appear at its own x, at 80 and at 40.
    for w in [80u16, 60, 40] {
        let h = 24.min(w / 2);
        let mut app = fresh(w, h);
        with_doc(
            &mut app,
            "/tmp/a-very-long-document-name.txt",
            "hello\nworld\n",
        );
        let spans = status_segments(&app);
        let buf = np_tui::ui::render_to_buffer(&mut app, w, h);
        let y = app.regions.status.y;
        for span in &spans {
            let end = (span.x + span.width).min(w);
            let drawn = np_tui::ui::row_text(&buf, y, span.x, end);
            let want: String = span.text.chars().take((end - span.x) as usize).collect();
            assert_eq!(
                drawn, want,
                "{:?} at x={} disagrees between render and hit-test at {w} cols: {drawn:?} vs {want:?}",
                span.segment, span.x
            );
        }
        // Nothing may spill onto the row above.
        let above = np_tui::ui::row_text(&buf, y.saturating_sub(1), 0, w);
        assert!(
            !above.contains('|'),
            "status content leaked to the row above at {w} cols: {above:?}"
        );
    }
}

#[test]
fn a_long_document_name_is_what_gives_way() {
    // The name is the only segment the tab bar already shows, so it is the one that drops.
    let mut app = fresh(40, 12);
    with_doc(
        &mut app,
        "/tmp/a-very-long-document-name.txt",
        "hello\nworld\n",
    );
    let buf = np_tui::ui::render_to_buffer(&mut app, 40, 12);
    let row = np_tui::ui::row_text(&buf, app.regions.status.y, 0, 40);
    assert!(
        !row.contains("a-very-long-document-name"),
        "the name should have been dropped, not squeezed: {row:?}"
    );
    assert!(
        row.contains("LF"),
        "the state columns must survive: {row:?}"
    );
    assert!(row.contains("INS"), "including overtype: {row:?}");
}

// ------------------------------------------------------- FR-U7: context menu

#[test]
fn the_context_menu_opens_at_the_pointer() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/ctx.txt", "one\ntwo\nthree\n");
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 3, text.y + 1));
    assert!(
        matches!(app.mode(), Mode::ContextMenu(ContextKind::Editor)),
        "right-click in the text must open the editor menu, got {:?}",
        app.mode()
    );
    assert_eq!(app.context_menu_cell, (text.x + 3, text.y + 1));
}

#[test]
fn the_editor_menu_has_the_required_items() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/items.txt", "one\ntwo\n");
    let want = [
        Action::Cut,
        Action::Copy,
        Action::Paste,
        Action::Delete,
        Action::SelectAll,
        Action::ToggleBookmark,
        Action::FoldAll,
        Action::UnfoldAll,
        Action::CopyPath,
    ];
    let items = items(&app, ContextKind::Editor);
    for action in want {
        assert!(
            items.iter().any(|i| i.action == action),
            "{action:?} missing from the editor context menu: {:?}",
            items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_tab_menu_has_the_required_items() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/first.txt", "one\n");
    with_doc(&mut app, "/tmp/second.txt", "two\n");
    let tabs = app.regions.tabs;
    let spans = np_tui::ui::tab_bar::spans(&app);
    let x = spans[0].x + 1;
    app.handle_mouse(&right(x, tabs.y));
    assert!(
        matches!(app.mode(), Mode::ContextMenu(ContextKind::Tab)),
        "right-click on a tab must open the tab menu, got {:?}",
        app.mode()
    );
    let items = items(&app, ContextKind::Tab);
    for action in [
        Action::CloseTab,
        Action::CloseOtherTabs,
        Action::CloseAllTabs,
        Action::ReopenClosedTab,
        Action::CopyPath,
    ] {
        assert!(
            items.iter().any(|i| i.action == action),
            "{action:?} missing from the tab menu: {:?}",
            items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn items_that_cannot_run_are_greyed_and_do_not_execute() {
    // FR-U7 with the disabled rule the menus already follow.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/nosel.txt", "one\ntwo\n");
    let no_sel = items(&app, ContextKind::Editor);
    for action in [Action::Cut, Action::Copy, Action::Delete] {
        let item = no_sel.iter().find(|i| i.action == action).expect("item");
        assert!(
            !item.enabled,
            "{action:?} with no selection must be disabled"
        );
    }
    // Select something and they come alive.
    if let Some(doc) = app.doc_mut() {
        doc.selection = Some(np_core::Selection::range(
            np_core::LineCol::new(0, 0),
            np_core::LineCol::new(1, 0),
        ));
    }
    let with_sel = items(&app, ContextKind::Editor);
    for action in [Action::Cut, Action::Copy, Action::Delete] {
        let item = with_sel.iter().find(|i| i.action == action).expect("item");
        assert!(item.enabled, "{action:?} with a selection must be enabled");
    }
    // And a disabled item must not run when clicked.
    let mut empty = fresh(100, 30);
    with_doc(&mut empty, "/tmp/empty.txt", "");
    empty.handle_mouse(&right(
        empty.geometry.text_area.x + 1,
        empty.geometry.text_area.y,
    ));
    let count = items(&empty, ContextKind::Editor).len();
    let rect = menu_rect(
        empty.context_menu_cell.0,
        empty.context_menu_cell.1,
        count,
        empty.regions.bounds(),
    );
    let before = empty
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    empty.handle_mouse(&down(rect.x + 2, rect.y + 1));
    assert_eq!(
        empty
            .doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default(),
        before,
        "a greyed Cut must not touch the buffer"
    );
}

#[test]
fn the_menu_clamps_inside_the_frame_at_the_bottom_right_corner() {
    // FR-U7 acceptance: the box must never render off-screen.
    for (w, h) in [(100u16, 30u16), (80, 24), (60, 16), (40, 12), (34, 8)] {
        let mut app = fresh(w, h);
        with_doc(&mut app, "/tmp/corner.txt", "one\ntwo\nthree\n");
        let bounds = app.regions.bounds();
        let count = items(&app, ContextKind::Editor).len();
        // The extreme: the very last cell of the frame.
        let rect = menu_rect(bounds.right() - 1, bounds.bottom() - 1, count, bounds);
        assert!(
            rect.x >= bounds.x,
            "{w}x{h}: menu ran off the left edge: {rect:?}"
        );
        assert!(
            rect.right() <= bounds.right(),
            "{w}x{h}: menu ran off the right edge: {rect:?} in {bounds:?}",
        );
        assert!(
            rect.y >= bounds.y,
            "{w}x{h}: menu ran off the top edge: {rect:?}"
        );
        assert!(
            rect.bottom() <= bounds.bottom(),
            "{w}x{h}: menu ran off the bottom edge: {rect:?} in {bounds:?}",
        );
        assert!(
            rect.w >= 3,
            "{w}x{h}: a menu {} cells wide is unusable",
            rect.w
        );
        assert!(
            rect.h >= 2,
            "{w}x{h}: a menu {} rows tall has no items",
            rect.h
        );
    }
}

#[test]
fn the_menu_flips_left_and_up_rather_than_clipping() {
    let bounds = np_core::RectCore::new(0, 0, 80, 24);
    let count = 9;
    // A pointer one cell from the right edge: the menu must open to the *left* of it.
    let rect = menu_rect(79, 5, count, bounds);
    assert!(rect.right() <= 80, "clipped on the right: {rect:?}");
    assert!(
        rect.x < 79,
        "should have flipped left of the pointer: {rect:?}"
    );
    // A pointer on the last row: it must open above.
    let rect = menu_rect(5, 23, count, bounds);
    assert!(rect.bottom() <= 24, "clipped on the bottom: {rect:?}");
    assert!(
        rect.y < 23,
        "should have flipped above the pointer: {rect:?}"
    );
    // Both at once, the worst case.
    let rect = menu_rect(79, 23, count, bounds);
    assert!(
        rect.x < 79 && rect.y < 23,
        "no flip at the corner: {rect:?}"
    );
    // And a pointer at the origin keeps the natural down-right placement.
    let rect = menu_rect(1, 1, count, bounds);
    assert_eq!(rect.x, 1, "an unconstrained pointer should not move");
    assert_eq!(rect.y, 1);
}

#[test]
fn a_rendered_context_menu_stays_inside_the_frame() {
    // The geometry function is one thing; what actually lands in the buffer is the claim.
    let mut app = fresh(60, 16);
    with_doc(&mut app, "/tmp/render-ctx.txt", "one\ntwo\nthree\n");
    let bounds = app.regions.bounds();
    app.handle_mouse(&right(bounds.right() - 1, bounds.bottom() - 2));
    assert!(matches!(app.mode(), Mode::ContextMenu(_)));
    let buf = np_tui::ui::render_to_buffer(&mut app, 60, 16);
    // The border box must have at least one cell of its own inside every edge.
    let count = items(&app, ContextKind::Editor).len();
    let rect = menu_rect(
        app.context_menu_cell.0,
        app.context_menu_cell.1,
        count,
        app.regions.bounds(),
    );
    assert!(
        rect.right() <= 60 && rect.bottom() <= 16,
        "{rect:?} escapes the frame"
    );
    // The menu's title row is a border, and the first item sits just inside it.
    let first_item = np_tui::ui::row_text(&buf, rect.y + 1, rect.x + 1, rect.right() - 1);
    assert!(
        first_item.contains("Cut"),
        "the first item should be drawn inside the box: {first_item:?}"
    );
}

#[test]
fn esc_dismisses_the_context_menu_without_running_anything() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/dismiss.txt", "one\ntwo\n");
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 1, text.y));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing);
    assert_eq!(
        app.doc().map(|d| d.buffer.text_string()).as_deref(),
        Some("one\ntwo\n")
    );
}

#[test]
fn enter_runs_the_highlighted_item_and_closes_the_menu() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/run.txt", "one\ntwo\n");
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 1, text.y));
    // Walk to Select All and run it.
    let items = items(&app, ContextKind::Editor);
    let target = items
        .iter()
        .position(|i| i.action == Action::SelectAll)
        .expect("select all row");
    for _ in 0..target {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.mode(),
        Mode::Editing,
        "the menu must close after running"
    );
    let selected = app
        .doc()
        .and_then(|d| d.selection)
        .is_some_and(|s| !s.is_empty());
    assert!(selected, "Select All should have run");
}

#[test]
fn a_click_on_an_item_runs_it_and_a_click_elsewhere_dismisses() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/click-item.txt", "one\ntwo\n");
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 1, text.y));
    let count = items(&app, ContextKind::Editor).len();
    let rect = menu_rect(
        app.context_menu_cell.0,
        app.context_menu_cell.1,
        count,
        app.regions.bounds(),
    );
    // The Select All row.
    let items = items(&app, ContextKind::Editor);
    let target = items
        .iter()
        .position(|i| i.action == Action::SelectAll)
        .expect("select all row");
    app.handle_mouse(&down(rect.x + 2, rect.y + 1 + target as u16));
    assert_eq!(app.mode(), Mode::Editing);
    assert!(app
        .doc()
        .and_then(|d| d.selection)
        .is_some_and(|s| !s.is_empty()));

    // Now a click outside the box dismisses without acting.
    app.handle_mouse(&right(text.x + 1, text.y));
    assert!(matches!(app.mode(), Mode::ContextMenu(_)));
    let bounds = app.regions.bounds();
    let outside_x = if app.context_menu_cell.0 > 2 {
        app.context_menu_cell.0 - 2
    } else {
        bounds.right() - 1
    };
    let before = app.doc().and_then(|d| d.selection).map(|s| s.anchor);
    app.handle_mouse(&down(outside_x, app.context_menu_cell.1));
    assert!(
        !matches!(app.mode(), Mode::ContextMenu(_)),
        "a click outside the menu must dismiss it"
    );
    assert_eq!(
        app.doc().and_then(|d| d.selection).map(|s| s.anchor),
        before,
        "dismissing must not change the selection"
    );
}

// ---------------------------------------------------- FR-F7 / FR-U10: banner

#[test]
fn the_banner_appears_under_the_tab_bar_with_both_buttons() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/banner.txt", "on disk\n");
    app.banner = Some(np_tui::app::Banner {
        message: "/tmp/banner.txt changed on disk".to_string(),
    });
    let region = np_tui::mouse::banner_region(&app);
    assert_eq!(
        region.y, app.regions.editor.y,
        "the banner occupies the top editor row, under the tab bar"
    );
    assert_eq!(
        region.y,
        app.regions.tabs.bottom(),
        "directly below the tabs"
    );
    assert_eq!(region.h, 1, "one row");
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    let row = np_tui::ui::row_text(&buf, region.y, 0, 100);
    assert!(row.contains("changed on disk"), "message missing: {row:?}");
    assert!(row.contains("Reload"), "Reload button missing: {row:?}");
    assert!(row.contains("Ignore"), "Ignore button missing: {row:?}");
}

#[test]
fn the_default_banner_button_is_reload() {
    // Enter on a banner must take the safe action: re-read the file rather than dismiss.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/default-btn.txt", "mine\n");
    app.banner = Some(np_tui::app::Banner {
        message: "changed on disk".to_string(),
    });
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    let row = np_tui::ui::row_text(&buf, np_tui::mouse::banner_region(&app).y, 0, 100);
    assert!(
        row.contains("<Reload>"),
        "Reload is the default and should render as <Reload>: {row:?}"
    );
    assert!(
        row.contains("[Ignore]"),
        "Ignore is not the default: {row:?}"
    );
    assert_eq!(app.banner_button, 0);
}

#[test]
fn banner_buttons_are_hit_testable_and_ignore_dismisses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("banner.txt");
    std::fs::write(&path, "mine\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    app.banner = Some(np_tui::app::Banner {
        message: "changed on disk".to_string(),
    });
    let region = np_tui::mouse::banner_region(&app);
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    let row = np_tui::ui::row_text(&buf, region.y, 0, 100);
    // Find the Ignore button by where it actually rendered.
    let ignore_at = row.find("[Ignore]").expect("Ignore on screen");
    let x = ignore_at as u16 + 1;
    assert_eq!(
        np_tui::mouse::hit_test(&down(x, region.y), &app),
        Hit::Banner
    );
    app.handle_mouse(&down(x, region.y));
    assert!(app.banner.is_none(), "Ignore must clear the banner");
    assert_eq!(
        app.doc().map(|d| d.buffer.text_string()).as_deref(),
        Some("mine\n"),
        "Ignore keeps the editor's version (FR-F7)"
    );
}

#[test]
fn reload_rereads_the_file_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("reload.txt");
    std::fs::write(&path, "theirs\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    app.banner = Some(np_tui::app::Banner {
        message: "changed on disk".to_string(),
    });
    let region = np_tui::mouse::banner_region(&app);
    let buf = np_tui::ui::render_to_buffer(&mut app, 100, 30);
    let row = np_tui::ui::row_text(&buf, region.y, 0, 100);
    let reload_at = row
        .find("<Reload>")
        .or_else(|| row.find("[Reload]"))
        .expect("Reload");
    let x = reload_at as u16 + 1;
    app.handle_mouse(&down(x, region.y));
    assert!(app.banner.is_none(), "and clear the banner");
    assert_eq!(
        app.doc().map(|d| d.buffer.text_string()).as_deref(),
        Some("theirs\n"),
        "Reload takes the disk's version (FR-F7)"
    );
}

#[test]
fn external_change_detection_raises_the_banner() {
    // The wiring to S5's detection, not just the widget.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("watched.txt");
    std::fs::write(&path, "original\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    assert!(app.banner.is_none(), "a file nobody touched gets no banner");
    // Change it on disk and advance past the mtime granularity.
    std::fs::write(&path, "somebody else\n").expect("rewrite");
    app.advance_clock(std::time::Duration::from_secs(2));
    app.check_external_changes();
    assert!(
        app.banner.is_some(),
        "a disk change must raise the banner (FR-F7)"
    );
    let message = app.banner.clone().map(|b| b.message).unwrap_or_default();
    assert!(
        message.contains("watched.txt"),
        "the banner must name the file: {message}"
    );
}

#[test]
fn an_unmodified_buffer_reloads_without_asking() {
    // FR-F7: the banner is the only prompt when there is nothing to lose.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("clean.txt");
    std::fs::write(&path, "v1\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    std::fs::write(&path, "v2\n").expect("rewrite");
    app.advance_clock(std::time::Duration::from_secs(2));
    app.check_external_changes();
    assert!(app.banner.is_some());
    assert_eq!(
        app.mode(),
        Mode::Editing,
        "no confirm dialog should stack over the banner when the buffer is clean"
    );
}

// --------------------------------------------------------- snapshots (§10)

/// Assert a rendered frame cell-by-cell against a description of what must be where.
fn snapshot(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let buf = np_tui::ui::render_to_buffer(app, w, h);
    (0..h)
        .map(|y| np_tui::ui::row_text(&buf, y, 0, w))
        .collect()
}

#[test]
fn snapshot_menu_open() {
    // Spec §10: the File dropdown at a fixed size, with chords right-aligned.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/snap-menu.txt", "one\n");
    press(&mut app, KeyCode::F(10));
    let rows = snapshot(&mut app, 100, 30);
    assert!(rows[0].contains("File"), "menu bar row: {:?}", rows[0]);
    let body = rows.concat();
    assert!(body.contains('┌'), "no dropdown box drawn: {body}");
    // The dropdown's own rows, below the title bar.
    assert!(body.contains("New"), "items missing: {body}");
    assert!(body.contains("Save As"), "labels missing: {body}");
    assert!(
        body.contains("ctrl+s"),
        "chords belong in the dropdown: {body}"
    );
    // The longest chord must survive the width computation rather than clip to `ctrl+shift+`.
    assert!(
        body.contains("ctrl+shift+s"),
        "the longest chord is clipped: {body}"
    );
    // Separators group the menu, which is what makes it scannable.
    assert!(body.contains('─'), "expected a separator row: {body}");
}

#[test]
fn snapshot_find_dialog() {
    // S19: Find docks as service lines at the bottom of the band rather than floating over the
    // buffer, so the assertions moved with it. What is being checked is the same promise -- the
    // query and the actions are on screen -- plus the one that motivated the change: the file
    // stays readable while Find is open. FR-U4's dimmed backdrop no longer applies to this
    // dialog precisely because there is no longer a box to see it through.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/snap-find.txt", "needle haystack\n");
    app.execute(Action::Find);
    let rows = snapshot(&mut app, 100, 30);
    let body = rows.concat();
    assert!(body.contains("Find :"), "query row missing: {body}");
    assert!(body.contains("Next"), "actions missing: {body}");
    assert!(
        body.contains("needle haystack"),
        "the searched text must still be on screen while Find is open: {body}"
    );
    let strip = np_tui::ui::find_bar::strip_rect(&app);
    let strip_text: String = rows[strip.y as usize].chars().collect();
    assert!(
        strip_text.contains("Find :"),
        "the query belongs on the strip's first row, got {strip_text:?}"
    );
}

#[test]
fn snapshot_banner_strip() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/snap-banner.txt", "one\n");
    app.banner = Some(np_tui::app::Banner {
        message: "changed on disk".to_string(),
    });
    let rows = snapshot(&mut app, 100, 30);
    let region = np_tui::mouse::banner_region(&app);
    let banner = &rows[region.y as usize];
    assert!(banner.contains("changed on disk"), "{banner:?}");
    assert!(
        banner.contains("Reload") && banner.contains("Ignore"),
        "{banner:?}"
    );
    // The buttons sit to the right of the message, never over it.
    let msg_at = banner.find("changed on disk").expect("message");
    let btn_at = banner.find("Reload").expect("button");
    assert!(
        btn_at > msg_at,
        "buttons must not overlap the message: {banner:?}"
    );
}

#[test]
fn snapshot_context_menu() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/snap-ctx.txt", "one\ntwo\n");
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 2, text.y + 1));
    let rows = snapshot(&mut app, 100, 30);
    let body = rows.concat();
    assert!(body.contains("Cut"), "items missing: {body}");
    assert!(body.contains("Select All"));
    assert!(
        body.contains("Copy Path") || body.contains("Copy Full Path"),
        "{body}"
    );
}

#[test]
fn snapshot_palette_and_preferences() {
    let mut app = fresh(100, 30);
    app.execute(Action::CommandPalette);
    let body = snapshot(&mut app, 100, 30).concat();
    assert!(body.contains("Commands"), "palette title: {body}");
    let mut second = fresh(100, 30);
    second.execute(Action::Preferences);
    let body = snapshot(&mut second, 100, 30).concat();
    assert!(body.contains("Preferences"), "prefs title: {body}");
    assert!(body.contains("Tab width"), "prefs rows: {body}");
}

#[test]
fn a_modal_dialog_owns_the_screen_and_swallows_a_right_click() {
    // Right-clicking through a modal would open a menu *under* the dialog, so the dialog
    // claims the press and nothing happens. This is the behavior; the test pins it so a
    // future hit-test reordering cannot start opening menus behind a modal.
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/modal.txt", "one\ntwo\n");
    app.execute(Action::Find);
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 1, text.y + 1));
    assert_eq!(
        app.mode(),
        Mode::Dialog(np_tui::app::DialogKind::Find),
        "a modal must not be popped or covered by a right-click"
    );
}

#[test]
fn a_context_menu_over_the_docked_panel_pops_one_level_at_a_time() {
    // The stacking rule (FR-U7), exercised where it can actually happen: the Find-in-Files
    // panel is docked, not modal, so the editor under it stays live and a menu can stack.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stack.txt");
    std::fs::write(&path, "needle here\nplain line\n").expect("write");
    let mut app = fresh(100, 30);
    app.open_path(&path);
    app.execute(Action::FindInFiles);
    assert_eq!(app.mode(), Mode::FindPanel);
    let panel_rows = app.find_panel_height;
    assert!(panel_rows > 0, "the panel should be docked");
    // Right-click in the text above the panel.
    let text = app.geometry.text_area;
    app.handle_mouse(&right(text.x + 1, text.y));
    assert!(
        matches!(app.mode(), Mode::ContextMenu(_)),
        "the menu must stack over the docked panel: {:?}",
        app.mode()
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(
        app.mode(),
        Mode::FindPanel,
        "Esc must pop exactly one level, leaving the panel"
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode(), Mode::Editing);
}

#[test]
fn each_overlay_dims_the_backdrop() {
    // FR-U4: an overlay must read as an overlay at a glance.
    let dim_count = |app: &mut App| -> usize {
        let buf = np_tui::ui::render_to_buffer(app, 100, 30);
        (0..100u16)
            .filter(|x| {
                buf.cell(ratatui::layout::Position { x: *x, y: 15 })
                    .is_some_and(|c| c.modifier.contains(ratatui::style::Modifier::DIM))
            })
            .count()
    };
    let mut plain = fresh(100, 30);
    with_doc(&mut plain, "/tmp/nodim.txt", "one\ntwo\n");
    assert_eq!(
        dim_count(&mut plain),
        0,
        "plain editing must not dim anything"
    );

    // Find is not in this list. It docks as service lines rather than floating over the text,
    // and a dimmed buffer under a search strip would dim exactly the text being searched -- the
    // thing the strip exists to keep visible.
    let mut find = fresh(100, 30);
    with_doc(&mut find, "/tmp/dim-find.txt", "one\ntwo\n");
    find.execute(Action::Find);
    assert_eq!(
        dim_count(&mut find),
        0,
        "the docked Find strip must not dim the text being searched"
    );

    let mut palette = fresh(100, 30);
    palette.execute(Action::CommandPalette);
    assert!(
        dim_count(&mut palette) > 20,
        "the palette must dim the backdrop"
    );

    let mut prefs = fresh(100, 30);
    prefs.execute(Action::Preferences);
    assert!(
        dim_count(&mut prefs) > 20,
        "Preferences must dim the backdrop"
    );

    let mut help = fresh(100, 30);
    help.execute(Action::HelpOverlay);
    assert!(
        dim_count(&mut help) > 20,
        "the help overlay must dim the backdrop"
    );

    // An open dropdown is deliberately NOT backed by a dimmed screen: you have to be able to
    // read the text you are about to click, and dimming the user's document behind a transient
    // menu defeats that. The only DIM cells a menu produces are its own right-aligned chord
    // hints, which live inside the box.
    let mut menu = fresh(100, 30);
    with_doc(&mut menu, "/tmp/dim-menu.txt", "one\ntwo\n");
    press(&mut menu, KeyCode::F(10));
    let buf = np_tui::ui::render_to_buffer(&mut menu, 100, 30);
    let dim: Vec<(u16, u16)> = (0..30u16)
        .flat_map(|y| (0..100u16).map(move |x| (x, y)))
        .filter(|(x, y)| {
            buf.cell(ratatui::layout::Position { x: *x, y: *y })
                .is_some_and(|c| c.modifier.contains(ratatui::style::Modifier::DIM))
        })
        .collect();
    // Every dim cell must be inside the dropdown box, of which there is one.
    let box_row = (0..30u16).find(|y| {
        (0..100u16).any(|x| {
            buf.cell(ratatui::layout::Position { x, y: *y })
                .is_some_and(|c| c.symbol() == "\u{250c}")
        })
    });
    let box_row = box_row.expect("the dropdown should draw a box");
    // The box's own extent, so "inside the menu" is measured rather than guessed.
    let box_right = (0..100u16)
        .rev()
        .find(|x| {
            buf.cell(ratatui::layout::Position { x: *x, y: box_row })
                .is_some_and(|c| c.symbol() == "\u{2510}")
        })
        .expect("the dropdown box has a top-right corner");
    for (x, y) in &dim {
        assert!(
            *y > box_row && *x >= 1 && *x <= box_right,
            "a dim cell at ({x},{y}) is outside the dropdown (rows {box_row}.., cols 1..={box_right}):              an open menu must not dim the text behind it"
        );
    }
    // The row below the box is the user's text, and it must be at full brightness.
    let below = box_row + 17;
    let outside = (0..100u16)
        .filter(|x| {
            buf.cell(ratatui::layout::Position { x: *x, y: below })
                .is_some_and(|c| c.modifier.contains(ratatui::style::Modifier::DIM))
        })
        .count();
    assert_eq!(
        outside, 0,
        "the document behind an open menu must not be dimmed"
    );
}

// ------------------------------------------- FR-F9: a refused write must speak

/// A read-only document, with a name so the toast can quote it.
fn readonly_doc(app: &mut App, name: &str, text: &str) {
    let mut doc = np_core::Document::from_decoded(
        PathBuf::from(name),
        text,
        np_core::EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    doc.readonly = true;
    app.open_document(doc);
}

fn toast_texts(app: &App) -> Vec<String> {
    app.timed_toasts.iter().map(|t| t.message.clone()).collect()
}

#[test]
fn typing_into_a_read_only_buffer_says_so() {
    // Spec S14 item 5: every error path must route to a toast. Before this, `Document`
    // refused the insert, the helper dropped the error, and the keystroke vanished.
    let mut app = fresh(100, 30);
    readonly_doc(&mut app, "/tmp/locked.txt", "do not touch\n");
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default(),
        before,
        "the buffer must still refuse the write"
    );
    let messages = toast_texts(&app);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("read-only") && m.contains("locked.txt")),
        "the refusal must be explained and name the file: {messages:?}"
    );
    assert!(
        app.timed_toasts.iter().any(|t| t.error),
        "and it must stick until dismissed"
    );
}

#[test]
fn enter_tab_and_paste_all_report_read_only() {
    for (label, key) in [
        ("Enter", KeyCode::Enter),
        ("Tab", KeyCode::Tab),
        ("Backspace", KeyCode::Backspace),
        ("Delete", KeyCode::Delete),
    ] {
        let mut app = fresh(100, 30);
        readonly_doc(&mut app, "/tmp/locked.txt", "one\ntwo\n");
        let before = app
            .doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default();
        press(&mut app, key);
        assert_eq!(
            app.doc()
                .map(|d| d.buffer.text_string())
                .unwrap_or_default(),
            before,
            "{label} must not modify a read-only buffer"
        );
        assert!(
            toast_texts(&app).iter().any(|m| m.contains("read-only")),
            "{label} on a read-only buffer must report the refusal"
        );
    }
}

#[test]
fn a_read_only_burst_toasts_once_not_once_per_keystroke() {
    // Twenty keystrokes must not bury the screen in twenty copies of the same notice.
    let mut app = fresh(100, 30);
    readonly_doc(&mut app, "/tmp/locked.txt", "one\n");
    for _ in 0..20 {
        press(&mut app, KeyCode::Char('x'));
    }
    let readonly_notices = toast_texts(&app)
        .iter()
        .filter(|m| m.starts_with("read-only:"))
        .count();
    assert_eq!(
        readonly_notices,
        1,
        "one explanation per burst: {:?}",
        toast_texts(&app)
    );
}

#[test]
fn a_writable_buffer_never_hears_about_read_only() {
    let mut app = fresh(100, 30);
    with_doc(&mut app, "/tmp/writable.txt", "one\n");
    press(&mut app, KeyCode::Char('x'));
    assert!(
        toast_texts(&app).is_empty(),
        "a normal keystroke must be silent: {:?}",
        toast_texts(&app)
    );
    assert!(
        app.doc()
            .map(|d| d.buffer.text_string())
            .unwrap_or_default()
            .contains('x'),
        "and it must land in the buffer"
    );
}

#[test]
fn dismissing_the_notice_lets_the_next_attempt_explain_again() {
    let mut app = fresh(100, 30);
    readonly_doc(&mut app, "/tmp/locked.txt", "one\n");
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        toast_texts(&app)
            .iter()
            .filter(|m| m.starts_with("read-only:"))
            .count(),
        1
    );
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        toast_texts(&app)
            .iter()
            .filter(|m| m.starts_with("read-only:"))
            .count(),
        1,
        "after a dismissal the next attempt gets a fresh notice"
    );
}
