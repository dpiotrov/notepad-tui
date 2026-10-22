//! The Find in Files bottom panel (spec §6.5 FR-FF1, FR-FF3).
//!
//! # Layout
//!
//! The panel owns the bottom `app.find_panel_height` rows of the editor region and spans its
//! full width. Rows, top to bottom: the header counters, the three input fields, a separator,
//! then the results. The row math lives in [`result_rows`] and nowhere else, because the mouse
//! layer has to agree with the renderer about which row a click landed on -- the same rule §6.9
//! states for the tab bar.
//!
//! # Highlighting inside a result line
//!
//! The match is highlighted by *style*, not by rewriting the text, for the same reason the S10
//! match overlay works that way: the row must show the file's text exactly. The highlight spans
//! `col..col + match_len` in characters, which is why [`PanelHit`] carries `match_len` -- the
//! alternative is re-running the regex per row per frame to find out how wide the match was.
//!
//! # Wide characters
//!
//! Columns are character offsets, but the screen is cells. A hit whose line contains CJK text
//! would have its highlight land early if the span were computed in characters, so the draw
//! converts with `unicode_width` while walking. The *caret* jump uses the character column,
//! which is what the buffer wants; only the highlight is in cells.

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::fif::PanelHit;
use crate::ui::menu_bar::{draw_str, rect};

/// Default panel height in rows when a search opens it.
pub const PANEL_HEIGHT: u16 = 10;

/// Rows above the result list: header, three fields, separator.
pub const HEADER_ROWS: u16 = 5;

/// The column width a field label is padded to, and the two after it (`": "`) where its value
/// begins. [`draw_field`] formats with this and [`cursor_position`] measures with it, so the
/// terminal cursor lands under the text the painter wrote rather than a column or two off.
pub const FIELD_LABEL_WIDTH: usize = 6;

/// The match inside a result line.
const MATCH_STYLE: Style = Style::new()
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::UNDERLINED);

/// How many result rows a panel of `height` rows can draw.
///
/// The single source of truth for "which row is result N", shared by the renderer and the
/// click router.
pub fn result_rows(height: u16) -> usize {
    if height <= HEADER_ROWS {
        return 0;
    }
    (height - HEADER_ROWS) as usize
}

/// The panel's rectangle: the bottom `find_panel_height` rows of the editor region.
pub fn panel_rect(app: &App) -> np_core::RectCore {
    // The band, not `editor`: `editor` is the band minus this panel, so anchoring here to
    // `editor` would put the panel directly beneath the text area -- and off the bottom of the
    // band -- rather than flush with its bottom edge.
    let band = app.regions.band;
    let h = app.find_panel_height.min(band.h);
    np_core::RectCore::new(band.x, band.y + band.h.saturating_sub(h), band.w, h)
}

/// The panel field on the row `screen_row`, if that row is a field.
///
/// The field rows are fixed relative to the panel top -- query, then directory, then the
/// glob-and-toggles row -- which is the same order and spacing `draw` walks. Deriving one from
/// the other is what keeps a click on the glob field from landing on the directory field.
pub fn field_at_row(app: &App, screen_row: u16) -> Option<crate::fif::FifField> {
    let panel = panel_rect(app);
    if screen_row <= panel.y {
        return None;
    }
    match screen_row - panel.y {
        1 => Some(crate::fif::FifField::Query),
        2 => Some(crate::fif::FifField::Dir),
        3 => Some(crate::fif::FifField::Glob),
        _ => None,
    }
}

