//! Mouse routing (spec §6.9, contract §15).
//!
//! # Why this is a decision function and not a pile of callbacks
//!
//! §6.9's hard requirement is "table-driven, unit-testable without a terminal". So `route`
//! takes a crossterm `MouseEvent` plus the state it needs and *returns* a `MouseIntent`; it
//! never touches the clock, the terminal, or `App`. `App::handle_mouse` is the impure half
//! that applies the intent. Every one of §6.9's twenty rows is an explicit arm below, either
//! implemented or marked with the stage that closes it, so a missing row is visible in the
//! source rather than silently absent at runtime.
//!
//! # Two things crossterm does not give you
//!
//! **No double-click event.** `MouseEventKind::Down(Left)` is all there is, so multi-click is
//! synthesized from the cell and an injected `now` (see `ClickState::register`). `route` takes
//! `now` as a parameter precisely so the 250 ms window is testable without sleeping.
//!
//! **No notion of what is under the pointer.** Hit-testing runs here, over the `RectCore`s
//! `App` stored during the S3 layout pass. Layout is never recomputed in this module: if it
//! were, the renderer and the mouse could disagree about where a tab ends, and the failure
//! would be a click landing one cell off — the kind of bug that reads as user error. The
//! tab and menu-bar spans come from the same functions the renderers call, which is what
//! keeps the two in step by construction rather than by vigilance.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use np_core::{
    layout_visible_rows, Buffer, LineCol, RectCore, ScrollPos, Selection, SelectionKind, VisualRow,
};

use crate::actions::{Action, MenuId};
use crate::app::{App, ClickState, ContextKind, HoverTarget, Mode};
use crate::ui::framework::FocusId;

/// Columns moved by shift+wheel, which scrolls horizontally (spec §6.9 row 9).
const HORIZONTAL_SCROLL_STEP: i32 = 3;

/// Where a mouse event landed. Purely a function of the pointer position and stored
/// geometry; nothing here knows about documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The text area, resolved to a document position already clamped into the buffer.
    Text {
        lc: LineCol,
        screen_row: u16,
    },
    /// The line-number field of the gutter (spec FR-V2, §6.9 row 11).
    GutterLine(usize),
    /// The fold marker column in the gutter (§6.9 row 12).
    ///
    /// Hit-tested by *column*, not by whether the line happens to be foldable: the gutter's
    /// marker columns are fixed for the whole strip, so a click in the fold column always
    /// means "toggle the fold on this row's line" and `ToggleFold` reports no-op for a line
    /// with no fold. Deciding by column is what keeps the target from moving as the pointer
    /// crosses foldable lines.
    GutterFold(usize),
    /// The bookmark marker column in the gutter (§6.9 row 13).
    GutterBookmark(usize),
    /// A scrollbar's thumb (§6.9 row 14), with the row-or-column clicked.
    ///
    /// The cell travels with the hit because a thumb's meaning is *where on the trough* it was
    /// grabbed, and the trough's axis differs between the two bars: a row for the vertical bar,
    /// a column for the horizontal one. Carrying the already-chosen cell keeps every consumer
    /// from re-deciding which axis applies, which is how the two bars would drift apart.
    ScrollbarThumb(VerticalOrHorizontal, u16),
    /// A scrollbar's trough, outside the thumb (§6.9 row 14), with the cell clicked.
    ScrollbarTrack(VerticalOrHorizontal, u16),
    /// A tab, or `None` for empty tab-bar space.
    TabBar(Option<usize>),
    /// A menu title, or `None` for empty menu-bar space.
    MenuBar(Option<(MenuId, usize)>),
    StatusBar(StatusSegment),
    /// The banner strip above the editor (S14 draws it).
    /// The disk-change banner, with which button was hit (FR-F7, FR-U10).
    Banner,
    /// A dropdown item in an open menu, by index (FR-U1).
    MenuItem(usize),
    /// A context menu item, by index (FR-U7).
    ContextMenuItem(usize),
    /// A row of the command palette (FR-U8).
    PaletteRow(usize),
    /// The Find-in-Files panel (S11 owns its layout).
    FindPanel,
    /// The Close word on a docked service strip (Find, Replace, or Find in Files).
    ///
    /// One hit for all three so a single intent closes whichever strip is up, rather than three
    /// near-identical arms that could disagree about what closing means.
    CloseStrip,
    /// A dialog or overlay, which outranks everything beneath it.
    Dialog,
    /// A click that missed a centered dialog box entirely.
    ///
    /// Separate from [`Hit::Dialog`] because the two want opposite answers: a click inside the
    /// box is padding or a border and must be swallowed so it cannot reach the text underneath,
    /// while a click past the box means the user is done with the dialog and it should close the
    /// same way `Esc` closes it. Folding both into `Dialog` is what made an outside click do
    /// nothing at all -- the arm could not tell the two apart, and the safer-looking "swallow"
    /// was the only option available to it.
    DialogOutside,
    /// A specific focusable child of the open dialog, by focus index (FR-U4).
    DialogWidget(usize),
    /// A context menu, which outranks a dialog: it is the most recently summoned thing.
    ContextMenu,
    Outside,
}

/// Which scrollbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalOrHorizontal {
    Vertical,
    Horizontal,
}

impl VerticalOrHorizontal {
    /// Whether this bar runs down the screen, i.e. whether its track is measured in rows.
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Vertical)
    }
}

/// Which part of the status bar. Named so S14 can bind actions to segments without the
/// mouse layer knowing what each one will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSegment {
    /// The document name, modified dot and read-only lock.
    Document,
    Position,
    Encoding,
    Eol,
    /// The language column, which S12 makes clickable.
    Language,
    Overtype,
    /// The trailing "chars / lines" readout.
    Counts,
    Other,
}

/// The decision `route` reached. `App::handle_mouse` turns these into state changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseIntent {
    Action(Action),
    SetCaret {
        lc: LineCol,
        /// Extend the selection from its anchor instead of collapsing it (shift+click).
        extend: bool,
    },
    BeginSelection {
        kind: SelectionKind,
        anchor: LineCol,
        focus: LineCol,
    },
    ExtendSelection {
        focus: LineCol,
    },
    /// Select a whole line from the gutter **without** moving the caret.
    ///
    /// Separate from `BeginSelection` because that one moves the caret to the focus, which is
    /// right for a sweep in the text but wrong here: the point of clicking the gutter is to
    /// grab a line while your caret stays where you were working.
    SelectLine {
        line: usize,
        keep_caret: bool,
    },
    /// Select a whole-line **range** from a gutter drag (§6.9 row 11), caret untouched.
    ///
    /// Separate from `BeginSelection` for the same reason as `SelectLine`: that one moves the
    /// caret to the focus. It is also separate from repeated `SelectLine`s because a range's
    /// anchor has to survive every drag event, and re-deriving it from a single line each time
    /// would collapse the selection to the last row the pointer crossed.
    SelectLineRange {
        anchor: LineCol,
        focus: LineCol,
    },
    /// Select a Find in Files result row (S11).
    SelectFindHit(usize),
    /// A click on the panel's header or a field: focus the panel, select nothing.
    FocusFindPanel,
    ToggleFold(usize),
    ToggleBookmark(usize),
    ActivateTab(usize),
    CloseTab(usize),
    CycleTabs(i32),
    Scroll {
        lines: i32,
    },
    ScrollH {
        cols: i32,
    },
    /// Scrollbar thumb, as a per-mille fraction of the document (0..=1000).
    ///
    /// Per-mille rather than `f32` so `MouseIntent` stays `Eq` and assertable in tests. A
    /// scrollbar cannot resolve finer than that on any real screen, and comparing floats for
    /// equality in an assertion is a test that fails for the wrong reason.
    ScrollTo(i32),
    /// Scroll so row `usize` of the document's row list is at the top of the viewport.
    ///
    /// Rows rather than lines, because that is the unit a thumb is dragged through: with wrap on
    /// a line is several rows, and a line-space offset taken from a row-space thumb lands
    /// somewhere above where the pointer is.
    ScrollToRow(usize, Option<Box<crate::app::ScrollbarDrag>>),
    /// Scroll so column `usize` is at the left edge. The horizontal twin of [`Self::ScrollToRow`].
    ScrollToCol(usize, Option<Box<crate::app::ScrollbarDrag>>),
    /// Record a thumb grab without moving the view. The first half of a drag.
    ///
    /// Exists because a thumb press and a trough press are different gestures. A trough press
    /// aims at a position; a thumb press takes hold of the thumb where the user caught it, and
    /// the movement they want is the movement they make afterwards. Emitting "hold" rather than
    /// "scroll to the cell's proportion" is the fix for the thumb jumping to the pointer on
    /// contact, and it needs its own variant so the grab is recorded on the same path that
    /// refuses to scroll -- otherwise the drag has no anchor to measure travel from.
    HoldRowScroll(Option<Box<crate::app::ScrollbarDrag>>),
    /// The horizontal twin of [`Self::HoldRowScroll`].
    HoldColScroll(Option<Box<crate::app::ScrollbarDrag>>),
    /// Jump to the very first offset of the document along the bar's axis.
    ///
    /// Emitted when a press or a drag lands on the trough's first cell. The thumb cannot be drawn
    /// above the trough and the ratio cannot reach offset 0 from the thumb's own cells, so the
    /// topmost cell of the bar is unreachable by proportion. A pointer put at the very top of a
    /// long file means "show me the beginning", and answering with "row 33" is a scrollbar that
    /// does not do what it is pointed at.
    ScrollToStart(VerticalOrHorizontal),
    /// Jump to the very last offset the view can take along the bar's axis.
    ///
    /// The mirror of [`Self::ScrollToStart`], and it exists for the same reason stated back to
    /// front: the trough's last cells stand for the end of the document, and an offset computed
    /// there is already at the ceiling, so the intent must say "the end" rather than carry a
    /// number the scroller is free to clamp. Going through the number is what made the bottom of
    /// the bar look broken -- the view moved to just-short-of the last page, the next event asked
    /// again, and the thumb sat at the bottom of the trough over text that had stopped moving.
    ScrollToEnd(VerticalOrHorizontal),
    /// Forget an in-progress scrollbar drag and any pending dialog press. Emitted on release.
    EndDialogPressAndDrag,
    /// Point the open dialog's row cursor at focus child `i`, without activating it.
    ///
    /// The press half of a list click. See [`Self::ActivateDialogRow`].
    SelectDialogRow(usize),
    /// Activate the dialog row `row`, which the press of this click resolved to.
    ///
    /// Emitted on a release that lands inside the same listing the press started in. Carrying the
    /// row rather than re-resolving it is deliberate: selecting a row scrolls the list, so the
    /// window the release sees is not the window the press saw, and the same cell in it is a
    /// different row.
    ActivateDialogRow(usize, usize),
    OpenContextMenu(ContextKind, LineCol),
    DismissContextMenu,
    /// Close the open dialog because the click landed outside its box.
    ///
    /// Deliberately its own intent rather than a reuse of [`Self::DismissContextMenu`]: the two
    /// pop different mode levels and one of them has side effects. A context menu is dismissed by
    /// popping a level, while a dialog's `Esc` path also writes Find's options back to config and
    /// decides whether a running Find in Files search should be cancelled. Routing a dialog close
    /// through the menu path would silently skip that work.
    CloseDialog,
    /// Run the action behind a menu, context menu, or palette row (FR-U1, U7, U8).
    ///
    /// One intent for all three because they all resolve to an `Action` before reaching
    /// `App`, which is what keeps the routing table from needing three near-identical arms.
    RunAction(Action),
    /// Run a resolved menu row, carrying the row itself rather than only its `Action`.
    ///
    /// `Action` is `Copy` and unparameterized by contract, so the codec a Convert To / Reload As
    /// row selects rides in the row's `hint`. Running the bare action loses it and the document
    /// silently keeps the encoding it already had, which read as "the row does nothing".
    RunMenuItem(Box<crate::ui::menus::MenuItem>),
    /// Open one specific recent file.
    ///
    /// A recent-file row's `Action` is the unparameterized `RecentFiles`, which opens the
    /// picker dialog. Run from the submenu that means clicking a file name reopened the list of
    /// file names. The path is the row's label, so it rides here instead.
    OpenRecentFile(std::path::PathBuf),
    /// Open a submenu level under the row the pointer is on.
    OpenSubmenu(MenuId, String),
    /// Close the menu bar because the click landed outside every menu box, then apply `inner`
    /// as though no menu had been open.
    ///
    /// A menu bar is modal to the pointer in the desktop sense: a click anywhere that is not
    /// part of the open menu dismisses it. The press is not swallowed, because a user who clicks
    /// a word to put the caret there expects the caret to move even if a menu happened to be in
    /// the way -- and expects it to move *there*, not one click later. Composing the dismissal
    /// with the ordinary intent is what gives both, and keeps `route` a pure
    /// event+hit -> intent function, which its tests depend on.
    DismissMenuThen(Box<MouseIntent>),
    /// Close whichever docked service strip is up (Find, Replace, or Find in Files).
    ///
    /// An intent rather than an `Action` because `Action` is a keyed command: adding one means a
    /// kebab name, a palette entry and a help-overlay row, which is the wrong shape for a widget
    /// that belongs to a strip already on screen. `Esc` remains the keyboard's way out and is
    /// unchanged.
    CloseServiceStrip,
    /// Start a Find in Files search from its Search word.
    ///
    /// An intent rather than a new `Action` for the same reason as `CloseServiceStrip`: it is a
    /// widget on a strip already on screen, not a keyed command. It runs exactly what `Enter`
    /// runs in the query field.
    RunFindPanelSearch,
    /// Point the Find in Files keyboard at the field under the pointer.
    SelectFindField(crate::fif::FifField),
    /// Flip the Find in Files toggle the pointer is on.
    ///
    /// The three boxes were reported as not reacting to a click. They were not merely unhittable:
    /// nothing in the panel was, because the hit arm below the bounds guard never ran. Once that
    /// was fixed the boxes still needed their own intent, since `field_at_row` answers `Glob` for
    /// the row they share.
    ToggleFindFlag(crate::fif::FifToggle),
    /// Scroll the Find in Files results list by `lines` under the wheel.
    ScrollFindResults {
        lines: i32,
    },
    /// Open the Find in Files result the pointer was double-clicked on.
    ///
    /// A single click only selects, which made the results feel dead -- "scrollable, but not
    /// selectable by mouse". Selecting a row that already looks selected is not feedback, so nothing
    /// appeared to happen. Opening needs a second gesture, and double-click is the one every desktop
    /// user already knows for "open this item".
    OpenFindHit(usize),
    /// Focus a dialog's child, and if it is a list, select the row under the pointer (FR-U4).
    FocusDialogChild(usize),
    /// Scroll the open dialog's list by `rows` under the wheel.
    ///
    /// The dialog owns the wheel over itself. Before this existed the wheel arm fell through to
    /// the ordinary document scroll, so a wheel over the Theme picker, the Preferences list, the
    /// Recent Files list or the command palette scrolled the *file behind the box* while the list
    /// the user was pointing at did not move. That is worse than doing nothing: the text jumps
    /// about behind a dialog the user is trying to read.
    ///
    /// Rows rather than pixels, because a dialog list has no sub-row position -- `list_window`
    /// places whole rows, and the cursor is the only scroll state a dialog keeps.
    ScrollDialogList {
        rows: i32,
    },
    /// Point the command palette's cursor at row `row` without running the command.
    ///
    /// The palette used to run the command on a single press, which is the one list in the app
    /// that did so. Every other list selects on click and acts on double-click, and closing the
    /// palette and firing a command is the most destructive "act" of any of them -- an accidental
    /// brush against a row could run an arbitrary command. The palette has no keyboard-focus
    /// concept to reuse here, so it needs its own select intent rather than
    /// [`Self::SelectDialogRow`].
    SelectPaletteRow(usize),
    /// Press a banner button: `true` is Reload, `false` is Ignore (FR-F7).
    BannerAction(bool),
    /// Hover changed; the caller updates highlight state. Carried as an intent so `route`
    /// stays pure instead of writing into `App`.
    Hover(Option<HoverTarget>),
    /// Middle-click in the text area: paste the primary selection (S7 supplies it).
    PastePrimary,
    Nothing,
}

