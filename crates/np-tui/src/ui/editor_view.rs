//! The editor text area, drawn from `layout_visible_rows` (spec §5.4).
//!
//! Only visible rows are laid out and drawn; a million-line file must cost the same per
//! frame as a ten-line one. `np_core::layout_visible_rows` already does the visual-row
//! math (tab expansion, wrap once S13 enables it, fold filtering), so this module's job is
//! purely to paint what it returns and to keep the gutter aligned with it.
//!
//! The caret is positioned by `App`'s caller via `frame.set_cursor_position`, not here,
//! because this function only has a `Buffer` and the cursor belongs to the terminal.
//! [`caret_position`] exposes the coordinate so the draw path can set it.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget};

use np_core::{layout_visible_rows, CoreStyle, LineCol, ThemePalette};

use crate::app::App;
use crate::ui::layers::Layers;
use crate::ui::menu_bar::{draw_str, rect};

/// Draw the visible text into `App::regions.editor`.
pub fn draw(app: &App, buf: &mut Buffer) {
    let Some(doc) = app.doc() else {
        return;
    };
    let area = rect(app.regions.editor);
    let geom = app.geometry;
    // Narrow the highlight cache to what the last edit could have invalidated, before any line
    // is drawn. Without this a one-character edit drops every cached line in the document.
    app.hl_store
        .invalidate_edited_range(app.active, &app.highlight, &doc.buffer);
    let rows = layout_visible_rows(&doc.buffer, &geom, doc.scroll.top_line, &doc.folds);

    // The gutter's columns, computed once. `mouse` asks the same module the same questions so
    // a click on a marker cannot land on the wrong feature.
    let gutter = crate::ui::gutter::Gutter::new(
        area.x,
        doc.buffer.line_count(),
        app.config.ui.line_numbers,
        !doc.folds.is_empty(),
        !doc.bookmarks.is_empty(),
    );
    let cursor_line = doc.cursor.pos.line;
    // Every color this frame paints comes from here. A color the theme does not define is
    // derived from the colors it does (np-core::palette), never left for the terminal to fill
    // in -- a terminal palette switch used to repaint the gutter, the caret line and the
    // service lines while the text kept its colors, which is the bug this converts away.
    let palette = app.highlight.palette();
    // Paint the ground for the whole text band before a single glyph lands on it.
    //
    // Rows that carry text get their ground from `apply_row_styles`, but the band is taller than
    // the document: everything below the last line, and the cells past the end of every short
    // line, were never written at all. A ratatui cell that is never written holds `Color::Reset`
    // in both channels, and `Reset` is not a color -- it is an instruction to the terminal to use
    // *its* default. On a dark emulator that happened to look right; switch to a light palette and
    // the empty part of the editor turned white below a dark one, with a hard seam at the last
    // line of text.
    //
    // Painting the ground first also makes the text area consistent with the chrome bands, which
    // have always filled their region, and means the frame no longer depends on which cells the
    // document happens to occupy.
    fill_editor_ground(buf, &area, &palette);
    for (screen_row, row) in rows.iter().enumerate() {
        let y = area.y.saturating_add(screen_row as u16);
        if y >= area.bottom() {
            break;
        }
        let on_cursor_line = row.line == cursor_line;
        // Syntax styles for this logical line, or None when highlighting is off. Taken once
        // per row: a wrapped line yields several rows of the same logical line and must not
        // ask the engine twice.
        let syntax_line = syntax_for_line(app, row.line);
        draw_gutter(buf, doc, gutter, y, row, on_cursor_line, &palette);
        // Text: start at the row's first column so wrapped continuation rows come out right,
        // and shifted left by the horizontal scroll so the window shows what the scroll says it
        // shows.
        //
        // `row.start_col` is the wrap slice -- which part of the line this screen row carries,
        // and is 0 for every row when wrap is off. The horizontal scroll is a *separate* offset
        // that only the painter applies: `np_core` lays rows out without ever seeing
        // `scroll.left_col`, and the caret, the thumb and the hit-tester each subtract it where
        // they need to. The text used to not subtract it at all, so scrolling right slid the
        // caret and the selection off a body of text that stayed pinned at column 0.
        //
        // The scroll is therefore skipped from the *character* index, not subtracted from the x
        // origin. Subtracting from x is a tempting one-liner and is wrong twice over: the origin
        // is a `u16` that saturates at 0 as soon as the scroll passes the gutter width (so the
        // shift silently stops working on any real horizontal scroll), and the characters that
        // scroll off the left would still be counted against the row's width, shifting every
        // remaining glyph one cell right per skipped character.
        let scroll_chars = if geom.word_wrap {
            0
        } else {
            // Display columns, converted to a character index on this line: a tab or a wide
            // character means the two are not the same number.
            doc.buffer
                .col_from_display(row.line, doc.scroll.left_col, geom.tab_width)
                .saturating_sub(row.start_col)
        };
        let text = doc
            .buffer
            .line_string(row.line)
            .chars()
            .skip(row.start_col + scroll_chars)
            .take(row.char_len().saturating_sub(scroll_chars))
            .collect::<Vec<char>>();
        // The origin stays at the text area's left edge: skipping the scrolled characters is
        // what moves the window, so the first character drawn is by definition the leftmost
        // visible one.
        let text_x = area.x.saturating_add(geom.gutter_width as u16);
        // Symbols first, then one composed style per cell. Splitting it this way is what lets
        // syntax spans, the selection and a match all touch the same character without any of
        // them clobbering the glyph.
        let plain: String = text.iter().collect();
        // Screen column each source character was written to. It is the identity `text_x + i`
        // except where a tab occupies more than one cell, and it is what `apply_row_styles`
        // uses so syntax, selection and match layers land on the character they describe.
        // Computing it here, beside the code that writes the cells, is the only way the two
        // cannot disagree about where a character went.
        let cell_x = cell_columns(text_x, &text, geom.tab_width);
        if app.config.ui.show_whitespace || app.config.ui.show_eol {
            // FR-E14: display-only substitution. The buffer is never touched, and the
            // replacement is one cell per source character so column math is unaffected.
            draw_whitespace(
                buf,
                WhitespaceDraw {
                    x: text_x,
                    y,
                    right: area.right(),
                    text: &plain,
                    show_ws: app.config.ui.show_whitespace,
                    // An EOL mark only belongs on the logical line's last visual row.
                    show_eol: app.config.ui.show_eol
                        && row.end_col >= doc.buffer.line_len_chars(row.line),
                    tab_width: geom.tab_width,
                    palette: &palette,
                },
            );
        } else {
            // A tab must never reach a buffer cell. Ratatui's cell model counts one column
            // per cell, but the crossterm backend emits each symbol with `Print`, so a
            // literal tab makes the *terminal* jump to its own tab stop while every column
            // calculation -- caret placement, click mapping, selection, the row style pass --
            // still counts one. The visible result is a caret several columns right of the
            // text being typed, drifting further with each tab on the line. `TestBackend`
            // cannot see this: it measures a tab as one cell wide, exactly as ratatui
            // intends, so the whole headless suite passed while a real terminal was wrong.
            //
            // Tabs expand to blanks on the same tab stops the whitespace-marker path uses.
            // Everything else is written through, and a wide character is written once into
            // its own cell -- ratatui marks the following cell as a continuation and the
            // backend prints the double-width glyph, which is the normal path and must not
            // be reimplemented here.
            let mut col = text_x;
            for ch in &text {
                if col >= area.right() {
                    break;
                }
                if *ch == '\t' {
                    for _ in 0..tab_cells(col - text_x, geom.tab_width) {
                        if col >= area.right() {
                            break;
                        }
                        if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x: col, y }) {
                            cell.set_symbol(" ");
                        }
                        col = col.saturating_add(1);
                    }
                    continue;
                }
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x: col, y }) {
                    cell.set_symbol(&ch.to_string());
                }
                // Advance by the glyph's own width, so a CJK character that occupies two
                // cells does not have the next character written on top of it.
                col =
                    col.saturating_add(
                        unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(1) as u16
                    );
            }
        }
        // One pass over the row's cells applying the composed layer stack. Doing this after the
        // symbols exist means every layer is style-only, so no layer can ever overwrite content.
        apply_row_styles(
            app,
            buf,
            RowStylePass {
                x: text_x,
                cell_x: &cell_x,
                y,
                right: area.right(),
                chars: &text,
                line: row.line,
                start_col: row.start_col,
                syntax: syntax_line,
                on_cursor_line,
                palette: &palette,
            },
        );
    }
    draw_right_margin(app, buf, &area, &palette);
    draw_scrollbars(app, buf, &area, &palette);
}

