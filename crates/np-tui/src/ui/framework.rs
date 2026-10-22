//! The dialog framework (FR-U4): geometry, backdrop, focus, and shared widget primitives.
//!
//! # The problem it solves
//!
//! Before S14 every dialog was drawn by hand and owned its keyboard in a `match` inside
//! `App::handle_key`. That produced five subtly different focus rules: the Find dialog
//! cycled two text fields and nothing else, the pickers had no `Tab` at all, Go To had no
//! focus concept whatsoever, and no dialog could be operated with only a mouse because none
//! of them knew which cell its own widgets occupied. This module makes both rules -- where
//! a widget is, and what `Tab` does -- a property of the dialog's declared child list, so
//! they cannot disagree with what gets drawn.
//!
//! # The contract a dialog follows
//!
//! 1. Build a [`DialogLayout`] from the frame: it centers a box and dims what is behind it.
//! 2. Declare focusable children in tab order as [`FocusItem`]s, each with its rectangle
//!    from [`DialogLayout::row`].
//! 3. Route keys through [`focus_cycle`]; route mouse presses through
//!    [`focus_at`] and [`DialogLayout::inside`].
//! 4. Draw with the primitives here: [`draw_field`], [`draw_checkbox`], [`draw_button`],
//!    [`draw_list`].
//!
//! Nothing else may hand-roll focus. `Enter` runs the layout's default item and `Esc` pops
//! exactly one `Mode` level, both of which are the framework's job, so a dialog that
//! forgets them still behaves.
//!
//! # Why focus is an index into a declared list, not a widget handle
//!
//! `App` is cloned wholesale by tests and holds no widget tree; dialogs are data
//! (`App::find`, `App::goto_input`) rendered on demand. A widget-object focus model would
//! mean materializing widgets every frame and storing identity across frames, and the
//! borrow checker makes "the focused widget object" a hazard on a struct this size. An
//! index into a list the dialog rebuilds identically each frame is honest about that:
//! [`FocusItem`]s are cheap values, and the only state is `App::dialog_focus: usize`.
//!
//! # Backdrop
//!
//! ratatui 0.30 has no `Dimmed` widget, and `Modifier::DIM` applied per cell is the
//! portable dim: it works on a 16-color terminal where a blended background color would
//! not. [`dim_region`] dims every cell in a region in place, which is why it runs *after*
//! the base frame is drawn and *before* the box is drawn on top.

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use np_core::{RectCore, ThemePalette};

use super::menu_bar::{draw_str, rect};
use super::text_input::TextInput;

// The dialog style helpers take a palette.
//
// These used to be `const`s built from `Style::default()` plus an ANSI color name, which meant a
// dialog's title, hints, errors and focus ring were all half-colored: the ground came from the
// terminal and so did the text, except where an ANSI name like `LightRed` pinned one channel to a
// color the theme never chose. On a light emulator the hint text went dark on a dark box, and the
// error red was whatever the user's palette called "light red" -- bright on some, near-invisible
// on others.
//
// They are functions of the palette now. There is no palette-free spelling left to call, which is
// the point: an omitted color used to be a legal program.

/// Style for a dialog title.
pub fn title_style(palette: &ThemePalette) -> Style {
    crate::style::dialog(palette).add_modifier(Modifier::BOLD)
}
/// Style for a hint line: secondary ink on the dialog ground.
pub fn hint_style(palette: &ThemePalette) -> Style {
    let ink = np_core::dim_ink(
        palette.dialog_bg,
        palette.dialog_fg,
        np_core::MIN_UI_CONTRAST,
    );
    crate::style::ink(ink, palette.dialog_bg).add_modifier(Modifier::DIM)
}
/// Style for an inline error inside a dialog.
pub fn error_style(palette: &ThemePalette) -> Style {
    crate::style::error_on(palette, palette.dialog_bg).add_modifier(Modifier::BOLD)
}
/// Style for a focused widget's frame and label.
pub fn focus_style(palette: &ThemePalette) -> Style {
    crate::style::ink(palette.accent_fg, palette.dialog_bg).add_modifier(Modifier::BOLD)
}

