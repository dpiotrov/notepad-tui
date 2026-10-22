//! S4 acceptance: the binary's observable behavior, driven as a child process.
//!
//! These are the only tests that exec the real `npt`. They cover what a unit test cannot:
//! exit codes, and the guarantee that `-h`/`-v` work with **no terminal attached at all**.
//!
//! Deliberately not tested here: entering the alternate screen. That needs a pty, and the
//! stage spec says explicitly not to add a pty dependency. The escape-sequence behavior is
//! covered instead by the recording driver in `term.rs`, which asserts the exact command
//! order in both directions.

use std::process::Command;

fn npt() -> Command {
    Command::new(env!("CARGO_BIN_EXE_npt"))
}

/// Run with stdin/out/err piped and no TTY, which is what a CI job looks like.
fn run(args: &[&str]) -> std::process::Output {
    npt()
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("binary spawns")
        .wait_with_output()
        .expect("wait")
}

#[test]
fn help_exits_zero_without_a_terminal() {
    for flag in ["-h", "--help"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag} should exit 0");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("USAGE:"), "{flag}: {text}");
        assert!(text.contains("--readonly"), "{flag}: {text}");
        assert!(out.stderr.is_empty(), "{flag} should not write stderr");
    }
}

#[test]
fn version_exits_zero_and_names_the_program() {
    for flag in ["-v", "--version"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag} should exit 0");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("npt "), "{flag}: {text}");
        // The version must match the crate, so it cannot drift.
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "{flag}: {text} lacks {}",
            env!("CARGO_PKG_VERSION")
        );
    }
}

#[test]
fn unknown_option_exits_two() {
    for flag in ["--bogus", "-z", "--line=oops"] {
        let out = run(&[flag]);
        assert_eq!(out.status.code(), Some(2), "{flag}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("npt:"), "{flag}: {err}");
    }
}

#[test]
fn missing_option_value_exits_two() {
    let out = run(&["--lang"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("needs a value"), "{err}");
}

#[test]
fn without_a_terminal_it_fails_cleanly_rather_than_hanging() {
    // The critical M0 property: no TTY must produce a clear message and a nonzero exit, not
    // a hang and not a panic. A hang here would time the whole suite out.
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(1), "runtime failure is exit 1");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("npt:"), "{err}");
    assert!(!err.contains("panicked"), "must not panic: {err}");
}

#[test]
fn a_file_that_does_not_exist_still_does_not_panic() {
    let out = run(&["/definitely/not/here/npt-test-file.txt"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "must not panic: {err}");
    // It fails at the terminal step, not at the file step, so the exit is still 1.
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn repeated_invocations_are_stable() {
    // No leaked global state between runs: the terminal depth counter lives per process, so
    // this only catches a crash-on-second-start class of bug.
    for _ in 0..3 {
        let out = run(&["--version"]);
        assert!(out.status.success());
    }
}

#[test]
fn help_does_not_create_a_log_or_touch_the_data_dir() {
    // -h must exit before logging initializes, so a run with an unwritable HOME still works.
    let dir = tempfile::tempdir().expect("tmpdir");
    let out = npt()
        .arg("--help")
        .env("HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path().join("nonexistent-but-fine"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn")
        .wait_with_output()
        .expect("wait");
    assert!(out.status.success());
    assert!(
        !dir.path().join("nonexistent-but-fine").exists(),
        "help must not write"
    );
}
