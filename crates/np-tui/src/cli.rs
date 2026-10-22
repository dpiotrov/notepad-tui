//! Command line parsing (spec §6.13).
//!
//! # Why by hand
//!
//! `clap` is not in the contracted dependency set, and the surface here is nine flags plus
//! positional paths. A hand-rolled parser also keeps the two behaviors the spec depends on
//! easy to get exactly right: `-h`/`-v` must exit 0 **without touching the terminal**, and
//! a bad flag must exit 2 with a short message rather than a panic.
//!
//! # Purity
//!
//! [`parse`] takes a slice and returns a `Result` — no printing, no process exit, no clock.
//! The binary decides what an error means; the parser only describes it. That is what makes
//! every flag combination unit-testable without spawning a process.
//!
//! # `--` and filenames that look like flags
//!
//! Everything after a bare `--` is a file path, so `npt -- --weird-name.txt` opens a file
//! rather than reporting an unknown option. This is the conventional POSIX rule and costs
//! one flag to implement.

use std::path::PathBuf;

/// Where the CLI wants the cursor to land in the first file (1-based line, 0-based column
/// in the parsed value; `Document` wants both 0-based, and `main` does that conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

/// What a well-formed command line asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cli {
    pub readonly: bool,
    /// Forced syntax language for the first file (FR-S6).
    pub lang: Option<String>,
    /// Forced encoding for the first file (FR-N3).
    pub encoding: Option<String>,
    pub position: Option<Position>,
    pub config: Option<PathBuf>,
    /// Skip session restore (FR-CG3). Accepted now, honored in S9.
    pub fresh: bool,
    pub files: Vec<PathBuf>,
}

impl Cli {
    /// True when the command line asks for nothing but an editor session.
    pub fn is_empty(&self) -> bool {
        *self == Cli::default()
    }
}

/// A CLI error. `usage` is the short one-liner for stderr; the binary prints [`HELP`] under
/// it so the user sees the valid options without the parser having to write anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub message: String,
    /// Process exit code. 2 for usage errors (the conventional value), 0 for the
    /// print-and-exit cases below.
    pub code: i32,
    /// Set for `-h`/`-v`: print `message`, exit 0, never touch the terminal.
    pub informational: bool,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: 2,
            informational: false,
        }
    }

    fn informational(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: 0,
            informational: true,
        }
    }
}

/// Whether a command line should restore the previous session (FR-CG3).
///
/// The rule, stated in one place because it is the question every editor gets asked: **CLI
/// file arguments win.** `npt foo.txt` opens `foo.txt`, not the tabs from last time plus
/// `foo.txt`. A user who names a file means that file, and silently reopening twelve old tabs
/// alongside it turns a targeted command into a mess. `--fresh` is the explicit opt-out.
///
/// The saved session is not destroyed by either case -- it is simply not restored this run,
/// and the next plain `npt` brings it back.
///
/// A pure function of the parsed arguments, so the policy is asserted directly rather than
/// through a child process that cannot reach an alternate screen in CI.
pub fn should_restore_session(args: &Cli) -> bool {
    !args.fresh && args.files.is_empty()
}

pub const HELP: &str = "\
npt — Notepad-TUI, a text editor for the terminal

USAGE:
    npt [OPTIONS] [FILE]...

OPTIONS:
    -r, --readonly        open files read-only
    -l, --lang <LANG>     force syntax language (first file)
    -e, --encoding <ENC>  force encoding (first file)
        --line N[:COL]    jump to position in first file (1-based line)
        --config <PATH>   alternative config file
        --fresh           do not restore the previous session
    -v, --version         print version and exit
    -h, --help            print this help and exit

ARGS:
    [FILE]...             files to open; missing files open as a new buffer

EXAMPLES:
    npt main.rs                 open a file
    npt --line 42:7 main.rs     open and jump to line 42, column 7
    npt -e windows-1251 log.txt open with an explicit encoding
    npt -- --help.txt           open a file whose name starts with '-'
";

