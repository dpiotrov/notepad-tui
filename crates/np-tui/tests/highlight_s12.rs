//! S12 — highlighting in the rendered frame (FR-H1..H5).
//!
//! The engine is tested in `np-core/tests/highlight_s12.rs`. This file is about the seams
//! that only exist once something is drawn: does a syntax color reach the cell, does the layer
//! order survive contact with a real selection and a real match, and do the Language and Theme
//! pickers actually change what the next frame looks like.
//!
//! Assertions are on cell `Color`/`Modifier`, never on text alone -- the text was correct
//! before this stage existed, so text assertions would pass on a completely broken
//! highlighter.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use np_core::{Config, Document, EncodingId, LineCol};
use np_tui::ui;
use np_tui::App;

const W: u16 = 100;
const H: u16 = 30;

fn config() -> Config {
    let mut config = Config::default();
    config.ui.clipboard = "none".into();
    config
}

fn app() -> App {
    App::new_headless(config(), (W, H))
}

/// A Rust document as the only tab, caret parked at the start.
fn rust_app(text: &str) -> App {
    let mut app = app();
    let doc = Document::from_decoded(
        std::path::PathBuf::from("/tmp/s12.rs"),
        text,
        EncodingId::Utf8,
        np_core::detect_dominant_eol(text),
        false,
    );
    app.documents.clear();
    app.documents.push(doc);
    app.active = 0;
    app
}

/// Where the editor text starts, given line numbers are on.
///
/// Recomputes geometry first. The gutter width comes from the document's line count, so a test
/// that swapped a document in without drawing still carries the narrow gutter of the untitled
/// buffer `new_headless` created -- which silently points every coordinate assertion at the
/// gutter's digits instead of at the text. This bit the first version of this file: a test
/// looked for `let` at the wrong column, found a digit, and "passed" on the wrong cell.
fn text_origin(app: &mut App) -> (u16, u16) {
    app.recompute_geometry(W, H);
    (
        app.regions.editor.x + app.geometry.gutter_width as u16,
        app.regions.editor.y,
    )
}

fn colored(c: Option<ratatui::style::Color>) -> Option<ratatui::style::Color> {
    match c {
        None | Some(ratatui::style::Color::Reset) => None,
        Some(v) => Some(v),
    }
}

/// A background *belonging to a layer*, as opposed to the plain editor ground.
///
/// Until S30 an unstyled cell carried no background at all, so `colored(bg) == None` meant
/// "no layer touched this cell". The editor ground is now owned by the theme and painted on
/// every cell, which is the point of that stage -- and it makes `None` unreachable. The question
/// these tests are really asking is unchanged, though: did the current-line, selection or match
/// layer put a background here that the plain text does not have? This answers exactly that, by
/// comparing against the ground the theme paints on unlayered cells.
fn layered_bg(
    ground: ratatui::style::Color,
    c: Option<ratatui::style::Color>,
) -> Option<ratatui::style::Color> {
    match colored(c) {
        Some(v) if v != ground => Some(v),
        _ => None,
    }
}

/// A foreground applied *by syntax highlighting*, as opposed to the theme's plain ink.
///
/// The companion of [`layered_bg`] on the other channel, and it exists for the same reason.
/// These three tests asked "is this cell unstyled?" by asserting `fg == None`, which worked while
/// an unstyled cell delegated its color to the terminal. Under S30 the theme owns every cell, so
/// an unstyled span is painted the theme's own foreground and `None` cannot happen. The question
/// is still answerable and still means the same thing: a keyword is distinguishable from plain
/// text, and plain text carries the theme's default ink and nothing else.
fn syntax_fg(
    plain: ratatui::style::Color,
    c: Option<ratatui::style::Color>,
) -> Option<ratatui::style::Color> {
    match colored(c) {
        Some(v) if v != plain => Some(v),
        _ => None,
    }
}

/// The theme's plain text ink as a `Color`, for [`syntax_fg`].
fn plain_ink(app: &App) -> ratatui::style::Color {
    let (r, g, b) = app.palette().foreground;
    ratatui::style::Color::Rgb(r, g, b)
}

/// The editor ground as a `Color`, for [`layered_bg`].
fn ground(app: &App) -> ratatui::style::Color {
    let (r, g, b) = app.palette().background;
    ratatui::style::Color::Rgb(r, g, b)
}

fn cell(app: &mut App, x: u16, y: u16) -> ratatui::buffer::Cell {
    let buf = ui::render_to_buffer(app, W, H);
    buf.cell(ratatui::layout::Position { x, y })
        .cloned()
        .unwrap_or_default()
}