/// A centered dialog box over a dimmed backdrop (FR-U4).
///
/// Computed from the frame every draw rather than stored: a resize during a dialog then
/// needs no handling at all, which is worth more than the arithmetic it costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogLayout {
    /// The box itself, border included.
    pub area: RectCore,
    /// Interior: `area` inset by one on every side.
    pub inner: RectCore,
}

impl DialogLayout {
    /// Center a `want_w` x `want_h` box inside `screen`, shrinking to fit.
    ///
    /// The box never exceeds the screen: a dialog taller than the terminal would have its
    /// bottom border clipped and its buttons unreachable, which is worse than a cramped
    /// dialog. Width clamps to at least 3 cells so the borders survive a 1-column screen.
    pub fn centered(screen: RectCore, want_w: u16, want_h: u16) -> DialogLayout {
        let w = want_w
            .min(screen.w.saturating_sub(0))
            .max(1)
            .min(screen.w.max(1));
        let h = want_h.min(screen.h).max(1).min(screen.h.max(1));
        let x = screen.x + screen.w.saturating_sub(w) / 2;
        let y = screen.y + screen.h.saturating_sub(h) / 2;
        let area = RectCore::new(x, y, w, h);
        let inner = RectCore::new(
            area.x + 1,
            area.y + 1,
            area.w.saturating_sub(2),
            area.h.saturating_sub(2),
        );
        DialogLayout { area, inner }
    }

    /// Center inside the whole app frame (menu row through status row).
    pub fn for_app(app: &super::super::app::App, want_w: u16, want_h: u16) -> DialogLayout {
        let screen = RectCore::new(0, 0, app.regions.menu.w, app.regions.status.bottom());
        DialogLayout::centered(screen, want_w, want_h)
    }

    /// The row `index` inside the interior, so a dialog's children line up without any
    /// caller adding the border offset by hand.
    pub fn row(&self, index: u16) -> RectCore {
        RectCore::new(
            self.inner.x,
            self.inner.y.saturating_add(index),
            self.inner.w,
            1,
        )
    }

    /// True when `(x, y)` is inside the box, border included.
    pub fn inside(&self, x: u16, y: u16) -> bool {
        self.area.contains(x, y)
    }

    /// Draw the border and background, assuming the backdrop is already dimmed.
    ///
    /// The box paints its own interior with the default background so a dialog is readable
    /// over arbitrary text; without this the document shows through the dialog's gaps.
    pub fn draw_box(&self, buf: &mut Buffer, title: &str, palette: &ThemePalette) {
        let area = rect(self.area);
        if area.width < 2 || area.height < 2 {
            return;
        }
        // Interior fill first, so the border overwrites the corners.
        for y in self.inner.y..self.inner.bottom() {
            for x in self.inner.x..self.inner.right() {
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
                    cell.set_symbol(" ");
                    cell.set_style(crate::style::dialog(palette));
                }
            }
        }
        let top = rect(RectCore::new(self.area.x, self.area.y, self.area.w, 1));
        let bottom = rect(RectCore::new(
            self.area.x,
            self.area.bottom().saturating_sub(1),
            self.area.w,
            1,
        ));
        // The title is written to the top border only, so only the top border may leave a
        // gap for it. The original version skipped the title zone on both rows, which left a
        // hole punched in the bottom border under the title -- visible as a broken frame line
        // whenever a dialog had a title, and easy to mistake for the file listing overlapping
        // the box.
        let title_from = title_start(self.area, title);
        let title_to = title_from + title.chars().count() as u16 + 2;
        for x in top.x..top.right() {
            let on_top = title.is_empty() || x < title_from || x >= title_to;
            if on_top {
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y: top.y }) {
                    cell.set_symbol("\u{2500}");
                    cell.set_style(crate::style::border(palette));
                }
            }
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y: bottom.y }) {
                cell.set_symbol("\u{2500}");
                cell.set_style(crate::style::border(palette));
            }
        }
        for y in self.area.y + 1..self.area.bottom().saturating_sub(1) {
            for (x, sym) in [
                (self.area.x, "\u{2502}"),
                (self.area.right().saturating_sub(1), "\u{2502}"),
            ] {
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
                    cell.set_symbol(sym);
                    cell.set_style(crate::style::border(palette));
                }
            }
        }
        for (pos, sym) in [
            (
                ratatui::layout::Position {
                    x: self.area.x,
                    y: self.area.y,
                },
                "\u{250c}",
            ),
            (
                ratatui::layout::Position {
                    x: self.area.right().saturating_sub(1),
                    y: self.area.y,
                },
                "\u{2510}",
            ),
            (
                ratatui::layout::Position {
                    x: self.area.x,
                    y: self.area.bottom().saturating_sub(1),
                },
                "\u{2514}",
            ),
            (
                ratatui::layout::Position {
                    x: self.area.right().saturating_sub(1),
                    y: self.area.bottom().saturating_sub(1),
                },
                "\u{2518}",
            ),
        ] {
            if let Some(cell) = buf.cell_mut(pos) {
                cell.set_symbol(sym);
                cell.set_style(crate::style::border(palette));
            }
        }
        if !title.is_empty() {
            let label = format!(" {title} ");
            draw_str(
                buf,
                title_start(self.area, title),
                self.area.y,
                &label,
                title_style(palette),
            );
        }
        let _ = area;
    }
}