/// Version string, taken from the workspace version so it can never drift from the crate.
pub fn version() -> String {
    format!("npt {}", env!("CARGO_PKG_VERSION"))
}

/// Parse `argv` excluding the program name.
///
/// Returns [`CliError::informational`] for `-h`/`-v` so the caller prints and exits 0, and
/// a usage error with code 2 otherwise. No allocation-heavy work and no I/O.
pub fn parse(args: &[String]) -> Result<Cli, CliError> {
    let mut out = Cli::default();
    let mut only_files = false;
    let mut iter = args.iter().peekable();

    while let Some(arg) = iter.next() {
        // After `--`, everything is a path, including things that look like options.
        if only_files {
            out.files.push(PathBuf::from(arg));
            continue;
        }
        if arg == "--" {
            only_files = true;
            continue;
        }

        // Long options, including the `--opt=value` form.
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            match name {
                "readonly" => {
                    reject_inline(name, inline.as_deref())?;
                    out.readonly = true;
                }
                "fresh" => {
                    reject_inline(name, inline.as_deref())?;
                    out.fresh = true;
                }
                "help" => return Err(CliError::informational(HELP.to_string())),
                "version" => return Err(CliError::informational(version())),
                "lang" => out.lang = Some(value(name, inline, &mut iter)?),
                "encoding" => out.encoding = Some(value(name, inline, &mut iter)?),
                "config" => out.config = Some(PathBuf::from(value(name, inline, &mut iter)?)),
                "line" => out.position = Some(parse_position(&value(name, inline, &mut iter)?)?),
                "" => return Err(CliError::usage("empty option name `--`")),
                other => return Err(unknown(&format!("--{other}"))),
            }
            continue;
        }

        // Short options. `-r`/`-l x`; bundled shorts (`-rf`) are supported because it is
        // cheap, but a value-taking short must be last in its bundle.
        if let Some(rest) = arg.strip_prefix('-') {
            if rest.is_empty() {
                // Bare `-` is conventionally stdin; not supported yet, and silently
                // ignoring it would open an empty buffer instead of complaining.
                return Err(CliError::usage(
                    "`-` (stdin) is not supported; pass a file path",
                ));
            }
            let chars: Vec<char> = rest.chars().collect();
            let mut ci = 0usize;
            while ci < chars.len() {
                let c = chars[ci];
                match c {
                    'r' => out.readonly = true,
                    'h' => return Err(CliError::informational(HELP.to_string())),
                    'v' => return Err(CliError::informational(version())),
                    'l' | 'e' => {
                        // The rest of this bundle is the value if present ("-l rust"),
                        // otherwise consume the next argument ("-l rust" with a space).
                        let tail: String = chars[ci + 1..].iter().collect();
                        let val = if !tail.is_empty() {
                            tail
                        } else {
                            take_next(&mut iter, &format!("-{c}"))?
                        };
                        if c == 'l' {
                            out.lang = Some(val);
                        } else {
                            out.encoding = Some(val);
                        }
                        ci = chars.len();
                        continue;
                    }
                    other => return Err(unknown(&format!("-{other}"))),
                }
                ci += 1;
            }
            continue;
        }

        out.files.push(PathBuf::from(arg));
    }
    Ok(out)
}

/// Reject `--flag=value` for flags that take no value. Saying so beats silently dropping it.
fn reject_inline(name: &str, inline: Option<&str>) -> Result<(), CliError> {
    match inline {
        Some(_) => Err(CliError::usage(format!(
            "`--{name}` does not take a value (write `--{name}`, not `--{name}=...`)"
        ))),
        None => Ok(()),
    }
}

/// Read an option's value: the inline `=x` form, else the next argv entry.
fn value(
    name: &str,
    inline: Option<String>,
    iter: &mut std::iter::Peekable<std::slice::Iter<'_, String>>,
) -> Result<String, CliError> {
    if let Some(v) = inline {
        return Ok(v);
    }
    take_next(iter, &format!("--{name}"))
}