/// The full routing decision for one mouse event.
///
/// `now` is injected rather than read from the clock so double-click timing is deterministic
/// under test; `click` is the caller's accumulated state, and the returned `ClickState` is
/// what it should store. The pair makes the whole function referentially transparent.
///
/// Returns the intent and the click state to persist.
pub fn route_with_click(
    ev: &MouseEvent,
    app: &App,
    click: &ClickState,
    now: std::time::Instant,
) -> (MouseIntent, ClickState) {
    let hit = hit_test(ev, app);
    let intent = decide(ev, app, &hit, click, now);
    // A press that missed every open menu box dismisses the bar, and then does what it would
    // have done anyway. Only presses: a hover or a wheel over the text must not close a menu
    // the user is still looking at.
    let intent = if dismisses_menu(ev, app, &hit) {
        MouseIntent::DismissMenuThen(Box::new(intent))
    } else {
        intent
    };
    (intent, next_click_state(ev, app, &hit, click, now))
}

/// Whether this event is a press that landed outside the whole menu bar.
///
/// The test is "the bar is open, and the hit is not part of it". `hit_test` already ranks the
/// open dropdown above the regions beneath it, so anything it reports as text, tab bar, status
/// bar or empty menu-bar space is by construction outside both the parent box and any submenu
/// box -- which is exactly the set of places a click should dismiss from.
///
/// A click on a menu *title* while a submenu is open is the interesting case: `Hit::MenuBar`
/// means "open that menu", which replaces the level, so it is not a dismissal and the title's
/// own intent runs alone.
fn dismisses_menu(ev: &MouseEvent, app: &App, hit: &Hit) -> bool {
    if !matches!(app.mode(), Mode::Menu { .. }) {
        return false;
    }
    if !matches!(
        ev.kind,
        MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Down(MouseButton::Right)
            | MouseEventKind::Down(MouseButton::Middle)
    ) {
        return false;
    }
    // Inside either box: the menu owns this click.
    if matches!(
        hit,
        Hit::MenuItem(_) | Hit::MenuBar(Some(_)) | Hit::DialogWidget(_)
    ) {
        return false;
    }
    // A click in the empty space of the menu bar itself only dismisses when the bar has no
    // title at that column; `Hit::MenuBar(None)` is that case.
    true
}

/// The convenience wrapper for callers that do not care about multi-click bookkeeping.
pub fn route(ev: &MouseEvent, app: &App) -> MouseIntent {
    route_with_click(ev, app, &ClickState::default(), std::time::Instant::now()).0
}

// ------------------------------------------------------------------ hit testing