/// Where a centered dialog title starts on the top border.
fn title_start(area: RectCore, title: &str) -> u16 {
    let label = title.chars().count() as u16 + 2;
    area.x + area.w.saturating_sub(label) / 2
}

/// Paint a strip's background so chrome reads as chrome rather than as text.
///
/// The tab bar, the menu bar, the status bar and the docked service lines all sit directly
/// beside the text area, and with no background of their own they were the same colour as
/// the buffer: the tab row read as another line of the file, and a search panel's rows looked
/// like text that happened to mention "Find". This is the one place that answer is set, so
/// every strip agrees.
///
/// The background is a named colour rather than `Modifier::REVERSED`. Reversal would have
/// been theme-proof, but it inverts whatever the row already uses to mean "selected": the
/// active tab, the highlighted menu row and the selected search result are all reversed, and
/// on a reversed band they would read as the *un*selected ones. A named colour keeps those
/// modifiers meaningful.
///
/// Text on the band is drawn with the caller's own style, so existing selection and dim
/// rules keep working unchanged.
pub fn draw_chrome_band(buf: &mut Buffer, region: RectCore, palette: &ThemePalette) {
    if region.w == 0 || region.h == 0 {
        return;
    }
    let style = crate::style::chrome(palette);
    for y in region.y..region.bottom() {
        for x in region.x..region.right() {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
                cell.set_symbol(" ");
                cell.set_style(style);
            }
        }
    }
}

/// The chrome strip style: a ground a step off the text area, with ink that is legible on it.
///
/// # Why both channels, when only the ground was ever mentioned
///
/// This function used to return `Style::default().bg(CHROME_BG)` -- a ground and nothing else.
/// The foreground was left for the terminal to supply, which is the exact mechanism of the
/// reported failure: a user switched their emulator to a light palette, the terminal's default
/// foreground went dark, and every label on the menu bar, tab bar, status bar and service lines
/// became dark text on the app's hardcoded dark band. Nothing in npt was wrong on its own terms;
/// it had simply outsourced half of every color to a setting the user was free to change.
///
/// A band is not a background. It is a background *and* the text on it, and both have to come
/// from the theme or the pair cannot be reasoned about at all.
pub fn chrome_style(palette: &ThemePalette) -> Style {
    crate::style::chrome(palette)
}

/// The ground under a field that currently has the keyboard.
///
/// # Why a background at all
///
/// The terminal has one real cursor, and pointing it into a field is the natural focus signal --
/// but it is not enough on its own, and was reported as such twice. A cursor is one cell wide, so
/// on an empty field it is a blinking block the size of nothing; and it disappears entirely
/// whenever the terminal loses focus, is scrolled, or is a terminal that draws the cursor under
/// the cell rather than over it. A user comparing two nearly identical fields could not tell
/// which one their typing would reach.
///
/// So focus gets a ground as well as a cursor: the field row reads as the live one, and it
/// survives everything that hides a cursor.
///
/// # Why this value
///
/// One visible step above `CHROME_BG` and below `CURRENT_LINE_BG`, so a focused field is clearly
/// lighter than its strip but never as loud as the editor's current line. Fixed RGB for the same
/// reason as `CHROME_BG`: the eight ANSI greys are all miles apart, and `REVERSED` is unavailable
/// because reversal is how selections are already drawn.
/// Style for a field row that has the keyboard.
///
/// Both channels again, for the same reason as [`chrome_style`]. A focused field is the one
/// surface the user is about to type into, so its ink must not be the terminal's guess.
pub fn field_focus_style(palette: &ThemePalette) -> Style {
    crate::style::ink(palette.dialog_fg, palette.field_focus_bg)
}

