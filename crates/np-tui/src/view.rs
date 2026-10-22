//! View features: wrap, folding, bookmarks, and the toggles that switch them (spec §6.7).
//!
//! # Why the behavior lives here and the state lives in `np-core`
//!
//! `Document` owns fold ranges and bookmark sets and knows how to shift them as lines come and
//! go. What it cannot know is which language the *UI* resolved a file to (an override, an
//! extension, a shebang), which config key a toggle writes to, or that a toggle should survive
//! a restart. Those are decisions about the editor, not about a buffer, and putting them in
//! `np-core` would drag config and session types into a crate that has no business knowing them.
//!
//! So this module is the seam: it turns an `Action` into a `Document` mutation plus the
//! follow-up the app owes -- re-derive folds, recompute geometry, mark the session dirty,
//! persist config -- in one place. Every entry point here ends by leaving `App` in a state the
//! next frame can draw without recomputing anything twice.
//!
//! # When folds are recomputed
//!
//! `refresh_folds` walks the whole text, so it is called at the four moments the answer can
//! change and nowhere else: a file opens, its language changes, the caret moves onto a line
//! (which is where a fold becomes worth having), and a structural edit lands. Not on every
//! keystroke -- that would be O(document) in the path that types a character, which is exactly
//! the budget §9 spends the whole stage on.

use np_core::LineCol;

use crate::app::App;

/// Re-detect folds for the active document, keeping the user's open/closed choices.
///
/// Cheap enough to call on a caret move because `merge_folds` preserves `collapsed` on match,
/// so a recompute never pops a fold shut under the user; it is the *detection* that costs, and
/// that only matters when the structure may have changed.
pub fn refresh_folds(app: &mut App) {
    refresh(app, false);
}

/// [`refresh_folds`], with the session rule: collapsed ranges the file no longer detects stay
/// closed.
///
/// A session's `folded` list records what the user closed, and detection runs on the reopened
/// file with whatever heuristics this build has. When the two disagree, the recorded answer wins
/// -- otherwise a restart quietly opens folds the user spent effort setting, and the view they
/// left is not the view they come back to.
pub fn refresh_folds_preserving(app: &mut App) {
    refresh(app, true);
}

fn refresh(app: &mut App, preserve_collapsed: bool) {
    let Some(language) = resolved_language(app) else {
        return;
    };
    if let Some(doc) = app.doc_mut() {
        doc.refresh_folds_with(&language, preserve_collapsed);
    }
}

/// The language folds should be computed for, or `None` when there is no document.
///
/// Goes through the same resolver as highlighting, so a fold set and its colors can never be
/// derived from two different language answers -- the failure mode where a Python file gets
/// brace folds because one path honored the override and the other did not.
fn resolved_language(app: &App) -> Option<String> {
    let doc = app.doc()?;
    Some(crate::highlight::resolve_language(
        &app.highlight,
        &doc.language,
        doc.path.as_deref(),
        &doc.buffer.line_string(0),
    ))
}

/// Toggle the fold whose header is `line` (§6.9 row 12, FR-V5).
///
/// A no-op when no fold starts there, which is the common case: the gutter's fold column is
/// fixed for the whole strip, so most clicks in it land between markers. Telling that apart
/// from a real toggle is why `Document::toggle_fold_at` returns a bool.
pub fn toggle_fold_at(app: &mut App, line: usize) {
    let toggled = app.doc_mut().map(|d| d.toggle_fold_at(line));
    match toggled {
        Some(true) => {
            app.session_dirty = true;
            app.dirty = true;
        }
        Some(false) => {}
        None => {}
    }
}

/// Toggle a bookmark on an explicit line (gutter click, §6.9 row 13).
///
/// Not `Document::toggle_bookmark`, which uses the caret's line: a gutter click is about the
/// line under the pointer and must not move the caret to reach it.
pub fn toggle_bookmark_at(app: &mut App, line: usize) {
    let Some(doc) = app.doc_mut() else {
        return;
    };
    let last = doc.buffer.line_count().saturating_sub(1);
    if line > last {
        return;
    }
    if !doc.bookmarks.remove(&line) {
        doc.bookmarks.insert(line);
    }
    app.session_dirty = true;
    app.dirty = true;
}

/// Collapse every fold (FR-V5).
pub fn fold_all(app: &mut App) {
    refresh_folds(app);
    if let Some(doc) = app.doc_mut() {
        doc.fold_all();
    }
    app.session_dirty = true;
    app.dirty = true;
}

/// Expand every fold (FR-V5).
pub fn unfold_all(app: &mut App) {
    if let Some(doc) = app.doc_mut() {
        doc.unfold_all();
    }
    app.session_dirty = true;
    app.dirty = true;
}