/// Resolve a pointer position to a [`Hit`], outermost layer first.
///
/// The order is the stacking order, and it is the whole reason a click inside a dialog that
/// overlaps the text area selects nothing in the buffer: the first match wins, and overlays
/// are tested before anything they cover.
pub fn hit_test(ev: &MouseEvent, app: &App) -> Hit {
    let (x, y) = (ev.column, ev.row);

    // Overlays first. A modal dialog owns the screen; nothing under it is clickable.
    match app.mode() {
        Mode::ContextMenu(kind) => {
            // The menu has geometry, so a click on an item runs that item and a click
            // anywhere else dismisses. Both use the same hit-test the renderer used.
            let items = crate::ui::context_menu::items(app, kind);
            let area = crate::ui::context_menu::menu_rect(
                app.context_menu_cell.0,
                app.context_menu_cell.1,
                items.len(),
                app.regions.bounds(),
            );
            return match crate::ui::context_menu::item_at(area, x, y, items.len()) {
                Some(i) => Hit::ContextMenuItem(i),
                None => Hit::ContextMenu,
            };
        }
        Mode::Palette => {
            // A click on a palette row runs that command; elsewhere it is swallowed rather
            // than reaching the document underneath.
            let layout = crate::ui::palette::palette_layout(app);
            if let Some(row) = crate::ui::palette::row_at(app, &layout, x, y) {
                return Hit::PaletteRow(row);
            }
            return Hit::Dialog;
        }
        Mode::HelpOverlay => {
            // The cheatsheet is a box over the document, and the gesture that dismisses a box is
            // a click past its edge. This used to answer `Hit::Dialog` for every point, which
            // swallowed the click wherever it landed: the overlay had no mouse support at all, so
            // Esc was the only way out. Same order as the dialog branch below -- inside the drawn
            // frame means the press belongs to the box and is swallowed, outside means dismissal.
            let area = crate::ui::help::layout(app).area;
            return if area.contains(x, y) {
                Hit::Dialog
            } else {
                Hit::DialogOutside
            };
        }
        // Find and Replace are docked service lines rather than a box, so their hit-testing
        // comes from the strip's own layout. Falling through to `dialog_focus_items` here would
        // measure the floating dialog's rectangles, which are no longer what is on screen -- the
        // clicks would land on rows the user is not looking at.
        Mode::Dialog(crate::app::DialogKind::Find)
        | Mode::Dialog(crate::app::DialogKind::Replace) => {
            let Some(id) = crate::ui::find_bar::hit_at(app, x, y) else {
                return Hit::Dialog;
            };
            // `Hit::DialogWidget` carries an index into the dialog's focus list, which is what
            // the activation path already takes. The strip answers with a `FocusId`, so the two
            // are joined here through the same focus list the Tab key walks -- a click and a Tab
            // therefore land on the same control.
            // Close is not in the dialog's focus list -- it is not a search control and Tab
            // must not land on it mid-query -- so it is answered directly rather than joined
            // through that list.
            if id == FocusId::CloseStrip {
                return Hit::CloseStrip;
            }
            let items = app.dialog_focus_items();
            return match items.iter().position(|it| it.id == id) {
                Some(i) => Hit::DialogWidget(i),
                None => Hit::Dialog,
            };
        }
        Mode::Dialog(_) => {
            // Resolve the click against the dialog's own focus items, so pressing a
            // checkbox toggles it and pressing a list row selects it. A click inside the box
            // but on nothing still counts as Dialog, which swallows the press.
            let items = app.dialog_focus_items();
            if let Some(i) = crate::ui::framework::focus_at(&items, x, y) {
                return Hit::DialogWidget(i);
            }
            // A box dialog that did not claim the point from its own widgets: if the point is
            // still inside the drawn frame it is the box's border or padding and the press is
            // swallowed; if it is outside, the user aimed past the dialog.
            //
            // The order matters. `focus_at` first, because the box contains its widgets, and a
            // point that is both inside the box and on a control must activate the control --
            // testing the box first would turn every click on a button into a dismissal.
            return match app.dialog_box_rect() {
                Some(area) if area.contains(x, y) => Hit::Dialog,
                Some(_) => Hit::DialogOutside,
                // Find and Replace are docked strips with no box; they answered above.
                None => Hit::Dialog,
            };
        }
        // The panel is docked, not modal: the editor under it stays clickable, so this is *not*
        // an early return. Only the panel's own rows are claimed, which the check below does by
        // geometry. A user who opened a search should still be able to click a tab.
        Mode::FindPanel => {}
        Mode::Editing | Mode::Menu { .. } => {}
    }

    // The docked Find in Files panel is claimed *before* the bounds guard.
    //
    // `regions.editor` is the band minus the panel -- that is how the panel gets its rows -- so
    // the panel's own rectangle sits partly below `editor`'s bottom edge. A guard on `editor`
    // therefore answered `Outside` for every panel row beneath that edge, and the `Hit::FindPanel`
    // arm further down was unreachable: clicks on results, on the field rows, and on the Search
    // and Close words all did nothing. The panel is still not modal, which is why this claims
    // only its own rectangle and lets every other row fall through as before.
    // The docked Find in Files panel is claimed *before* the bounds guard.
    //
    // `regions.editor` is the band minus the panel -- that is how the panel gets its rows -- so
    // the panel's own rectangle sits partly below `editor`'s bottom edge. A guard on `editor`
    // therefore answered `Outside` for every panel row beneath that edge, and the `Hit::FindPanel`
    // arm further down was unreachable: clicks on results, on the field rows, and on the Search
    // and Close words all did nothing. The panel is still not modal, which is why this claims
    // only its own rectangle and lets every other row fall through as before.
    if app.find_panel_height > 0 && find_panel_region(app).contains(x, y) {
        return Hit::FindPanel;
    }

    if !app.regions.contains(x, y) {
        return Hit::Outside;
    }

    // An open dropdown outranks the regions it covers: it is drawn over the editor, and a
    // click on an item must run that item rather than place the caret behind the box.
    if let Mode::Menu { .. } = app.mode() {
        // The topmost level wins. A submenu box sits beside its parent and the two can
        // overlap once the box flips left at the screen edge, so asking the parent first
        // would steal clicks that belong to the child.
        if let Some(area) = active_dropdown_rect(app) {
            if area.contains(x, y) {
                return match dropdown_item_at(app, area, y) {
                    Some(i) => Hit::MenuItem(i),
                    None => Hit::MenuBar(None),
                };
            }
        }
    }
    if app.regions.menu.contains(x, y) {
        return Hit::MenuBar(menu_hit_at(app.regions.menu, x));
    }
    if app.regions.tabs.contains(x, y) {
        return Hit::TabBar(tab_hit_at(app, x));
    }
    if app.banner.is_some() && banner_region(app).contains(x, y) {
        return Hit::Banner;
    }
    if app.regions.status.contains(x, y) {
        return Hit::StatusBar(status_segment(app, x));
    }
    if !app.geometry.text_area.contains(x, y) {
        // Inside the editor band but outside the text area. Three things live out here: the
        // gutter strip, the vertical bar's track (the text area's last column is reserved for
        // it), and the horizontal bar's row.
        //
        // The scrollbar gets its chance before the gutter and before `Outside`. It did not, and
        // that is why the left end of the horizontal bar was dead: the track spans the editor
        // band's full width while the text area stops short of the gutter, so the bar's first
        // columns fell outside the text area, were answered `Outside`, and never reached the bar
        // at all. A drag that started anywhere else and travelled left stopped responding exactly
        // there -- the view parked at whatever offset the last in-bounds cell asked for and would
        // not come back to the beginning however far the pointer was pulled.
        if let Some(hit) = scrollbar_hit(app, x, y) {
            return hit;
        }
        if let Some(hit) = gutter_hit(app, x, y) {
            return hit;
        }
        return Hit::Outside;
    }
    if let Some(hit) = scrollbar_hit(app, x, y) {
        // The vertical track is painted over the text area's rightmost column, so this has to
        // be checked before the text arm or a click on the thumb would place the caret.
        return hit;
    }
    Hit::Text {
        lc: text_position_at(app, x, y),
        screen_row: y.saturating_sub(app.geometry.text_area.y),
    }
}

/// The banner strip: one row directly above the editor, at the top of the editor region.
///
/// The row is reserved here rather than derived by the renderer, so `ui::dialogs::draw_banner`
/// and this hit-test agree on where the banner is without either importing the other's
/// arithmetic.
pub fn banner_region(app: &App) -> RectCore {
    let editor = app.regions.editor;
    RectCore::new(editor.x, editor.y, editor.w, 1)
}

/// The Find-in-Files panel band, docked to the bottom of the editor *band*.
///
/// Must track `ui::find_in_files::panel_rect` exactly. It used to read `regions.editor`, which
/// since S19 is the band minus the panel, so this rect sat one panel-height below the rows the
/// panel was actually painted into: clicks on results landed on nothing, and clicks in the text
/// just above the panel were routed to the panel.
fn find_panel_region(app: &App) -> RectCore {
    let band = app.regions.band;
    let h = app.find_panel_height.min(band.h);
    RectCore::new(band.x, band.bottom().saturating_sub(h), band.w, h)
}

/// Which menu title, if any, is at `x`.
///
/// Uses the same span computation the renderer walks, so a click always lands on the title
/// the user saw.
fn menu_hit_at(menu: RectCore, x: u16) -> Option<(MenuId, usize)> {
    let mut col = menu.x.saturating_add(1);
    for (i, menu_id) in MenuId::ALL.iter().enumerate() {
        let title = menu_id.title();
        let width = title.chars().count() as u16;
        if x >= col && x < col.saturating_add(width) {
            return Some((*menu_id, i));
        }
        // Two spaces between titles, matching `menu_bar::draw`.
        col = col.saturating_add(width + 2);
    }
    None
}

/// Which tab, if any, is at `x`. `None` is empty tab-bar space.
///
/// Delegates to `ui::tab_bar::spans`, which is the same layout the renderer drew. The S6
/// version walked raw label widths here, which was fine while a label was always the whole
/// filename; with elision and horizontal scrolling, a locally computed layout would put
/// clicks on the wrong document.
fn tab_hit_at(app: &App, x: u16) -> Option<usize> {
    crate::ui::tab_bar::spans(app)
        .into_iter()
        .find(|span| span.contains(x))
        .map(|span| span.index)
}

/// Whether `x` is the `x` close button of a visible tab.
///
/// Checked before the tab itself, because the close cell is *inside* the tab's span: if the
/// tab arm won first, clicking the X would just activate the tab and the affordance would do
/// nothing.
fn tab_close_hit_at(app: &App, x: u16) -> Option<usize> {
    crate::ui::tab_bar::spans(app)
        .into_iter()
        .find(|span| span.close == Some(x))
        .map(|span| span.index)
}

/// The tab-bar label for a document, without a close button.
///
/// Kept as the no-close convenience form; the bar itself calls
/// [`crate::tabs::tab_label`] with the close flag set for the active or hovered tab.
pub fn tab_label(doc: &np_core::Document) -> String {
    crate::tabs::tab_label(doc, false)
}

/// The menu-bar spans, in screen order. Shared with the renderer for the same reason as
/// [`tab_label`].
pub fn menu_bar_spans(menu: RectCore) -> Vec<(MenuId, usize, u16, u16)> {
    let mut out = Vec::new();
    let mut col = menu.x.saturating_add(1);
    for (i, menu_id) in MenuId::ALL.iter().enumerate() {
        let title = menu_id.title();
        let width = title.chars().count() as u16;
        if col.saturating_add(width) > menu.right() {
            break;
        }
        out.push((*menu_id, i, col, width));
        col = col.saturating_add(width + 2);
    }
    out
}

/// Which status-bar segment is at `x`.
///
/// Asks `ui::status_bar::status_segments` for the same spans the renderer drew, so the two
/// can never disagree about where the encoding column starts. The previous version of this
/// function rebuilt the strings and re-counted the columns by hand, which meant any change
/// to the status bar silently moved the click targets.
fn status_segment(app: &App, x: u16) -> StatusSegment {
    let segments = crate::ui::status_bar::status_segments(app);
    for span in segments {
        if x >= span.x && x < span.x.saturating_add(span.width) {
            return span.segment;
        }
    }
    StatusSegment::Other
}

/// The gutter's column layout, as both the renderer and this hit-tester compute it.
///
/// Built from the same inputs `ui::editor_view::draw` uses, which is the only reason a click
/// on a `\u{25b6}` toggles the fold under it. Re-deriving it here from `geometry.gutter_width`
/// alone would work today and drift the moment the gutter grows a column.
pub fn gutter_layout(app: &App) -> crate::ui::gutter::Gutter {
    let editor = app.regions.editor;
    let Some(doc) = app.doc() else {
        return crate::ui::gutter::Gutter::default();
    };
    crate::ui::gutter::Gutter::new(
        editor.x,
        doc.buffer.line_count(),
        app.config.ui.line_numbers,
        !doc.folds.is_empty(),
        !doc.bookmarks.is_empty(),
    )
}

/// What a pointer in the gutter strip hit, resolved through [`Gutter::hit_at`].
///
/// The row is resolved through `layout_visible_rows`, so a folded-away line cannot be clicked
/// and a wrapped line's second screen row reports its own line (rows 11-13 all key off it).
fn gutter_hit(app: &App, x: u16, y: u16) -> Option<Hit> {
    let gutter = gutter_layout(app);
    if !gutter.contains(x) {
        return None;
    }
    let line = gutter_line_at(app, x, y)?;
    Some(match gutter.hit_at(x) {
        crate::ui::gutter::GutterTarget::Fold => Hit::GutterFold(line),
        crate::ui::gutter::GutterTarget::Bookmark => Hit::GutterBookmark(line),
        crate::ui::gutter::GutterTarget::LineNumber => Hit::GutterLine(line),
        crate::ui::gutter::GutterTarget::Outside => return None,
    })
}

/// The document line under a pointer in the gutter, or `None` if the row is blank.
fn gutter_line_at(app: &App, _x: u16, y: u16) -> Option<usize> {
    let geom = app.geometry;
    if geom.gutter_width == 0 {
        return None;
    }
    let screen_row = y.checked_sub(geom.text_area.y)?;
    let rows = visible_rows(app);
    rows.get(screen_row as usize).map(|r| r.line)
}

