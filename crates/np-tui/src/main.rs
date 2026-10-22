//! `npt` — Notepad-TUI. Binary entry point.
//!
//! Deliberately thin: `App` owns editor state and is testable headless, so everything here
//! is wiring in a fixed order. The order matters and is documented at each step.
//!
//! # Startup order
//!
//! 1. Parse the CLI. `-h`/`-v` exit 0 here, **before** anything touches the terminal.
//! 2. Install the panic hook. Before logging and before the terminal, so a panic at any
//!    later step is reported and the terminal is restored.
//! 3. Init logging. Failure is non-fatal; the editor runs without a log.
//! 4. Load config. Never fails (FR-CG2); warnings go to stderr.
//! 5. Open CLI files through `file::load` (decode, per-line EOLs, size policy). A read
//!    failure becomes a warning and an empty named buffer, never a refusal to start.
//! 6. Enter the terminal, run the loop, and let `TerminalGuard::drop` restore it.
//!
//! # Exit codes
//!
//! 0 success · 1 runtime failure · 2 usage error. `-h`/`-v` are 0. These match the CLI
//! convention the stage spec asks for and are asserted by the integration tests.

use std::process::ExitCode;

use np_tui::cli::{self, Cli, CliError};
use np_tui::{event, logging, panic_hook, signal, term, App};

const EXIT_OK: u8 = 0;
const EXIT_RUNTIME: u8 = 1;
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = match cli::parse(&argv) {
        Ok(parsed) => parsed,
        Err(err) => return report_cli_error(&err),
    };

    // Installed before the terminal exists: a panic during terminal setup is exactly the
    // case where restoring matters most, and there is no guard alive to do it.
    panic_hook::install();
    // Same reasoning for signals: `term::TerminalGuard::enter` captures the terminal's
    // original termios, and a handler installed afterwards could fire against a terminal the
    // snapshot does not describe. Order: hook, signals, then the terminal.
    if !signal::install() {
        log::warn!("termination signal handlers not fully installed; a kill may not restore the terminal cleanly");
    }
    logging::init(simplelog::LevelFilter::Info);
    log::info!("argv: {argv:?}");

    match run(cli) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(err) => {
            // The terminal is already restored by the time we get here: either the guard
            // dropped or the panic hook ran.
            log::error!("fatal: {err:#}");
            eprintln!("npt: {err:#}");
            ExitCode::from(EXIT_RUNTIME)
        }
    }
}

/// Turn a CLI error into output plus an exit code.
fn report_cli_error(err: &CliError) -> ExitCode {
    if err.informational {
        // -h / -v: print to stdout and exit 0 without ever touching the terminal.
        println!("{}", err.message);
        return ExitCode::from(EXIT_OK);
    }
    eprintln!("npt: {}", err.message);
    eprintln!("{}", cli::HELP);
    ExitCode::from(err.code.max(EXIT_USAGE as i32) as u8)
}

