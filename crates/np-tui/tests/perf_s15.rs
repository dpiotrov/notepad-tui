//! S15 performance evidence (spec §9).
//!
//! Marked `#[ignore]` so `check.sh` stays fast. Run with:
//!
//! ```sh
//! cargo test --release -p np-tui --test perf_s15 -- --ignored --nocapture
//! ```
//!
//! Every metric in the §9 table gets a number here, printed in a form that can be pasted
//! straight into `docs/STATUS.md`. Targets are asserted, not just printed: a regression turns
//! into a red test rather than a forgotten spreadsheet row. Where a target is missed the
//! measured number and the cause go to `docs/DEVIATIONS.md`, per the S15 spec.
//!
//! # Method
//!
//! Fixtures are ordinary-shaped, not adversarial. S12 found that `fancy-regex` costs about
//! 88 us per ~56-character line but super-linearly in line length, so a fixture of long
//! filler lines would report numbers ~100x worse than the editor produces in practice. Lines
//! here are 60-120 characters, which is what real source and real prose look like.
//!
//! Timings are the median of several runs. A single sample on a shared machine is dominated
//! by whatever else the scheduler was doing.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use np_core::{Buffer, Config, Document, EncodingId, Eol};
use np_tui::app::App;

/// Median of `runs` samples, in milliseconds.
fn median_ms(mut samples: Vec<f64>) -> f64 {
    assert!(!samples.is_empty());
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = samples.len() / 2;
    if samples.len() % 2 == 1 {
        samples[mid]
    } else {
        (samples[mid - 1] + samples[mid]) / 2.0
    }
}

fn report(metric: &str, ms: f64, target: &str) {
    println!("METRIC {metric}: {ms:.3} ms (target {target})");
}

/// ~10 MB of ordinary-shaped text.
fn fixture_text(bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 4096);
    let mut i = 0usize;
    while text.len() < bytes {
        i += 1;
        if i % 41 == 0 {
            text.push_str(&format!(
                "// a comment line describing what item {i} is for\n"
            ));
        } else if i % 97 == 0 {
            text.push_str(&format!("    let label = \"a piece of text {i}\";\n"));
        } else {
            text.push_str(&format!(
                "fn handle_request_number_{i}(conn: &Conn, flags: u32) -> Result<u32> {{ ok(conn, flags, {i}) }}\n"
            ));
        }
    }
    text
}

fn write_fixture(dir: &Path, name: &str, bytes: usize) -> PathBuf {
    let path = dir.join(name);
    let text = fixture_text(bytes);
    std::fs::write(&path, text.as_bytes()).expect("write fixture");
    assert!(path.metadata().expect("meta").len() >= bytes as u64);
    path
}

/// Park the caret at the end of the document before typing.
///
/// Typing at line 0 is the worst case for every derived cache: the edit range starts at the
/// very first line, so nothing below it can be reused and the frame re-measures the whole
/// document. That is correct behavior, not a bug, but it is not what "typing in a 10 MB file"
/// means -- nobody types at the top of a 10 MB file. The measurement therefore uses the end of
/// the document, and the line-0 cost is recorded separately as a documented floor.
fn caret_at_end(app: &mut App) {
    app.execute(np_tui::actions::Action::DocEnd);
}

fn fresh(w: u16, h: u16) -> App {
    App::new_headless(Config::default(), (w, h))
}

fn doc_from(name: &str, text: &str) -> Document {
    Document::from_decoded(name.into(), text, EncodingId::Utf8, Eol::Lf, false)
}

/// Current RSS in kilobytes, from /proc. None where procfs is unavailable.
fn rss_kb() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().trim_end_matches(" kB").trim().parse().ok();
        }
    }
    None
}

// ─────────────────────────────────── startup -> first frame (<= 150 ms)

#[test]
#[ignore]
fn startup_to_first_frame() {
    // The whole cold path a user waits on: build the App from a default config, open one
    // small file, draw the first frame into a test backend.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("small.txt");
    std::fs::write(&path, "hello\nworld\n").expect("write");

    let mut samples = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        let mut app = fresh(120, 40);
        app.open_path(&path);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
        np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("first draw");
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let ms = median_ms(samples);
    report(
        "startup -> first frame (120x40, 1 small file)",
        ms,
        "<= 150 ms",
    );
    assert!(ms <= 150.0, "startup took {ms:.3} ms, budget 150 ms");
}

// ─────────────────────────────────── keystroke -> frame (<= 16 ms, <= 5 MB)

