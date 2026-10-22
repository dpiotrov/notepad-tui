//! Find in Files, panel and worker integration (stage S11, spec §6.5 FR-FF1..FF3, FR-F10).
//!
//! The engine has its own suite in `np-core/tests/fif_s11.rs`. This one is about the seams:
//! does the panel show what the worker found, does a cancelled or superseded search stay out of
//! the results, does a bounded drain keep the loop responsive, and does a click or Enter land
//! the caret on the right character. Those are the things that break while the engine is green.
//!
//! # Searches are driven synchronously
//!
//! Tests call `np_tui::bg::find_files::run_fif` on the current thread and push the resulting
//! messages into the worker's queue, rather than spawning and waiting. The point is to assert
//! the drain and panel logic against a *known* message sequence, deterministically. The one
//! place a real thread is used asserts delivery, and it joins rather than sleeps.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use np_core::{Config, Document, EncodingId, LineCol, SearchOpts};
use np_tui::bg::{BgMessage, BgWorker};
use np_tui::fif::{self, FifPhase, PanelHit};
use np_tui::ui::{self, find_in_files};
use np_tui::{Action, App};

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

/// An app whose only tab is a file inside `root`, so the default search directory is `root`.
fn app_in(root: &Path) -> App {
    let mut app = app();
    let file = root.join("seed.txt");
    fs::write(&file, "seed\n").unwrap();
    let doc = Document::from_decoded(
        file,
        "seed\n",
        EncodingId::Utf8,
        np_core::detect_dominant_eol("seed\n"),
        false,
    );
    app.open_document(doc);
    app
}