/// Fill the text band with the theme's editor ground.
///
/// Every cell in the band, including the gutter strip and the scrollbar tracks, which the
/// per-row passes never touch. Anything drawn afterwards overwrites what it owns; this only
/// guarantees there is never an unwritten cell left showing the terminal's background.
fn fill_editor_ground(buf: &mut Buffer, area: &ratatui::layout::Rect, palette: &ThemePalette) {
    let style = crate::style::text(palette);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
                cell.reset();
                cell.set_symbol(" ");
                cell.set_style(style);
            }
        }
    }
}

/// The gutter for one screen row: line number, fold marker, bookmark marker.
///
/// Markers are **glyphs in the gutter**, not a background on the text. A background here would
/// have to enter the layer stack to compose at all, and per S12 the compose rule is "topmost
/// layer with a color wins" -- a gutter background would either fight the syntax highlighting
/// or be swallowed by it. A glyph in a column the text never occupies composes with nothing,
/// which is what makes it the right representation.
fn draw_gutter(
    buf: &mut Buffer,
    doc: &np_core::Document,
    gutter: crate::ui::gutter::Gutter,
    y: u16,
    row: &np_core::VisualRow,
    on_cursor_line: bool,
    palette: &ThemePalette,
) {
    if gutter.width == 0 {
        return;
    }
    // Blank the whole strip first. A previous frame may have left a marker on this row, and
    // ratatui buffers are reused across frames through the diff, so a marker that is only ever
    // written and never cleared would stay on screen after its bookmark was removed.
    let blanks = " ".repeat(gutter.width as usize);
    draw_str(
        buf,
        gutter.x,
        y,
        &blanks,
        crate::style::ink(palette.gutter_fg, palette.gutter_bg),
    );

    // FR-V2: the caret's line number is emphasized so the eye can find it in a long file.
    // Bold rather than a background, because a background would fight syntax and selection.
    let number = format!("{}", row.line + 1);
    let number_style = if on_cursor_line {
        crate::style::ink(palette.foreground, palette.gutter_bg).add_modifier(Modifier::BOLD)
    } else {
        crate::style::ink(palette.gutter_fg, palette.gutter_bg)
    };
    // Right-aligned inside its own field, so the digits sit flush against the text.
    let right = gutter.x.saturating_add(gutter.width);
    let digits = number.chars().count() as u16;
    let number_x = right.saturating_sub(digits).max(gutter.x);
    draw_str(buf, number_x, y, &number, number_style);

    // FR-V5: a fold marker on every foldable line, pointing right when collapsed and down when
    // expanded, so the state is readable without clicking.
    if let Some(col) = gutter.fold_col() {
        if let Some(fold) = doc.fold_at(row.line) {
            let glyph = if fold.collapsed {
                crate::ui::gutter::FOLD_CLOSED
            } else {
                crate::ui::gutter::FOLD_OPEN
            };
            let marker = crate::style::ink(palette.gutter_fg, palette.gutter_bg)
                .add_modifier(Modifier::BOLD);
            draw_str(buf, col, y, &glyph.to_string(), marker);
        }
    }
    // FR-V6: a diamond on a bookmarked line. Independent of the fold marker -- a line can be
    // both, and both must show, which is why they are separate columns.
    if let Some(col) = gutter.bookmark_col() {
        if doc.bookmarks.contains(&row.line) {
            let marker = crate::style::ink(palette.accent_fg, palette.gutter_bg);
            draw_str(
                buf,
                col,
                y,
                &crate::ui::gutter::BOOKMARK.to_string(),
                marker,
            );
        }
    }
}