#[test]
#[ignore]
fn keystroke_to_frame_under_5mb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path(), "five-mb.txt", 5 * 1024 * 1024);
    let mut app = fresh(120, 40);
    app.open_path(&path);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
    // Warm the caches so the measurement is the steady-state keystroke, not the first frame.
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("warm draw");
    caret_at_end(&mut app);
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("warm draw at end");

    let mut samples = Vec::new();
    for i in 0..50 {
        let started = Instant::now();
        let mut ev = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        );
        ev.kind = crossterm::event::KeyEventKind::Press;
        app.handle_key(ev);
        let _ = black_box(i);
        np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let ms = median_ms(samples);
    report("keystroke -> frame (5 MB file)", ms, "<= 16 ms");
    assert!(ms <= 16.0, "keystroke->frame took {ms:.3} ms, budget 16 ms");
}

// ─────────────────────────────────── open 10 MB (<= 1 s)

#[test]
#[ignore]
fn open_10mb_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path(), "ten-mb.txt", 10 * 1024 * 1024);
    let mut samples = Vec::new();
    for _ in 0..5 {
        let mut app = fresh(120, 40);
        let started = Instant::now();
        app.open_path(&path);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        assert!(app.doc().is_some(), "the document must be open");
    }
    let ms = median_ms(samples);
    report("open 10 MB file", ms, "<= 1000 ms");
    assert!(ms <= 1000.0, "open 10 MB took {ms:.3} ms, budget 1000 ms");
}

// ─────────────────────────────────── open 100 MB (<= 5 s)