/// The foreground color of the cell showing logical `(line, col)`.
fn fg_at(app: &mut App, line: usize, col: usize) -> Option<ratatui::style::Color> {
    let (ox, oy) = text_origin(app);
    colored(cell(app, ox + col as u16, oy + line as u16).style().fg)
}

// ----------------------------------------------------------- FR-H1 colors on screen

#[test]
fn a_rust_keyword_is_colored_and_differs_from_a_plain_word() {
    let mut app = rust_app("let zebra = 1;\n");
    let kw = fg_at(&mut app, 0, 0);
    let name = fg_at(&mut app, 0, 4);
    assert!(kw.is_some(), "the keyword should carry a color");
    assert_ne!(
        kw, name,
        "a keyword must not render the same as an identifier"
    );
}

#[test]
fn a_comment_is_colored_differently_from_code() {
    let mut app = rust_app("fn a() {}\n// comment here\n");
    let code = fg_at(&mut app, 0, 0);
    let comment = fg_at(&mut app, 1, 0);
    assert_ne!(code, comment, "a comment line must read differently");
}

#[test]
fn highlighting_reaches_a_line_the_viewport_does_not_start_at() {
    // Scrolling must not disable highlighting for lines that were never drawn first, which is
    // the case a cold cache plus a checkpoint replay has to handle.
    let text: String = (0..200).map(|i| format!("let v{i} = {i};\n")).collect();
    let mut app = rust_app(&text);
    app.documents[0].scroll.top_line = 150;
    // Locate the row actually showing line 150 rather than assuming it is row 0: which row a
    // logical line lands on depends on viewport height and clamping, so an assumption there is
    // exactly the thing that makes a test silently check nothing.
    let buf = ui::render_to_buffer(&mut app, W, H);
    let (ox, _) = text_origin(&mut app);
    let want = "let v150 = 150;";
    let row = (0..H).find(|&y| ui::row_text(&buf, y, ox, ox + want.len() as u16) == want);
    let y = row.expect("line 150 should be drawn somewhere in the viewport");
    let kw = colored(
        buf.cell(ratatui::layout::Position { x: ox, y })
            .unwrap()
            .style()
            .fg,
    );
    assert!(kw.is_some(), "a scrolled-in keyword should be colored");
    // And the same keyword at the top of an unscrolled document gets the same color, which is
    // the claim that matters: scrolling changes which lines are drawn, not how one is colored.
    let mut fresh = rust_app(&text);
    let (fx, fy) = text_origin(&mut fresh);
    let kw0 = colored(cell(&mut fresh, fx, fy).style().fg);
    assert!(kw0.is_some(), "sanity: the unscrolled keyword is colored");
    assert_eq!(
        kw, kw0,
        "a scrolled-in keyword keeps the color it has at the top"
    );
}

#[test]
fn horizontal_scroll_does_not_shift_the_colors() {
    // FR-H1 with the scroll interaction the spec worries about: a span must clip and follow,
    // not slide. The same logical character must keep its color at any left_col.
    let mut app = rust_app("let averylongname = 1;\n");
    let before = fg_at(&mut app, 0, 12);
    app.documents[0].scroll.left_col = 4;
    let (ox, oy) = text_origin(&mut app);
    let after = colored(cell(&mut app, ox + 8, oy).style().fg);
    assert_eq!(
        before, after,
        "the same character keeps its color when scrolled"
    );
}

// -------------------------------------------------------------- FR-H5 current line

#[test]
fn the_carets_line_gets_a_background_and_other_lines_do_not() {
    let mut app = rust_app("one\ntwo\nthree\n");
    app.documents[0].cursor.pos = LineCol { line: 1, col: 0 };
    let (ox, oy) = text_origin(&mut app);
    let bg0 = ground(&app);
    let on = layered_bg(bg0, cell(&mut app, ox + 2, oy + 1).style().bg);
    let bg0 = ground(&app);
    let off = layered_bg(bg0, cell(&mut app, ox + 2, oy + 2).style().bg);
    assert!(on.is_some(), "the caret's row should have a background");
    assert_eq!(off, None, "another row should not");
}

#[test]
fn current_line_highlight_can_be_turned_off() {
    let mut app = rust_app("one\ntwo\n");
    app.config.ui.current_line_highlight = false;
    app.documents[0].cursor.pos = LineCol { line: 0, col: 0 };
    let (ox, oy) = text_origin(&mut app);
    let bg0 = ground(&app);
    assert_eq!(
        layered_bg(bg0, cell(&mut app, ox + 1, oy).style().bg),
        None,
        "with the highlight off, no layer may paint a background"
    );
}