/// The right-margin guide (FR-V3), at `[ui] right_margin_col`; 0 means off.
///
/// Drawn as a dim vertical rule in the cell at that column. It is painted **before** the text
/// in z-order terms -- the text pass already ran, so the guide only fills cells the text left
/// alone -- which is what "must not obscure text" means here: a character at the margin column
/// keeps its glyph, and the guide shows only in the whitespace past the end of a short line.
/// Reversing that would put a line through the middle of every long line of code.
fn draw_right_margin(
    app: &App,
    buf: &mut Buffer,
    area: &ratatui::layout::Rect,
    palette: &ThemePalette,
) {
    let col = app.config.ui.right_margin_col;
    if col == 0 {
        return;
    }
    let geom = app.geometry;
    // The margin is a column of the *document*, measured from the text area's left edge, so it
    // does not move when the gutter changes width. Horizontal scroll moves it with the text;
    // under wrap there is no horizontal scroll and the rule sits where the column is.
    let x = area
        .x
        .saturating_add(geom.gutter_width as u16)
        .saturating_add(col as u16)
        .saturating_sub(app.doc().map(|d| d.scroll.left_col).unwrap_or(0) as u16);
    if x >= area.right() {
        return;
    }
    for row in 0..area.height {
        let y = area.y.saturating_add(row);
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
            // Style only, never a symbol: an empty cell becomes a visible rule, and a cell with
            // a character in it keeps that character.
            cell.set_style(crate::style::ink(palette.gutter_fg, palette.background));
        }
    }
}