/// Where the terminal's cursor belongs while the panel owns the keyboard.
///
/// Same reasoning as `find_bar::cursor_position`: the terminal has one real cursor and
/// `draw_if_dirty` points it at the document caret, so a panel that owns the keyboard shows no
/// cursor at all in the field being typed into.
///
/// `None` when the keyboard is driving the results list rather than a field (`editing == false`)
/// -- there is no text position to show, and the selected result row is already highlighted.
pub fn cursor_position(app: &App) -> Option<(u16, u16)> {
    // The panel must both be up and be in field-editing mode. When the keyboard is driving the
    // results list there is no text position to show, and the selected row is already
    // highlighted, so pointing the terminal cursor at a field would contradict the highlight.
    if !matches!(app.mode(), crate::app::Mode::FindPanel) || !app.fif.editing {
        return None;
    }
    let panel = panel_rect(app);
    if panel.w == 0 || panel.h < 4 {
        return None;
    }
    // `(row within the panel, the label the painter writes, the field's text)`. The rows are the
    // same three `draw` walks, in the same order.
    let (row, label, value) = match app.fif.editing_field {
        crate::fif::FifField::Query => (1usize, "Find", app.fif.query.as_str()),
        crate::fif::FifField::Dir => (2, "Dir", app.fif.dir.as_str()),
        crate::fif::FifField::Glob => (3, "Files", app.fif.glob.as_str()),
    };
    if row >= panel.h as usize {
        return None;
    }
    // `draw_field` writes `format!("{label:<6}: {value}")`: the label padded to six, then ": ".
    // Deriving the offset from the same label the painter uses is what keeps the cursor under
    // the text rather than a column or two off if a label is ever renamed.
    // Padded exactly as `draw_field` pads it, so the offset cannot drift from the painted row.
    let value_start = label.len().max(FIELD_LABEL_WIDTH) + 2;
    let want = value_start + value.chars().count();
    let limit = panel.w.saturating_sub(1) as usize;
    Some((
        panel.x.saturating_add(want.min(limit) as u16),
        panel.y + row as u16,
    ))
}

/// Which result row a screen row maps to, or `None`.
///
/// Used by the click router. Kept next to [`result_rows`] so the two cannot disagree.
pub fn row_index_at(app: &App, screen_row: u16) -> Option<usize> {
    let panel = panel_rect(app);
    if screen_row < panel.y + HEADER_ROWS || screen_row >= panel.bottom() {
        return None;
    }
    let visible = (screen_row - (panel.y + HEADER_ROWS)) as usize;
    Some(app.fif.scroll + visible)
}

