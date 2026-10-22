//! S12 performance evidence (spec §9, FR-H4).
//!
//! Marked `#[ignore]` so `check.sh` stays fast. Run with:
//!
//! ```sh
//! cargo test --release -p np-core --test highlight_perf_s12 -- --ignored --nocapture
//! ```
//!
//! # What the budget actually constrains
//!
//! Spec §9 asks for "keystroke -> rendered frame <= 16 ms" and "editing a 10 MB file meets
//! the 16 ms budget (incremental highlighting)". Highlighting is one stage of that frame, so
//! it has to leave room for the buffer edit, the layout, and the draw. The numbers below are
//! therefore compared against a fraction of 16 ms, not against 16 ms itself.
//!
//! # Why a cold random jump is not in the budget
//!
//! Reaching line N requires the parser state at N, and the only way to get it is to have
//! parsed something at or above N. A checkpoint at line 128 says nothing about line 500 000.
//! So a single cold jump into a huge file costs the distance travelled, and no cache design
//! changes that. What the cache guarantees is that the cost is paid once per line and bounded
//! by `CHECKPOINT_EVERY` after the first pass -- which is what `scroll_forward_*` measures,
//! and it is the number that corresponds to actual editing and scrolling.

use std::hint::black_box;
use std::time::Instant;

use np_core::highlight::{HighlightEngine, CHECKPOINT_EVERY};
use np_core::Buffer;

/// A ~10 MB Rust-ish source file: 200 chars per line, 50 000 lines.
fn big_fixture() -> (String, usize) {
    // Realistic source: lines of the length people actually write. This matters more than it
    // sounds. The C-free `fancy-regex` backend syntect uses here costs roughly 88 us per
    // ~56-character line, but super-linearly in line length -- a padded 300-character line
    // measured 6.4 ms. A fixture made of long filler lines would report a 100x worse number
    // than the editor produces on real files, so the fixture is deliberately ordinary.
    let target = 10 * 1024 * 1024;
    let mut text = String::with_capacity(target + 4096);
    let mut lines = 0usize;
    let mut i = 0usize;
    while text.len() < target {
        i += 1;
        lines += 1;
        if i % 37 == 0 {
            text.push_str(&format!("// a comment describing item {i}\n"));
        } else if i % 53 == 0 {
            text.push_str(&format!("    let s = \"some text {i}\";\n"));
        } else {
            text.push_str(&format!(
                "fn function_number_{i}(a: u64, b: u64) -> u64 {{ a + b + {i} }}\n"
            ));
        }
    }
    (text, lines)
}

fn big_buffer() -> (Buffer, usize) {
    let (text, lines) = big_fixture();
    assert!(
        text.len() >= 10 * 1024 * 1024,
        "fixture should be at least 10 MB, is {}",
        text.len()
    );
    (Buffer::from_str_lossy(&text), lines)
}

