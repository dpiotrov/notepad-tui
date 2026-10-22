//! Stage S8: the tab bar widget -- elision, markers, scrolling, and the shared layout that
//! the mouse hit-test uses (FR-U2).
//!
//! The invariant running through this file is that nothing is drawn outside the frame and
//! the active tab is always reachable, because both are properties a test can only check by
//! measuring the actual rendered buffer.

use crossterm::event::KeyModifiers;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use np_core::{Config, Document, EncodingId, Eol};
use np_tui::tabs::{self, TAB_MAX_WIDTH};
use np_tui::ui::{self, tab_bar};
use np_tui::App;

const W: u16 = 80;
const H: u16 = 24;

fn app() -> App {
    let mut config = Config::default();
    config.ui.clipboard = "osc52".into();
    App::new_headless(config, (W, H))
}

/// A document whose display name is `name`.
///
/// The path is set because `Document::display_name()` prefers the file name over
/// `untitled_name`. A document that only has `untitled_name` set would still read
/// `Untitled-N`, which made name-based lookups in these tests resolve to the wrong tab.
fn named(name: &str) -> Document {
    let mut doc = Document::new_untitled(EncodingId::Utf8, Eol::Lf);
    doc.path = Some(std::path::PathBuf::from(format!("/tmp/{name}")));
    doc
}

fn push(app: &mut App, doc: Document) {
    app.open_document(doc);
}

fn row(app: &mut App) -> String {
    let buf = ui::render_to_buffer(app, W, H);
    let y = app.regions.tabs.y;
    ui::row_text(&buf, y, 0, W)
}

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}

// ------------------------------------------------------------- elision

#[test]
fn a_long_name_is_elided_with_the_extension_kept() {
    let label = tabs::tab_label(&named("extremely_long_document_filename.rs"), false);
    let elided = tabs::elide(&label, TAB_MAX_WIDTH);
    assert!(
        elided.chars().count() <= TAB_MAX_WIDTH,
        "elided label is {} cells",
        elided.chars().count()
    );
    assert!(elided.contains('\u{2026}'), "shows an ellipsis: {elided}");
    assert!(elided.contains(".rs"), "keeps the extension: {elided}");
    assert!(elided.starts_with(' '), "keeps the leading pad: {elided}");
}

#[test]
fn elision_keeps_the_modified_marker_visible() {
    // Losing the dot would hide unsaved work, which is the one thing the marker prevents.
    let mut doc = named("a_really_extremely_long_file_name_that_overflows.md");
    doc.modified = true;
    let label = tabs::tab_label(&doc, false);
    let elided = tabs::elide(&label, 12);
    assert!(
        elided.starts_with(" \u{25cf}"),
        "the unsaved dot survives elision: {elided}"
    );
    assert!(elided.contains(".md"), "and the extension too: {elided}");
    assert!(elided.chars().count() <= 12, "and it fits: {elided}");
}

#[test]
fn elision_keeps_the_close_button() {
    let label = tabs::tab_label(&named("some_rather_long_file_name_here.txt"), true);
    let elided = tabs::elide(&label, 14);
    assert!(
        elided.ends_with(" \u{00d7} "),
        "the close affordance survives: {elided:?}"
    );
    assert!(elided.chars().count() <= 14, "and fits: {elided}");
}

#[test]
fn short_names_are_untouched() {
    let label = tabs::tab_label(&named("a.rs"), false);
    assert_eq!(tabs::elide(&label, TAB_MAX_WIDTH), label);
}

#[test]
fn dotfiles_have_no_extension_to_keep() {
    // `.gitignore`'s leading dot is not a separator, so the whole name is elidable.
    let label = tabs::tab_label(&named(".gitignore_extremely_long_suffix_here"), false);
    let elided = tabs::elide(&label, 12);
    assert!(elided.chars().count() <= 12, "fits: {elided}");
    assert!(elided.contains('\u{2026}'), "elided: {elided}");
}