/// Which part of which scrollbar the pointer is on, thumb winning over trough.
///
/// The geometry comes from `App::vertical_scrollbar` / `App::horizontal_scrollbar`, the same
/// calls the renderer makes, so the grab target is exactly the cells that were painted as the
/// thumb. §6.9 forbids reading ratatui's internals here, and this is also the reason: the
/// shared arithmetic is testable without a terminal, an internals read is not.
pub fn scrollbar_hit(app: &App, x: u16, y: u16) -> Option<Hit> {
    let bars = app.geometry.bars;
    let editor = app.regions.editor;
    // Each bar owns a strip of the editor band, and the strips are exactly the cells
    // `BarOccupancy` reserved: the vertical bar the band's last `cols` columns down its whole
    // height, the horizontal bar the band's last `rows` rows across the width minus that
    // corner. Testing the strips rather than a single centreline is what makes a click *on* a
    // bar resolve to the bar -- the old version only claimed the one cell the bar was drawn
    // in, and only after the text arm had been offered the cell, so a click on the trough
    // placed the caret instead of scrolling and the horizontal bar was unreachable entirely.
    //
    // The corner cell belongs to the vertical bar. It is drawn that way (the horizontal bar's
    // area stops short of it), and picking one owner is the only way that cell never means two
    // things.
    //
    if bars.cols > 0 && x >= editor.right().saturating_sub(bars.cols) && editor.contains(x, y) {
        if let Some(bar) = app.vertical_scrollbar() {
            return Some(thumb_or_track(VerticalOrHorizontal::Vertical, bar, y));
        }
    }
    if bars.rows > 0
        && y >= editor.bottom().saturating_sub(bars.rows)
        // The corner cell is the vertical bar's, and the arm above already claimed it.
        && x < editor.right().saturating_sub(bars.cols)
        && editor.contains(x, y)
    {
        if let Some(bar) = app.horizontal_scrollbar() {
            return Some(thumb_or_track(VerticalOrHorizontal::Horizontal, bar, x));
        }
    }
    None
}

/// Whether focus child `i` of the open dialog is a list row.
///
/// The question the mouse layer asks so it can tell "the user is pointing at an entry" from "the
/// user is pointing at a control". Rows are selected by a press and activated by a click;
/// controls -- buttons, checkboxes, fields -- take their own action on the press, which is what
/// every toolkit does and what makes a dialog's OK button feel immediate.
fn is_row_child(app: &App, i: usize) -> bool {
    matches!(
        app.dialog_focus_items().get(i).map(|item| item.id),
        Some(crate::ui::framework::FocusId::ListRow(_))
    )
}

/// The thumb-or-track decision for a cell already known to be on `bar`'s track.
fn thumb_or_track(
    kind: VerticalOrHorizontal,
    bar: crate::ui::gutter::ScrollbarGeom,
    cell: u16,
) -> Hit {
    // `cell` is a screen row (vertical) or column (horizontal), and `ScrollbarGeom` is expressed
    // in the same screen coordinate -- `track_start` is the band's first row, not 0 -- so the
    // comparison is direct. Reporting the screen cell in `Hit` unchanged is what lets a caller
    // compare it back against the event that produced it.
    if bar.is_thumb(cell) {
        Hit::ScrollbarThumb(kind, cell)
    } else {
        Hit::ScrollbarTrack(kind, cell)
    }
}

// ------------------------------------------------------- text-area coordinates

/// The visible visual rows, from the stored geometry. Never recomputed from scratch.
fn visible_rows(app: &App) -> Vec<VisualRow> {
    let Some(doc) = app.doc() else {
        return Vec::new();
    };
    layout_visible_rows(&doc.buffer, &app.geometry, doc.scroll.top_line, &doc.folds)
}

/// Map a pointer position in the text area to a clamped document position.
///
/// Three things have to line up here and each has its own trap:
///  - the row comes from `layout_visible_rows`, so folded lines are skipped and a wrapped
///    line resolves to the right slice of itself;
///  - the column is a *display* column, converted with `col_from_display` so tabs and
///    double-width graphemes land on the right character rather than a cell count;
///  - `scroll.left_col` shifts the column when wrap is off, and is 0 when wrap is on (FR-V1).
///
/// A click right of a short line clamps to that line's end (§6.9 row 1), and a click below
/// the last line clamps to the last line, because a caret that refuses to go where the user
/// clicked reads as a broken mouse rather than as correct clamping.
pub fn text_position_at(app: &App, x: u16, y: u16) -> LineCol {
    let Some(doc) = app.doc() else {
        return LineCol::new(0, 0);
    };
    let geom = app.geometry;
    let screen_row = y.saturating_sub(geom.text_area.y);
    let rows = visible_rows(app);

    let row = match rows.get(screen_row as usize) {
        Some(r) => *r,
        // Below the last visible row: the drag case (row 2) and the click-below-content case
        // both want the last row that actually exists.
        None => match rows.last() {
            Some(r) => *r,
            None => return doc.buffer.clamp(LineCol::new(0, 0)),
        },
    };

    let display_col = display_column(app, x);
    position_in_row(&doc.buffer, &geom, row, display_col)
}

/// Resolve a display column within one visual row to a document position.
///
/// Split out so the wrapped-row arithmetic is testable on its own, without a terminal: with
/// wrap on, `layout_visible_rows` emits several rows per logical line and each carries its own
/// `start_col`/`end_col` slice.
///
/// The column is measured **from `row.start_col`**, which is the part that goes wrong if you
/// write it naively: converting the screen offset against the whole line makes the second
/// screen row of a wrapped line point back at the beginning of the line.
pub fn position_in_row(
    buffer: &Buffer,
    geom: &np_core::ViewGeometry,
    row: VisualRow,
    display_col: usize,
) -> LineCol {
    let line_len = buffer.line_len_chars(row.line);
    // Measure within the row's own slice. `col_from_display` counts from the start of the
    // line, so the row's leading width is subtracted; doing the conversion against the whole
    // line is the naive version and it makes the second screen row of a wrapped line point
    // back at the beginning of that line.
    let row_lead = buffer.display_width_range(row.line, 0, row.start_col, geom.tab_width.max(1));
    let cells_into_row = display_col.saturating_add(row_lead);
    let mut col = buffer.col_from_display(row.line, cells_into_row, geom.tab_width.max(1));

    // A wrapped row owns only its own slice: clicking past its end means the end of the row,
    // not the end of the logical line, which would otherwise jump the caret several characters
    // to the right of where the user clicked.
    if row.end_col > row.start_col && col >= row.end_col {
        col = row.end_col;
    }
    if col > line_len {
        col = line_len;
    }
    buffer.clamp(LineCol::new(row.line, col))
}

/// The character column within a row that a screen `x` refers to.
fn display_column(app: &App, x: u16) -> usize {
    let geom = app.geometry;
    let Some(doc) = app.doc() else {
        return 0;
    };
    let in_row_offset = x.saturating_sub(geom.text_area.x) as usize;
    // Horizontal scroll shifts which characters are on screen. While wrap is on, left_col is
    // forced to 0, so this is just the x offset.
    let shift = if geom.word_wrap {
        0
    } else {
        doc.scroll.left_col
    };
    in_row_offset.saturating_add(shift)
}

/// The first and last visible document positions, used to bound a drag.
fn viewport_bounds(app: &App) -> Option<(LineCol, LineCol)> {
    let rows = visible_rows(app);
    let first = *rows.first()?;
    let last = *rows.last()?;
    let doc = app.doc()?;
    let top = doc.buffer.clamp(LineCol::new(first.line, 0));
    let bottom = doc.buffer.clamp(LineCol::new(
        last.line,
        doc.buffer.line_len_chars(last.line),
    ));
    Some((top, bottom))
}

// ----------------------------------------------------------------- the table