/// The scrollbars, drawn into the cells `App::geometry.bars` reserved for them (FR-V7).
///
/// # Why the areas are built here rather than passed in
///
/// Ratatui renders a `Scrollbar` into the *whole* area it is given: a `HorizontalBottom` bar
/// paints the area's last row, and a `VerticalRight` bar its last column. Handing both the
/// editor band means the horizontal bar paints across the band's full width, including the
/// corner cell that belongs to the vertical bar, and the two bars overwrite each other's
/// last cell -- which is what made the bars look mangled at their meeting point. Narrowing
/// each bar to the cells `bars` says it owns is what makes the drawing match the hit-test.
///
/// Ratatui's thumb math divides by the track length, so the narrowed area changes the thumb's
/// cell. `App::vertical_scrollbar_detail` and `App::horizontal_scrollbar_detail` compute the
/// thumb against these same narrowed tracks, and both the pixels and the grab target read
/// those numbers, so they cannot drift apart.
///
/// # Why the thumb geometry still comes from `App`
///
/// Ratatui does not expose where it put the thumb, and §6.9 row 14 requires distinguishing a
/// thumb drag from a track click, so a second implementation of the thumb math -- one that
/// could drift from the drawing -- is the failure mode to avoid. Both the drawing and the
/// hit-test read the same numbers.
fn draw_scrollbars(
    app: &App,
    buf: &mut Buffer,
    area: &ratatui::layout::Rect,
    palette: &ThemePalette,
) {
    let Some(doc) = app.doc() else { return };
    let geom = app.geometry;
    // Both bars ask `App` whether they exist and where their thumb is. The same calls answer the
    // mouse layer's "thumb or track", so the bar cannot be drawn at one place and grabbed at
    // another -- which is the only failure mode a scrollbar has that a user notices.
    if let Some(g) = app.vertical_scrollbar() {
        // The same range `App::vertical_scrollbar_detail` drew the thumb over: the offsets the
        // view can take, plus one viewport. Ratatui's `ScrollbarState::new` takes the content
        // length, and it places the thumb by `position / (content - 1)`, so feeding it the
        // document's row count would put the painted thumb at a cell the hit-test resolves to a
        // different offset -- a thumb you can see in one place and grab in another.
        let max_offset = app.max_viewport_row_offset();
        let viewport = geom.viewport_rows().max(1);
        let span = max_offset.saturating_add(viewport).max(2);
        let mut state =
            ScrollbarState::new(span.saturating_sub(1)).position(app.top_row_offset(max_offset));
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(scrollbar_ink(palette, true))
            .track_style(scrollbar_ink(palette, false));
        // The full band height, corner cell included: the horizontal bar's area stops short of
        // it, so this cell is drawn by exactly one bar.
        bar.render(*area, buf, &mut state);
        debug_assert!(
            g.thumb_start + g.thumb_len <= g.track_len,
            "thumb outside the track: {g:?}"
        );
    }
    if let Some(g) = app.horizontal_scrollbar() {
        let longest = longest_line_cols(app, doc);
        let mut state =
            ScrollbarState::new(longest.saturating_sub(1)).position(doc.scroll.left_col);
        let bar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(scrollbar_ink(palette, true))
            .track_style(scrollbar_ink(palette, false));
        // One cell narrower than the band whenever the vertical bar is drawn, which is the
        // track `horizontal_scrollbar_detail` measured the thumb against.
        let track = ratatui::layout::Rect {
            width: area
                .width
                .saturating_sub(geom.bars.cols)
                .max(1)
                .min(area.width),
            ..*area
        };
        bar.render(track, buf, &mut state);
        debug_assert!(
            g.thumb_start + g.thumb_len <= g.track_len,
            "thumb outside the track: {g:?}"
        );
    }
}