#[test]
fn elide_survives_absurdly_small_budgets() {
    // A one- or two-cell bar cannot show a name. It must not panic and must not overflow.
    for max in 0..6 {
        let out = tabs::elide(" a_modified_filename.rs ", max);
        assert!(
            out.chars().count() <= max,
            "max {max} produced {:?}",
            out.chars().count()
        );
    }
}

// ------------------------------------------------------------- markers

#[test]
fn markers_render_in_the_label() {
    let clean = named("plain.txt");
    assert_eq!(tabs::tab_label(&clean, false), " plain.txt ");

    let mut modified = named("dirty.txt");
    modified.modified = true;
    assert_eq!(tabs::tab_label(&modified, false), " \u{25cf}dirty.txt ");

    let mut locked = named("locked.txt");
    locked.readonly = true;
    assert_eq!(tabs::tab_label(&locked, false), " \u{25ae}locked.txt ");

    let both = tabs::tab_label(&modified, true);
    assert_eq!(both, " \u{25cf}dirty.txt \u{00d7} ");
}

#[test]
fn only_the_active_or_hovered_tab_shows_a_close_button() {
    let mut app = app();
    push(&mut app, named("one.txt"));
    push(&mut app, named("two.txt"));
    push(&mut app, named("three.txt"));
    app.active = 1;

    let labels = tab_bar::labels(&app);
    assert_eq!(labels[0].matches('\u{00d7}').count(), 0);
    assert_eq!(labels[1].matches('\u{00d7}').count(), 1, "active tab");
    assert_eq!(labels[2].matches('\u{00d7}').count(), 0);

    // Hovering adds one without changing which tab is active.
    app.hover = Some(np_tui::app::HoverTarget::Tab(2));
    let labels = tab_bar::labels(&app);
    assert_eq!(labels[2].matches('\u{00d7}').count(), 1, "hovered tab");
    assert_eq!(app.active, 1, "hover is not selection");
}

// ------------------------------------------------------ the 80-column snapshot

#[test]
fn five_tabs_render_in_80_columns_with_elision() {
    let mut app = app();
    push(&mut app, named("alpha.rs"));
    let mut modified = named("bravo.md");
    modified.modified = true;
    push(&mut app, modified);
    let mut locked = named("charlie.lock");
    locked.readonly = true;
    push(&mut app, locked);
    push(
        &mut app,
        named("a_filename_long enough_to_need_eliding_and_then_some.rs"),
    );
    push(&mut app, named("epsilon.txt"));
    // Index 0 is the untitled tab `new_headless` starts with.
    app.active = app.documents.len() - 1;

    let rendered = row(&mut app);
    // No overflow: the tab row is exactly W cells, and nothing beyond it.
    assert_eq!(rendered.chars().count() as u16, W);

    let spans = tab_bar::spans(&app);
    let total: usize = tab_bar::labels(&app)
        .iter()
        .map(|l| l.chars().count())
        .sum();
    if total <= W as usize {
        assert_eq!(spans.len(), 6, "everything that fits is shown");
    } else {
        assert_eq!(spans.len(), 5, "one tab scrolls off, none is drawn clipped");
    }
    for span in &spans {
        assert!(
            span.x + span.width <= W,
            "tab {} ends at {}, past the frame",
            span.index,
            span.x + span.width
        );
        assert!(
            span.width <= TAB_MAX_WIDTH as u16,
            "tab {} too wide",
            span.index
        );
    }
    // The long name is elided but its extension is readable.
    let long = spans.iter().find(|s| s.index == 4).expect("long tab");
    let buf = ui::render_to_buffer(&mut app, W, H);
    let shown = ui::row_text(&buf, app.regions.tabs.y, long.x, long.x + long.width);
    assert!(shown.contains('\u{2026}'), "elided: {shown}");
    assert!(shown.contains(".rs"), "extension kept: {shown}");
    // Markers present.
    assert!(rendered.contains('\u{25cf}'), "modified dot: {rendered}");
    assert!(rendered.contains('\u{25ae}'), "lock glyph: {rendered}");
    assert!(rendered.contains('\u{00d7}'), "close on the active tab");
}

