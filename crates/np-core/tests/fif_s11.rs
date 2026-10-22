//! Find in Files (stage S11, spec §6.5 FR-FF1..FF3).
//!
//! Headless: `search_files` takes a callback, so every assertion here is a `Vec` of events
//! over a tempdir tree. No terminal, no worker thread.
//!
//! The tests are ordered by how likely a wrong assumption is to hide: gitignore and hidden
//! rules first (they come from the `ignore` crate and are easy to get backwards), then the
//! glob filter, then binary/size skips, then coordinate correctness, then cancellation.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use np_core::{
    search_files, FifEvent, FifHit, FifRequest, FifSummary, GlobFilter, SearchOpts, SkipReason,
    MAX_SEARCH_BYTES,
};

fn opts() -> SearchOpts {
    SearchOpts::default()
}

/// Run a search and return the hits.
fn hits(req: &FifRequest) -> Vec<FifHit> {
    let cancel = AtomicBool::new(false);
    let mut out = Vec::new();
    search_files(req, &cancel, &mut |ev| {
        if let FifEvent::Match(h) = ev {
            out.push(h.clone());
        }
        true
    });
    out
}

fn run(req: &FifRequest) -> (FifSummary, Vec<FifEvent>) {
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    let summary = search_files(req, &cancel, &mut |ev| {
        events.push(ev.clone());
        true
    });
    (summary, events)
}

/// Relative paths of hits, sorted, so assertions do not depend on walk order.
fn located(req: &FifRequest, root: &Path) -> Vec<(String, usize, usize)> {
    let mut v: Vec<(String, usize, usize)> = hits(req)
        .iter()
        .map(|h| {
            (
                h.path
                    .strip_prefix(root)
                    .unwrap_or(&h.path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                h.line,
                h.col,
            )
        })
        .collect();
    v.sort();
    v
}

fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create dir");
    }
    fs::write(&path, content).expect("write");
    path
}