/// Lay the focus ground under one field row, from `x` to `right` exclusive.
///
/// Painted *before* the field's text so the text keeps its own foreground and only the ground
/// changes. The row gets the ground rather than just the characters typed so far: a field the user
/// has not typed into yet is exactly the case where a one-cell cursor is hardest to see, and a
/// short highlight over an empty value would say nothing.
pub fn draw_field_focus_ground(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    right: u16,
    palette: &ThemePalette,
) {
    for cx in x..right {
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x: cx, y }) {
            cell.set_style(
                cell.style()
                    .bg(crate::style::to_color(palette.field_focus_bg)),
            );
        }
    }
}

/// Dim every cell in `region` in place (FR-U4 "dimmed backdrop").
///
/// `Modifier::DIM` rather than a blended background: it survives a 16-color terminal, it
/// does not destroy the underlying symbol, and it composes with whatever the base frame
/// drew instead of replacing it.
pub fn dim_region(buf: &mut Buffer, region: RectCore, palette: &ThemePalette) {
    // The ground every dimmed cell is measured against. Cells that already carry a background --
    // syntax colors, the chrome band -- keep it and only gain the DIM attribute; cells that name
    // no background get the theme's, because an unset background here means the backdrop shows the
    // emulator's default rather than the document's ground.
    let ground = palette.background;
    for y in region.y..region.bottom() {
        for x in region.x..region.right() {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y }) {
                let style = cell
                    .style()
                    .bg(cell.style().bg.unwrap_or(crate::style::to_color(ground)))
                    .fg(cell
                        .style()
                        .fg
                        .unwrap_or(crate::style::to_color(palette.foreground)))
                    .add_modifier(Modifier::DIM);
                cell.set_style(style);
            }
        }
    }
}

/// Dim the whole frame the dialog will cover.
pub fn dim_frame(buf: &mut Buffer, app: &super::super::app::App) {
    dim_region(buf, app.regions.bounds(), &app.palette());
}

// ------------------------------------------------------------------ focus

/// One focusable child of a dialog, in tab order.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusItem {
    /// Stable identity so a dialog can tell which item got focus without counting.
    pub id: FocusId,
    /// Where it is, for mouse clicks and for drawing the focus ring.
    pub area: RectCore,
    /// Whether `Enter` activates it as the default action. Exactly one item per dialog
    /// should set this; the first one wins if more than one does.
    pub is_default: bool,
}

impl FocusItem {
    pub fn new(id: FocusId, area: RectCore) -> Self {
        FocusItem {
            id,
            area,
            is_default: false,
        }
    }

    pub fn default_action(mut self) -> Self {
        self.is_default = true;
        self
    }
}

/// Identity of a focusable widget. Dialog-specific so a click can be routed without
/// comparing rectangles in `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusId {
    /// The dialog's primary text field.
    Field(usize),
    /// A checkbox, by index within the dialog.
    Checkbox(usize),
    /// A button, by index within the dialog.
    Button(usize),
    /// A list or picker.
    List,
    /// A row in a list, addressed absolutely so a click can select it.
    ListRow(usize),
    /// The Close word on a docked service strip.
    ///
    /// A variant of its own rather than `Button(i)`, because `Button` indices are interpreted as
    /// search commands by `press_find_button` and are walked by `Tab`. Close is neither: it is
    /// the way out, and Tab must not park the user's cursor on it in the middle of a query.
    CloseStrip,
}