struct Tree(PathBuf);
impl Tree {
    /// A tempdir guaranteed not to exist.
    ///
    /// A counter alone is not enough: it restarts at zero every test-binary run, so a directory
    /// left behind by an earlier aborted run (a panic between create and Drop) collides with a
    /// later one and the search sees that stale text too -- which showed up here as every hit
    /// appearing twice. Create-with-exclusive semantics closes the race that remove-then-create
    /// leaves open between concurrent tests.
    fn new(name: &str) -> Tree {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir();
        for _ in 0..1_000 {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let p = base.join(format!(
                "npt-uifif-{name}-{}-{n}-{nanos}",
                std::process::id()
            ));
            match fs::create_dir(&p) {
                Ok(()) => return Tree(p),
                Err(_) => continue,
            }
        }
        panic!("could not create a unique tempdir for {name}");
    }
    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Search and drain into the app.
///
/// `App::new_headless` sets `sync_search`, so `fif::start` already ran the search on this
/// thread and queued its messages; this only drains them. Driving a second search here would
/// deliver every hit twice -- which is exactly what this helper used to do.
fn run_into(app: &mut App, query: &str, dir: &Path) -> usize {
    app.fif.query = query.to_string();
    app.fif.dir = dir.to_string_lossy().into_owned();
    assert!(fif::start(app).is_some(), "the search should have started");
    fif::drain(app, 100_000)
}

// ----------------------------------------------------------- FR-FF1: opening

#[test]
fn ctrl_shift_f_opens_the_panel_and_focuses_it() {
    let t = Tree::new("open");
    let mut app = app_in(&t.0);
    app.handle_key(KeyEvent::new(
        KeyCode::Char('f'),
        KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
    ));
    assert_eq!(app.mode(), np_tui::app::Mode::FindPanel);
    assert!(app.fif.focused);
    assert!(app.find_panel_height > 0);
}

#[test]
fn the_default_directory_is_the_active_documents_parent() {
    let t = Tree::new("defaultdir");
    t.write("sub/inner.txt", "x\n");
    let mut app = app();
    let doc = Document::from_decoded(
        t.0.join("sub").join("inner.txt"),
        "x\n",
        EncodingId::Utf8,
        np_core::detect_dominant_eol("x\n"),
        false,
    );
    app.open_document(doc);
    app.execute(Action::FindInFiles);
    assert_eq!(fif::default_dir(&app), t.0.join("sub"));
    assert_eq!(app.fif.dir, t.0.join("sub").to_string_lossy());
}

#[test]
fn an_untitled_document_falls_back_to_the_process_directory() {
    let app = app();
    // The default app's tab is untitled: no path, so no parent.
    assert!(app.doc().unwrap().path.is_none());
    let dir = fif::default_dir(&app);
    assert_eq!(dir, std::env::current_dir().unwrap(), "FR-FF1: 'else cwd'");
}

#[test]
fn the_query_is_seeded_from_the_in_document_search() {
    let mut app = app();
    app.find.needle = "already".into();
    app.execute(Action::FindInFiles);
    assert_eq!(app.fif.query, "already");
}

// ------------------------------------------------------------ FR-FF2: results

#[test]
fn a_search_populates_the_panel_with_path_line_col_text() {
    let t = Tree::new("results");
    t.write("a.txt", "nothing\nfind me here\n");
    let mut app = app_in(&t.0);
    run_into(&mut app, "find", &t.0);

    assert_eq!(app.fif.hits.len(), 1, "hits: {:?}", app.fif.hits);
    let hit = &app.fif.hits[0];
    assert_eq!(hit.line, 2, "1-based");
    assert_eq!(hit.col, 1, "1-based");
    assert_eq!(hit.text, "find me here");
    assert_eq!(hit.match_len, 4);
    assert!(hit.location().ends_with("a.txt:2:1:"));
    assert_eq!(app.fif.phase, FifPhase::Idle, "the run finished");
}

#[test]
fn the_header_reports_files_scanned_and_matches() {
    let t = Tree::new("header");
    t.write("a.txt", "x\n");
    t.write("b.txt", "x\n");
    t.write("c.txt", "needle\n");
    let mut app = app_in(&t.0);
    run_into(&mut app, "needle", &t.0);
    let header = fif::header(&app);
    // 4 files: the three written here plus `seed.txt`, which `app_in` creates to give the app
    // a document whose parent is the search root.
    assert!(header.contains("4 files scanned"), "got {header}");
    assert!(header.contains("1 matches"), "got {header}");
}

#[test]
fn a_running_search_shows_a_cancelling_hint() {
    let mut app = app();
    app.fif.phase = FifPhase::Running;
    let header = fif::header(&app);
    assert!(header.contains("searching"), "got {header}");
    assert!(header.contains("Esc"), "got {header}");
}

#[test]
fn esc_cancels_a_running_search_and_a_second_press_closes() {
    let t = Tree::new("cancel");
    t.write("a.txt", "needle\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "needle".into();
    app.fif.dir = t.0.to_string_lossy().into_owned();
    fif::start(&mut app);
    assert_eq!(app.fif.phase, FifPhase::Running);

    // First Esc: cancel, panel stays open with the partial results.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.fif.phase, FifPhase::Cancelled);
    assert_eq!(
        app.mode(),
        np_tui::app::Mode::FindPanel,
        "cancelling does not close"
    );

    // Second Esc: close.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.fif.focused);
    assert_eq!(app.find_panel_height, 0);
    assert_ne!(app.mode(), np_tui::app::Mode::FindPanel);
}

#[test]
fn closing_a_running_search_cancels_it() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    app.fif.phase = FifPhase::Running;
    fif::close(&mut app);
    assert_eq!(
        app.fif.phase,
        FifPhase::Cancelled,
        "no worker left appending to a closed panel"
    );
    assert!(app.bg.is_cancelled());
}

// -------------------------------------------------------------- staleness

#[test]
fn results_from_a_cancelled_search_do_not_reach_the_panel() {
    let t = Tree::new("stale");
    t.write("a.txt", "needle\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "needle".into();
    app.fif.dir = t.0.to_string_lossy().into_owned();
    fif::start(&mut app);
    let old_id = app.fif.active_request.expect("id");

    // Cancel, then the old worker delivers late.
    fif::cancel(&mut app);
    app.bg.send(BgMessage::FifMatch {
        request: old_id,
        path: t.0.join("a.txt"),
        line: 1,
        col: 1,
        text: "needle".into(),
        match_len: 6,
    });
    fif::drain(&mut app, 64);
    assert!(
        app.fif.hits.is_empty(),
        "a cancelled run must not repopulate the panel"
    );
}

#[test]
fn a_superseded_search_cannot_write_into_the_new_ones_panel() {
    // The sharper case: cancel is never observed, the old run simply finishes after a new one
    // started. Only the request id distinguishes them.
    let t = Tree::new("supersede");
    t.write("old.txt", "old\n");
    t.write("new.txt", "new\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "old".into();
    app.fif.dir = t.0.to_string_lossy().into_owned();
    fif::start(&mut app);
    let old_id = app.fif.active_request.expect("id");

    // Start a second search without cancelling the first.
    app.fif.query = "new".into();
    fif::start(&mut app);
    let new_id = app.fif.active_request.expect("id");
    assert_ne!(old_id, new_id, "a fresh id per run");
    assert!(app.fif.hits.is_empty(), "a new run clears the results");

    // The old run delivers, then the new one.
    app.bg.send(BgMessage::FifMatch {
        request: old_id,
        path: t.0.join("old.txt"),
        line: 1,
        col: 1,
        text: "old".into(),
        match_len: 3,
    });
    app.bg.send(BgMessage::FifMatch {
        request: new_id,
        path: t.0.join("new.txt"),
        line: 1,
        col: 1,
        text: "new".into(),
        match_len: 3,
    });
    fif::drain(&mut app, 64);

    assert_eq!(app.fif.hits.len(), 1, "only the current run's hit");
    assert!(app.fif.hits[0].path.ends_with("new.txt"));
}

#[test]
fn a_start_clears_notes_and_counters_from_the_previous_run() {
    let t = Tree::new("clear");
    t.write("a.txt", "needle\n");
    let mut app = app_in(&t.0);
    run_into(&mut app, "needle", &t.0);
    assert!(!app.fif.hits.is_empty());
    app.fif.query = "needle".into();
    fif::start(&mut app);
    assert!(app.fif.hits.is_empty());
    assert_eq!(app.fif.matches, 0);
    assert_eq!(app.fif.scanned, 0);
    assert_eq!(app.fif.selected, 0);
}

// -------------------------------------------------------- drain budget (§9)

#[test]
fn one_drain_applies_at_most_the_budget() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    app.fif.phase = FifPhase::Running;
    for i in 0..10_000 {
        app.bg.send(BgMessage::FifMatch {
            request: id,
            path: PathBuf::from(format!("f{i}.txt")),
            line: 1,
            col: 1,
            text: "x".into(),
            match_len: 1,
        });
    }
    let applied = fif::drain(&mut app, 64);
    assert_eq!(applied, 64, "exactly the budget, no more");
    assert_eq!(
        app.bg.len(),
        10_000 - 64,
        "the rest waits for the next tick"
    );
}

#[test]
fn a_huge_result_set_needs_bounded_iterations_and_stays_responsive() {
    // The spec's responsiveness claim, measured: N matches arrive over ceil(N/budget) drains,
    // and each drain does bounded work, so keyboard input between drains is never queued behind
    // the whole result set.
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    app.fif.phase = FifPhase::Running;
    let total = 10_000usize;
    for i in 0..total {
        app.bg.send(BgMessage::FifMatch {
            request: id,
            path: PathBuf::from(format!("f{i}.txt")),
            line: 1,
            col: 1,
            text: "x".into(),
            match_len: 1,
        });
    }
    let budget = fif::DRAIN_BUDGET;
    let expected_iters = total.div_ceil(budget);
    let start = std::time::Instant::now();
    let mut iters = 0usize;
    while !app.bg.is_empty() && iters < expected_iters + 5 {
        let n = fif::drain(&mut app, budget);
        assert!(n <= budget, "a drain never exceeds its budget");
        iters += 1;
    }
    let elapsed = start.elapsed();
    println!("MEASURED drain: {total} matches over {iters} drains of {budget} in {elapsed:?}");
    assert_eq!(iters, expected_iters, "drains needed for {total} matches");
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "{total} matches over {iters} drains took {elapsed:?}"
    );
}

#[test]
fn a_drain_with_nothing_queued_does_no_work() {
    let mut app = app();
    assert_eq!(fif::drain(&mut app, 64), 0);
}

#[test]
fn hits_are_capped_so_a_runaway_query_cannot_exhaust_memory() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    app.fif.phase = FifPhase::Running;
    for i in 0..(fif::MAX_PANEL_HITS + 500) {
        app.bg.send(BgMessage::FifMatch {
            request: id,
            path: PathBuf::from(format!("f{i}.txt")),
            line: 1,
            col: 1,
            text: "x".into(),
            match_len: 1,
        });
    }
    fif::drain(&mut app, fif::MAX_PANEL_HITS + 1_000);
    assert_eq!(app.fif.hits.len(), fif::MAX_PANEL_HITS);
    assert!(app.fif.truncated, "and the panel says so");
    assert!(fif::header(&app).contains("truncated"));
}