fn take_next(
    iter: &mut std::iter::Peekable<std::slice::Iter<'_, String>>,
    option: &str,
) -> Result<String, CliError> {
    match iter.next() {
        Some(v) => Ok(v.clone()),
        None => Err(CliError::usage(format!("`{option}` needs a value"))),
    }
}

fn unknown(flag: &str) -> CliError {
    CliError::usage(format!("unknown option `{flag}`"))
}

/// Parse the `--line` value: `42` or `42:7`.
///
/// Bad input is a usage error rather than a crash, per the stage spec. Line 0 is rejected
/// because lines are 1-based in the readout, so `0` cannot mean anything the user intended.
/// This is total — no `expect`, since a malformed CLI argument must never panic.
fn parse_position(spec: &str) -> Result<Position, CliError> {
    parse_position_opt(spec).ok_or_else(|| {
        CliError::usage(format!(
            "invalid `--line` value `{spec}` (expected N or N:COL, e.g. 42 or 42:7)"
        ))
    })
}

fn parse_position_opt(spec: &str) -> Option<Position> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (line_s, col_s) = match spec.split_once(':') {
        Some((l, c)) => (l, Some(c)),
        None => (spec, None),
    };
    let line: usize = line_s.parse().ok()?;
    if line == 0 {
        return None;
    }
    let col: usize = match col_s {
        Some(c) => c.parse().ok()?,
        None => 0,
    };
    Some(Position { line, col })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_is_a_valid_empty_request() {
        let cli = parse(&argv(&[])).expect("parses");
        assert!(cli.is_empty());
        assert!(cli.files.is_empty());
    }

    #[test]
    fn positional_files_only() {
        let cli = parse(&argv(&["a.txt", "b.txt"])).expect("parses");
        assert_eq!(
            cli.files,
            vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]
        );
        assert!(!cli.readonly);
    }

    #[test]
    fn repeated_file_arguments_are_kept_in_order() {
        let cli = parse(&argv(&["c.txt", "a.txt", "b.txt"])).expect("parses");
        assert_eq!(cli.files.len(), 3);
        assert_eq!(cli.files[0], PathBuf::from("c.txt"));
        assert_eq!(cli.files[2], PathBuf::from("b.txt"));
    }

    #[test]
    fn long_and_short_flags_agree() {
        let long = parse(&argv(&["--readonly"])).expect("parses");
        let short = parse(&argv(&["-r"])).expect("parses");
        assert_eq!(long, short);
        assert!(long.readonly);
    }

    #[test]
    fn bundled_short_flags() {
        // `-r` is readonly; `--fresh` has no short form in spec §6.13, so it stays long.
        let cli = parse(&argv(&["-rv"])).expect_err("`-v` inside a bundle still stops");
        assert!(cli.informational);
        // A bundle of one real short plus a positional is not a bundle at all.
        let cli = parse(&argv(&["-r", "f.txt"])).expect("parses");
        assert!(cli.readonly);
        assert_eq!(cli.files.len(), 1);
    }

    #[test]
    fn bundling_a_short_that_takes_a_value_consumes_the_rest() {
        // `-rl rust` would read "l rust" as the language, so `-l` must be last in a bundle.
        let cli = parse(&argv(&["-rlrust"])).expect("parses");
        assert!(cli.readonly);
        assert_eq!(cli.lang.as_deref(), Some("rust"));
    }

    #[test]
    fn fresh_has_no_short_form() {
        let err = parse(&argv(&["-f"])).expect_err("`-f` is not in spec §6.13");
        assert_eq!(err.code, 2);
    }

    #[test]
    fn value_options_take_the_next_argument() {
        let cli = parse(&argv(&[
            "-l",
            "rust",
            "-e",
            "utf-16",
            "--config",
            "/tmp/c.toml",
        ]))
        .expect("parses");
        assert_eq!(cli.lang.as_deref(), Some("rust"));
        assert_eq!(cli.encoding.as_deref(), Some("utf-16"));
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/c.toml")));
    }

    #[test]
    fn value_options_accept_the_inline_form() {
        let cli = parse(&argv(&[
            "--lang=rust",
            "--encoding=utf-8",
            "--config=/x.toml",
        ]))
        .expect("parses");
        assert_eq!(cli.lang.as_deref(), Some("rust"));
        assert_eq!(cli.encoding.as_deref(), Some("utf-8"));
        assert_eq!(cli.config, Some(PathBuf::from("/x.toml")));
    }

    #[test]
    fn short_value_may_be_glued_to_the_flag() {
        let cli = parse(&argv(&["-lrust"])).expect("parses");
        assert_eq!(cli.lang.as_deref(), Some("rust"));
    }

    #[test]
    fn flag_order_does_not_matter() {
        let a = parse(&argv(&["--fresh", "-r", "f.txt", "--line", "3"])).expect("parses");
        let b = parse(&argv(&["f.txt", "--line", "3", "--fresh", "-r"])).expect("parses");
        assert_eq!(a, b);
    }

    #[test]
    fn line_accepts_line_only() {
        let cli = parse(&argv(&["--line", "42"])).expect("parses");
        assert_eq!(cli.position, Some(Position { line: 42, col: 0 }));
    }

    #[test]
    fn line_accepts_line_and_column() {
        let cli = parse(&argv(&["--line", "5:3"])).expect("parses");
        assert_eq!(cli.position, Some(Position { line: 5, col: 3 }));
    }

    #[test]
    fn line_rejects_junk_without_panicking() {
        for bad in [
            "bad", "", "0", "0:1", "-3", "3:", "3:x", "a:b", "3:4:5", " 3 x ",
        ] {
            let r = std::panic::catch_unwind(|| parse(&argv(&["--line", bad])));
            let outcome = match r {
                Ok(v) => v,
                Err(_) => panic!("parse(--line {bad:?}) panicked"),
            };
            let err = outcome.expect_err("{bad:?} should be a usage error");
            assert_eq!(err.code, 2, "{bad:?}");
            assert!(!err.informational, "{bad:?}");
            assert!(
                err.message.contains("--line"),
                "{bad:?} got {}",
                err.message
            );
        }
    }

    #[test]
    fn line_accepts_large_values() {
        let cli = parse(&argv(&["--line", "1000000:25"])).expect("parses");
        assert_eq!(
            cli.position,
            Some(Position {
                line: 1_000_000,
                col: 25
            })
        );
    }

    #[test]
    fn unknown_long_option_is_a_usage_error() {
        let err = parse(&argv(&["--nope"])).expect_err("should fail");
        assert_eq!(err.code, 2);
        assert!(err.message.contains("--nope"), "{}", err.message);
    }

    #[test]
    fn unknown_short_option_is_a_usage_error() {
        let err = parse(&argv(&["-z"])).expect_err("should fail");
        assert_eq!(err.code, 2);
        assert!(err.message.contains("-z"), "{}", err.message);
    }

    #[test]
    fn missing_option_value_is_a_usage_error() {
        for flag in ["--lang", "--encoding", "--config", "--line", "-l", "-e"] {
            let err = parse(&argv(&[flag])).expect_err("should fail");
            assert_eq!(err.code, 2, "{flag}");
            assert!(
                err.message.contains("needs a value"),
                "{flag} got {}",
                err.message
            );
        }
    }

    #[test]
    fn value_flags_reject_an_inline_value() {
        let err = parse(&argv(&["--readonly=yes"])).expect_err("should fail");
        assert_eq!(err.code, 2);
        assert!(
            err.message.contains("does not take a value"),
            "{}",
            err.message
        );
    }

    #[test]
    fn help_is_informational_and_exit_zero() {
        for flag in ["-h", "--help"] {
            let err = parse(&argv(&[flag])).expect_err("help is a stop signal");
            assert!(err.informational, "{flag}");
            assert_eq!(err.code, 0, "{flag}");
            assert!(err.message.contains("USAGE:"), "{flag}");
            assert!(err.message.contains("--readonly"), "{flag}");
        }
    }

    #[test]
    fn version_is_informational_and_exit_zero() {
        for flag in ["-v", "--version"] {
            let err = parse(&argv(&[flag])).expect_err("version is a stop signal");
            assert!(err.informational, "{flag}");
            assert_eq!(err.code, 0, "{flag}");
            assert!(
                err.message.starts_with("npt "),
                "{flag} got {}",
                err.message
            );
        }
    }

    #[test]
    fn help_wins_over_later_errors() {
        // `-h` short-circuits: printing help must not be blocked by a bad flag after it.
        let err = parse(&argv(&["-h", "--bogus"])).expect_err("help");
        assert!(err.informational);
    }

    #[test]
    fn double_dash_makes_everything_a_file() {
        let cli = parse(&argv(&["--", "--weird.txt", "-r", "--"])).expect("parses");
        assert_eq!(cli.files.len(), 3);
        assert_eq!(cli.files[0], PathBuf::from("--weird.txt"));
        assert_eq!(cli.files[1], PathBuf::from("-r"));
        assert!(!cli.readonly, "`-r` after `--` is a filename");
    }

    #[test]
    fn bare_dash_is_rejected_not_silently_ignored() {
        let err = parse(&argv(&["-"])).expect_err("should fail");
        assert_eq!(err.code, 2);
    }

    #[test]
    fn empty_option_name_is_rejected() {
        let err = parse(&argv(&["--=x"])).expect_err("should fail");
        assert_eq!(err.code, 2);
    }

    #[test]
    fn values_that_look_like_flags_are_still_values() {
        // `npt --lang --fresh` is a mistake, but `npt --line=-3` must not be read as a flag.
        let cli = parse(&argv(&["--lang=-x"])).expect("parses");
        assert_eq!(cli.lang.as_deref(), Some("-x"));
    }

    #[test]
    fn full_combination() {
        let cli = parse(&argv(&[
            "-r",
            "--lang",
            "python",
            "-e",
            "utf-8",
            "--line",
            "10:4",
            "--config",
            "/tmp/n.toml",
            "--fresh",
            "a.py",
            "b.py",
        ]))
        .expect("parses");
        assert!(cli.readonly);
        assert!(cli.fresh);
        assert_eq!(cli.lang.as_deref(), Some("python"));
        assert_eq!(cli.encoding.as_deref(), Some("utf-8"));
        assert_eq!(cli.position, Some(Position { line: 10, col: 4 }));
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/n.toml")));
        assert_eq!(cli.files.len(), 2);
    }

    #[test]
    fn non_utf8_arguments_do_not_panic() {
        // Real filenames can be arbitrary bytes; OsString to String lossy conversion is the
        // documented trade-off, and a lossy path still opens something sensible.
        use std::ffi::OsString;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let raw = OsString::from_vec(vec![b'a', 0xff, b'.', b't', b'x', b't']);
            let s = raw.to_string_lossy().to_string();
            let cli = parse(&[s]).expect("parses lossily");
            assert_eq!(cli.files.len(), 1);
        }
    }

    #[test]
    fn every_flag_in_help_is_a_real_flag() {
        // Guards against documenting an option that the parser then rejects.
        for flag in [
            "--readonly",
            "--lang",
            "--encoding",
            "--line",
            "--config",
            "--fresh",
            "--version",
            "--help",
            "-r",
            "-l",
            "-e",
            "-v",
            "-h",
        ] {
            let probe = match flag {
                "--lang" | "-l" => vec![flag.to_string(), "x".into()],
                "--encoding" | "-e" => vec![flag.to_string(), "x".into()],
                "--config" => vec![flag.to_string(), "/tmp/x".into()],
                "--line" => vec![flag.to_string(), "1".into()],
                _ => vec![flag.to_string()],
            };
            let r = parse(&probe);
            // -h/-v stop with an informational error; everything else parses.
            match r {
                Ok(_) => {}
                Err(e) => assert!(e.informational, "{flag} rejected: {}", e.message),
            }
        }
    }
}