/// The §6.9 table itself. One arm per event kind, and inside each, one branch per row.
#[allow(clippy::too_many_lines)]
fn decide(
    ev: &MouseEvent,
    app: &App,
    hit: &Hit,
    click: &ClickState,
    now: std::time::Instant,
) -> MouseIntent {
    let mods = ev.modifiers;
    let shift = mods.contains(KeyModifiers::SHIFT);
    let alt = mods.contains(KeyModifiers::ALT);

    match ev.kind {
        // ---- row 20: hover. `Moved` is a pointer move with no button held.
        MouseEventKind::Moved => {
            // A move must never change the selection; only the highlight state follows it.
            match hit {
                Hit::TabBar(Some(i)) => MouseIntent::Hover(Some(HoverTarget::Tab(*i))),
                Hit::MenuBar(Some((_, i))) => MouseIntent::Hover(Some(HoverTarget::Menu(*i))),
                Hit::Text { .. } => MouseIntent::Hover(Some(HoverTarget::Text)),
                _ => MouseIntent::Hover(None),
            }
        }

        // ---- rows 8, 9, 10, 17: the wheel.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let dir = match ev.kind {
                MouseEventKind::ScrollUp => -1,
                _ => 1,
            };
            if matches!(hit, Hit::TabBar(_)) {
                // Row 17: wheel over the tab bar cycles tabs.
                return MouseIntent::CycleTabs(dir);
            }
            if matches!(hit, Hit::FindPanel) {
                // The panel owns the wheel over itself. Scrolling the results is the obvious
                // gesture and was reported missing; scrolling the document underneath a panel the
                // pointer is resting on is not, and would make the results impossible to read
                // while the file jumped about behind them.
                return MouseIntent::ScrollFindResults { lines: dir };
            }
            // The wheel over an open dialog or palette belongs to that box, over whatever it is
            // pointing at. This is checked before the fall-through to document scroll because the
            // alternative was the reported behaviour: a wheel over the Theme picker scrolled the
            // file behind it. A list dialog scrolls its list; a box with no list -- a confirm
            // prompt, About, Go To -- has nothing to scroll and swallows the event rather than
            // moving text the user cannot act on while a modal box is up.
            if matches!(
                app.mode(),
                Mode::Palette | Mode::Dialog(_) | Mode::HelpOverlay | Mode::ContextMenu(_)
            ) {
                // One gesture, one distance: the same step the wheel uses in the buffer, so a
                // Preferences list and a file scroll the same amount for one notch.
                let rows = dir * app.config.ui.mouse_scroll_lines.max(1) as i32;
                return MouseIntent::ScrollDialogList { rows };
            }
            if matches!(hit, Hit::MenuBar(_) | Hit::StatusBar(_) | Hit::Outside) {
                return MouseIntent::Nothing;
            }
            if shift {
                // Row 9: horizontal scroll. Inert while wrap is on, because with wrap there
                // is no horizontal position to move (FR-V1).
                if app.geometry.word_wrap {
                    return MouseIntent::Nothing;
                }
                return MouseIntent::ScrollH {
                    cols: dir * HORIZONTAL_SCROLL_STEP,
                };
            }
            if mods.contains(KeyModifiers::CONTROL) {
                // Row 10. §6.9 asks for Ctrl+wheel zoom; there is no font size to change in a
                // terminal, so this is the documented deviation: a full-page scroll (FR-V8).
                return MouseIntent::Scroll {
                    lines: dir * page_height(app) as i32,
                };
            }
            let step = app.config.ui.mouse_scroll_lines.max(1) as i32;
            MouseIntent::Scroll { lines: dir * step }
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            if app.geometry.word_wrap || matches!(hit, Hit::Outside) {
                return MouseIntent::Nothing;
            }
            let dir = match ev.kind {
                MouseEventKind::ScrollLeft => -1,
                _ => 1,
            };
            MouseIntent::ScrollH {
                cols: dir * HORIZONTAL_SCROLL_STEP,
            }
        }

        // ---- rows 1, 3, 4, 5, 6, 15, 16, 18: button presses.
        MouseEventKind::Down(MouseButton::Left) => {
            // A context menu is open. S14 resolves item clicks through `Hit`, so only a
            // click that missed the menu dismisses it.
            if matches!(hit, Hit::ContextMenu) {
                return MouseIntent::DismissContextMenu;
            }
            match hit {
                Hit::Dialog => {
                    // Inside the box but not on a widget: consumed, so it cannot also reach
                    // the text underneath.
                    MouseIntent::Nothing
                }
                Hit::DialogOutside => MouseIntent::CloseDialog,
                // A press on a list row selects that row; a press on anything else moves focus.
                // The split is because a press is also the first half of a drag, and dragging
                // down a listing is scanning, not choosing.
                Hit::DialogWidget(i) if is_row_child(app, *i) => MouseIntent::SelectDialogRow(*i),
                Hit::DialogWidget(i) => MouseIntent::FocusDialogChild(*i),
                Hit::Banner => banner_press_intent(app, ev.column),
                Hit::ContextMenuItem(i) => {
                    let kind = match app.mode() {
                        Mode::ContextMenu(k) => k,
                        _ => return MouseIntent::Nothing,
                    };
                    match crate::ui::context_menu::items(app, kind)
                        .get(*i)
                        .filter(|it| it.enabled)
                        .map(|it| it.action)
                    {
                        Some(a) => MouseIntent::RunAction(a),
                        // A disabled row still belongs to the menu, so the click is consumed
                        // rather than falling through to the text behind it.
                        None => MouseIntent::Nothing,
                    }
                }
                Hit::PaletteRow(i) => {
                    // Select, do not run. The palette runs *commands*, so a single press that
                    // fired one was the most damaging instance of the click-to-open problem: the
                    // pointer crossed a row on its way to the query field and the app executed an
                    // arbitrary command. Double-click activates, through the same click-count
                    // state every other list uses, and `Enter` in the field is unchanged.
                    MouseIntent::SelectPaletteRow(*i)
                }
                Hit::FindPanel => {
                    // The command words are tried before the field rows, even though they sit on
                    // the glob field's own row. `field_at_row` answers `Glob` for that row, so a
                    // click on Search would otherwise focus the glob instead of searching.
                    if let Some(cmd) = crate::ui::find_in_files::hit_command(app, ev.column, ev.row)
                    {
                        return if cmd.ends_with("Close") {
                            MouseIntent::CloseServiceStrip
                        } else {
                            MouseIntent::RunFindPanelSearch
                        };
                    }
                    // A click on a field row points the keyboard at that field, so typing goes
                    // where the pointer was put. Without this the only way to change field was Tab.
                    // Then the toggle boxes, which live on the glob field's row for the same
                    // reason the commands do and lose to it on the same terms.
                    if let Some(toggle) =
                        crate::ui::find_in_files::hit_toggle(app, ev.column, ev.row)
                    {
                        return MouseIntent::ToggleFindFlag(toggle);
                    }
                    if let Some(field) = crate::ui::find_in_files::field_at_row(app, ev.row) {
                        return MouseIntent::SelectFindField(field);
                    }
                    // S11: a click on a result row selects that row. Which row is decided by
                    // the same row math the renderer uses (§6.9: renderer and hit-tester must
                    // agree), computed here from the event rather than passed in, so the intent
                    // is a resolved result index and the App never re-derives geometry.
                    match crate::ui::find_in_files::row_index_at(app, ev.row) {
                        Some(row) => {
                            // `click` is the state from *before* this press, so the run count for
                            // the row this press is about is one more than it holds. `route` returns
                            // the next state separately, which is why this reads as a +1 rather than
                            // as a lookup of the current press.
                            let repeat = click.count.saturating_add(1);
                            let same_place = click.cell == Some((ev.column, ev.row));
                            if same_place && repeat >= 2 {
                                MouseIntent::OpenFindHit(row)
                            } else {
                                MouseIntent::SelectFindHit(row)
                            }
                        }
                        None => MouseIntent::FocusFindPanel,
                    }
                }
                Hit::MenuItem(i) => {
                    let Mode::Menu { open, parent, .. } = app.mode() else {
                        return MouseIntent::Nothing;
                    };
                    // The level the pointer is actually in. Resolving the parent's top-level
                    // list here meant a click on a codec row ran whatever sat at that index in
                    // the Encoding menu.
                    let items = crate::ui::menus::level_items(app, open, parent);
                    let Some(entry) = items.get(*i).cloned() else {
                        return MouseIntent::Nothing;
                    };
                    // A submenu row is a leaf; only a top-level arrow row opens a level.
                    if parent.is_none() {
                        if let crate::ui::menus::MenuItemKind::Submenu(parent_id) = entry.kind {
                            let children =
                                crate::ui::menus::submenu_items(app, parent_id, &entry.label);
                            if !children.is_empty() {
                                return MouseIntent::OpenSubmenu(parent_id, entry.label.clone());
                            }
                        }
                    }
                    if entry.action == Some(Action::RecentFiles) {
                        return MouseIntent::OpenRecentFile(std::path::PathBuf::from(&entry.label));
                    }
                    match entry.action {
                        Some(_) => MouseIntent::RunMenuItem(Box::new(entry)),
                        None => MouseIntent::Nothing,
                    }
                }
                Hit::CloseStrip => MouseIntent::CloseServiceStrip,
                Hit::MenuBar(Some((menu_id, _))) => MouseIntent::Action(Action::MenuBar(*menu_id)),
                Hit::MenuBar(None) => MouseIntent::Nothing,
                Hit::StatusBar(segment) => status_click_intent(app, *segment),
                Hit::TabBar(Some(i)) => {
                    // The `x` on the active or hovered tab closes it; anywhere else on the
                    // tab activates. Both come from the same span layout the renderer drew.
                    if tab_close_hit_at(app, ev.column).is_some() {
                        MouseIntent::CloseTab(*i)
                    } else {
                        MouseIntent::ActivateTab(*i)
                    }
                }
                Hit::TabBar(None) => MouseIntent::Nothing,
                Hit::GutterFold(line) => {
                    // Row 12: the fold marker column. `ToggleFold` does nothing on a line with
                    // no fold, which is the right answer for a click in the column but between
                    // markers -- the column is fixed for the whole strip, the markers are not.
                    MouseIntent::ToggleFold(*line)
                }
                Hit::GutterBookmark(line) => {
                    // Row 13: the bookmark column always means "toggle on this line".
                    MouseIntent::ToggleBookmark(*line)
                }
                Hit::GutterLine(line) => {
                    // Row 11: click in the gutter selects the line and leaves the caret alone.
                    MouseIntent::SelectLine {
                        line: *line,
                        keep_caret: true,
                    }
                }
                Hit::ScrollbarThumb(kind, _) => {
                    // Row 14, thumb: the press jumps to the pointer rather than waiting for a
                    // drag, and the Drag arm below keeps following it while the button is held.
                    scrollbar_press(
                        app,
                        *kind,
                        if kind.is_vertical() {
                            ev.row
                        } else {
                            ev.column
                        },
                    )
                }
                Hit::ScrollbarTrack(kind, cell) => {
                    // Row 14, trough: a press *aims*. The clicked cell stands for a position in
                    // the document and the view goes there, which is the same rule a thumb drag
                    // follows -- so a click and a drag on the same cell cannot disagree, and the
                    // ends of the bar reach the ends of the file.
                    //
                    // The alternative, paging one viewport toward the click, was a deliberate
                    // choice in S6 and it is the reason the bar felt dead at both ends: a click in
                    // the trough's last cell moved one page, so reaching the bottom of a long
                    // file meant clicking the same cell dozens of times, and the pointer sitting
                    // at the bottom of the bar while the text crawled read as a scrollbar that
                    // only works in the middle. Paging is still what a wheel does, and the wheel
                    // is where that gesture belongs.
                    scrollbar_press(app, *kind, *cell)
                }
                Hit::Outside => MouseIntent::Nothing,
                Hit::Text { lc, .. } => {
                    // Row 18: right-click is handled under Down(Right); this is left.
                    let (count, _) = click.register((ev.column, ev.row), now);
                    if alt {
                        // Row 6: alt+drag is a block selection. The drag arm continues it.
                        return MouseIntent::BeginSelection {
                            kind: SelectionKind::Block,
                            anchor: *lc,
                            focus: *lc,
                        };
                    }
                    if count >= 3 {
                        // Row 5: triple click selects the line. Sticky, so 4+ clicks stay
                        // line-selected rather than toggling back to a caret.
                        return select_line_intent(app, lc.line);
                    }
                    if count == 2 {
                        // Row 4: double click selects the word.
                        return word_select_intent(app, *lc);
                    }
                    // Row 3 then row 1: shift+click extends, plain click places the caret.
                    if shift {
                        return MouseIntent::SetCaret {
                            lc: *lc,
                            extend: true,
                        };
                    }
                    MouseIntent::SetCaret {
                        lc: *lc,
                        extend: false,
                    }
                }
                Hit::ContextMenu => MouseIntent::DismissContextMenu,
            }
        }

        MouseEventKind::Down(MouseButton::Right) => match hit {
            // Row 18: right click opens the context menu for what it is over, and must not
            // move the caret — a menu that relocates the cursor before copying is a data bug.
            Hit::Text { lc, .. } => MouseIntent::OpenContextMenu(ContextKind::Editor, *lc),
            Hit::TabBar(Some(i)) => {
                MouseIntent::OpenContextMenu(ContextKind::Tab, LineCol::new(*i, 0))
            }
            _ => MouseIntent::Nothing,
        },

        MouseEventKind::Down(MouseButton::Middle) => match hit {
            // Row 7: middle click pastes the primary selection. S7 owns the provider; the
            // intent is routed now so wiring the clipboard changes nothing here.
            Hit::Text { lc, .. } => {
                let _ = lc;
                MouseIntent::PastePrimary
            }
            // Row 16: middle click on a tab closes it.
            Hit::TabBar(Some(i)) => MouseIntent::CloseTab(*i),
            _ => MouseIntent::Nothing,
        },

        // ---- row 2: drag extends the selection.
        MouseEventKind::Drag(MouseButton::Left) => {
            // Row 11, dragging in the gutter: extend the line range to the row under the
            // pointer. The anchor stays where the press started, so a 3 -> 7 sweep ends up
            // selecting five whole lines rather than just the last row touched.
            if let Hit::GutterLine(line) = hit {
                return gutter_drag_intent(app, *line);
            }
            // Row 14, dragging a scrollbar: thumb or trough, the drag belongs to the grab.
            //
            // The thumb case matters more than it looks. The thumb slides under the pointer, so
            // the pointer can end up *inside* it mid-drag; treating that as "no longer dragging"
            // freezes the view the moment the thumb catches up, which reads as a scrollbar with a
            // dead zone in the middle of its travel. And a drag on the trough is the normal case,
            // not an edge case -- the thumb is a few cells and the pointer crosses the whole
            // trough -- so a hit-test that stopped answering off-thumb would leave the view stuck
            // at whatever the last on-thumb event produced.
            if let Hit::ScrollbarThumb(kind, cell) | Hit::ScrollbarTrack(kind, cell) = hit {
                if let Some(grab) = app.scrollbar_drag.filter(|g| g.kind == *kind) {
                    return scrollbar_drag_intent(app, grab, *cell);
                }
                // Nothing grabbed: a drag that arrived without a press, which happens after a
                // focus loss or a dropped event. Grabbing from where the pointer is now is the
                // recoverable reading; paging on it would scroll the view on what looks to the
                // user like a failed grab.
                return scrollbar_press(app, *kind, *cell);
            }
            let Hit::Text { lc, .. } = hit else {
                return MouseIntent::Nothing;
            };
            let kind = if alt {
                SelectionKind::Block
            } else {
                SelectionKind::Range
            };
            let focus = bound_to_viewport(app, *lc);
            // Anchor precedence matters and is the difference between a drag that selects the
            // range the user swept and one that selects from an unrelated place:
            //  1. the anchor captured when *this* drag started, so every subsequent drag event
            //     extends the same selection;
            //  2. otherwise the caret, because a drag that begins without one starts here.
            // A pre-existing selection's anchor is deliberately NOT consulted: shift+click
            // then drag must re-anchor at the press point, and reusing the old anchor made the
            // first drag event select from wherever a previous selection happened to start.
            let anchor = app.drag_anchor.unwrap_or(app.cursor_pos());
            if app.drag_anchor.is_none() {
                return MouseIntent::BeginSelection {
                    kind,
                    anchor,
                    focus,
                };
            }
            MouseIntent::ExtendSelection { focus }
        }
        MouseEventKind::Drag(MouseButton::Right) | MouseEventKind::Drag(MouseButton::Middle) => {
            MouseIntent::Nothing
        }

        // Button release ends the drag. Nothing else happens: the selection is already right,
        // and doing more here would fight the primary-selection semantics S7 depends on.
        MouseEventKind::Up(button) => {
            // A release on the same dialog widget the press landed on is a click, and a click
            // activates. This is the second half of the split that lets a list press select
            // without acting: the press records what it pressed, and the release only fires when
            // the pointer never left it.
            // Only a row waits for the release to activate. A button or checkbox already acted on
            // its press -- which is what makes those controls feel immediate -- so firing the
            // release at them as well would run the action twice: a click on a Find checkbox
            // toggled it on and straight back off again.
            // The row is decided from what the *press* recorded, not from what the release
            // hit-tests to. The two are usually the same child, and after a press they cannot be
            // relied on to be: selecting a row moves the list's window, so a listing scrolled by
            // the press itself re-resolves the same screen cell to a different row. The press
            // already settled which row the pointer meant; the release only has to confirm the
            // pointer is still inside the listing, which the widget-kind test does.
            if matches!(button, MouseButton::Left) {
                // The command palette: select on the first click, run on the second.
                //
                // The palette is not a `Mode::Dialog`, so it has no `dialog_press` record to read
                // the pressed row from, and it does not need one: its rows never scroll the window
                // out from under the pointer between the two clicks of a deliberate double-click at
                // the same cell, because selecting a palette row cannot change the query and so
                // cannot change the filtered entry list. The row the release hit-tests to is the
                // row the press meant.
                if let (Mode::Palette, Hit::PaletteRow(row)) = (app.mode(), hit) {
                    if click.count >= 2 {
                        return MouseIntent::RunAction(
                            crate::ui::palette::palette_entries(app, &app.palette_query)
                                .get(*row)
                                .map(|e| e.action)
                                .unwrap_or(Action::Nop),
                        );
                    }
                    return MouseIntent::Nothing;
                }
                if let (Some(pressed), Hit::DialogWidget(_)) = (app.dialog_press, hit) {
                    // A row opens on the second click, not the first.
                    //
                    // Single-click-to-open in a file listing is the riskier gesture: the pointer
                    // brushes a neighbouring name while reaching for a button and a file is gone
                    // into a new tab. Selecting on press and opening on double-click is what every
                    // desktop file list does, and it keeps the press's own feedback -- the row
                    // highlights -- without the consequence.
                    //
                    // Only rows wait. A button or checkbox already fired on its press, which is why
                    // those still feel immediate, and firing on release as well would run them
                    // twice -- a Find checkbox toggled on and straight back off.
                    // The count comes from `ClickState`, not from the press record: a release
                    // always clears `dialog_press`, so the first click's record is gone by the time
                    // the second click's release arrives. `ClickState` is the one thing that
                    // survives a release, and it is keyed by the cell the user is repeating.
                    if pressed.row.is_some() && click.count >= 2 {
                        if let Some(row) = pressed.row {
                            return MouseIntent::ActivateDialogRow(pressed.child, row);
                        }
                    }
                }
            }
            // A release also ends any scrollbar drag. Without it the grab outlives the button and
            // the next time the pointer crosses the bar -- with the button up -- the old drag
            // answers, scrolling the view on a hover. Nothing else clears it, because the drag
            // state lives on `App` rather than in the event.
            MouseIntent::EndDialogPressAndDrag
        }
    }
}