/// Advance a focus index by `delta`, wrapping at both ends (FR-U4).
///
/// Wrapping is required by the acceptance criteria: `Shift+Tab` on the first item lands on
/// the last. An empty list returns 0 rather than underflowing, because a dialog can be
/// momentarily childless while its data reloads.
pub fn focus_cycle(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let current = current.min(count.saturating_sub(1));
    if delta >= 0 {
        (current + delta as usize) % count
    } else {
        // Add a multiple of count so the subtraction cannot go negative.
        let steps = (-delta) as usize % count;
        (current + count - steps) % count
    }
}

/// Move a value by `delta`, clamped to `0..=max` and **not** wrapping.
///
/// This is the counterpart to [`focus_cycle`], and the difference is the whole point. Tab
/// order is a ring: `Shift+Tab` on the first child must reach the last, because the children
/// are a handful of named controls and there is nowhere else to go. A list cursor is a
/// position in a sequence: wrapping from the last row to the first is not navigation, it is
/// teleporting past everything the user was trying to reach.
///
/// The user report that pinned this down: in a file picker containing only `../`, pressing
/// `Down` appeared to "rotate the list infinitely". It was not rotating the list -- `Tab`
/// order wrapped through the field and the buttons and came back to the one row, which on a
/// one-row listing is indistinguishable from a spin. Rows clamp; only Tab wraps.
pub fn clamp_step(current: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let max = count - 1;
    let cur = current.min(max);
    if delta >= 0 {
        cur.saturating_add(delta as usize).min(max)
    } else {
        cur.saturating_sub(delta.unsigned_abs() as usize)
    }
}

/// What a dialog key should do, decided once for every dialog.
///
/// # Why this exists
///
/// Nine dialogs each carried their own key handler, and each had independently invented the
/// same three answers: where the arrow keys go, what `Enter` runs, and how `Tab` moves. They
/// disagreed. The Themes picker had no way to move focus in from the query field, so its list
/// could not be reached by keyboard at all; Preferences had arrows but no mouse, because it
/// never asked the framework which cell its rows occupied; the file picker wrapped its row
/// cursor through the buttons. The bug report called it correctly: a lot of different code
/// with the same functionality instead of one dialog object.
///
/// [`DialogInput`] is that one answer. A dialog no longer decides arrow semantics; it declares
/// its children (which it already had to do for the mouse) and the framework routes. What
/// stays per-dialog is only genuinely per-dialog behaviour: typing into a field, the Find
/// dialog's flags, Preferences' inline numeric editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogInput {
    /// Consume the key with no effect (an unhandled key must not fall through to the editor).
    Consumed,
    /// Move focus among children by `delta`, wrapping. `Tab` and `Shift+Tab`.
    Focus(i32),
    /// Move the list cursor by `delta`, clamped. `Up`/`Down` while the list holds focus.
    Row(i32),
    /// Move focus to the list, leaving the cursor where it is. `Down` from a field.
    EnterList,
    /// Move focus back out of the list to the next control. `Up` from the first row.
    LeaveList,
    /// Run the focused control: the field's accept, the row's open, the button's action.
    Activate,
    /// Run the dialog's default control regardless of focus. A bare `Enter` in a field.
    Default,
    /// Close the dialog, discarding. `Esc`.
    Cancel,
    /// Insert a character into the focused field.
    Char(char),
    /// Delete backwards in the focused field.
    Backspace,
    /// Delete forwards in the focused field.
    Delete,
    /// Move the caret within a field, `Home`/`End`.
    FieldHome,
    FieldEnd,
    /// Page the list cursor by `delta` rows, clamped.
    PageRows(i32),
    /// Pick a button by its position, the digit shortcut on a confirm prompt.
    Button(usize),
    /// Toggle the focused checkbox. `Space`.
    Toggle,
    /// Step a horizontal control (an enum or boolean row) by `delta`. `Left`/`Right`.
    Step(i32),
    /// A key this dialog does not own: the caller may handle it (global bindings).
    Pass,
}