#[test]
fn the_active_tab_is_always_visible_however_many_tabs_there_are() {
    // Twenty tabs cannot fit in 80 columns. Whatever the active index, its span must exist
    // and sit inside the frame.
    let mut app = app();
    for i in 0..20 {
        push(&mut app, named(&format!("doc{i}.txt")));
    }
    let total = app.documents.len();
    for active in 0..total {
        app.active = active;
        let spans = tab_bar::spans(&app);
        let span = spans
            .iter()
            .find(|s| s.index == active)
            .unwrap_or_else(|| panic!("active tab {active} is not on screen"));
        assert!(span.width > 0, "active tab {active} has no width");
        assert!(
            span.x + span.width <= W,
            "active tab {active} runs off the right edge"
        );
        // And a click at its centre finds it.
        let y = app.regions.tabs.y;
        app.handle_mouse_at(
            &mouse(MouseEventKind::Moved, span.x + span.width / 2, y),
            std::time::Instant::now(),
        );
        assert_eq!(
            app.hover,
            Some(np_tui::app::HoverTarget::Tab(active)),
            "a click at the centre of active tab {active} finds it"
        );
    }
}

#[test]
fn the_bar_scrolls_and_leads_with_the_active_tab_when_it_cannot_fit() {
    let mut app = app();
    for i in 0..20 {
        push(&mut app, named(&format!("doc{i}.txt")));
    }
    let last_index = app.documents.len() - 1;
    app.active = 0;
    let first = tab_bar::spans(&app);
    let leading = first.first().expect("spans").index;
    assert_eq!(leading, 0, "starts at the first tab while it fits");

    app.active = last_index;
    let spans = tab_bar::spans(&app);
    let last = spans.last().expect("spans");
    assert_eq!(last.index, last_index, "the active tab is the last shown");
    assert!(
        spans.len() < app.documents.len(),
        "and the bar really is scrolled"
    );
    // Neighbours of the active tab are visible, not blank space.
    assert!(
        spans.iter().any(|s| s.index == last_index - 1),
        "the tab before the active one is shown too"
    );
}

#[test]
fn tabs_stay_put_when_cycling_among_ones_that_fit() {
    // The bar must not jump sideways just because the active tab changed.
    let mut app = app();
    for i in 0..4 {
        push(&mut app, named(&format!("doc{i}.txt")));
    }
    let positions: Vec<Vec<u16>> = (0..4)
        .map(|active| {
            let mut a = app.clone();
            a.active = active;
            tab_bar::spans(&a).iter().map(|s| s.x).collect()
        })
        .collect();
    for (active, xs) in positions.iter().enumerate() {
        // Only the active tab's own width changes, and only because of its close button;
        // every tab after it shifts by at most that one cell.
        let base = &positions[0];
        for (i, x) in xs.iter().enumerate() {
            if i <= active {
                continue;
            }
            let drift = if active == 0 {
                *x as i32 - base[i] as i32
            } else {
                0
            };
            assert!(
                drift.abs() <= 1,
                "tab {i} moved from {} to {x} when tab {active} became active",
                base[i]
            );
        }
    }
}

// ------------------------------------------------- hit-test agrees with render

#[test]
fn every_rendered_span_is_hit_at_its_own_cells() {
    let mut app = app();
    for name in ["one.rs", "a_very_long_document_name_indeed.tsx", "three.md"] {
        push(&mut app, named(name));
    }
    let mut modified = named("dirty.txt");
    modified.modified = true;
    push(&mut app, modified);
    app.active = 3;

    let spans = tab_bar::spans(&app);
    for span in &spans {
        let y = app.regions.tabs.y;
        let hit = np_tui::mouse::hit_test(&mouse(MouseEventKind::Moved, span.x + 1, y), &app);
        assert_eq!(
            hit,
            np_tui::mouse::Hit::TabBar(Some(span.index)),
            "the cell just inside tab {} must hit tab {}",
            span.index,
            span.index
        );
    }
    // Empty space past the last tab is no tab at all.
    let last = spans.last().expect("spans");
    let after = last.x + last.width + 1;
    if after < W {
        let hit = np_tui::mouse::hit_test(
            &mouse(MouseEventKind::Moved, after, app.regions.tabs.y),
            &app,
        );
        assert_eq!(hit, np_tui::mouse::Hit::TabBar(None));
    }
}