/// The whole session, minus terminal ownership (which the guard handles).
fn run(cli: Cli) -> anyhow::Result<()> {
    let (config, warnings) = np_core::Config::load(cli.config.as_deref());
    for warning in &warnings {
        // Before the alternate screen, so the message is actually readable.
        eprintln!("npt: config: {warning}");
    }

    // Session restore (FR-CG3). The precedence rule lives in `cli::should_restore_session`
    // and is asserted there: CLI file arguments win, and `--fresh` opts out explicitly.
    let session_path = np_core::Session::default_path();
    let saved_session = if cli::should_restore_session(&cli) {
        session_path.as_deref().and_then(np_core::Session::load)
    } else if cli.fresh {
        log::info!("--fresh: session restore skipped");
        None
    } else {
        log::info!(
            "{} file argument(s) given; session restore skipped in favor of them",
            cli.files.len()
        );
        None
    };

    // Real file loading (encoding detection, per-line EOLs, binary detection) is S5. Until
    // then a file is read as UTF-8 and becomes a normal document, and an unreadable file is
    // a warning rather than a failed startup: opening `npt missing.txt` must still give the
    // user an editor.
    let mut app = App::new_headless(config, initial_size());
    app.session_path = session_path;
    // FR-CG5: Preferences writes go here. Resolved the same way `Config::load` resolved it,
    // so the file the dialog saves is the file the next launch reads -- including when
    // `--config` named somewhere else.
    app.config_path = Some(
        cli.config
            .clone()
            .or_else(np_core::Config::default_path)
            .unwrap_or_else(np_core::Config::editable_default_path),
    );

    if let Some(session) = &saved_session {
        for note in app.restore_session(session) {
            // Before the alternate screen, so the note is readable and does not compete with
            // the frame for a row.
            eprintln!("npt: {note}");
            log::warn!("session restore: {note}");
        }
        // A restored session must not be re-saved until something actually changes, or a
        // read-only look at the editor rewrites the session file every launch.
        app.session_dirty = false;
    }

    open_cli_files(&mut app, &cli);

    // Enter the terminal only once everything that can fail cheaply has succeeded, so a bad
    // config or a missing file never leaves the user staring at an alt screen.
    let _guard = term::TerminalGuard::enter()?;
    let size = terminal_size_or_default();
    app.recompute_geometry(size.0, size.1);

    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
            .map_err(|err| anyhow::anyhow!("cannot initialize the renderer: {err}"))?;

    // The catch_unwind the stage spec asks for. With unwinding (debug/test) a panic becomes a
    // clean shutdown here; with `panic = "abort"` (release) the process dies and the panic
    // hook is what restores the terminal and reports. Both paths are covered, which is why
    // the release profile keeps `abort`.
    // `AssertUnwindSafe` is justified, not a workaround: on the panic path we never observe
    // `app` or `terminal` again — the hook has already restored the terminal and the process
    // exits with a failure code. The default `UnwindSafe` bound guards against reading
    // half-mutated state after a catch, which is precisely what this call does not do.
    let loop_result = panic_hook::guard_running(std::panic::AssertUnwindSafe(|| {
        event::run(&mut terminal, &mut app)
    }));
    match loop_result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err),
        Err(panic_hook::Panicked) => {
            // The hook already printed the report and restored the terminal.
            anyhow::bail!("npt stopped after an internal error; see the log for details");
        }
    }
    Ok(())
}

/// Open each CLI file through the real S5 load path.
///
/// Three outcomes per path, chosen so that `npt` always starts with a usable editor:
///  - readable file -> `file::load` (decode + per-line EOLs + size policy) -> `Document`
///  - unreadable/missing -> empty document carrying the name, and a stderr note
///  - forced `-e` that does not name a codec -> warn and fall back to detection
fn open_cli_files(app: &mut App, args: &Cli) {
    let force = match args.encoding.as_deref() {
        Some(name) => match np_core::EncodingId::parse(name) {
            Some(enc) => Some(enc),
            None => {
                eprintln!("npt: unknown encoding '{name}', detecting instead");
                log::warn!("unknown encoding '{name}'");
                None
            }
        },
        None => None,
    };

    for path in &args.files {
        match open_one(app, path, args.readonly, force.clone()) {
            Ok(()) => {}
            Err(err) => {
                // FR-F1: a file we cannot read still opens, as an empty named buffer.
                eprintln!(
                    "npt: {}: {} (opening as a new file)",
                    path.display(),
                    err.user_message()
                );
                log::warn!("open failed for {}: {err}", path.display());
                let mut doc = np_core::Document::from_decoded(
                    path.clone(),
                    "",
                    app.config.default_encoding(),
                    app.config.default_eol(),
                    args.readonly,
                );
                doc.overtype = app.overtype;
                doc.modified = false;
                doc.tab_width = app.config.editor.tab_width.max(1);
                app.open_document(doc);
                app.note_recent(path);
            }
        }
    }

    // Position and language apply to the first file only (spec §6.13).
    if let Some(position) = args.position {
        if let Some(doc) = app.doc_mut() {
            // The CLI line number is 1-based; the model is 0-based. `set_cursor` clamps to
            // the document, so `--line 9999` on a short file lands on the last line rather
            // than erroring.
            let line = position.line.saturating_sub(1);
            doc.set_cursor(np_core::LineCol::new(line, position.col), false);
        }
    }
    if args.lang.is_some() {
        log::info!("forced language {:?} takes effect in S12", args.lang);
    }
}