/// Scrollbar ink: thumb and track, both on the editor ground.
///
/// Ratatui's `Scrollbar` defaults its thumb and track to `Color::White` with no background, which
/// is a hardcoded guess about the terminal. On a light theme it paints a near-white thumb on a
/// near-white track -- invisible -- and the track's unset background lets the ground come from the
/// emulator. Both channels are named here instead.
///
/// The thumb rides a *glyph* the bar draws itself, so the thumb's contrast comes from its
/// foreground against the ground; the track is a blank cell whose visible difference from the
/// text is the background. `dim_ink` solves for a legible foreground while the background reuses
/// the current-line tone, which is already a theme-derived step away from the ground.
fn scrollbar_ink(palette: &ThemePalette, thumb: bool) -> ratatui::style::Style {
    let ground = palette.background;
    if thumb {
        let ink = np_core::dim_ink(ground, palette.foreground, np_core::MIN_UI_CONTRAST);
        crate::style::ink(ink, ground)
    } else {
        crate::style::ink(ground, palette.current_line_bg)
    }
}

/// The widest line in the document, in display cells.
///
/// Delegates to [`np_core::Buffer::max_line_width`], which memoizes on the buffer revision.
/// The local walk this replaced cost 2.5 s per frame on a 100 MB file.
fn longest_line_cols(app: &App, doc: &np_core::Document) -> usize {
    doc.buffer.max_line_width(app.geometry.tab_width)
}

/// Syntax styles for one logical line, or `None` when there is no syntax layer.
///
/// `None` means "no opinion", which the compose step treats differently from "an empty style".
/// With the theme set to `none` (FR-H3) the editor must show the terminal's own colors rather
/// than a forced default, and that distinction only survives if absence is representable.
fn syntax_for_line(app: &App, line: usize) -> Option<Vec<(CoreStyle, String)>> {
    let doc = app.doc()?;
    if app.highlight.theme_is_none() {
        return None;
    }
    let first_line = doc.buffer.line_string(0);
    let language = crate::highlight::resolve_language(
        &app.highlight,
        &doc.language,
        doc.path.as_deref(),
        &first_line,
    );
    let key = crate::highlight::CacheKey {
        tab: app.active,
        revision: doc.buffer.revision(),
        language,
        theme: app.highlight.theme_name().to_string(),
    };
    let styles = app
        .hl_store
        .styles_for_line(&key, &app.highlight, line, &doc.buffer);
    if styles.is_empty() {
        None
    } else {
        Some(styles)
    }
}

/// Everything `apply_row_styles` needs, bundled so the signature stays readable.
struct RowStylePass<'a> {
    /// Screen column of the row's first character.
    x: u16,
    /// Screen column of each entry of `chars`. Differs from `x + i` wherever a tab occupies
    /// more than one cell; see [`cell_columns`].
    cell_x: &'a [u16],
    y: u16,
    /// Exclusive right edge of the text area.
    right: u16,
    /// The row's characters, already horizontally scrolled.
    chars: &'a [char],
    /// Logical line index.
    line: usize,
    /// Char column within the logical line of `chars[0]`.
    start_col: usize,
    /// Syntax spans for the logical line, if any.
    syntax: Option<Vec<(CoreStyle, String)>>,
    on_cursor_line: bool,
    /// Theme colors, so a layer that expresses no color falls back to the theme's own ink and
    /// ground rather than to the terminal's defaults.
    palette: &'a ThemePalette,
}