/// Toggle the fold on the caret's line (FR-V5, `ToggleFold`).
///
/// The caret line itself is checked first, then the folds containing it from the innermost
/// outward. That order is what makes the key usable inside a collapsed block: the marker for a
/// hidden line is not on screen, so without the containing-fold rule the user would have to
/// unfold by scrolling to the header.
pub fn toggle_fold_under_caret(app: &mut App) {
    let Some(line) = app.doc().map(|d| d.cursor.pos.line) else {
        return;
    };
    if toggle_fold_at_headed(app, line) {
        return;
    }
    // Nothing starts here: walk the folds containing the caret, innermost first.
    let Some(folds) = app.doc().map(|d| d.folds.clone()) else {
        return;
    };
    let mut containing: Vec<usize> = folds
        .iter()
        .filter(|f| f.contains_line(line) || f.start == line)
        .map(|f| f.start)
        .collect();
    containing.sort_by_key(|start| std::cmp::Reverse(*start));
    for header in containing {
        if toggle_fold_at_headed(app, header) {
            return;
        }
    }
}

/// Toggle the fold starting at `header`; true when one was there.
fn toggle_fold_at_headed(app: &mut App, header: usize) -> bool {
    let toggled = app.doc_mut().map(|d| d.toggle_fold_at(header));
    let done = toggled == Some(true);
    if done {
        app.session_dirty = true;
        app.dirty = true;
    }
    done
}

/// `F2`: next bookmark, wrapping, unfolding a collapsed range to reach it (FR-V6).
pub fn next_bookmark(app: &mut App) {
    let found = app.doc_mut().and_then(|d| d.next_bookmark());
    match found {
        Some(line) => reveal(app, line),
        None => app.push_toast("No bookmarks", false),
    }
}

/// `Shift+F2`: previous bookmark, wrapping.
pub fn prev_bookmark(app: &mut App) {
    let found = app.doc_mut().and_then(|d| d.prev_bookmark());
    match found {
        Some(line) => reveal(app, line),
        None => app.push_toast("No bookmarks", false),
    }
}

/// `Ctrl+F2` on the caret's line (FR-V6).
pub fn toggle_bookmark_under_caret(app: &mut App) {
    let Some(line) = app.doc().map(|d| d.cursor.pos.line) else {
        return;
    };
    toggle_bookmark_at(app, line);
}

/// Clear every bookmark (FR-V6).
pub fn clear_bookmarks(app: &mut App) {
    if let Some(doc) = app.doc_mut() {
        doc.clear_bookmarks();
    }
    app.session_dirty = true;
    app.dirty = true;
}

/// Scroll so `line` is visible, unfolding it if a fold was hiding it.
///
/// The caret is already on the line by the time this runs -- `Document::next_bookmark` moved
/// it -- so this is only about the viewport. It has to be row-aware: with wrap on, a line's
/// screen row is not its line index, and centering on the line index puts the caret off screen
/// on any file with a long line in front of it.
fn reveal(app: &mut App, line: usize) {
    let target = app
        .doc()
        .map(|d| d.buffer.clamp(LineCol::new(line, 0)))
        .unwrap_or_else(|| LineCol::new(line, 0));
    let geom = app.geometry;
    if let Some(doc) = app.doc_mut() {
        doc.scroll_to_show(&geom, target);
    }
    app.session_dirty = true;
    app.dirty = true;
}

/// `Alt+Z` and the other view toggles (FR-V1, FR-V2, FR-E14, FR-V3).
///
/// Each writes through to `config.ui` and re-derives the geometry, because the gutter width and
/// the wrap state are both inputs to it. Persistence is deliberate: §7.1 puts these in `[ui]`,
/// so they are meant to be the editor's shape tomorrow as much as today. A failed write is
/// reported rather than swallowed -- the user asked for a setting, and "it worked until you
/// restarted" is worse than a toast saying the config file could not be written.
pub fn toggle(app: &mut App, which: Toggle) {
    match which {
        Toggle::WordWrap => {
            app.config.ui.word_wrap = !app.config.ui.word_wrap;
            // FR-V1: wrap and horizontal scroll are mutually exclusive. Turning wrap on with a
            // nonzero left_col would leave the viewport scrolled sideways in a layout where every
            // row already fits, which paints as text clipped on the left for no reason.
            if app.config.ui.word_wrap {
                if let Some(doc) = app.doc_mut() {
                    doc.scroll.left_col = 0;
                }
            }
        }
        Toggle::LineNumbers => app.config.ui.line_numbers = !app.config.ui.line_numbers,
        Toggle::Whitespace => app.config.ui.show_whitespace = !app.config.ui.show_whitespace,
        Toggle::EolMarks => app.config.ui.show_eol = !app.config.ui.show_eol,
    }
    let (w, h) = app.frame_size();
    app.recompute_geometry(w, h);
    persist_ui(app);
}