/// Load one file and push it as a tab.
fn open_one(
    app: &mut App,
    path: &std::path::Path,
    readonly: bool,
    force: Option<np_core::EncodingId>,
) -> Result<(), np_core::CoreError> {
    // FR-F10: decide from the file's size *before* reading it. Reading first and deciding
    // afterwards is what made a 100 MB argument freeze the terminal for the whole read,
    // since the alt screen does not exist yet at this point -- there is nothing on screen to
    // show progress against, so the user sees a hung process.
    if let Ok(meta) = std::fs::metadata(path) {
        if np_core::file::open_policy(meta.len()) == np_core::file::OpenDecision::Background {
            eprintln!(
                "npt: {} is {} bytes, loading in the background",
                path.display(),
                meta.len()
            );
            log::info!("open policy: background load for {}", path.display());
            app.open_path(path);
            return Ok(());
        }
    }

    let loaded = np_core::file::load(path, force)?;

    match np_core::file::open_policy(loaded.size) {
        np_core::file::OpenDecision::Open => {}
        np_core::file::OpenDecision::AskReadOnly => {
            // The interactive prompt is S14 (it needs the dialog chrome). The size warning
            // is emitted now so the behavior is visible and testable.
            eprintln!(
                "npt: {} is {} bytes; consider opening it read-only (-r)",
                path.display(),
                loaded.size
            );
            log::info!("open policy: ask read-only for {}", path.display());
        }
        np_core::file::OpenDecision::Background => {
            // Unreachable: the check above hands a Background-sized file to
            // `App::open_path` and returns before the read. Left as an arm rather than a
            // `_ =>` so a future size policy that changes the bands fails to compile here
            // instead of silently loading a huge file on this thread.
            log::info!("open policy: background for {}", path.display());
        }
    }

    if loaded.binary {
        // FR-N5: "this may be a binary file" is a confirmation dialog, which is S14. Until
        // then the file opens and the state is logged, which beats refusing to open it.
        eprintln!(
            "npt: {} looks like a binary file, opening as text",
            path.display()
        );
        log::info!("binary file opened as text: {}", path.display());
    }
    if loaded.decoded.lossy {
        log::info!(
            "lossy decode of {} as {}",
            path.display(),
            loaded.decoded.encoding.label()
        );
    }

    let mut doc = np_core::Document::from_loaded(&loaded);
    // `-r` wins over the file's own write bit; the reverse never does.
    doc.readonly |= readonly;
    doc.overtype = app.overtype;
    // Config applies to the document from the moment it exists: column math, tab rendering
    // and the indentation an Enter produces all read `tab_width` from here rather than from a
    // global, which is what keeps np-core free of config access.
    doc.tab_width = app.config.editor.tab_width.max(1);
    app.open_document(doc);
    app.note_recent(path);

    log::info!(
        "opened {} ({} bytes, {}, {:?} lines)",
        path.display(),
        loaded.size,
        loaded.decoded.encoding.label(),
        loaded.line_eols.len()
    );
    Ok(())
}

/// Terminal size before the alt screen, best effort.
fn initial_size() -> (u16, u16) {
    terminal_size_or_default()
}

/// Ask crossterm for the real size; fall back to 80x24 when there is no terminal.
fn terminal_size_or_default() -> (u16, u16) {
    crossterm::terminal::size()
        .map(|(w, h)| (w.max(1), h.max(1)))
        .unwrap_or((80, 24))
}