/// Apply the composed layer stack to one row's cells (FR-H1, FR-H5, FR-S2).
///
/// Style only: symbols were laid down by the text pass, so no layer can overwrite content.
/// That is the property the S10 match overlay was deliberately limited to, generalized to
/// every layer, which is what makes "syntax ate my text" impossible by construction rather
/// than a bug to remember to avoid.
///
/// # The two coordinate spaces, once
///
/// `chars[i]` is logical column `start_col + i`. Its screen cell is `x + i`, because the text
/// pass already skipped `start_col` and horizontal scroll -- both are baked into `x` and
/// `chars`. Mixing the two is the classic off-by-scroll bug, so this function is the only
/// place the mapping exists and it never re-derives a screen column.
fn apply_row_styles(app: &App, buf: &mut Buffer, pass: RowStylePass<'_>) {
    let RowStylePass {
        x,
        cell_x,
        y,
        right,
        chars,
        line,
        start_col,
        syntax,
        on_cursor_line,
        palette,
    } = pass;
    if x >= right || chars.is_empty() {
        return;
    }
    let doc = app.doc();
    let tab_width = app.geometry.tab_width;
    let current_line_on = on_cursor_line && app.config.ui.current_line_highlight;
    let current_line = if current_line_on {
        Some(CoreStyle {
            bg: Some(np_core::palette::core(palette.current_line_bg)),
            ..CoreStyle::default()
        })
    } else {
        None
    };

    // Syntax spans describe the whole logical line, so flatten to a style per logical column.
    let syntax = syntax_columns(syntax.as_deref());
    // Selection bounds in logical coordinates, restricted to this line.
    let sel = doc.and_then(|d| d.selection).map(|s| s.bounds());

    // Match cells were already computed in screen space by `match_cells`, which owns the
    // multi-line and tab-width arithmetic. Reusing it keeps one definition of "is this cell a
    // match" instead of a second implementation that drifts.
    let matches = match_cells(app, line, start_col, chars.len(), 0);
    let match_at = |screen_x: u16| matches.iter().find(|c| c.x == screen_x).map(|c| c.current);

    for (i, _) in chars.iter().enumerate() {
        // The writer's own map, so a style lands on the cell its character was drawn into
        // rather than on the cell `x + i` would suggest.
        let screen_x = cell_x
            .get(i)
            .copied()
            .unwrap_or_else(|| x.saturating_add(i as u16));
        if screen_x >= right {
            break;
        }
        let logical_col = start_col + i;

        let syntax_style = syntax
            .get(logical_col)
            .copied()
            .filter(|st| *st != CoreStyle::default());

        let selection_style = sel.and_then(|(from, to)| {
            if span_covers(tab_width, doc, line, logical_col, from, to) {
                Some(CoreStyle {
                    bg: Some(np_core::palette::core(palette.selection_bg)),
                    // The selection always states its foreground. This is the one layer allowed to
                    // override a syntax span's color, and it has to be: the bar is a saturated
                    // accent chosen so that *its* ink is legible on it, and a keyword's own purple
                    // landing on that accent reads at 1.1:1. Measured -- no accent ground keeps
                    // arbitrary span colors readable, so the choice is between readable selected
                    // text and theme-faithful but unreadable selected text.
                    fg: Some(np_core::palette::core(palette.highlight_fg)),
                    ..CoreStyle::default()
                })
            } else {
                None
            }
        });

        let match_style = match_at(screen_x).map(crate::ui::layers::match_style);

        let composed = Layers {
            syntax: syntax_style.as_ref(),
            current_line: current_line.as_ref(),
            selection: selection_style.as_ref(),
            search_match: match_style.as_ref(),
            annotation: None,
        }
        .compose();
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x: screen_x, y }) {
            // `span_over` rather than the bare conversion: a syntax span legitimately carries no
            // background so the editor ground shows through, and now that the app owns that
            // ground it has to be named. Passing the conversion straight through left these
            // cells on the terminal's default, which is the reported bug in the text area.
            cell.set_style(crate::style::span_over(&composed, palette));
        }
    }
}

/// Flatten syntax spans into one style per logical character column.
///
/// Spans arrive as `(style, fragment)` pairs, so the column a style starts at is the running
/// character count of everything before it. Materializing per column costs one Vec per visible
/// row and makes the per-cell lookup a direct index instead of a search.
fn syntax_columns(spans: Option<&[(CoreStyle, String)]>) -> Vec<CoreStyle> {
    let Some(spans) = spans else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(64);
    for (st, frag) in spans {
        for _ in 0..frag.chars().count() {
            out.push(*st);
        }
    }
    out
}

/// True when logical column `col` on `line` is inside the selection `[from, to]`.
///
/// The selection is half-open at `to`, which is why the last position is excluded: a caret at
/// the end of a word is not a selection of the following character. A selection covering whole
/// lines must also light the line's trailing position, or the highlight stops one cell short
/// of where the selection actually is.
fn span_covers(
    tab_width: usize,
    doc: Option<&np_core::Document>,
    line: usize,
    col: usize,
    from: LineCol,
    to: LineCol,
) -> bool {
    let _ = tab_width;
    if line < from.line || line > to.line {
        return false;
    }
    let lo = if line == from.line { from.col } else { 0 };
    let hi = if line == to.line {
        to.col
    } else {
        // Not the last line of the selection: everything on this line is inside it.
        doc.map(|d| d.buffer.line_len_chars(line) + 1)
            .unwrap_or(col + 1)
    };
    col >= lo && col < hi
}