/// Draw the panel.
pub fn draw(app: &App, buf: &mut Buffer) {
    let area = rect(panel_rect(app));
    if area.width < 4 || area.height < 2 {
        return;
    }
    let width = area.width as usize;
    let pal = app.highlight.palette();
    let bold = crate::style::chrome(&pal).add_modifier(Modifier::BOLD);
    // Service lines, not text: the band keeps them from reading as buffer content.
    crate::ui::framework::draw_chrome_band(buf, panel_rect(app), &pal);

    // Header (FR-FF2).
    draw_str(buf, area.x, area.y, &crate::fif::header(app), bold);

    // Fields. Each is `Label: value`, truncated to the panel width. The field the keyboard is in
    // gets the focus ground first, so the live one is obvious even where a cursor cannot be --
    // an empty value, an unfocused terminal, a terminal that draws the cursor underneath.
    let mut row = area.y + 1;
    draw_field(PanelField {
        buf,
        x: area.x,
        y: row,
        label: "Find",
        value: &app.fif.query,
        width,
        focused: is_field_focused(app, crate::fif::FifField::Query),
        palette: &pal,
    });
    row = row.saturating_add(1);
    draw_field(PanelField {
        buf,
        x: area.x,
        y: row,
        label: "Dir",
        value: &app.fif.dir,
        width,
        focused: is_field_focused(app, crate::fif::FifField::Dir),
        palette: &pal,
    });
    row = row.saturating_add(1);
    // The glob shares its row with the toggles, so it cannot be drawn by `draw_field` and cannot
    // be drawn as one string either. `toggles_line` returns the whole row including the boxes, so
    // painting that row with the focus style laid the highlight under the toggle boxes as well --
    // a focused `Files` field looked like it owned the checkboxes. The ground and the text are
    // both clipped to where the first box starts, and the boxes are then painted over the top.
    let (glob_text, toggle_spans) = toggles_line(app);
    let glob_focused = is_field_focused(app, crate::fif::FifField::Glob);
    let glob_end = toggle_spans
        .first()
        .map(|(start, _, _)| *start)
        .unwrap_or(width as u16)
        .min(width as u16);
    if glob_focused {
        crate::ui::framework::draw_field_focus_ground(buf, area.x, row, area.x + glob_end, &pal);
    }
    let glob_only: String = glob_text.chars().take(glob_end as usize).collect();
    draw_str(
        buf,
        area.x,
        row,
        &glob_only,
        if glob_focused {
            crate::ui::framework::field_focus_style(&pal)
        } else {
            crate::style::chrome(&pal)
        },
    );
    // The toggle boxes, painted over the row so each one can carry its own style. A focused
    // toggle gets the ground too: the boxes are now in the Tab order, and a Tab stop with no
    // visible stop is the same complaint that opened this round.
    let (cmd_spans, cmd_text) = command_spans(app);
    let cmd_start = cmd_spans
        .first()
        .map(|(c, _, _)| *c)
        .unwrap_or(width as u16);
    // The command words. They are appended to the toggles' own text by `command_spans`, so the
    // slice from the first command onward is exactly what belongs on the row -- and painting it is
    // not optional: switching this row to `toggles_line` briefly made the whole command text
    // disappear, taking `> Search` and `< Close` off the screen.
    if !cmd_spans.is_empty() {
        let from = cmd_start as usize;
        let words: String = cmd_text.chars().skip(from).collect();
        draw_str(
            buf,
            area.x + cmd_start,
            row,
            &truncate(&words, width),
            crate::style::chrome(&pal),
        );
    }
    for (start, _, toggle) in &toggle_spans {
        let hot = app.fif.focused_toggle == Some(*toggle);
        let style = if hot {
            crate::ui::framework::field_focus_style(&pal)
        } else {
            crate::style::chrome(&pal)
        };
        if hot {
            // The box and its label together, so the highlight is the whole word rather than a
            // patch over the brackets.
            crate::ui::framework::draw_field_focus_ground(
                buf,
                area.x + start,
                row,
                area.x + start + word_width(*toggle) as u16,
                &pal,
            );
        }
        let word = box_flag(toggle.get(app), toggle_label(*toggle));
        draw_str(buf, area.x + start, row, &word, style);
    }
    row = row.saturating_add(1);

    // Error line, or the separator when there is none.
    match &app.fif.error {
        Some(err) => {
            let text = truncate(&format!("! {err}"), width);
            draw_str(buf, area.x, row, &text, bold);
        }
        None => {
            // The rule doubles as the place the hint lives. The toggles' own row is already full
            // at any sane width -- the three boxes plus two commands leave about three cells -- so
            // an attempt to fit the advice there simply never drew it. The separator has the whole
            // row to spare, and it is the one row that carries no information of its own.
            let sep = "─".repeat(width);
            draw_str(buf, area.x, row, &sep, crate::style::chrome_dim(&pal));
            let hint = USAGE_HINT.chars().count();
            if hint + 1 < width {
                let hx = area.x + (width - hint) as u16;
                draw_str(buf, hx, row, USAGE_HINT, crate::style::chrome_dim(&pal));
            }
        }
    }

    // Results.
    let rows = result_rows(app.find_panel_height);
    let first = app.fif.scroll;
    for visible in 0..rows {
        let y = area.y + HEADER_ROWS + visible as u16;
        if y >= area.bottom() {
            break;
        }
        let Some(index) = first.checked_add(visible) else {
            break;
        };
        let Some(hit) = app.fif.hits.get(index) else {
            break;
        };
        draw_hit(buf, area.x, y, hit, index == app.fif.selected, width, &pal);
    }
}