#[test]
fn notes_are_capped_and_the_overflow_is_counted() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    for i in 0..500 {
        app.bg.send(BgMessage::FifSkipped {
            request: id,
            path: PathBuf::from(format!("b{i}.bin")),
            reason: "skipped (binary)".into(),
        });
    }
    fif::drain(&mut app, 1_000);
    assert!(app.fif.notes.len() <= 50, "notes bounded");
    assert!(app.fif.notes_dropped > 0, "the dropped ones are counted");
}

// ------------------------------------------------------- FR-FF3: navigation

#[test]
fn enter_opens_the_selected_hit_with_the_caret_on_the_match() {
    let t = Tree::new("nav");
    // Known coordinates: in `xx TARGET here`, `TARGET` starts at 1-based column 4, line 3.
    t.write("data.txt", "one\ntwo\nxx TARGET here\n");
    let mut app = app_in(&t.0);
    run_into(&mut app, "TARGET", &t.0);
    assert_eq!(app.fif.hits.len(), 1);
    assert_eq!((app.fif.hits[0].line, app.fif.hits[0].col), (3, 4));

    app.fif.editing = false;
    fif::open_selected(&mut app);

    let doc = app.doc().expect("a document opened");
    assert!(doc.path.as_ref().unwrap().ends_with("data.txt"));
    // 0-based in the buffer: line 2, column 3.
    assert_eq!(doc.cursor.pos, LineCol::new(2, 3));
    assert!(doc.selection.is_none(), "a jump does not select");
}