/// Keep a drag focus inside the visible rows (work item 6).
fn bound_to_viewport(app: &App, lc: LineCol) -> LineCol {
    let Some((top, bottom)) = viewport_bounds(app) else {
        return lc;
    };
    if lc.line < top.line {
        top
    } else if lc.line > bottom.line {
        bottom
    } else {
        lc
    }
}

/// Row 4: select the word under the pointer.
fn word_select_intent(app: &App, lc: LineCol) -> MouseIntent {
    let Some(doc) = app.doc() else {
        return MouseIntent::Nothing;
    };
    let (start, end) = doc.buffer.word_range_at(lc);
    if start == end {
        // No word under the pointer: place the caret instead of selecting nothing at all,
        // which is what every other editor does on an empty cell.
        return MouseIntent::SetCaret { lc, extend: false };
    }
    MouseIntent::BeginSelection {
        kind: SelectionKind::Range,
        anchor: start,
        focus: end,
    }
}

/// Row 5: select a whole line.
///
/// The selection runs from column 0 of `line` to column 0 of the next line, so it includes
/// that line's terminator and a cut leaves no blank line behind — Notepad++'s behavior. On
/// the last line there is no next line, so it stops at the end of the text.
fn select_line_intent(app: &App, line: usize) -> MouseIntent {
    let Some(doc) = app.doc() else {
        return MouseIntent::Nothing;
    };
    let anchor = LineCol::new(line, 0);
    let focus = if line + 1 < doc.buffer.line_count() {
        LineCol::new(line + 1, 0)
    } else {
        LineCol::new(line, doc.buffer.line_len_chars(line))
    };
    MouseIntent::BeginSelection {
        kind: SelectionKind::Range,
        anchor,
        focus,
    }
}

/// The number of screen rows the editor can show, i.e. one "page".
///
/// Deliberately the viewport height and **not** `visible_rows().len()`. The row list is
/// capped by the document's length, so a 5-line file in a 21-row window would make a page
/// 5 rows and Ctrl+wheel would barely move. A page is what the screen can hold, which is
/// what a paging gesture means regardless of how much text exists.
fn page_height(app: &App) -> usize {
    app.geometry.viewport_rows().max(1)
}

/// The intent for pressing a scrollbar thumb at track cell `cell`.
///
/// A press puts the content the pointer is pointing at at the top of the view, which is what a
/// click on a scrollbar means everywhere: aim, click, that part of the file comes into view. It
/// is deliberately *not* "put the thumb's leading edge under the pointer" -- that reading makes a
/// click near the bottom of a long file jump a whole half-thumb further than the user aimed, and
/// it makes the first drag event slide the content by the depth of the grab.
///
/// A press on the **thumb itself** must not scroll at all. Notepad++ and every toolkit behave
/// this way, and in a terminal it is not a nicety: a click delivers its `Down` at the pointer's
/// position, so if grabbing the thumb also jumped the content, a click one cell off center would
/// shove the view a half-page further with every attempt to grab it, and the thumb could never be
/// held still. The `Down` arm asks the scope whether the cell is the thumb and leaves it alone.
fn scrollbar_press(app: &App, kind: VerticalOrHorizontal, cell: u16) -> MouseIntent {
    let Some(scope) = scroll_scope(app, kind) else {
        return MouseIntent::Nothing;
    };
    // The bar has two different gestures and they must not share one rule.
    //
    // **A press in the trough aims.** The clicked cell stands for a position and the view goes
    // there, which is what makes a click near the bottom of a long file jump near the bottom.
    //
    // **A press on the thumb holds.** The document does not move; the grab is recorded and the
    // following drag moves the view by the pointer's *travel*. This is what every desktop
    // scrollbar does and it is the fix for "on click it jumps upside and moves with an increased
    // offset upper than the mouse pointer".
    //
    // The arithmetic is why aiming reads as a jump. The thumb is 1 or 2 cells on a 21-cell trough
    // over a 200-line file, so one cell is worth roughly 9 lines. Grabbing the thumb at its top
    // cell aimed the view at that cell's proportional position and yanked the document by up to
    // 84 lines -- four screens -- before the user had moved the mouse at all. Measured on a
    // 2000-line file: the worst possible grab jumped 84 lines.
    //
    // The S17 note for this function argued that a thumb press must aim, because a press that
    // does nothing feels inert and the view then lurches on the first drag event. That argument
    // holds only if the drag is also absolute. With a travel-based drag the press *should* do
    // nothing: the hold is the first half of one continuous gesture, and the motion the user
    // asked for is the motion they make with the pointer. Both halves are answered here.
    if scope.geom.is_thumb(cell) {
        return thumb_grab_intent(app, kind, cell);
    }
    // The trough's first cell means "the beginning". Handled directly rather than through the
    // ratio: the thumb cannot be drawn past the trough, so the extreme cells are the ones
    // proportion cannot reach, and they are exactly the ones a user aims at.
    if cell <= scope.geom.track_start {
        return MouseIntent::ScrollToStart(kind);
    }
    let offset = scope.offset_at_cell(cell);
    press_intent(kind, cell, offset)
}

/// The intent for pressing directly on a thumb: hold, and let the drag do the moving.
///
/// Split out because it is the only path that must *not* move the view, and because the answer
/// depends on whether the bar can scroll at all in the direction the grab implies.
fn thumb_grab_intent(app: &App, kind: VerticalOrHorizontal, cell: u16) -> MouseIntent {
    let current = scroll_offset(app, kind);
    // A thumb already at an end cannot be dragged further that way, and the grab is still worth
    // recording: the drag arm below will report "no further" honestly rather than silently
    // scrolling the other way from a stale anchor.
    let grab = Box::new(crate::app::ScrollbarDrag {
        kind,
        start_offset: current,
        start_cell: cell,
        on_thumb: true,
    });
    match kind {
        VerticalOrHorizontal::Vertical => MouseIntent::HoldRowScroll(Some(grab)),
        VerticalOrHorizontal::Horizontal => MouseIntent::HoldColScroll(Some(grab)),
    }
}