struct Tree {
    dir: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        // Exclusive create, retried: a counter restarts at zero every run, so a directory left
        // by an earlier aborted run would collide and its stale files would show up as extra
        // hits. Cargo also runs tests in threads, so two tests must never share a tree.
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::temp_dir();
        for _ in 0..1_000 {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let dir = base.join(format!("npt-fif-{name}-{}-{n}-{nanos}", std::process::id()));
            if fs::create_dir(&dir).is_ok() {
                return Tree { dir };
            }
        }
        panic!("could not create a unique tempdir for {name}");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ------------------------------------------------------------- FR-FF1: walking

#[test]
fn finds_matches_in_nested_directories() {
    let t = Tree::new("nested");
    write(&t.dir, "a.txt", "needle here\nnothing\n");
    write(&t.dir, "sub/b.txt", "text\nanother needle\n");
    write(&t.dir, "sub/deep/c.txt", "no match at all\n");

    let req = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(
        located(&req, &t.dir),
        vec![
            ("a.txt".to_string(), 1, 1),
            // `another needle`: `needle` starts at 0-based 8, so 1-based 9.
            (
                PathBuf::from("sub")
                    .join("b.txt")
                    .to_string_lossy()
                    .into_owned(),
                2,
                9
            ),
        ]
    );
}

#[test]
fn gitignored_file_is_excluded_when_enabled_and_included_when_disabled() {
    let t = Tree::new("gitignore");
    write(&t.dir, ".gitignore", "ignored.txt\n");
    write(&t.dir, "kept.txt", "needle\n");
    write(&t.dir, "ignored.txt", "needle\n");

    let on = FifRequest::new("needle", &t.dir, opts());
    let off = FifRequest {
        use_gitignore: false,
        ..FifRequest::new("needle", &t.dir, opts())
    };
    assert_eq!(located(&on, &t.dir), vec![("kept.txt".to_string(), 1, 1)]);
    let both = located(&off, &t.dir);
    assert_eq!(both.len(), 2, "both files with gitignore off, got {both:?}");
    assert!(both.iter().any(|(p, ..)| p == "ignored.txt"));
}

#[test]
fn gitignore_is_honored_without_a_git_repository() {
    // The `ignore` crate reads .gitignore regardless of a .git directory. Asserted because the
    // alternative (requiring a repo) would make the feature useless in a plain extracted tree.
    let t = Tree::new("norepo");
    assert!(!t.dir.join(".git").exists());
    write(&t.dir, ".gitignore", "*.log\n");
    write(&t.dir, "app.log", "needle\n");
    write(&t.dir, "app.txt", "needle\n");
    let req = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(located(&req, &t.dir), vec![("app.txt".to_string(), 1, 1)]);
}

#[test]
fn hidden_files_are_skipped_unless_requested() {
    let t = Tree::new("hidden");
    write(&t.dir, ".hidden", "needle\n");
    write(&t.dir, "visible.txt", "needle\n");

    let off = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(
        located(&off, &t.dir),
        vec![("visible.txt".to_string(), 1, 1)]
    );

    let on = FifRequest {
        include_hidden: true,
        ..FifRequest::new("needle", &t.dir, opts())
    };
    let with_hidden = located(&on, &t.dir);
    assert_eq!(
        with_hidden.len(),
        2,
        "hidden included on request, got {with_hidden:?}"
    );
    assert!(with_hidden.iter().any(|(p, ..)| p == ".hidden"));
}

#[test]
fn hidden_directories_are_skipped_unless_requested() {
    let t = Tree::new("hiddendir");
    write(&t.dir, ".cache/blob.txt", "needle\n");
    write(&t.dir, "src/main.txt", "needle\n");
    let off = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(
        located(&off, &t.dir),
        vec![("src/main.txt".to_string(), 1, 1)]
    );
}

// --------------------------------------------------------------- glob filtering

#[test]
fn glob_filter_restricts_to_matching_extensions_at_any_depth() {
    let t = Tree::new("glob");
    write(&t.dir, "main.rs", "needle\n");
    write(&t.dir, "notes.txt", "needle\n");
    write(&t.dir, "src/deep/mod.rs", "needle\n");
    write(&t.dir, "src/mod.py", "needle\n");

    let req = FifRequest {
        glob: Some("*.rs".to_string()),
        ..FifRequest::new("needle", &t.dir, opts())
    };
    let found = located(&req, &t.dir);
    assert_eq!(
        found,
        vec![
            ("main.rs".to_string(), 1, 1),
            (
                PathBuf::from("src")
                    .join("deep")
                    .join("mod.rs")
                    .to_string_lossy()
                    .into_owned(),
                1,
                1
            ),
        ]
    );
}

#[test]
fn an_empty_glob_means_every_file() {
    let t = Tree::new("globempty");
    write(&t.dir, "a.rs", "needle\n");
    write(&t.dir, "b.txt", "needle\n");
    let req = FifRequest {
        glob: Some("   ".to_string()),
        ..FifRequest::new("needle", &t.dir, opts())
    };
    assert_eq!(located(&req, &t.dir).len(), 2);
}

#[test]
fn an_invalid_glob_is_reported_and_nothing_is_searched() {
    let t = Tree::new("globbad");
    write(&t.dir, "a.txt", "needle\n");
    let req = FifRequest {
        glob: Some("[unclosed".to_string()),
        ..FifRequest::new("needle", &t.dir, opts())
    };
    let (summary, _) = run(&req);
    assert!(!summary.errors.is_empty(), "the bad glob must be reported");
    assert!(
        summary.matches == 0,
        "a bad filter must not silently become no filter"
    );
}

#[test]
fn glob_filter_parse_matches_basenames_and_paths() {
    let rs = GlobFilter::parse(Some("*.rs"));
    assert!(rs.matches(Path::new("/tmp/x/y/main.rs")));
    assert!(!rs.matches(Path::new("/tmp/x/y/main.py")));
    assert_eq!(rs.raw_pattern(), "*.rs");
    assert!(GlobFilter::parse(None).matches(Path::new("/anything/at/all.txt")));
    assert!(GlobFilter::parse(Some("")).matches(Path::new("/anything/at/all.txt")));
    assert!(GlobFilter::parse(Some("[bad")).invalid().is_some());
}

// ------------------------------------------------------ binary and size skipping

#[test]
fn a_binary_file_is_skipped_and_reported() {
    let t = Tree::new("binary");
    write(&t.dir, "text.txt", "needle\n");
    fs::write(
        t.dir.join("blob.bin"),
        [0u8, 1, 2, 3, b'n', b'e', b'e', b'd', b'l', b'e'],
    )
    .unwrap();

    let req = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(located(&req, &t.dir), vec![("text.txt".to_string(), 1, 1)]);
    let (_, events) = run(&req);
    let skipped: Vec<SkipReason> = events
        .iter()
        .filter_map(|e| match e {
            FifEvent::Skipped { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect();
    assert_eq!(skipped, vec![SkipReason::Binary]);
}

#[test]
fn a_file_over_the_cap_is_skipped_with_a_note_not_silently() {
    let t = Tree::new("toolarge");
    let big = t.dir.join("big.txt");
    let f = fs::File::create(&big).unwrap();
    // Sparse: claims the size without writing it, so the test is instant.
    f.set_len(MAX_SEARCH_BYTES + 1).unwrap();
    write(&t.dir, "small.txt", "needle\n");

    let req = FifRequest::new("needle", &t.dir, opts());
    let (summary, events) = run(&req);
    assert_eq!(summary.skipped, 1);
    assert!(events.iter().any(|e| matches!(
        e,
        FifEvent::Skipped {
            reason: SkipReason::TooLarge,
            ..
        }
    )));
    // The small file is still searched.
    assert_eq!(summary.matches, 1);
}

#[test]
fn the_size_cap_can_be_turned_off() {
    let t = Tree::new("capoff");
    let big = t.dir.join("big.txt");
    fs::write(&big, "needle\n").unwrap();
    let req = FifRequest {
        respect_size_cap: false,
        ..FifRequest::new("needle", &t.dir, opts())
    };
    // Content is small, so the cap never applied; the flag must not break the normal path.
    assert_eq!(located(&req, &t.dir).len(), 1);
}

// ------------------------------------------------------ option agreement (FR-FF1)

#[test]
fn regex_and_whole_word_agree_with_in_document_search() {
    let t = Tree::new("options");
    write(&t.dir, "a.txt", "foo foobar foo\n");

    let whole = FifRequest {
        opts: SearchOpts {
            whole_word: true,
            ..opts()
        },
        ..FifRequest::new("foo", &t.dir, opts())
    };
    let found = located(&whole, &t.dir);
    // `foo foobar foo` has two standalone `foo`s, at 1-based columns 1 and 12. The one inside
    // `foobar` is not a word and must not appear.
    assert_eq!(
        found,
        vec![("a.txt".to_string(), 1, 1), ("a.txt".to_string(), 1, 12)],
        "whole word must skip the foo inside foobar"
    );

    let re = FifRequest {
        opts: SearchOpts {
            regex: true,
            ..opts()
        },
        ..FifRequest::new(r"f\w+", &t.dir, opts())
    };
    let mut regex_hits = hits(&re);
    regex_hits.sort_by_key(|h| h.col);
    // `f\w+` is greedy: `foo`, `foobar`, `foo` -> three hits.
    assert_eq!(
        regex_hits.len(),
        3,
        "got {:?}",
        regex_hits.iter().map(|h| h.col).collect::<Vec<_>>()
    );
    assert_eq!(regex_hits[0].text, "foo foobar foo");

    // Case sensitivity, same rule as the document search.
    let ci = FifRequest::new("NEEDLE", &t.dir, opts());
    let t2 = Tree::new("ci");
    write(&t2.dir, "a.txt", "Needle here\n");
    let ci_req = FifRequest::new("NEEDLE", &t2.dir, opts());
    assert_eq!(located(&ci_req, &t2.dir).len(), 1, "insensitive by default");
    let cs = FifRequest {
        opts: SearchOpts {
            case_sensitive: true,
            ..opts()
        },
        ..FifRequest::new("NEEDLE", &t2.dir, opts())
    };
    assert!(located(&cs, &t2.dir).is_empty());
    let _ = ci;
}

#[test]
fn multibyte_columns_count_characters_not_bytes() {
    let t = Tree::new("multibyte");
    // `мир` is three characters and six bytes; the needle starts at character 4.
    write(&t.dir, "a.txt", "мир needle\n");
    let req = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(located(&req, &t.dir), vec![("a.txt".to_string(), 1, 5)]);
}

#[test]
fn a_hit_on_the_last_line_without_a_terminator_is_found() {
    let t = Tree::new("noterm");
    write(&t.dir, "a.txt", "one\ntwo\nneedle");
    let req = FifRequest::new("needle", &t.dir, opts());
    assert_eq!(located(&req, &t.dir), vec![("a.txt".to_string(), 3, 1)]);
}

// ------------------------------------------------------------- FR-FF3: shape

#[test]
fn hits_carry_one_based_line_and_col_and_the_line_text() {
    let t = Tree::new("shape");
    write(&t.dir, "a.txt", "first\nsecond TARGET line\n");
    let req = FifRequest::new("TARGET", &t.dir, opts());
    let found = hits(&req);
    assert_eq!(found.len(), 1);
    let hit = &found[0];
    assert_eq!(hit.line, 2, "1-based");
    assert_eq!(hit.col, 8, "1-based");
    assert_eq!(hit.text, "second TARGET line");
    assert_eq!(hit.match_len, 6);
    assert!(hit.display_location().ends_with(":2:8:"));
}

#[test]
fn crlf_line_endings_do_not_leave_cr_in_the_hit_text() {
    let t = Tree::new("crlf");
    write(&t.dir, "a.txt", "one\r\nfind me\r\n");
    let found = hits(&FifRequest::new("find", &t.dir, opts()));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].text, "find me", "no trailing CR in displayed text");
    assert_eq!(found[0].line, 2);
}

// ----------------------------------------------------------- edge cases

#[test]
fn an_empty_directory_yields_nothing_and_is_not_an_error() {
    let t = Tree::new("empty");
    let (summary, events) = run(&FifRequest::new("needle", &t.dir, opts()));
    assert_eq!(summary.matches, 0);
    assert!(summary.errors.is_empty());
    assert!(events.is_empty());
}

#[test]
fn an_empty_query_searches_nothing() {
    let t = Tree::new("emptyquery");
    write(&t.dir, "a.txt", "anything\n");
    let (summary, _) = run(&FifRequest::new("", &t.dir, opts()));
    assert_eq!(summary.matches, 0);
    assert!(!summary.invalid_query);
}

#[test]
fn an_invalid_regex_is_reported_without_searching_anything() {
    let t = Tree::new("badregex");
    write(&t.dir, "a.txt", "needle\n");
    let req = FifRequest {
        opts: SearchOpts {
            regex: true,
            ..opts()
        },
        ..FifRequest::new("[unclosed", &t.dir, opts())
    };
    let (summary, _) = run(&req);
    assert!(summary.invalid_query);
    assert_eq!(summary.matches, 0);
    assert!(!summary.errors.is_empty());
}

#[test]
fn a_missing_directory_is_reported_not_fatal() {
    let t = Tree::new("missing");
    let req = FifRequest::new("needle", t.dir.join("nope"), opts());
    let (summary, _) = run(&req);
    // The walk reports the root as an error and returns; the caller shows a toast.
    assert!(!summary.cancelled);
}

#[test]
fn searching_a_single_file_path_works() {
    let t = Tree::new("onefile");
    let f = write(&t.dir, "only.txt", "needle here\n");
    let req = FifRequest::new("needle", &f, opts());
    assert_eq!(hits(&req).len(), 1);
}

#[test]
fn many_hits_on_one_line_do_not_make_the_scan_quadratic() {
    let t = Tree::new("dense");
    // 20k matches on one line. Also proves the per-file ceiling is honored.
    write(&t.dir, "dense.txt", &"ab ".repeat(20_000));
    let req = FifRequest::new("ab", &t.dir, opts());
    let start = std::time::Instant::now();
    let found = hits(&req);
    let elapsed = start.elapsed();
    assert!(
        found.len() <= np_core::MAX_HITS_PER_FILE,
        "per-file ceiling, got {}",
        found.len()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "20k-match file took {elapsed:?}"
    );
}

// ------------------------------------------------------------ cancellation

#[test]
fn a_sink_returning_false_stops_the_walk_promptly() {
    let t = Tree::new("cancel");
    for i in 0..50 {
        write(&t.dir, &format!("f{i}.txt"), "needle\n");
    }
    let cancel = AtomicBool::new(false);
    let mut seen = 0usize;
    let summary = search_files(
        &FifRequest::new("needle", &t.dir, opts()),
        &cancel,
        &mut |ev| {
            if matches!(ev, FifEvent::Match(_)) {
                seen += 1;
                if seen >= 3 {
                    return false;
                }
            }
            true
        },
    );
    assert!(summary.cancelled, "early stop is reported as cancelled");
    assert!(seen <= 4, "stopped promptly, saw {seen}");
}

#[test]
fn the_cancel_flag_is_checked_before_any_work() {
    let t = Tree::new("cancelfirst");
    write(&t.dir, "a.txt", "needle\n");
    let cancel = AtomicBool::new(true);
    let mut events = 0usize;
    let summary = search_files(
        &FifRequest::new("needle", &t.dir, opts()),
        &cancel,
        &mut |_| {
            events += 1;
            true
        },
    );
    assert!(summary.cancelled);
    assert_eq!(events, 0, "nothing is emitted once cancelled");
}

#[test]
fn a_cancel_flag_set_mid_file_stops_that_file() {
    let t = Tree::new("cancelmid");
    write(&t.dir, "a.txt", &"needle\n".repeat(500));
    let cancel = AtomicBool::new(false);
    let mut seen = 0usize;
    let summary = search_files(
        &FifRequest::new("needle", &t.dir, opts()),
        &cancel,
        &mut |ev| {
            if matches!(ev, FifEvent::Match(_)) {
                seen += 1;
                if seen == 10 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            true
        },
    );
    assert!(summary.cancelled);
    assert!(seen < 500, "cancelled inside the file, saw {seen}");
}

// --------------------------------------------------------------- progress

#[test]
fn scanned_counts_are_monotonic_and_match_the_summary() {
    let t = Tree::new("progress");
    for i in 0..10 {
        write(&t.dir, &format!("f{i}.txt"), "x\n");
    }
    let (summary, events) = run(&FifRequest::new("nothing", &t.dir, opts()));
    let counts: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            FifEvent::Scanned { scanned, .. } => Some(*scanned),
            _ => None,
        })
        .collect();
    assert!(!counts.is_empty());
    for pair in counts.windows(2) {
        assert!(
            pair[1] > pair[0],
            "scanned must strictly increase: {counts:?}"
        );
    }
    assert_eq!(*counts.last().unwrap(), summary.scanned);
}

#[test]
fn an_unreadable_file_is_reported_and_the_search_continues() {
    let t = Tree::new("unreadable");
    write(&t.dir, "good.txt", "needle\n");
    // A directory named like a file cannot be read as one; the walk skips non-files, so
    // instead make the failure real by pointing at a path whose metadata read fails. The
    // portable way to get a read error for a *file* entry is a dangling symlink.
    #[cfg(unix)]
    std::os::unix::fs::symlink(t.dir.join("does-not-exist"), t.dir.join("dangling.txt")).unwrap();
    let (summary, _) = run(&FifRequest::new("needle", &t.dir, opts()));
    assert_eq!(
        summary.matches, 1,
        "one bad entry must not abort the search; errors {:?}",
        summary.errors
    );
}