impl DialogInput {
    /// Decide what one key does in a dialog, from the dialog's declared shape alone.
    ///
    /// `has_field` and `has_list` describe the children; `list_focused` says where focus is.
    /// Nothing here knows which dialog it is looking at, which is what makes the behaviour
    /// uniform -- and what makes a new dialog correct the moment it declares its children.
    ///
    /// # The two rules that the per-dialog handlers got wrong
    ///
    /// - **Arrows leave a field for the list.** A field owns `Left`/`Right` (caret movement)
    ///   and the list owns `Up`/`Down` (cursor movement), but `Down` from a field and `Up` from
    ///   the list's first row have to cross that boundary or a mouse-less user is stranded in
    ///   the field. That is exactly the Themes-picker report: the list was reachable by mouse
    ///   and by nothing else.
    /// - **Rows clamp.** See [`clamp_step`].
    pub fn route(
        key: &crossterm::event::KeyEvent,
        has_field: bool,
        has_list: bool,
        list_focused: bool,
    ) -> Self {
        use crossterm::event::{KeyCode, KeyModifiers};
        let code = key.code;
        // A dialog swallows everything by default. A key that fell through to the editor would
        // type into the buffer behind an open prompt, which is a data-corruption bug wearing
        // a keystroke costume.
        match code {
            KeyCode::Esc => DialogInput::Cancel,
            KeyCode::Tab => DialogInput::Focus(if key.modifiers.contains(KeyModifiers::SHIFT) {
                -1
            } else {
                1
            }),
            KeyCode::BackTab => DialogInput::Focus(-1),
            KeyCode::Enter => DialogInput::Activate,
            KeyCode::Down if has_list => {
                if list_focused {
                    DialogInput::Row(1)
                } else {
                    DialogInput::EnterList
                }
            }
            KeyCode::Up if has_list => {
                if list_focused {
                    DialogInput::Row(-1)
                } else {
                    DialogInput::EnterList
                }
            }
            KeyCode::PageDown if has_list => DialogInput::PageRows(1),
            KeyCode::PageUp if has_list => DialogInput::PageRows(-1),
            KeyCode::Home if has_list && list_focused => DialogInput::Row(i32::MIN / 2),
            KeyCode::End if has_list && list_focused => DialogInput::Row(i32::MAX / 2),
            KeyCode::Left | KeyCode::Right if has_list && list_focused => {
                DialogInput::Step(if code == KeyCode::Right { 1 } else { -1 })
            }
            KeyCode::Char(' ') if has_list && list_focused => DialogInput::Toggle,
            KeyCode::Char(c) if has_field && !list_focused => {
                if c.is_ascii_digit() && !key.modifiers.contains(KeyModifiers::ALT) {
                    // Digits are ordinary text in a field. They are a shortcut only on a
                    // button row, which is decided by the dialog, not here.
                    DialogInput::Char(c)
                } else {
                    DialogInput::Char(c)
                }
            }
            KeyCode::Backspace if has_field && !list_focused => DialogInput::Backspace,
            KeyCode::Delete if has_field && !list_focused => DialogInput::Delete,
            KeyCode::Home if has_field && !list_focused => DialogInput::FieldHome,
            KeyCode::End if has_field && !list_focused => DialogInput::FieldEnd,
            // Left/Right in a field move its caret; the field owns them.
            KeyCode::Left | KeyCode::Right if has_field && !list_focused => DialogInput::Pass,
            _ => DialogInput::Consumed,
        }
    }
}

/// Which item a pointer press hits, or `None` when it misses every child.
pub fn focus_at(items: &[FocusItem], x: u16, y: u16) -> Option<usize> {
    items.iter().position(|it| it.area.contains(x, y))
}

/// The index of the default item, falling back to the first.
pub fn default_index(items: &[FocusItem]) -> usize {
    items.iter().position(|it| it.is_default).unwrap_or(0)
}

// -------------------------------------------------------------- primitives