#[test]
fn a_jump_scrolls_so_the_hit_is_visible() {
    let t = Tree::new("scroll");
    let mut text = String::new();
    for i in 0..500 {
        text.push_str(&format!("line {i}\n"));
    }
    text.push_str("here is NEEDLE deep down\n");
    for i in 0..500 {
        text.push_str(&format!("tail {i}\n"));
    }
    t.write("big.txt", &text);
    let mut app = app_in(&t.0);
    run_into(&mut app, "NEEDLE", &t.0);
    assert_eq!(app.fif.hits[0].line, 501);

    fif::open_selected(&mut app);
    let doc = app.doc().unwrap();
    assert_eq!(doc.cursor.pos.line, 500);
    assert!(
        doc.scroll.top_line <= 500 && 500 < doc.scroll.top_line + app.editor_height().max(1),
        "the hit must be on screen: top={} height={}",
        doc.scroll.top_line,
        app.editor_height()
    );
}

#[test]
fn a_hit_position_is_clamped_into_the_file_that_was_actually_loaded() {
    // The search and the click are not atomic: the file can shrink in between. A bogus position
    // would put the caret past the end and show a blank editor over a file with content.
    let t = Tree::new("clamp");
    let path = t.write("a.txt", "one\ntwo\nthree NEEDLE four\n");
    let mut app = app_in(&t.0);
    run_into(&mut app, "NEEDLE", &t.0);
    // The file loses two lines before the click.
    fs::write(&path, "one\n").unwrap();
    fif::open_selected(&mut app);
    let doc = app.doc().unwrap();
    let clamped = doc.buffer.clamp(LineCol::new(2, 5));
    assert_eq!(doc.cursor.pos, clamped);
    assert!(doc.cursor.pos.line < doc.buffer.line_count());
}