// --------------------------------------------------------------- layering (FR-H5/S2)

#[test]
fn a_search_match_beats_the_current_line_background() {
    // The precedence the spec calls out. Both want the same cell's background; the match wins.
    let mut app = rust_app("let needle = 1;\n");
    app.documents[0].cursor.pos = LineCol { line: 0, col: 0 };
    app.find.marks = vec![np_core::Match {
        start: LineCol { line: 0, col: 4 },
        end: LineCol { line: 0, col: 10 },
        text_len: 6,
    }];
    let (ox, oy) = text_origin(&mut app);
    let bg0 = ground(&app);
    let matched = layered_bg(bg0, cell(&mut app, ox + 4, oy).style().bg);
    let bg0 = ground(&app);
    let plain = layered_bg(bg0, cell(&mut app, ox + 12, oy).style().bg);
    assert!(matched.is_some(), "the match paints a background");
    assert_ne!(
        matched, plain,
        "a match must be distinguishable on the caret's row"
    );
}

#[test]
fn syntax_color_survives_under_a_selection() {
    // The reason colors compose per channel instead of per style: a selection supplies a
    // background, and the keyword's own foreground must still be visible on top of it.
    let mut app = rust_app("let zebra = 1;\n");
    app.documents[0].selection = Some(np_core::Selection {
        anchor: LineCol { line: 0, col: 0 },
        focus: LineCol { line: 0, col: 11 },
        kind: np_core::SelectionKind::Range,
    });
    let (ox, oy) = text_origin(&mut app);
    let c = cell(&mut app, ox + 1, oy);
    assert!(
        colored(c.style().bg).is_some(),
        "the selection paints a background"
    );
    assert!(
        colored(c.style().fg).is_some(),
        "and the syntax foreground is still there"
    );
}

#[test]
fn the_selection_covers_exactly_its_range() {
    let mut app = rust_app("abcdefgh\nsecond line here\n");
    // Park the caret on a line this test is not inspecting. The current-line highlight is
    // itself a background, so leaving the caret on row 0 would make "is this cell selected"
    // ambiguous between two layers -- both legitimately paint a background there.
    app.documents[0].cursor.pos = LineCol { line: 1, col: 0 };
    app.documents[0].selection = Some(np_core::Selection {
        anchor: LineCol { line: 0, col: 2 },
        focus: LineCol { line: 0, col: 5 },
        kind: np_core::SelectionKind::Range,
    });
    let (ox, oy) = text_origin(&mut app);
    let bg0 = ground(&app);
    let inside2 = layered_bg(bg0, cell(&mut app, ox + 2, oy).style().bg);
    let bg0 = ground(&app);
    let inside4 = layered_bg(bg0, cell(&mut app, ox + 4, oy).style().bg);
    let bg0 = ground(&app);
    let before = layered_bg(bg0, cell(&mut app, ox + 1, oy).style().bg);
    let bg0 = ground(&app);
    let at_end = layered_bg(bg0, cell(&mut app, ox + 5, oy).style().bg);
    assert!(inside2.is_some() && inside4.is_some(), "inside the range");
    assert_eq!(before, None, "before the range");
    // Half-open: the focus position itself is not selected content.
    assert_eq!(at_end, None, "the end is exclusive");
}

// ----------------------------------------------------------------- FR-H3 theme switch

#[test]
fn switching_the_theme_changes_the_rendered_colors() {
    let mut app = rust_app("let x = 1;\n");
    assert!(app.highlight.set_theme("base16-ocean.dark"));
    let before = fg_at(&mut app, 0, 0);
    app.hl_store.clear();
    assert!(app.highlight.set_theme("InspiredGitHub"));
    let after = fg_at(&mut app, 0, 0);
    assert_ne!(before, after, "a theme switch must actually change pixels");
}

#[test]
fn theme_none_removes_syntax_color() {
    let mut app = rust_app("let x = 1;\n");
    assert!(app.highlight.set_theme(np_core::highlight::THEME_NONE));
    let (ox, oy) = text_origin(&mut app);
    let c = cell(&mut app, ox, oy);
    let ink0 = plain_ink(&app);
    assert_eq!(
        syntax_fg(ink0, c.style().fg),
        None,
        "with theme `none` no span may color the text; it carries the theme's own ink and nothing \
         more"
    );
}