/// The intent for a thumb **drag** event at `cell`.
///
/// A drag answers one question: where is the pointer now, in content terms. The offset is read
/// from the pointer's *absolute* cell against the trough's own geometry, which never moves while
/// the document scrolls -- so the drag cannot feed back into itself, and so the end of the
/// document stays reachable no matter how far the pointer has already travelled.
///
/// # Why not a delta from the grab
///
/// The previous implementation added the pointer's travel since the grab to the offset the grab
/// recorded. That is smooth, and it is wrong: travel is bounded by the trough, and one trough
/// cell is worth `scrollable / reachable` offsets, so a 19-cell trough over 159 scrollable rows
/// tops out at 171 of 173 reachable rows and -- worse -- a drag that started near the middle of
/// a long file can never reach either end. The user's report was exactly this: "very difficult to
/// reach the left or right end of the text using the mouse and the scrollbar".
///
/// The absolute reading has no such ceiling. Its cost is that the scale is coarse -- one cell is
/// worth many rows -- which is what a scrollbar on a long document *is*, and which the ends
/// handle directly: the last cells of the trough nudge to the true end rather than clamping just
/// short of it.
fn scrollbar_drag_intent(app: &App, grab: crate::app::ScrollbarDrag, cell: u16) -> MouseIntent {
    let Some(scope) = scroll_scope(app, grab.kind) else {
        return MouseIntent::Nothing;
    };
    // A drag moves by the pointer's **travel since the grab**, scaled by the same ratio the
    // trough uses. The anchor is the offset the bar was at when the button went down, not the
    // cell's absolute position.
    //
    // Two properties the reported bugs turned on, both preserved:
    //
    //  - The scale is the trough's, so one cell of pointer travel is one cell's worth of content
    //    however long the document. That is what a scrollbar means, and it is smooth: no
    //    teleporting on the first event.
    //  - Reach is still bounded by the *trough*, not by how far the finger happened to move,
    //    because the travel is measured in trough cells and the pointer can travel the whole
    //    trough. Anchoring to the offset rather than the cell does not reintroduce S17's ceiling:
    //    that ceiling came from adding travel to a *starting cell* whose own offset was near one
    //    end, so the sum ran out of trough. Here the sum runs out of *document*, and the ends are
    //    answered explicitly below.
    let offset = if grab.on_thumb {
        let travel = cell as i64 - grab.start_cell as i64;
        scope.scale_from(grab.start_offset, travel)
    } else {
        // A trough grab aims: the pointer's absolute cell, exactly as before.
        scope.offset_at_cell(cell)
    };
    // The ends are sticky on a thumb drag, in both directions.
    //
    // `offset == 0` is the wrong test for "the user wants the top". A drag that starts mid-file
    // has fewer cells of travel available upward than its offset needs -- 10 cells against the
    // ~10.1 a 1000-row offset is worth -- so the last tenth of the journey can never be made and
    // the view parks at row 10 with the pointer flat against the trough's top cell. That reads as
    // a scrollbar that will not quite reach the beginning, and the same arithmetic strands the
    // right-hand end of a long line.
    //
    // So a thumb drag that has travelled *past* either end of the trough means the end of the
    // document. The pointer cannot go further than the trough, so asking there is unambiguous --
    // and it is only this gesture that gets the concession; a trough press still aims at its cell,
    // where the absolute mapping already lands exactly on the ends.
    if grab.on_thumb {
        // One cell of slack at each end. The pointer's last usable cell is not always the
        // trough's nominal last one -- the horizontal bar's final cell is the corner the vertical
        // bar owns -- so a rule keyed to the exact end cell is unreachable on that bar, and the
        // view still parks a few rows short. A thumb drag at the boundary of the trough means the
        // boundary of the document.
        let last = scope
            .geom
            .track_start
            .saturating_add(scope.geom.track_len)
            .saturating_sub(1);
        if cell <= scope.geom.track_start + 1 {
            return MouseIntent::ScrollToStart(grab.kind);
        }
        if cell + 1 >= last {
            return MouseIntent::ScrollToEnd(grab.kind);
        }
    }
    if offset >= scope.max_offset {
        return MouseIntent::ScrollToEnd(grab.kind);
    }
    if offset == 0 {
        return MouseIntent::ScrollToStart(grab.kind);
    }
    // The grab's `on_thumb` is carried forward, not re-derived from where the pointer happens to
    // be now -- see `apply_grab`.
    apply_grab(
        grab.kind,
        cell,
        offset,
        grab.on_thumb,
        Some((grab.start_offset, grab.start_cell)),
    )
}

/// Build the press intent, carrying the grab record a following drag needs.
///
/// `Box`ed because `ScrollbarDrag` would otherwise be inlined into every variant of
/// `MouseIntent`, which is moved around during an event batch; the indirection lands on a path
/// that runs once per click.
fn press_intent(kind: VerticalOrHorizontal, cell: u16, offset: usize) -> MouseIntent {
    // A trough press aims, and the grab it leaves behind aims too: `start_offset` is unused on
    // that path because the drag re-reads the pointer's absolute cell.
    apply_grab(kind, cell, offset, false, None)
}

/// Build the intent for a drag that has already resolved its `offset`, carrying the grab forward.
///
/// The grab's `on_thumb` must survive every event of the drag, not just the first. Rebuilding it
/// with `on_thumb: false` -- which is what the shared `press_intent` does -- hands the *next*
/// drag event to the trough's absolute-aiming rule, and a thumb drag then alternates between the
/// two models: measured as 100, 113, 108, 117, ..., where the backwards step is the absolute rule
/// answering a cell the thumb had already slid under. The grab is the memory of how the gesture
/// started, so it is copied forward unchanged apart from the current cell.
fn apply_grab(
    kind: VerticalOrHorizontal,
    cell: u16,
    offset: usize,
    on_thumb: bool,
    anchor: Option<(usize, u16)>,
) -> MouseIntent {
    // The grab is the memory of *where the gesture began*, and both halves of it stay frozen for
    // the whole drag: the offset the bar stood at, and the cell the button went down on. A drag
    // measures travel from that cell, so rewriting it to the pointer's current cell leaves every
    // event one cell from the last and the view stops moving -- measured as an offset pinned at
    // 901 while the pointer travelled ten cells. A press has no earlier memory, so it records
    // itself as the anchor.
    let (start_offset, start_cell) = anchor.unwrap_or((offset, cell));
    let grab = Some(Box::new(crate::app::ScrollbarDrag {
        kind,
        start_offset,
        start_cell,
        on_thumb,
    }));
    match kind {
        VerticalOrHorizontal::Vertical => MouseIntent::ScrollToRow(offset, grab),
        VerticalOrHorizontal::Horizontal => MouseIntent::ScrollToCol(offset, grab),
    }
}

/// One scrollbar's scroll range, as the numbers a click or drag needs.
///
/// Bundling them keeps the quantities that must agree -- the thumb geometry, the total content,
/// the viewport, and the range the scroller will actually accept -- from being re-derived
/// separately by the press path and the drag path, which is how a thumb ends up landing a few
/// rows off the pointer.
pub struct ScrollScope {
    geom: crate::ui::gutter::ScrollbarGeom,
    /// The largest offset the scroller accepts, in the bar's own unit.
    ///
    /// This, not `content - viewport`, is what the trough's reachable span stands for. The two
    /// differ whenever the scroller cannot land on every unit of content, and for the vertical
    /// bar that is the ordinary case: `scroll.top_line` names a *line*, so the positions the view
    /// can take are line starts, and on wrapped text the last line start sits well below
    /// `total_rows - viewport_rows`.
    ///
    /// Scaling against the row count instead is what produced two of the reported bugs at once.
    /// The trough's bottom cells asked `scroll_to_row` for offsets it clamped away, so the view
    /// refused to go where the bar said it should; and because ratatui draws the thumb from the
    /// row-count position while the click was resolved against the same over-tall range, a thumb
    /// near the bottom resolved to an offset past the end, snapped back to the top, and looked
    /// exactly like "the bar jumps under the pointer and the text jumps to the first line and
    /// returns".
    max_offset: usize,
}

impl ScrollScope {
    /// The offset a pointer at track cell `cell` is aiming at.
    ///
    /// Measured from the trough's own first cell, so the offset is a function of the cell alone
    /// and linear in it. Those two properties are what the reported bugs turned on. Linear means a
    /// drag is continuous: one cell of pointer travel is one cell's worth of content, never a
    /// screenful, which is the "bar jumps under the pointer" report. A function of the cell alone
    /// means reach is bounded by the bar rather than by where the drag started, which is the
    /// "very difficult to reach the left or right end" report -- a delta model caps a drag at the
    /// trough's length, so grabbing a thumb two cells down a nineteen-cell travel leaves nine
    /// tenths of the document unreachable however hard you drag.
    fn offset_at_cell(&self, cell: u16) -> usize {
        self.scale(cell.saturating_sub(self.geom.track_start) as i64)
    }

    /// `n` cells from the trough's first cell, as a document offset.
    ///
    /// The divisor is `track_len - 1`, the last cell's index, and it is chosen so that cell 0
    /// means offset 0 and the last cell means `max_offset` -- the ends of the bar are the ends of
    /// the document, exactly. Dividing by `track_len` instead is the off-by-one that made the
    /// bottom of the bar wrap to the top: the last cell fell one increment short of the maximum,
    /// the end-nudge then pushed past it, and the view reset.
    ///
    /// `max_offset` is the range the *scroller* accepts, not `content - viewport`. Under wrap
    /// those differ, because a viewport can only begin at a line's first row; scaling against the
    /// row count produced offsets the scroller clamped away, so the thumb slid down the bar while
    /// the text stayed put -- "the text jumps to the first line and returns".
    fn scale(&self, n: i64) -> usize {
        let span = (self.geom.track_len as i64 - 1).max(1);
        let max = self.max_offset as i64;
        // Round to nearest so a short document, where one cell is worth less than one offset,
        // still answers 1 rather than 0 for its first cells and does not look stuck.
        let offset = (n * max + span / 2) / span;
        offset.clamp(0, max.max(0)) as usize
    }

    /// `start` plus `travel` trough cells, in the bar's own unit.
    ///
    /// The same per-cell ratio as `scale`, applied to a displacement instead of an absolute cell.
    ///
    /// Rounding is anchored on the *absolute* destination rather than on the displacement, which
    /// is the whole subtlety here. Rounding the displacement itself is wrong twice over: Rust's
    /// integer division truncates toward zero, so a negative travel rounds the opposite way to a
    /// positive one, and a drag upward then moved the view *backwards* one row between two cells
    /// -- measured as the sequence 100, 109, 108, 117, ... Rounding the sum keeps the mapping
    /// monotonic in the pointer's position, which is the property a drag has to have.
    ///
    /// A travel of 0 lands exactly back on `start`, because the anchor is an offset that was
    /// itself produced by this ratio -- so a grab that has not moved cannot scroll.
    fn scale_from(&self, start: usize, travel: i64) -> usize {
        let span = (self.geom.track_len as i64 - 1).max(1);
        let max = self.max_offset as i64;
        // Round the *travel* to nearest and add it to an untouched anchor. Two properties follow,
        // and both were broken before this was written down:
        //
        //  - `travel == 0` returns `start` exactly. Rounding the sum instead of the displacement
        //    put a `+ max / 2` bias on the anchor as well, so a grab with no movement reported
        //    1049 for an anchor of 1000 -- the view jumped a hundred rows on the first event,
        //    which is the very defect this stage exists to remove.
        //  - The rounding is symmetric, because `div_euclid` on the travel's own sign rounds away
        //    from the anchor rather than toward zero. Rust's `/` truncates toward zero, which made
        //    a drag upward move one row *backwards* between cells: 100, 109, 108, 117.
        //
        // `travel * max` cannot overflow in practice (a trough is tens of cells, an offset at most
        // the document's row count), and the clamp below bounds the result either way.
        let offsets = (travel * max + span / 2).div_euclid(span);
        let moved = start as i64 + offsets;
        moved.clamp(0, max.max(0)) as usize
    }

    /// The ceiling itself, for the intent applier that has to answer "go to the end".
    pub fn max_offset(&self) -> usize {
        self.max_offset
    }
}

/// The current scroll offset of one bar's axis, in that bar's own unit.
///
/// The grab anchor needs this, and it has to be read from the same place the thumb is drawn
/// from: `top_row_offset` is what positions the thumb, so a grab anchored to anything else
/// would start a drag already out of step with what the user is looking at.
fn scroll_offset(app: &App, kind: VerticalOrHorizontal) -> usize {
    match kind {
        VerticalOrHorizontal::Vertical => {
            if app.doc().is_none() {
                return 0;
            }
            let max = app.max_viewport_row_offset();
            app.top_row_offset(max)
        }
        VerticalOrHorizontal::Horizontal => app.doc().map(|doc| doc.scroll.left_col).unwrap_or(0),
    }
}