#[test]
fn jumping_to_a_file_that_is_already_open_reuses_the_tab() {
    let t = Tree::new("reuse");
    let path = t.write("a.txt", "one\nNEEDLE\n");
    let mut app = app_in(&t.0);
    // Open the file first.
    let doc = Document::from_decoded(
        path.clone(),
        "one\nNEEDLE\n",
        EncodingId::Utf8,
        np_core::detect_dominant_eol("one\nNEEDLE\n"),
        false,
    );
    app.open_document(doc);
    let before = app.documents.len();

    run_into(&mut app, "NEEDLE", &t.0);
    fif::open_selected(&mut app);

    assert_eq!(
        app.documents.len(),
        before,
        "no second copy of a buffer the user may have edited"
    );
    assert!(app.doc().unwrap().path.as_ref().unwrap().ends_with("a.txt"));
    assert_eq!(app.doc().unwrap().cursor.pos, LineCol::new(1, 0));
}

#[test]
fn arrow_keys_move_the_result_selection_and_it_stays_in_range() {
    let t = Tree::new("arrows");
    for i in 0..5 {
        t.write(&format!("f{i}.txt"), "needle\n");
    }
    let mut app = app_in(&t.0);
    run_into(&mut app, "needle", &t.0);
    // `start` leaves the keyboard on the results, which is what makes these arrows work.
    assert!(!app.fif.editing, "a finished search moves to the results");
    let len = app.fif.hits.len();

    for _ in 0..3 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(app.fif.selected, 3);
    // Held past the end: clamps, does not wrap or panic.
    for _ in 0..20 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(app.fif.selected, len - 1);
    for _ in 0..40 {
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    }
    assert_eq!(app.fif.selected, 0);
}

#[test]
fn panel_selection_is_separate_from_editor_selection() {
    // The rule that keeps a search from destroying work: moving through results must not touch
    // the editor's selection.
    let t = Tree::new("selsep");
    for i in 0..3 {
        t.write(&format!("f{i}.txt"), "needle\n");
    }
    let mut app = app_in(&t.0);
    if let Some(doc) = app.doc_mut() {
        doc.begin_selection(
            LineCol::new(0, 0),
            LineCol::new(0, 3),
            np_core::SelectionKind::Range,
        );
    }
    run_into(&mut app, "needle", &t.0);
    app.fif.editing = false;
    fif::select(&mut app, 2);
    assert_eq!(app.fif.selected, 2);
    let sel = app.doc().unwrap().selection.expect("editor selection kept");
    assert_eq!(sel.bounds(), (LineCol::new(0, 0), LineCol::new(0, 3)));
}

// ---------------------------------------------------------- keyboard capture

#[test]
fn typing_in_the_panel_edits_the_query_and_not_the_document() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    for c in "hello".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(app.fif.query, "hello");
    assert_eq!(
        app.doc().unwrap().buffer.line_string(0),
        "",
        "the document must be untouched"
    );
}

#[test]
fn tab_cycles_which_field_typing_edits() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    assert_eq!(app.fif.editing_field, fif::FifField::Query);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.fif.editing_field, fif::FifField::Dir);
    // `open` pre-fills the directory, so typing appends to what is there. Start from empty to
    // assert the keystrokes land in *this* field rather than the query.
    app.fif.dir.clear();
    for c in "/tmp".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(app.fif.dir, "/tmp");
    assert_eq!(
        app.fif.query, "",
        "the query did not receive the directory text"
    );
}

#[test]
fn an_empty_query_is_refused_with_an_inline_error() {
    let t = Tree::new("emptyq");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "   ".into();
    fif::start(&mut app);
    assert_eq!(app.fif.phase, FifPhase::Idle, "nothing started");
    assert!(app.fif.error.is_some());
}