/// One result row: `path:line:col: text`, with the match styled.
fn draw_hit(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    hit: &PanelHit,
    selected: bool,
    width: usize,
    palette: &np_core::ThemePalette,
) {
    let location = hit.location();
    let body = format!("{location} {}", hit.text);
    let text = truncate(&body, width);
    let base = if selected {
        crate::style::highlight(palette)
    } else {
        crate::style::chrome(palette)
    };
    draw_str(buf, x, y, &text, base);

    // Highlight the match. The location prefix occupies cells before the text begins, so the
    // span is offset by the prefix's *display width*, not its character count.
    let prefix_display = display_width(&location);
    let lead = hit.col.saturating_sub(1);
    let start_cells =
        prefix_display + 1 + display_width(&hit.text.chars().take(lead).collect::<String>());
    let len_cells = display_width(
        &hit.text
            .chars()
            .skip(lead)
            .take(hit.match_len)
            .collect::<String>(),
    );
    if len_cells == 0 {
        return;
    }
    for cell in 0..len_cells {
        let cx = x + start_cells as u16 + cell as u16;
        if cx >= x + width as u16 {
            break;
        }
        if let Some(slot) = buf.cell_mut((cx, y)) {
            slot.set_style(MATCH_STYLE);
        }
    }
}

/// A field row: `Label: value`, clipped to the panel width.
/// Whether this field currently takes typing.
///
/// `editing` is the whole question: while the keyboard drives the results list no field is live,
/// and painting one as focused would point the user at a field that ignores them.
pub fn is_field_focused(app: &App, field: crate::fif::FifField) -> bool {
    app.fif.editing && app.fif.editing_field == field
}

/// One panel field and everything the painter needs to draw it.
///
/// Grouped because the positional version reached eight arguments once the palette was added, and
/// clippy is right that a signature like that is where a caller silently swaps two `&str`s. The
/// Find bar's `FieldRow` is the same shape for the same reason.
struct PanelField<'a> {
    buf: &'a mut Buffer,
    x: u16,
    y: u16,
    label: &'a str,
    value: &'a str,
    width: usize,
    focused: bool,
    palette: &'a np_core::ThemePalette,
}

fn draw_field(f: PanelField<'_>) {
    let PanelField {
        buf,
        x,
        y,
        label,
        value,
        width,
        focused,
        palette,
    } = f;
    // The ground spans the full row, not just the text: an empty field is the case where a
    // text-width highlight would show nothing at all.
    if focused {
        crate::ui::framework::draw_field_focus_ground(buf, x, y, x + width as u16, palette);
    }
    let line = format!("{label:<FIELD_LABEL_WIDTH$}: {value}");
    let style = if focused {
        crate::ui::framework::field_focus_style(palette)
    } else {
        crate::style::chrome(palette)
    };
    draw_str(buf, x, y, &truncate(&line, width), style);
}