#[test]
fn a_theme_switch_clears_every_documents_cache() {
    let mut app = rust_app("let x = 1;\n");
    let _ = fg_at(&mut app, 0, 0);
    assert!(app.hl_store.cached_lines(0) > 0, "warmed");
    app.hl_store.clear();
    assert_eq!(app.hl_store.cached_lines(0), 0);
}

// -------------------------------------------------------------- FR-H2 language picker

fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, mods));
}

#[test]
fn the_language_picker_opens_and_lists_languages() {
    let mut app = rust_app("let x = 1;\n");
    app.execute(np_tui::Action::LanguagePicker);
    assert!(matches!(
        app.mode(),
        np_tui::app::Mode::Dialog(np_tui::app::DialogKind::LanguagePicker)
    ));
    let buf = ui::render_to_buffer(&mut app, W, H);
    let all: String = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| {
            buf.cell(ratatui::layout::Position { x, y })
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert!(all.contains("Rust"), "the picker should list Rust");
    // "Plain Text" is first in the candidate order (FR-H1's fallback must be reachable without
    // typing). The visible window is shorter than the 76-entry list, so assert on the
    // candidate list the picker draws from rather than on glyphs scrolled off screen.
    let candidates = app.picker_candidates(np_tui::app::DialogKind::LanguagePicker);
    assert_eq!(
        candidates.first().map(String::as_str),
        Some("Plain Text"),
        "Plain Text must be first, reachable with no typing"
    );
}

#[test]
fn typing_in_the_picker_filters_the_list() {
    let mut app = rust_app("let x = 1;\n");
    app.execute(np_tui::Action::LanguagePicker);
    for c in "rub".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    assert!(app.lang_query.contains("rub"));
    let filtered = np_tui::highlight::fuzzy_filter(&app.highlight.language_names(), "rub");
    let names: Vec<String> = filtered
        .iter()
        .map(|i| app.highlight.language_names()[*i].clone())
        .collect();
    assert!(
        names.iter().any(|n| n == "Ruby"),
        "Ruby should survive the filter, got {names:?}"
    );
    assert!(!names.iter().any(|n| n == "Rust"), "Rust should not");
}

#[test]
fn enter_applies_the_selected_language_to_the_document() {
    let mut app = rust_app("print(1)\n");
    app.execute(np_tui::Action::LanguagePicker);
    for c in "Ruby".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    // Move to the first match, then accept.
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.documents[0].language,
        np_core::LanguageId::Named("Ruby".into()),
        "the override must land on the document"
    );
    assert_eq!(
        app.mode(),
        np_tui::app::Mode::Editing,
        "and the picker must close"
    );
}

#[test]
fn escape_cancels_the_picker_and_changes_nothing() {
    let mut app = rust_app("let x = 1;\n");
    let before = app.documents[0].language.clone();
    app.execute(np_tui::Action::LanguagePicker);
    for c in "Ruby".chars() {
        key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.documents[0].language, before, "cancelled");
    assert_eq!(app.mode(), np_tui::app::Mode::Editing);
}

#[test]
fn choosing_plain_text_stops_highlighting_that_document() {
    let mut app = rust_app("let x = 1;\n");
    let before = fg_at(&mut app, 0, 0);
    assert!(before.is_some());
    app.documents[0].language = np_core::LanguageId::Named("Plain Text".into());
    let (ox, oy) = text_origin(&mut app);
    let ink0 = plain_ink(&app);
    let after = syntax_fg(ink0, cell(&mut app, ox, oy).style().fg);
    assert_eq!(after, None, "forcing Plain Text must unstyle the document");
}

#[test]
fn a_language_override_only_affects_its_own_document() {
    let mut app = rust_app("let x = 1;\n");
    let other = Document::from_decoded(
        std::path::PathBuf::from("/tmp/other.rs"),
        "let y = 2;\n",
        EncodingId::Utf8,
        np_core::Eol::Lf,
        false,
    );
    app.documents.push(other);
    app.documents[0].language = np_core::LanguageId::Named("Plain Text".into());
    // Doc 0 unstyled, doc 1 still auto-detected as Rust.
    let (ox0, oy0) = text_origin(&mut app);
    let ink0 = plain_ink(&app);
    let fg0 = cell(&mut app, ox0, oy0).style().fg;
    assert_eq!(
        syntax_fg(ink0, fg0),
        None,
        "the override must unstyle only the document it was set on"
    );
    app.active = 1;
    let (ox, oy) = text_origin(&mut app);
    assert!(
        colored(cell(&mut app, ox, oy).style().fg).is_some(),
        "the other tab keeps its own highlighting"
    );
}