/// One run of cells that belongs to a search match, in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchCell {
    pub x: u16,
    pub y: u16,
    /// True for the match the caret is on, which gets the stronger of the two styles.
    pub current: bool,
}

/// Which cells of one visual row are covered by a search match (FR-S2).
///
/// Pure, and deliberately separate from drawing: the whole class of bug here is coordinate
/// arithmetic -- a match that highlights the wrong columns, or only the first line of a
/// multi-line match -- and that is assertable without a buffer. The renderer is three lines.
///
/// A match spanning several lines highlights every line it touches, from the match's start
/// column on its first line through the match's end column on its last. A multi-line match is
/// a normal outcome once `\n` is allowed in a pattern (FR-S4), so clipping it to the first
/// line would make the highlight disagree with the selection the same search produced.
pub fn match_cells(
    app: &App,
    row_line: usize,
    row_start_col: usize,
    row_char_len: usize,
    screen_row: u16,
) -> Vec<MatchCell> {
    let area = rect(app.regions.editor);
    let gutter = app.geometry.gutter_width as u16;
    let mut out = Vec::new();
    for m in match_candidates(app) {
        if row_line < m.start.line || row_line > m.end.line {
            continue;
        }
        // The row covers characters [row_start_col, row_start_col + row_char_len) of the
        // logical line. Intersect that with the match's columns on this line.
        let from = if row_line == m.start.line {
            m.start.col
        } else {
            0
        };
        let to = if row_line == m.end.line {
            m.end.col
        } else {
            // Through the end of the line, including the position just past the last
            // character, which is where a match ending at column 0 of the next line reaches.
            row_start_col + row_char_len
        };
        let row_from = from.max(row_start_col);
        let row_to = to.min(row_start_col + row_char_len);
        if row_to <= row_from {
            continue;
        }
        for col in row_from..row_to {
            let display = app
                .doc()
                .map(|d| d.buffer.display_col(row_line, col, app.geometry.tab_width))
                .unwrap_or(col);
            let x = area
                .x
                .saturating_add(gutter)
                .saturating_add(display as u16)
                .saturating_sub(app.doc().map(|d| d.scroll.left_col).unwrap_or(0) as u16);
            let y = area.y.saturating_add(screen_row);
            if x >= area.right() || y >= area.bottom() {
                continue;
            }
            out.push(MatchCell {
                x,
                y,
                current: app.find.current == Some(m),
            });
        }
    }
    out
}

/// The matches worth highlighting: every mark when marking is on, otherwise just the current
/// one. When marking is on the current match is still included, since a search can move the
/// caret to a match the mark list was derived from.
fn match_candidates(app: &App) -> Vec<np_core::Match> {
    let mut out = app.find.marks.clone();
    if let Some(cur) = app.find.current {
        if !out.contains(&cur) {
            out.push(cur);
        }
    }
    out
}

/// The terminal cell the caret should occupy, or `None` when it is scrolled out of view.
///
/// Returns screen coordinates, not buffer offsets, so the caller can hand them straight to
/// `frame.set_cursor_position`.
pub fn caret_position(app: &App) -> Option<LineCol> {
    let doc = app.doc()?;
    let geom = app.geometry;
    let rows = layout_visible_rows(&doc.buffer, &geom, doc.scroll.top_line, &doc.folds);
    let screen_row = geom.screen_row_of(&rows, doc.cursor.pos)?;
    let display_col =
        doc.buffer
            .display_col(doc.cursor.pos.line, doc.cursor.pos.col, geom.tab_width);
    let area = rect(app.regions.editor);
    let x = area
        .x
        .saturating_add(geom.gutter_width as u16)
        .saturating_add(display_col as u16)
        .saturating_sub(doc.scroll.left_col as u16);
    let y = area.y.saturating_add(screen_row);
    // The terminal's own cursor is placed wherever we point it, over anything that was drawn
    // there, so the scrollbar cells have to be refused rather than trusted to the scroll math
    // upstream. The refusal is expressed in viewport terms -- the same two numbers the row
    // layout and `ensure_caret_visible` use -- rather than against the band's edges, because
    // the band is exactly one row and one column bigger than the text, and a guard that let
    // the outermost band cells through put the cursor on the bar whenever only one bar existed.
    if x >= area
        .x
        .saturating_add(geom.gutter_width as u16)
        .saturating_add(geom.viewport_cols() as u16)
        || y >= area.y.saturating_add(geom.viewport_rows() as u16)
    {
        return None;
    }
    Some(LineCol {
        line: y as usize,
        col: x as usize,
    })
}