/// Ratatui's own ceiling for a bar drawn over `content` elements with a `viewport`-sized window.
///
/// `ScrollbarGeom::new` clamps the thumb's start to `content - 1 - viewport`, and that clamp is
/// the real limit on where the view can be put: an offset above it is asked for, refused, and
/// snapped back, which reads as a dead region at the bottom or right of the bar. Both bars take
/// their ceiling from here so the drawing and the hit-test cannot disagree about it.
fn scope_bound(content: usize, viewport: usize) -> usize {
    content.saturating_sub(1).saturating_sub(viewport)
}

/// The scroll range for one bar, or `None` when that bar does not exist.
///
/// `pub` because the intent applier needs it for the end-of-drag nudge: "one page further" has
/// to be answered in the bar's unit, and only the scope knows what that unit's pages are.
pub fn scroll_scope(app: &App, kind: VerticalOrHorizontal) -> Option<ScrollScope> {
    let (geom, content, viewport) = match kind {
        VerticalOrHorizontal::Vertical => app.vertical_scrollbar_detail()?,
        VerticalOrHorizontal::Horizontal => app.horizontal_scrollbar_detail()?,
    };
    // The range the scroller will actually accept, in the bar's own unit. This is the number the
    // trough is scaled against, so it has to be the ceiling the *scroller* honours rather than the
    // one the content suggests -- see `ScrollScope::max_offset`.
    let max_offset = match kind {
        // Rows: the last position the painter will honour, which under wrap is a line start and
        // not simply `total_rows - viewport_rows`.
        VerticalOrHorizontal::Vertical => app.max_viewport_row_offset(),
        // Columns: any column can be a left edge, so the content's own bound is the answer.
        VerticalOrHorizontal::Horizontal => scope_bound(content, viewport),
    };
    Some(ScrollScope { geom, max_offset })
}

/// Row 11 while dragging: select whole lines from the press line to the current one.
///
/// The range is `anchor..=line` inclusive and runs to the *end* of the lower line, so dragging
/// 3 -> 7 selects five lines. A drag in the gutter deliberately does not touch the caret: the
/// whole point of the gesture is to grab lines without leaving your editing position, and a
/// caret that jumps to row 7 makes the next keystroke land in the wrong place.
fn gutter_drag_intent(app: &App, line: usize) -> MouseIntent {
    let Some(doc) = app.doc() else {
        return MouseIntent::Nothing;
    };
    let anchor_line = app.gutter_drag_anchor.unwrap_or(line);
    let first = anchor_line.min(line);
    let last = anchor_line.max(line);
    let focus = if last + 1 < doc.buffer.line_count() {
        LineCol::new(last + 1, 0)
    } else {
        LineCol::new(last, doc.buffer.line_len_chars(last))
    };
    MouseIntent::SelectLineRange {
        anchor: LineCol::new(first, 0),
        focus,
    }
}

/// The click state to store after handling `ev`.
///
/// Only a left press updates it, and only on a target a multi-click can mean something: the text
/// area, a Find in Files result row, or a dialog listing row. Everything else resets the count, so a
/// double click on a tab cannot look like a double click in the buffer or in a list, and vice versa.
///
/// Counted by screen cell throughout, including in the lists. Keying a list by row *index* looks
/// more correct and is not: selecting a row re-centres the renderer's window around the cursor, so
/// the entry under a fixed cell changes between the two clicks of a deliberate double-click -- it
/// made a double-click on `d29` at the top of a scrolled list resolve to whatever slid into that
/// cell, and the picker went to `d19`. What the user repeats is the pointer position. Which entry
/// opens is decided separately and correctly: `dialog_press.row` and `MouseIntent::OpenFindHit` both
/// carry the row resolved from the press.
fn next_click_state(
    ev: &MouseEvent,
    app: &App,
    hit: &Hit,
    click: &ClickState,
    now: std::time::Instant,
) -> ClickState {
    if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
        return *click;
    }
    let countable = matches!(
        hit,
        Hit::Text { .. } | Hit::FindPanel | Hit::DialogWidget(_) | Hit::PaletteRow(_)
    );
    if !countable {
        return ClickState {
            cell: None,
            count: 0,
            at: now,
        };
    }
    let (_, state) = click.register((ev.column, ev.row), now);
    let _ = app;
    state
}

/// Apply a selection from an intent, keeping the anchor semantics in one place.
pub fn apply_selection(doc: &mut np_core::Document, sel: Selection) {
    doc.selection = Some(sel);
    doc.cursor.pos = sel.focus;
    doc.cursor.goal_col = None;
}

/// The scroll position a `ScrollTo` fraction implies.
///
/// The fraction is a position in the **row** scroll range, converted to a top *line* through
/// `np_core::line_at_row_offset`, which is the same row-height arithmetic the layout and the
/// thumb use. Converting against a line count instead is the naive version and it is wrong for
/// any wrapped document: with wrap on, a 200-line file may be 900 rows, and 100% of 200 lands
/// the thumb two thirds of the way up a file the user believes they are looking at the bottom of.
pub fn scroll_for_fraction(app: &App, fraction: f32) -> ScrollPos {
    let Some(doc) = app.doc() else {
        return ScrollPos::default();
    };
    let geom = app.geometry;
    let visible = page_height(app).max(1);
    let total = np_core::total_row_count(&doc.buffer, &geom, &doc.folds);
    let max_row = total.saturating_sub(visible);
    let f = fraction.clamp(0.0, 1.0);
    let row = (f * max_row as f32).round() as usize;
    let top_line = np_core::line_at_row_offset(&doc.buffer, &geom, &doc.folds, row)
        .unwrap_or(0)
        .min(doc.buffer.line_count().saturating_sub(1));
    ScrollPos {
        top_line,
        left_col: doc.scroll.left_col,
    }
}

/// Re-export so callers can name the row type without importing np-core twice.
pub type Row = VisualRow;

/// Exposed for tests that need to build the same buffer the router sees.
pub fn buffer_of(app: &App) -> Option<&Buffer> {
    app.doc().map(|d| &d.buffer)
}

// ------------------------------------------------------------- S14 helpers

/// The open dropdown's rectangle, or `None` when the bar has no items to show.
///
/// Recomputed from the same registry call the renderer makes, which is the only way a click
/// can be trusted to land on the row the user read.
/// The *top-level* dropdown's rectangle for `menu`, ignoring any submenu open above it.
///
/// Named for what it computes. The old `open_dropdown_rect` was the only rect available and
/// quietly returned the parent's box even when a submenu was open, which is how a click in a
/// submenu selected a row of the parent list.
pub fn top_dropdown_rect(app: &App, menu: crate::actions::MenuId) -> Option<RectCore> {
    let items = crate::ui::menus::menu_items(app, menu);
    if items.is_empty() {
        return None;
    }
    let screen_w = app.regions.menu.w;
    let screen_h = app.regions.status.bottom();
    let box_w = crate::ui::menus::dropdown_width(&items)
        .min(screen_w.max(1))
        .max(3);
    let box_h = crate::ui::menus::dropdown_height(items.len(), screen_h);
    let (title_x, title_w) = crate::ui::menu_bar::title_span(app.regions.menu, menu);
    let bar = RectCore::new(0, app.regions.menu.y, screen_w, 1);
    Some(crate::ui::menus::dropdown_rect(
        bar, title_x, title_w, box_w, box_h, screen_w,
    ))
}

/// The rectangle of the dropdown level the user is actually looking at.
///
/// The submenu box when a submenu is open, otherwise the top-level box. Both the hit-test and
/// the "is this click inside a menu at all" question have to ask this; asking
/// [`top_dropdown_rect`] while a submenu is open is what made a click on a submenu row select
/// an unrelated row of the parent list.
pub fn active_dropdown_rect(app: &App) -> Option<RectCore> {
    let Mode::Menu {
        open, ref parent, ..
    } = app.mode()
    else {
        return None;
    };
    let screen_w = app.regions.menu.w;
    let screen_h = app.regions.status.bottom();
    if let Some(submenu) = parent {
        let items = crate::ui::menus::level_items(app, open, Some(*submenu));
        if items.is_empty() {
            return None;
        }
        let parent_area = top_dropdown_rect(app, submenu.parent())?;
        let offset = crate::ui::menus::menu_items(app, submenu.parent())
            .iter()
            .position(|it| {
                it.kind == crate::ui::menus::MenuItemKind::Submenu(submenu.parent())
                    && it.label == submenu.label()
            })
            .unwrap_or(0) as u16;
        let anchored = RectCore {
            y: parent_area.y.saturating_add(offset),
            ..parent_area
        };
        return Some(crate::ui::menus::submenu_rect(
            anchored, &items, screen_w, screen_h,
        ));
    }
    top_dropdown_rect(app, open)
}

/// Which dropdown row `y` is over, skipping the border and counting separators so the index
/// matches the item list the renderer walked.
fn dropdown_item_at(app: &App, area: RectCore, y: u16) -> Option<usize> {
    let Mode::Menu {
        open,
        ref parent,
        item,
    } = app.mode()
    else {
        return None;
    };
    let items = crate::ui::menus::level_items(app, open, *parent);
    if !area.contains(area.x, y) {
        return None;
    }
    let inner_top = area.y + 1;
    if y < inner_top {
        return None;
    }
    let row = (y - inner_top) as usize;
    let visible = area.h.saturating_sub(2) as usize;
    // The same scroll window the painter used, which is scrolled to the highlighted item.
    // Hardcoding 0 here made every row of a scrolled list off by however far it had scrolled.
    let start = crate::ui::framework::list_window(items.len(), item, visible);
    let index = start + row;
    if index < items.len() && items[index].enabled {
        Some(index)
    } else {
        None
    }
}

/// What clicking a status bar segment does (FR-U3).
fn status_click_intent(app: &App, segment: StatusSegment) -> MouseIntent {
    match segment {
        // Ln/Col opens Go To, which is where a user who just read their line number wants to
        // be able to type one.
        StatusSegment::Position => MouseIntent::Action(Action::GoToLine),
        StatusSegment::Eol => MouseIntent::Action(Action::EolMenu),
        StatusSegment::Encoding => MouseIntent::Action(Action::EncodingMenu),
        StatusSegment::Language => MouseIntent::Action(Action::LanguagePicker),
        StatusSegment::Overtype => MouseIntent::Action(Action::ToggleOvertype),
        // The document name and the counts have no click action; a click there does nothing
        // rather than guessing at an intent.
        StatusSegment::Document | StatusSegment::Counts | StatusSegment::Other => {
            let _ = app;
            MouseIntent::Nothing
        }
    }
}

/// Which banner button a click hit: Reload on the left, Ignore on the right (FR-F7).
///
/// The split is computed from the same label widths the renderer used, so the hit zones and
/// the drawn `[Reload] [Ignore]` cannot drift apart.
fn banner_press_intent(app: &App, x: u16) -> MouseIntent {
    let area = banner_region(app);
    let reload_w = "[Reload]".chars().count() as u16;
    let ignore_w = "[Ignore]".chars().count() as u16;
    let total = reload_w + ignore_w + 2;
    let start = area.right().saturating_sub(total + 1);
    let reload_end = start.saturating_add(reload_w);
    if x >= start && x < reload_end {
        MouseIntent::BannerAction(true)
    } else {
        MouseIntent::BannerAction(false)
    }
}