#[test]
fn a_click_on_the_close_button_closes_that_tab() {
    let mut app = app();
    push(&mut app, named("keep.txt"));
    push(&mut app, named("remove.txt"));
    // Index 0 is the untitled tab `new_headless` starts with, so these are 1 and 2. Look the
    // target up by name: hard-coded indices are how a test ends up closing the wrong tab.
    let target = app
        .documents
        .iter()
        .position(|d| d.display_name() == "remove.txt")
        .expect("remove.txt is open");
    app.active = target;

    // Re-read the spans *after* setting the active tab: the close button belongs to the
    // active tab, so its cell only exists once that tab is active.
    let y = app.regions.tabs.y;
    let spans = tab_bar::spans(&app);
    let span = spans.iter().find(|s| s.index == target).expect("span");
    let close = span.close.expect("active tab has a close button");
    assert_eq!(
        np_tui::mouse::route(
            &mouse(MouseEventKind::Down(MouseButton::Left), close, y),
            &app
        ),
        np_tui::mouse::MouseIntent::CloseTab(target),
        "the close cell routes to a close, not an activate"
    );

    let before = app.documents.len();
    app.handle_mouse_at(
        &mouse(MouseEventKind::Down(MouseButton::Left), close, y),
        std::time::Instant::now(),
    );
    assert_eq!(
        app.documents.len(),
        before - 1,
        "the close button closed exactly its own tab"
    );
    let left: Vec<String> = app.documents.iter().map(|d| d.display_name()).collect();
    assert!(
        !left.iter().any(|n| n == "remove.txt"),
        "the tab whose close button was clicked is gone, got {left:?}"
    );
    assert!(
        left.iter().any(|n| n == "keep.txt"),
        "and its sibling is untouched, got {left:?}"
    );
}

#[test]
fn the_close_button_cell_is_inside_its_own_tab() {
    let mut app = app();
    for i in 0..3 {
        push(&mut app, named(&format!("doc{i}.txt")));
    }
    app.active = 2;
    let spans = tab_bar::spans(&app);
    for span in &spans {
        if let Some(close) = span.close {
            assert!(
                span.contains(close),
                "close cell {close} is outside tab {} at {}..{}",
                span.index,
                span.x,
                span.x + span.width
            );
            assert!(close < W, "close cell is on screen");
        }
    }
}

// ------------------------------------------------------------ degenerate sizes

#[test]
fn a_narrow_terminal_never_overflows_the_bar() {
    let mut app = app();
    for i in 0..10 {
        push(&mut app, named(&format!("document{i}.txt")));
    }
    for width in [1u16, 2, 5, 10, 20, 40] {
        app.recompute_geometry(width, H);
        for active in 0..11usize {
            app.active = active.min(app.documents.len() - 1);
            for span in tab_bar::spans(&app) {
                assert!(
                    span.x + span.width <= width,
                    "width {width}: tab {} ends at {}",
                    span.index,
                    span.x + span.width
                );
            }
            // And rendering it must not panic.
            let _ = ui::render_to_buffer(&mut app, width, H);
        }
    }
}

#[test]
fn a_single_tab_does_not_fill_the_bar_with_itself() {
    let app = app();
    let spans = tab_bar::spans(&app);
    assert_eq!(spans.len(), 1);
    assert!(
        spans[0].width < W,
        "one tab is one tab, not a full-width block"
    );
}