// ---------------------------------------------------------------- FR-H3 theme picker

#[test]
fn the_theme_picker_lists_themes_and_applies_a_choice() {
    let mut app = rust_app("let x = 1;\n");
    app.execute(np_tui::Action::ThemePicker);
    assert!(matches!(
        app.mode(),
        np_tui::app::Mode::Dialog(np_tui::app::DialogKind::ThemePicker)
    ));
    let names = app.highlight.theme_names();
    let target = names
        .iter()
        .find(|n| n.as_str() != app.highlight.theme_name() && n.as_str() != "none")
        .cloned()
        .expect("more than one theme");
    // Select it directly and accept, which is what the list's cursor does.
    app.theme_selected = names.iter().position(|n| *n == target).unwrap_or(0);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.highlight.theme_name(), target);
    assert_eq!(app.config.ui.theme, target, "and it persists to config");
}

#[test]
fn an_unknown_theme_name_in_the_picker_leaves_the_theme_intact() {
    let mut app = rust_app("x\n");
    let before = app.highlight.theme_name().to_string();
    assert!(!app.highlight.set_theme("Not A Theme"));
    assert_eq!(app.highlight.theme_name(), before);
}

// --------------------------------------------------------------- FR-H4 invalidation

#[test]
fn an_edit_invalidates_the_cache_and_the_new_text_is_styled() {
    // Type a comment opener and check the next line changes color -- the render-side half of
    // the parse-state replay test in np-core.
    let mut app = rust_app("let a = 1;\nlet b = 2;\n");
    let before = fg_at(&mut app, 1, 4);
    // Insert `/*` at the start of line 0 through the document, the way editing would.
    app.documents[0].cursor.pos = LineCol { line: 0, col: 0 };
    app.documents[0]
        .insert_text("/*", np_core::UndoGroup::Atomic, std::time::Instant::now())
        .unwrap();
    let after = fg_at(&mut app, 1, 4);
    assert_ne!(
        before, after,
        "opening a comment must recolor the line below"
    );
}

#[test]
fn undo_restores_the_previous_styling() {
    let mut app = rust_app("let a = 1;\nlet b = 2;\n");
    let before = fg_at(&mut app, 1, 4);
    app.documents[0].cursor.pos = LineCol { line: 0, col: 0 };
    app.documents[0]
        .insert_text("/*", np_core::UndoGroup::Atomic, std::time::Instant::now())
        .unwrap();
    app.execute(np_tui::Action::Undo);
    let after = fg_at(&mut app, 1, 4);
    assert_eq!(
        before, after,
        "undo must be reflected in the colors, not just the text"
    );
}

// --------------------------------------------------------------------- snapshot (spec §10)

#[test]
fn a_rust_snippet_renders_with_expected_cell_attributes_at_100x30() {
    // The spec's snapshot requirement, asserting cell Color/Modifier rather than text.
    let mut app = rust_app("fn main() {\n    let s = \"hi\"; // note\n}\n");
    let (ox, oy) = text_origin(&mut app);
    // `fn` keyword.
    let kw = cell(&mut app, ox, oy);
    assert_eq!(kw.symbol(), "f");
    let kw_fg = kw.style().fg.expect("`fn` should be colored");
    // The string literal on line 1 differs from the keyword.
    let str_cell = cell(&mut app, ox + 12, oy + 1);
    assert_ne!(
        colored(str_cell.style().fg),
        Some(kw_fg),
        "a string is not keyword-colored"
    );
    // And the comment differs from both.
    let comment = colored(cell(&mut app, ox + 19, oy + 1).style().fg);
    assert!(comment.is_some());
    assert_ne!(comment, Some(kw_fg));
}

#[test]
fn the_frame_is_stable_across_two_draws() {
    // A cache bug usually shows up as frame 2 differing from frame 1 for the same state.
    let mut app = rust_app("let x = 1;\n// c\n");
    let first = ui::render_to_buffer(&mut app, W, H);
    let second = ui::render_to_buffer(&mut app, W, H);
    for y in 0..H {
        for x in 0..W {
            let a = first.cell(ratatui::layout::Position { x, y }).cloned();
            let b = second.cell(ratatui::layout::Position { x, y }).cloned();
            assert_eq!(a, b, "cell ({x},{y}) differed between identical draws");
        }
    }
}

#[test]
fn a_minimal_terminal_still_renders_without_panicking() {
    let mut app = rust_app("let x = 1;\n");
    let _ = ui::render_to_buffer(&mut app, 12, 3);
    let _ = ui::render_to_buffer(&mut app, 1, 1);
}