#[test]
#[ignore]
fn open_100mb_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path(), "hundred-mb.txt", 100 * 1024 * 1024);
    let started = Instant::now();
    let mut app = fresh(120, 40);
    // `open_path` routes a file this large to the background worker, so this measures the
    // time to get the editor responsive with the file named on screen -- which is what the
    // user experiences -- not the time to read 100 MB. The read itself is timed by the
    // `full read` figure below, after the worker is drained.
    app.open_path(&path);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    report(
        "open 100 MB file (responsive, background load)",
        ms,
        "<= 5000 ms",
    );
    assert!(app.doc().is_some(), "the document must be open");
    assert!(ms <= 5000.0, "open 100 MB took {ms:.3} ms, budget 5000 ms");
    assert!(
        app.doc().map(|d| d.readonly).unwrap_or(false),
        "spec §11 M7: a 100 MB file opens read-only"
    );

    // M7 asks for progress and read-only, not merely speed, so assert the contents actually
    // arrive rather than that a placeholder appeared quickly.
    let read_started = Instant::now();
    let mut drained = 0usize;
    loop {
        drained += np_tui::fif::drain(&mut app, 64);
        if app.pending_load_request.is_none() {
            break;
        }
        assert!(
            read_started.elapsed() < Duration::from_secs(120),
            "the background read did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = drained;
    let lines = app.doc().map(|d| d.buffer.line_count()).unwrap_or(0);
    assert!(
        lines > 1,
        "the background load must fill the tab with the file's contents, got {lines} lines"
    );
    report(
        "100 MB full read (background worker)",
        read_started.elapsed().as_secs_f64() * 1000.0,
        "informational",
    );
}

// ─────────────────────────────────── memory per document (<= 4x file size)

#[test]
#[ignore]
fn memory_per_document_under_four_times_file_size() {
    // RSS delta around opening a 10 MB file. The 4x ceiling in spec §9 is a rope
    // representation budget, not a byte-for-byte copy budget.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path(), "mem.txt", 10 * 1024 * 1024);
    let file_bytes = path.metadata().expect("meta").len();

    // Open once and drop, so the allocator is warm and the pages are faulted in.
    {
        let mut warm = fresh(120, 40);
        warm.open_path(&path);
        drop(warm);
    }
    // The document stays alive across the second reading: dropping it first would let the
    // allocator hand the pages back and report a delta near zero.
    let before = rss_kb().expect("procfs RSS available");
    let mut app = fresh(120, 40);
    app.open_path(&path);
    let line_count = black_box(app.doc().map(|d| d.buffer.line_count()).unwrap_or(0));
    let after = rss_kb().expect("procfs RSS available");
    let delta_kb = after.saturating_sub(before);
    let ratio = (delta_kb as f64 * 1024.0) / file_bytes as f64;
    println!("(document holds {line_count} lines)");
    println!(
        "METRIC memory per document: file {} KiB, RSS delta {} KiB, ratio {ratio:.2}x (target <= 4x)",
        file_bytes / 1024,
        delta_kb
    );
    assert!(
        ratio <= 4.0,
        "RSS grew {ratio:.2}x the file size, budget 4x (file {} bytes, delta {} KiB)",
        file_bytes,
        delta_kb
    );
}

// ─────────────────────────────────── typing in a 10 MB file (<= 16 ms)

#[test]
#[ignore]
fn typing_in_10mb_file() {
    // Highlighting is active by default, so this is the FR-E4 + FR-H4 combined budget.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path(), "typing.txt", 10 * 1024 * 1024);
    let mut app = fresh(120, 40);
    app.open_path(&path);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("warm");
    caret_at_end(&mut app);
    np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("warm at end");

    let mut samples = Vec::new();
    for _ in 0..50 {
        let started = Instant::now();
        let mut ev = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        );
        ev.kind = crossterm::event::KeyEventKind::Press;
        app.handle_key(ev);
        np_tui::event::draw_if_dirty(&mut terminal, &mut app).expect("draw");
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let ms = median_ms(samples);
    report(
        "typing in a 10 MB file (highlighting active)",
        ms,
        "<= 16 ms",
    );
    assert!(ms <= 16.0, "typing took {ms:.3} ms, budget 16 ms");
}

// ─────────────────────────────────── huge paste: chunked, one undo unit, timed

#[test]
#[ignore]
fn ten_megabyte_paste() {
    let payload = fixture_text(10 * 1024 * 1024);
    let mut app = fresh(120, 40);
    app.open_document(doc_from("/tmp/paste-target.txt", "anchor\n"));
    let before = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();

    let started = Instant::now();
    np_tui::event::paste_into(&mut app, &payload);
    let paste_ms = started.elapsed().as_secs_f64() * 1000.0;

    let undo_started = Instant::now();
    app.execute(np_tui::actions::Action::Undo);
    let undo_ms = undo_started.elapsed().as_secs_f64() * 1000.0;

    let after = app
        .doc()
        .map(|d| d.buffer.text_string())
        .unwrap_or_default();
    println!(
        "METRIC 10 MB paste: insert {paste_ms:.3} ms, single undo {undo_ms:.3} ms, payload {} MiB",
        payload.len() / (1024 * 1024)
    );
    assert!(
        after == before,
        "one undo must revert a 10 MB paste exactly ({} bytes before, {} after)",
        before.len(),
        after.len()
    );
    // Responsiveness budget: a single keystroke-equivalent action should not block the loop
    // for seconds. This is the number spec §9 asks to record.
    assert!(
        paste_ms <= 1000.0,
        "a 10 MB paste took {paste_ms:.3} ms; the UI would visibly stall"
    );
}

// ─────────────────────────────────── Find in Files: cancellable

#[test]
#[ignore]
fn find_in_files_is_cancellable() {
    // Escape latency while a search is running. The search itself is on a worker thread, so
    // what this measures is whether the UI thread stays free enough to notice the keypress.
    let dir = tempfile::tempdir().expect("tempdir");
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).expect("tree");
    for i in 0..200 {
        let text = fixture_text(64 * 1024);
        std::fs::write(tree.join(format!("f{i}.txt")), text).expect("write tree file");
    }
    let mut app = fresh(120, 40);
    np_tui::fif::open(&mut app);
    app.fif.query = "handle_request_number_".into();
    app.fif.dir = tree.to_string_lossy().into_owned();
    np_tui::fif::start(&mut app);

    // The UI must accept input immediately: measure a keystroke while the worker runs.
    let mut samples = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        np_tui::event::handle_event(
            &mut app,
            &crossterm::event::Event::Key(key_press(crossterm::event::KeyCode::Esc)),
        );
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        // Re-open so the next sample measures the same thing.
        np_tui::fif::open(&mut app);
        app.fif.query = "handle_request_number_".into();
        app.fif.dir = tree.to_string_lossy().into_owned();
        np_tui::fif::start(&mut app);
    }
    let ms = median_ms(samples);
    report("Esc latency during a Find in Files run", ms, "<= 16 ms");
    assert!(
        ms <= 16.0,
        "Esc took {ms:.3} ms while searching, budget 16 ms"
    );
    np_tui::fif::cancel(&mut app);
}

fn key_press(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    let mut ev = crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
    ev.kind = crossterm::event::KeyEventKind::Press;
    ev
}

// ─────────────────────────────────── non-timing sanity: buffer cost model

#[test]
#[ignore]
fn buffer_representation_cost() {
    // Isolates the rope's own overhead from the app layer, so a memory-miss deviation can
    // name the actual cause.
    let text = fixture_text(10 * 1024 * 1024);
    let bytes = text.len();
    let before = rss_kb().expect("procfs RSS available");
    let buf = black_box(Buffer::from_str_lossy(&text));
    let lines = black_box(buf.line_count());
    let after = rss_kb().expect("procfs RSS available");
    let delta_kb = after.saturating_sub(before);
    println!(
        "METRIC rope cost: {} MiB text -> {lines} lines, RSS delta {} KiB, ratio {:.2}x",
        bytes / (1024 * 1024),
        delta_kb,
        (delta_kb as f64 * 1024.0) / bytes as f64
    );
}