/// The panel's clickable words on the toggles row: the three search commands and Close.
///
/// `(column, width, command)`. The commands are the strings `hit_command` maps back to an action,
/// so a click and the drawn word are the same computation -- the same rule the Find strip follows
/// with `options_spans`.
///
/// # Why a Search button
///
/// `Enter` starts the search and always has, but nothing on screen said so; the report was that
/// the panel "has no start button". A word on the row is the cheapest affordance that makes the
/// gesture obvious without taking another row from the text.
///
/// # Why Close on both strips
///
/// Same complaint: the tool areas should carry an obvious way out. It sits at the far right of the
/// last row on every strip, so it is always in the same place.
pub fn command_spans(app: &App) -> (Vec<(u16, u16, &'static str)>, String) {
    let (mut text, _) = toggles_line(app);
    let mut spans: Vec<(u16, u16, &'static str)> = Vec::new();
    let strip_w = panel_rect(app).w;
    let mut col = text.chars().count() as u16 + 3;
    // `>` on the command and `<` on the way out, matching the Find strip and the menu arrows, so
    // the two service strips read as the same kind of control.
    for cmd in ["> Search", "< Close"] {
        let w = cmd.chars().count() as u16;
        if strip_w <= col + w {
            break;
        }
        // Pad so the drawn string and the span agree on where the word starts.
        while (text.chars().count() as u16) < col {
            text.push(' ');
        }
        spans.push((col, w, cmd));
        text.push_str(cmd);
        col += w + 3;
    }
    (spans, text)
}

/// The toggle boxes' `(column, width, toggle)` on the toggles row, relative to the panel.
///
/// Public so a test can assert the painted box and the clickable box are the same rectangle
/// rather than trusting that they are.
pub fn toggle_spans(app: &App) -> (String, Vec<(u16, u16, crate::fif::FifToggle)>) {
    toggles_line(app)
}

/// The hint that tells the user the boxes are reachable, printed after the last box.
///
/// A control nobody can find does not exist. The boxes moved into the `Tab` cycle this round, and
/// a cycle that is not written down is still undiscoverable -- so the row says what it takes to
/// use it. Kept short and to the right because the row also carries the commands, and a hint that
/// pushes `< Close` off the edge is worse than no hint.
pub const USAGE_HINT: &str = "Tab/Space or Ctrl+C/W/G";

/// Which toggle the pointer is over, if it is over one.
///
/// Same row as the commands and the glob field, so the three hit-tests are tried in a fixed order
/// by the mouse layer: command, toggle, field. Any other order and a click on a box would be
/// answered as "focus the glob field", which is what this used to do.
pub fn hit_toggle(app: &App, x: u16, y: u16) -> Option<crate::fif::FifToggle> {
    let panel = panel_rect(app);
    if panel.h < 4 || y != panel.y + 3 {
        return None;
    }
    let (_, spans) = toggles_line(app);
    let offset = x.checked_sub(panel.x)?;
    spans
        .into_iter()
        .find(|(start, w, _)| offset >= *start && offset < start.saturating_add(*w))
        .map(|(_, _, toggle)| toggle)
}

/// Which command the pointer is over on the toggles row, if any.
pub fn hit_command(app: &App, x: u16, y: u16) -> Option<&'static str> {
    let panel = panel_rect(app);
    if panel.h < 4 || y != panel.y + 3 {
        return None;
    }
    let (spans, _) = command_spans(app);
    let offset = x.checked_sub(panel.x)?;
    spans
        .into_iter()
        .find(|(start, w, _)| offset >= *start && offset < start.saturating_add(*w))
        .map(|(_, _, cmd)| cmd)
}

/// The label the panel paints for a toggle.
///
/// Keyed by the toggle rather than held in a list that has to stay in screen order, because a
/// positional list is exactly how a click on one box ends up flipping another.
pub fn toggle_label(toggle: crate::fif::FifToggle) -> &'static str {
    match toggle {
        crate::fif::FifToggle::Case => "match case",
        crate::fif::FifToggle::Word => "word",
        crate::fif::FifToggle::Gitignore => "gitignore",
    }
}

/// The glob field and the toggles, on one row to keep the panel short.
///
/// Returns the row's text and the `(column, width)` of each toggle, relative to the panel's left
/// edge. The columns are produced while the string is being built, which is what keeps a click on
/// a box landing on that box: the S20 report was that the three boxes could not be clicked at all,
/// because the row was one opaque string with nothing to hit them by.
fn toggles_line(app: &App) -> (String, Vec<(u16, u16, crate::fif::FifToggle)>) {
    let glob = if app.fif.glob.trim().is_empty() {
        "*".to_string()
    } else {
        app.fif.glob.trim().to_string()
    };
    let mut text = format!("Files : {glob}");
    let mut spans = Vec::new();
    for toggle in crate::fif::FifToggle::ALL {
        let word = box_flag(toggle.get(app), toggle_label(toggle));
        // Pad to the gap first so the span start and the painted word cannot drift apart.
        text.push_str("   ");
        let start = text.chars().count() as u16;
        text.push_str(&word);
        spans.push((start, word.chars().count() as u16, toggle));
    }
    (text, spans)
}

fn box_flag(on: bool, label: &str) -> String {
    format!("[{}] {label}", if on { "x" } else { " " })
}

/// How many cells a toggle occupies on the row, box and label included.
fn word_width(toggle: crate::fif::FifToggle) -> usize {
    box_flag(true, toggle_label(toggle)).chars().count()
}

/// Display width in terminal cells: wide characters count two, combining marks zero.
fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate to `width` *characters*, appending an ellipsis when something was dropped.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let keep = width - 1;
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}