/// One of the four config-backed view toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    WordWrap,
    LineNumbers,
    Whitespace,
    EolMarks,
}

/// Write `[ui]` back to the user's config file.
fn persist_ui(app: &mut App) {
    // Routed through `App::persist_config` rather than resolving a path here. This used to call
    // `Config::default_path()` directly, which writes to `~/.config/npt/config.toml` even when
    // `--config` named somewhere else -- so with an explicit config file the toggle would apply
    // for the session, write to a file nobody reads, and the next launch would come back to the
    // old value while the file the user was editing stayed untouched.
    app.persist_config();
}

// ------------------------------------------------------ keep the caret visible

/// Scroll the viewport so the caret is on screen. Called after every caret move.
///
/// # Why this exists
///
/// Nothing did this on an ordinary caret move. `Document::scroll_to_show` was reached only
/// from jumps -- a bookmark, a search hit, a Go To -- so pressing Down at the bottom of the
/// window moved the caret onto a line the viewport was not showing, and the cursor walked out
/// of the editor region into the status bar and then off the screen. Moving Up from the top
/// did the same in the other direction. The headless suite never noticed because it asserts on
/// the caret's *document* position, which was always right; only the viewport disagreed.
///
/// # Why not `Document::scroll_to_show`
///
/// That function is correct and fold-aware, but it builds the whole document's row list to
/// answer one question. S15 removed exactly that pattern from the frame path for costing
/// hundreds of milliseconds on a large file, and this runs on every keystroke. The walk below
/// counts rows from the current top line down, which is one line of work in the common case,
/// and only walks up from the caret when the caret is above the viewport -- already the case
/// where a scroll is owed.
///
/// # The horizontal half is not optional
///
/// With wrap off, a caret moved past the right edge has no column for the terminal to put a
/// cursor in, which is the same class of failure as the vertical one. Under wrap every row
/// fits by construction, so there is nothing to do.
pub fn ensure_caret_visible(app: &mut App) {
    let geom = app.geometry;
    let Some(doc) = app.doc() else { return };
    let caret = doc.cursor.pos;
    // The viewport height with the horizontal scrollbar's row already removed. Using the
    // band's height instead promises the caret a screen row that the bar is drawn over --
    // the row layout stops one row short of it, so the caret's line never gets a row and
    // `caret_position` refuses to place it.
    let height = geom.viewport_rows().max(1);
    let buf = &doc.buffer;
    let folds = doc.folds.clone();
    let line_count = buf.line_count();

    // Rows occupied between the viewport top and the caret's line.
    let top = doc.scroll.top_line.min(line_count.saturating_sub(1));
    let mut new_top = top;
    if caret.line >= top {
        let mut rows = 0usize;
        let mut line = top;
        while line < caret.line && line < line_count {
            rows += np_core::view::rows_for_line(buf, &geom, line);
            if rows >= height {
                // Everything between the top and the caret already overflows the window; the
                // caret has to sit at the bottom and no further counting helps.
                break;
            }
            line = np_core::view::next_visible_line(line, &folds);
        }
        let caret_rows = np_core::view::rows_for_line(buf, &geom, caret.line);
        if rows + caret_rows > height {
            // Anchor the caret's line at the bottom of the window rather than scrolling by a
            // line, so repeated Downs do not reveal one new line at a time.
            new_top = caret.line;
            let mut budget = height.saturating_sub(caret_rows);
            let mut guard = 0usize;
            while guard < line_count + 1 {
                guard += 1;
                let prev = np_core::view::prev_visible_line(new_top.saturating_sub(1), &folds);
                if prev >= new_top || prev > new_top {
                    break;
                }
                let h = np_core::view::rows_for_line(buf, &geom, prev);
                if h > budget {
                    break;
                }
                budget -= h;
                new_top = prev;
                if budget == 0 {
                    break;
                }
            }
        }
    } else {
        // Above the viewport: bring it to the top.
        new_top = np_core::view::prev_visible_line(caret.line, &folds).min(caret.line);
    }

    let mut new_left = doc.scroll.left_col;
    if !geom.word_wrap {
        // Columns, likewise with the vertical bar's column taken out: a caret scrolled into
        // that column is a caret drawn on top of the scrollbar.
        let width = geom.viewport_cols().max(1);
        let caret_cell = buf.display_col(caret.line, caret.col, geom.tab_width);
        if caret_cell < new_left {
            new_left = caret_cell;
        } else if caret_cell >= new_left + width {
            new_left = caret_cell + 1 - width;
        }
    }

    let moved = new_top != doc.scroll.top_line || new_left != doc.scroll.left_col;
    if moved {
        if let Some(doc) = app.doc_mut() {
            doc.scroll.top_line = new_top;
            doc.scroll.left_col = new_left;
        }
        app.dirty = true;
    }
}