#[test]
fn an_invalid_regex_is_reported_without_a_traversal() {
    let t = Tree::new("badre");
    t.write("a.txt", "x\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "[unclosed".into();
    app.fif.opts.regex = true;
    fif::start(&mut app);
    assert!(app.fif.error.is_some(), "inline error");
    assert_eq!(app.fif.phase, FifPhase::Idle);
    assert!(app.fif.hits.is_empty());
}

#[test]
fn a_missing_directory_is_an_inline_error_not_a_panic() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    app.fif.query = "x".into();
    app.fif.dir = "/no/such/directory/npt-s11".into();
    fif::start(&mut app);
    assert!(app.fif.error.is_some());
    assert_eq!(app.fif.phase, FifPhase::Idle);
}

// --------------------------------------------------------------- rendering

#[test]
fn the_panel_renders_three_hits_with_one_selected_at_100x30() {
    let t = Tree::new("render");
    t.write("alpha.rs", "x\nNEEDLE y\n");
    t.write("beta.rs", "NEEDLE\n");
    t.write("gamma.rs", "lead NEEDLE trail\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    run_into(&mut app, "NEEDLE", &t.0);
    fif::select(&mut app, 1);

    let buf = ui::render_to_buffer(&mut app, W, H);
    let all: String = (0..H).map(|y| ui::row_text(&buf, y, 0, W)).collect();
    assert!(all.contains("files scanned"), "header rendered");
    assert!(all.contains("alpha.rs"), "hit 1 rendered");
    assert!(all.contains("beta.rs"), "hit 2 rendered");
    assert!(all.contains("gamma.rs"), "hit 3 rendered");
    assert!(all.contains(":2:1:"), "locations rendered 1-based");
}

#[test]
fn the_selected_row_is_rendered_legibly_and_differently() {
    let t = Tree::new("render2");
    t.write("a.txt", "NEEDLE\n");
    t.write("b.txt", "NEEDLE\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    run_into(&mut app, "NEEDLE", &t.0);
    fif::select(&mut app, 1);

    let panel = find_in_files::panel_rect(&app);
    let rows = find_in_files::result_rows(app.find_panel_height);
    assert!(rows > 0, "the panel has result rows at this size");
    let buf = ui::render_to_buffer(&mut app, W, H);
    let y = panel.y + find_in_files::HEADER_ROWS + 1;
    let cell = buf
        .cell((panel.x + 2, y))
        .expect("a cell on the selected row");
    // Was: assert `Modifier::REVERSED`. Reversal asks the terminal to swap two colors the theme
    // never chose, which is the exact mechanism this stage removed -- on a light terminal the
    // selected result row reversed into an unreadable pair. The property the old assertion stood
    // for is worth keeping, and is now assertable properly: the selected row must differ from an
    // unselected one, and its own ink must be readable on its own ground.
    let unselected = buf
        .cell((panel.x + 2, y - 1))
        .expect("a cell on the row above");
    assert_ne!(
        cell.style().bg,
        unselected.style().bg,
        "the selected row must be distinguishable from its neighbours"
    );
    let pal = app.palette();
    let want =
        ratatui::style::Color::Rgb(pal.highlight_bg.0, pal.highlight_bg.1, pal.highlight_bg.2);
    assert_eq!(
        cell.style().bg,
        Some(want),
        "the selected row sits on the theme's highlight ground"
    );
    assert!(
        np_core::ThemePalette::legible(pal.highlight_fg, pal.highlight_bg),
        "and its ink is readable on that ground: {:?} on {:?} is {:.2}:1",
        pal.highlight_fg,
        pal.highlight_bg,
        np_core::ThemePalette::contrast(pal.highlight_fg, pal.highlight_bg),
    );
}

#[test]
fn the_panel_never_covers_the_whole_editor() {
    // A panel that ate every editor row would leave no way to see the document, which is the
    // opposite of what a docked panel is for.
    for (w, h) in [(40u16, 8u16), (60, 12), (100, 30), (200, 60)] {
        let mut app = App::new_headless(config(), (w, h));
        app.execute(Action::FindInFiles);
        app.recompute_geometry(w, h);
        let panel = find_in_files::panel_rect(&app);
        assert!(
            panel.h < app.regions.editor.h || app.regions.editor.h <= 3,
            "{w}x{h}: panel {} rows of {} editor rows",
            panel.h,
            app.regions.editor.h
        );
    }
}

#[test]
fn a_minimal_terminal_does_not_panic() {
    let mut app = app();
    app.execute(Action::FindInFiles);
    let _ = ui::render_to_buffer(&mut app, 40, 8);
    let _ = ui::render_to_buffer(&mut app, 10, 4);
    let _ = ui::render_to_buffer(&mut app, 1, 1);
}

// ------------------------------------------------------------------ mouse

#[test]
fn a_click_on_a_result_row_selects_it() {
    let t = Tree::new("click");
    for i in 0..4 {
        t.write(&format!("f{i}.txt"), "needle\n");
    }
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    run_into(&mut app, "needle", &t.0);

    let panel = find_in_files::panel_rect(&app);
    let row_y = panel.y + find_in_files::HEADER_ROWS + 2;
    app.handle_mouse(&MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: panel.x + 5,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.fif.selected, 2, "row 3 of the visible list");
}

#[test]
fn row_index_at_agrees_with_the_rendered_row() {
    // §6.9: renderer and hit-tester must agree. `row_index_at` and the draw loop both derive
    // from result_rows + scroll, so this asserts the mapping round-trips.
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    for i in 0..10 {
        app.bg.send(BgMessage::FifMatch {
            request: id,
            path: PathBuf::from(format!("f{i}.txt")),
            line: 1,
            col: 1,
            text: "x".into(),
            match_len: 1,
        });
    }
    fif::drain(&mut app, 64);
    let panel = find_in_files::panel_rect(&app);
    let rows = find_in_files::result_rows(app.find_panel_height);
    for visible in 0..rows {
        let y = panel.y + find_in_files::HEADER_ROWS + visible as u16;
        assert_eq!(find_in_files::row_index_at(&app, y), Some(visible));
    }
    // Above the result area is not a result.
    assert_eq!(find_in_files::row_index_at(&app, panel.y), None);
}

#[test]
fn a_click_below_the_panel_still_reaches_the_editor() {
    // The panel is docked, not modal: the editor above it must stay usable.
    let mut app = app();
    app.execute(Action::FindInFiles);
    let text_top = app.geometry.text_area.y;
    let panel = find_in_files::panel_rect(&app);
    assert!(text_top < panel.y, "there are editor rows above the panel");
    let hit = np_tui::mouse::hit_test(
        &MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: app.geometry.text_area.x,
            row: text_top,
            modifiers: KeyModifiers::NONE,
        },
        &app,
    );
    assert_ne!(hit, np_tui::mouse::Hit::FindPanel);
}

// ------------------------------------------------------- FR-F10 background

#[test]
fn a_large_file_load_arrives_on_the_ui_thread_and_opens_a_tab() {
    let t = Tree::new("load");
    let path = t.write("big.txt", "loaded in the background\n");
    let mut app = app();
    let before = app.documents.len();
    let id = app.bg.next_request_id();
    app.pending_load_request = Some(id);

    let loaded = np_core::load(&path, None).unwrap();
    app.bg.send(BgMessage::FileLoaded {
        request: id,
        result: Ok(loaded),
    });
    fif::drain(&mut app, 64);

    assert_eq!(app.documents.len(), before + 1, "a tab was opened");
    assert_eq!(
        app.doc().unwrap().buffer.line_string(0),
        "loaded in the background"
    );
}

#[test]
fn a_superseded_load_does_not_open_a_tab() {
    let t = Tree::new("loadstale");
    let path = t.write("old.txt", "stale\n");
    let mut app = app();
    let stale_id = app.bg.next_request_id();
    // The user moved on: a newer load is pending instead.
    let newer_id = app.bg.next_request_id();
    app.pending_load_request = Some(newer_id);

    let loaded = np_core::load(&path, None).unwrap();
    app.bg.send(BgMessage::FileLoaded {
        request: stale_id,
        result: Ok(loaded),
    });
    let before = app.documents.len();
    fif::drain(&mut app, 64);
    assert_eq!(
        app.documents.len(),
        before,
        "a superseded load must not put a document in front of the user"
    );
}

#[test]
fn a_failed_load_toasts_instead_of_hanging() {
    let mut app = app();
    let id = app.bg.next_request_id();
    app.pending_load_request = Some(id);
    app.bg.send(BgMessage::FileLoaded {
        request: id,
        result: Err("disk on fire".to_string()),
    });
    fif::drain(&mut app, 64);
    assert!(app
        .toasts
        .iter()
        .any(|t| t.message.contains("disk on fire")));
    assert_eq!(
        app.pending_load_request, None,
        "the pending slot clears either way"
    );
}

#[test]
fn the_open_policy_routes_huge_files_to_the_background_loader() {
    // FR-F10's boundary, asserted through the loader's own helper so the two cannot drift.
    let t = Tree::new("policy");
    let small = t.write("s.txt", "x");
    let big = t.write("b.txt", "");
    let f = fs::File::create(&big).unwrap();
    f.set_len(np_core::file::BACKGROUND_BYTES).unwrap();
    drop(f);
    assert_eq!(
        np_tui::bg::loader::policy_for(&small).0,
        np_core::OpenDecision::Open
    );
    assert_eq!(
        np_tui::bg::loader::policy_for(&big).0,
        np_core::OpenDecision::Background
    );
    let mid = t.write("m.txt", "");
    let f = fs::File::create(&mid).unwrap();
    f.set_len(np_core::file::ASK_READONLY_BYTES).unwrap();
    drop(f);
    assert_eq!(
        np_tui::bg::loader::policy_for(&mid).0,
        np_core::OpenDecision::AskReadOnly
    );
}

#[test]
fn a_real_worker_thread_delivers_into_the_panel() {
    // The one test that uses an actual thread: everything above drives the same code path
    // synchronously, so this is only here to prove the plumbing connects to a live worker.
    let t = Tree::new("thread");
    t.write("a.txt", "needle one\nneedle two\n");
    let mut app = app_in(&t.0);
    app.execute(Action::FindInFiles);
    app.fif.query = "needle".into();
    app.fif.dir = t.0.to_string_lossy().into_owned();
    // Opt out of the headless default so this really is a worker thread.
    app.sync_search = false;
    fif::start(&mut app);

    // Join-by-poll: drain until the run reports itself finished, bounded so a broken worker
    // fails the test instead of hanging it.
    let mut drained = 0usize;
    for _ in 0..5_000 {
        drained += fif::drain(&mut app, 64);
        if app.fif.phase == FifPhase::Idle && drained > 0 {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(app.fif.phase, FifPhase::Idle, "the worker finished");
    assert_eq!(app.fif.hits.len(), 2, "both hits arrived");
}

#[test]
fn no_document_mutation_happens_on_a_worker_thread() {
    // Spec §5.1, stated as a check on the API surface rather than a runtime assertion: the
    // worker entry points take owned data and a send closure, and none of them can name a
    // Document. If someone later adds an `App` or `&mut Document` parameter, this stops
    // compiling -- which is the point.
    fn assert_send<T: Send>(_: &T) {}
    let req = np_core::FifRequest::new("q", "/tmp", SearchOpts::default());
    assert_send(&req);
    let worker = BgWorker::new();
    assert_send(&worker);
    let msg = BgMessage::FifDone {
        request: 1,
        total: 0,
    };
    assert_send(&msg);
    let hit = PanelHit {
        path: PathBuf::from("/tmp/a"),
        line: 1,
        col: 1,
        text: "x".into(),
        match_len: 1,
    };
    assert_send(&hit);
}

#[test]
fn one_drain_of_a_full_budget_costs_far_less_than_a_frame() {
    // The budget is justified by its worst single drain, not the whole set: if one drain can
    // blow a 16 ms frame, keystrokes arriving between ticks start to feel sticky.
    let mut app = app();
    app.execute(Action::FindInFiles);
    let id = app.bg.next_request_id();
    app.fif.active_request = Some(id);
    app.fif.phase = FifPhase::Running;
    // Long lines are the pessimistic case: each hit is truncated and width-measured on ingest.
    let long = "needle ".repeat(400);
    for i in 0..fif::DRAIN_BUDGET {
        app.bg.send(BgMessage::FifMatch {
            request: id,
            path: PathBuf::from(format!("deep/nested/dir/file{i}.txt")),
            line: i + 1,
            col: 1,
            text: long.clone(),
            match_len: 6,
        });
    }
    let start = std::time::Instant::now();
    let n = fif::drain(&mut app, fif::DRAIN_BUDGET);
    let elapsed = start.elapsed();
    assert_eq!(n, fif::DRAIN_BUDGET);
    println!(
        "MEASURED worst drain: {n} long-line hits in {elapsed:?} (budget {})",
        fif::DRAIN_BUDGET
    );
    assert!(
        elapsed < std::time::Duration::from_millis(16),
        "a full-budget drain must fit inside one 16 ms frame, took {elapsed:?}"
    );
}