/// Everything `draw_whitespace` needs, bundled so the call site stays readable and the
/// signature stays under the argument limit that keeps a helper easy to check at a glance.
struct WhitespaceDraw<'a> {
    /// Screen column to draw at.
    x: u16,
    y: u16,
    /// Exclusive right edge of the text area.
    right: u16,
    text: &'a str,
    show_ws: bool,
    show_eol: bool,
    tab_width: usize,
    palette: &'a ThemePalette,
}

/// Marker glyphs for FR-E14. A middle dot for a space, a right arrow for a tab, and a
/// pilcrow at end of line.
const SPACE_MARK: char = '\u{b7}';
const TAB_MARK: char = '\u{2192}';
const EOL_MARK: char = '\u{b6}';

/// Draw a line with whitespace made visible (spec FR-E14).
///
/// Strictly a rendering concern: the buffer keeps its real spaces and tabs, and only the
/// cells written here change. Each substitution occupies exactly one cell, the same as the
/// character it replaces, so caret positioning and hit-testing need no special case. A tab
/// still consumes its full expansion width; the arrow marks the first cell and the rest stay
/// blank, which is how most editors draw it and keeps the row from shifting right.
fn draw_whitespace(buf: &mut Buffer, at: WhitespaceDraw<'_>) {
    let WhitespaceDraw {
        mut x,
        y,
        right,
        text,
        show_ws,
        show_eol,
        tab_width,
        palette,
    } = at;
    let start_x = x;
    // Markers are annotation, not content: dim ink on the editor ground. Precomputed rather
    // than `Modifier::DIM`, which asks the terminal to dim and therefore dims toward *its*
    // default -- toward black on a dark theme, and toward black on a light one too, where the
    // ground is already near white and the mark simply vanishes.
    let mark = crate::style::ink(palette.gutter_fg, palette.background);
    for ch in text.chars() {
        if x >= right {
            break;
        }
        if ch == '\t' {
            // A tab occupies several cells. The arrow marks the first and the rest stay
            // blank, which is how tabs are conventionally drawn and keeps the row from
            // shifting. Width comes from the tab stop the cursor is at.
            let cells = tab_cells(x - start_x, tab_width);
            let glyph = if show_ws { TAB_MARK } else { ' ' };
            x = draw_str(buf, x, y, &glyph.to_string(), mark);
            for _ in 1..cells {
                if x >= right {
                    break;
                }
                x = draw_str(buf, x, y, " ", mark);
            }
            continue;
        }
        if ch == ' ' && show_ws {
            x = draw_str(buf, x, y, &SPACE_MARK.to_string(), mark);
            continue;
        }
        x = draw_str(buf, x, y, &ch.to_string(), crate::style::text(palette));
    }
    if show_eol && x < right {
        draw_str(buf, x, y, &EOL_MARK.to_string(), mark);
    }
}

/// The screen column each character of `text` occupies, starting at `x`.
///
/// The plain-text branch of the row writer fills one cell for an ordinary character and
/// `tab_cells` cells for a tab, so a line containing tabs is wider in cells than in
/// characters. Every layer that styles cells afterwards needs this same map: deriving a
/// position as `x + index` puts each character after a tab progressively further left of
/// where it was drawn, which shows up as syntax colors and selection running off behind the
/// text. One function, computed beside the writer, is the only way the two cannot drift.
fn cell_columns(x: u16, text: &[char], tab_width: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len());
    let mut col = x;
    for ch in text {
        out.push(col);
        col = col.saturating_add(if *ch == '\t' {
            tab_cells(col - x, tab_width) as u16
        } else {
            1
        });
    }
    out
}

/// Cells a tab occupies when it starts at cell `col`.
fn tab_cells(col: u16, tab_width: usize) -> usize {
    let tab = tab_width.max(1) as u16;
    let next = (col / tab + 1) * tab;
    next.saturating_sub(col) as usize
}