/// Draw a labeled text field, with `[` `]` as its frame and the caret as an inverted cell.
///
/// The brackets are drawn rather than a ratatui `Block` because a dialog row is one cell
/// tall and a bordered widget needs three. The caret is the character under the cursor
/// inverted when there is one, and a space when the caret is at the end -- inverting the
/// cell rather than drawing a `_` is what keeps the field's width constant as the caret
/// moves, which a `_`-caret does not.
pub fn draw_field(
    buf: &mut Buffer,
    row: RectCore,
    label: Option<&str>,
    input: &TextInput,
    focused: bool,
    error: bool,
    palette: &ThemePalette,
) {
    if row.w == 0 {
        return;
    }
    let prefix = match label {
        Some(l) if !l.is_empty() => format!("{l} "),
        _ => String::new(),
    };
    let style = if error {
        error_style(palette)
    } else if focused {
        focus_style(palette)
    } else {
        crate::style::dialog(palette)
    };
    // Two cells of bracket, minus the label.
    let room = row
        .w
        .saturating_sub(prefix.width() as u16)
        .saturating_sub(2) as usize;
    let text = input.window_text(room);
    let caret_col = input.caret_display_col(room);
    draw_str(buf, row.x, row.y, &prefix, style);
    let box_x = row.x + prefix.width() as u16;
    draw_str(buf, box_x, row.y, "[", style);
    let inner_x = box_x + 1;
    // The value is drawn in plain dialog ink. It used to get `Style::default()`, which is how a
    // typed path in the file dialog came out in the terminal's foreground while its brackets were
    // drawn in the theme's -- two colors for one field.
    let value_style = if focused {
        focus_style(palette)
    } else {
        crate::style::dialog(palette)
    };
    let drawn = draw_str(buf, inner_x, row.y, &text, value_style);
    // Pad to the box's full width so a short field still closes its bracket.
    for x in drawn..inner_x + room as u16 {
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position { x, y: row.y }) {
            cell.set_symbol(" ");
            cell.set_style(if focused {
                field_focus_style(palette)
            } else {
                crate::style::dialog(palette)
            });
        }
    }
    if room > 0 {
        let cx = inner_x + caret_col as u16;
        if let Some(cell) = buf.cell_mut(ratatui::layout::Position {
            x: cx.min(inner_x + room as u16 - 1),
            y: row.y,
        }) {
            // The caret is the one place a true inversion is wanted, and it is spelled with the
            // theme's own pair rather than `Modifier::REVERSED`. Reversal asks the terminal to
            // swap two colors the app never chose, so on a light theme it could put a light caret
            // on a light ground. Swapping the theme's ink and ground gives the same signal and
            // stays legible because that pair is legible in the first place.
            cell.set_style(
                crate::style::ink(palette.background, palette.caret).add_modifier(Modifier::BOLD),
            );
        }
    }
    draw_str(buf, box_x + 1 + room as u16, row.y, "]", style);
}

/// Draw a checkbox: `[x] Label` / `[ ] Label`.
pub fn draw_checkbox(
    buf: &mut Buffer,
    row: RectCore,
    label: &str,
    on: bool,
    focused: bool,
    palette: &ThemePalette,
) {
    let mark = if on { 'x' } else { ' ' };
    let text = format!("[{mark}] {label}");
    let style = if focused {
        focus_style(palette)
    } else {
        crate::style::dialog(palette)
    };
    draw_str(buf, row.x, row.y, &fit(&text, row.w as usize), style);
}

/// Draw a button: `[Label]`, or `<Label>` when it is the dialog's default.
pub fn draw_button(
    buf: &mut Buffer,
    row: RectCore,
    label: &str,
    state: ButtonState,
    palette: &ThemePalette,
) {
    let text = match state {
        ButtonState::Normal => format!("[{label}]"),
        ButtonState::Default => format!("<{label}>"),
        ButtonState::Focused => format!("[{label}]"),
        ButtonState::Disabled => format!(" {label} "),
    };
    let style = match state {
        ButtonState::Focused => focus_style(palette),
        ButtonState::Default => crate::style::dialog(palette).add_modifier(Modifier::BOLD),
        ButtonState::Disabled => hint_style(palette),
        ButtonState::Normal => crate::style::dialog(palette),
    };
    draw_str(buf, row.x, row.y, &fit(&text, row.w as usize), style);
}

/// How a button renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    /// The `Enter` target: drawn with angle brackets so it reads as default.
    Default,
    Focused,
    Disabled,
}