#[test]
#[ignore = "10 MB perf evidence; run explicitly and record in docs/STATUS.md"]
fn ten_megabyte_highlight_budget() {
    let engine = HighlightEngine::load();
    let (buf, fixture_lines) = big_buffer();
    let mb = buf.char_count() as f64 / (1024.0 * 1024.0);
    println!(
        "fixture: {} lines ({} generated), {:.1} MB",
        buf.line_count(),
        fixture_lines,
        mb
    );

    // ---- 1. cold first viewport: the worst realistic first-frame cost.
    let mut cache = engine.new_cache(Some("Rust"));
    let t = Instant::now();
    for l in 0..40 {
        black_box(engine.line_at(&mut cache, l, &buf));
    }
    let cold_viewport = t.elapsed();
    println!("cold first viewport (40 lines): {cold_viewport:?}");
    // A cold viewport is the *first* frame after opening a file, not a keystroke frame, so it
    // is not held to 16 ms. It is held to well under spec's 1 s "open 10 MB" budget.
    assert!(
        cold_viewport.as_millis() < 100,
        "a cold first viewport took {cold_viewport:?}"
    );

    // ---- 2. the spec's case: an edit near the top, then re-highlight the viewport.
    // Prime a viewport, invalidate at its top, redraw it. This is what FR-H4 is about.
    let mut cache = engine.new_cache(Some("Rust"));
    for l in 0..600 {
        black_box(engine.line_at(&mut cache, l, &buf));
    }
    let t = Instant::now();
    engine.invalidate_from(&mut cache, 10);
    for l in 10..50 {
        black_box(engine.line_at(&mut cache, l, &buf));
    }
    let edit_rehighlight = t.elapsed();
    println!("edit at line 10 then re-highlight 40 lines: {edit_rehighlight:?}");
    assert!(
        edit_rehighlight.as_millis() < 16,
        "re-highlighting a viewport after an edit took {edit_rehighlight:?}, over the 16 ms frame"
    );

    // ---- 3. scrolling: one lex per line, forever.
    // Measured on a fresh cache scrolled monotonically, so the count is the scroll's own and
    // not contaminated by the priming walk's checkpoint gaps.
    let mut cache3 = engine.new_cache(Some("Rust"));
    for l in 0..600 {
        black_box(engine.line_at(&mut cache3, l, &buf));
    }
    let base = HighlightEngine::lex_count(&cache3);
    let t = Instant::now();
    for l in 600..1_100 {
        black_box(engine.line_at(&mut cache3, l, &buf));
    }
    let scroll = t.elapsed();
    let lexed = HighlightEngine::lex_count(&cache3) - base;
    println!("scroll 500 lines: {scroll:?} ({lexed} lexes)");
    assert_eq!(lexed, 500, "scrolling must lex each line exactly once");
    let per_line = scroll / 500;
    let viewport40 = per_line * 40;
    println!("per-line scroll cost: {per_line:?} -> a 40-line viewport redraw: {viewport40:?}");
    assert!(
        per_line.as_micros() < 200,
        "per-line scroll cost {per_line:?} is unreasonable"
    );
    // The number that maps to spec §9: redrawing a viewport's worth of syntax must leave the
    // rest of the 16 ms frame for the edit, layout and draw.
    assert!(
        viewport40.as_millis() < 8,
        "a 40-line viewport of syntax costs {viewport40:?}, leaving too little of a 16 ms frame"
    );

    // ---- 4. repeated keystrokes in one place: the actual typing loop.
    let mut cache = engine.new_cache(Some("Rust"));
    for l in 0..600 {
        black_box(engine.line_at(&mut cache, l, &buf));
    }
    let t = Instant::now();
    for _ in 0..20 {
        engine.invalidate_from(&mut cache, 5);
        for l in 0..40 {
            black_box(engine.line_at(&mut cache, l, &buf));
        }
    }
    let typing = t.elapsed();
    let per_keystroke = typing / 20;
    println!("20 edit+redraw cycles: {typing:?} -> {per_keystroke:?} per keystroke");
    assert!(
        per_keystroke.as_millis() < 16,
        "highlighting alone took {per_keystroke:?} per keystroke, over the 16 ms frame"
    );

    // ---- 5. memory: the cache must track the viewport, not the file.
    let mut cache = engine.new_cache(Some("Rust"));
    for l in 0..buf.line_count() {
        black_box(engine.line_at(&mut cache, l, &buf));
    }
    let held = cache.cached_line_count();
    println!("cache holds {held} of {} lines", buf.line_count());
    assert!(
        held < buf.line_count() / 2,
        "cache grew with the file: {held} lines held"
    );

    // ---- 6. checkpoint density on a big file.
    let cps = cache.checkpoint_lines();
    println!(
        "{} checkpoints, every {} lines",
        cps.len(),
        CHECKPOINT_EVERY
    );
    assert!(
        cps.len() > buf.line_count() / CHECKPOINT_EVERY / 2,
        "only {} checkpoints for {} lines",
        cps.len(),
        buf.line_count()
    );
    assert!(cps.iter().all(|c| c % CHECKPOINT_EVERY == 0));

    // ---- 7. a cold random jump pays for the distance; recorded, not asserted as fast.
    let mut cold = engine.new_cache(Some("Rust"));
    for l in 0..130 {
        black_box(engine.line_at(&mut cold, l, &buf));
    }
    let t = Instant::now();
    black_box(engine.line_at(&mut cold, buf.line_count() - 1, &buf));
    let jump = t.elapsed();
    println!(
        "cold jump from checkpoint 128 to line 49 999: {jump:?} (expected: pays the distance)"
    );
}