/// Lay a row of buttons out left to right, returning each one's rectangle.
///
/// Returned rather than drawn-in-place because the caller needs the rectangles for the
/// focus list *and* the click target, and computing them twice is exactly how a click ends
/// up on the button to the left of the one the user aimed at.
pub fn button_row(row: RectCore, labels: &[&str]) -> Vec<(RectCore, usize)> {
    let mut out = Vec::with_capacity(labels.len());
    let mut x = row.x;
    for (i, label) in labels.iter().enumerate() {
        // 4 cells: two brackets plus one space of gap, no gap after the last.
        let w = label.width() as u16 + 2;
        let area = RectCore::new(x, row.y, w.min(row.right().saturating_sub(x)), 1);
        if area.w == 0 {
            break;
        }
        out.push((area, i));
        x = x.saturating_add(w + 1);
        if x >= row.right() {
            break;
        }
    }
    out
}

/// Draw a list of rows with a selection bar, scrolling to keep the selection visible.
///
/// `selected` may exceed the row count while a filter is being typed; it is clamped rather
/// than trusted. The window rule matches the picker that shipped in S12 so the two look
/// identical.
pub fn draw_list(
    buf: &mut Buffer,
    area: RectCore,
    rows: &[String],
    selected: usize,
    focused: bool,
    palette: &ThemePalette,
) {
    let visible = area.h as usize;
    if visible == 0 {
        return;
    }
    let start = list_window(rows.len(), selected, visible);
    if rows.is_empty() {
        draw_str(buf, area.x, area.y, "(empty)", hint_style(palette));
        return;
    }
    for (i, row) in rows.iter().skip(start).take(visible).enumerate() {
        let y = area.y + i as u16;
        let is_sel = start + i == selected.min(rows.len().saturating_sub(1));
        // The selection bar is the theme's highlight pair, not `Modifier::REVERSED`. Reversal was
        // the reason an unfocused list looked wrong on a light theme: it swapped two terminal
        // colors, and an unfocused selection has no business restating the terminal's palette.
        // An unfocused bar keeps the same ground in dimmer ink, so "selected" still reads while
        // "this list has the keyboard" stays a separate signal.
        let style = if is_sel && focused {
            crate::style::highlight(palette).add_modifier(Modifier::BOLD)
        } else if is_sel {
            let ink = np_core::dim_ink(
                palette.highlight_bg,
                palette.highlight_fg,
                np_core::MIN_UI_CONTRAST,
            );
            crate::style::ink(ink, palette.highlight_bg)
        } else {
            crate::style::dialog(palette)
        };
        let line = fit(&format!("  {row}"), area.w as usize);
        draw_str(buf, area.x, y, &line, style);
    }
}

/// Which rows of a clipped list are on screen, as `(first, last_exclusive)`.
///
/// The mouse layer needs the same answer [`draw_list`] computes: a click on the fifth row of
/// a scrolled listing means row five *of the window*, and an absolute index there selects
/// whatever happens to sit five down the data. Sharing one function is the only way the two
/// cannot drift apart when the windowing rule changes.
///
/// An empty count yields `(0, 0)`, so a caller iterating the range does nothing rather than
/// drawing one phantom row.
pub fn visible_rows(count: usize, selected: usize, visible: usize) -> (usize, usize) {
    if count == 0 || visible == 0 {
        return (0, 0);
    }
    let start = list_window(count, selected, visible);
    (start, start + visible.min(count - start))
}

/// The first visible row for a clipped list, keeping the selection on screen.
pub fn list_window(count: usize, selected: usize, visible: usize) -> usize {
    if visible == 0 || count <= visible {
        return 0;
    }
    let selected = selected.min(count.saturating_sub(1));
    if selected < visible {
        return 0;
    }
    selected - visible + 1
}

/// Pad or truncate to exactly `cells` display columns.
///
/// Truncation is by display width, not by `char` count: a field holding full-width
/// characters must not push its closing bracket off the row, which was a real defect in the
/// pre-S14 find dialog that counted `char`s.
pub fn fit(s: &str, cells: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for g in s.graphemes(true) {
        let w = g.width();
        if width + w > cells {
            break;
        }
        out.push_str(g);
        width += w;
    }
    for _ in width..cells {
        out.push(' ');
    }
    out
}

/// Draw a dialog's hint line about what keys do what, inside the box.
pub fn draw_hint(buf: &mut Buffer, row: RectCore, text: &str, palette: &ThemePalette) {
    draw_str(
        buf,
        row.x,
        row.y,
        &fit(text, row.w as usize),
        hint_style(palette),
    );
}
