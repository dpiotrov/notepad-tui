# Notepad-TUI (`npt`)

A text editor for the Linux terminal: multi-tab editing, syntax highlighting,
find/replace and Find in Files, column selection, bookmarks, code folding, encodings, and
complete mouse support. Written in Rust with [ratatui](https://docs.rs/ratatui) and
[crossterm](https://docs.rs/crossterm).

Created by **Dpiotrov and Inorganic Beings**. Released under the
[MIT license](LICENSE).

## Build

```sh
cargo build --release      # binary at target/release/npt
./check.sh                 # fmt + clippy + tests + release build + crate purity
```

**No C toolchain is required.** Nothing in the dependency tree compiles C, and no
`cc`, `cmake`, `pkg-config`, `openssl`, or X11 development headers are needed.
`syntect` is built with `default-features = false` and `features = ["default-fancy"]`,
which selects the pure-Rust `fancy-regex` backend instead of `onig`; `check.sh` fails
the build if `onig` ever reappears in `cargo tree`. The clipboard uses `arboard`'s
pure-Rust path and is constructed lazily on first use, so a machine with no display
server, no Wayland, and no X11 edits normally and simply has nowhere to paste to.

Rust 1.81+ (pinned to 1.98.0 via `rust-toolchain.toml`).

## Run

```sh
npt                     # restore last session
npt file.rs             # open a file
npt a.rs b.rs           # several files, one tab each
npt --line 42:7 main.rs # open, jumping to line 42, column 7
npt -e windows-1251 log.txt
npt -l rust snippet.txt # force syntax language
npt --readonly big.log
npt --fresh             # ignore the saved session
npt -- --help.txt       # a file whose name starts with '-'
```

A file that does not exist opens as a new buffer at that path, which is how you create
a file. Missing directories are reported on save, not on open.

### CLI

Matches spec §6.13 exactly; `npt --help` is generated from the same parser.

| Option | Effect |
| --- | --- |
| `-r`, `--readonly` | open files read-only |
| `-l`, `--lang <LANG>` | force syntax language (first file) |
| `-e`, `--encoding <ENC>` | force encoding (first file) |
| `--line N[:COL]` | jump to position in first file (1-based line) |
| `--config <PATH>` | alternative config file |
| `--fresh` | do not restore the previous session |
| `-v`, `--version` | print version and exit |
| `-h`, `--help` | print help and exit |

### Session restore

On exit, open tabs, cursor positions, scroll offsets, bookmarks, fold ranges, language
overrides, read-only flags, and the recent-files list are written to `session.toml`
in the data directory, and restored on the next start. `--fresh` skips the restore; the
saved session is left on disk either way. `recent = [...]` and `active_tab` are also
honoured when edited by hand. A half-written `session.toml` never blocks startup: it is
replaced rather than refused.

## Keys

Generated from the live `Keymap::default_bindings()` table, so this list is the actual
default binding set rather than a hand-copied approximation. Every action is also
reachable from the menu bar (`F10`) or the command palette (`Ctrl+Shift+P`), and
`Shift+F1` shows the same set in-app.

| Key | Action |
| --- | --- |
| `Ctrl+N` | new file |
| `Ctrl+O` | open file |
| `Ctrl+S` | save |
| `Ctrl+Shift+S` | save as |
| `Ctrl+W` | close tab |
| `Ctrl+Q` | exit |
| `Ctrl+Shift+R` | reload file |
| `Ctrl+Z` | undo |
| `Ctrl+Y, Ctrl+Shift+Z` | redo |
| `Ctrl+X` | cut |
| `Ctrl+C` | copy |
| `Ctrl+V` | paste |
| `Ctrl+A` | select all |
| `Del` | delete |
| `Ctrl+F` | find |
| `F3` | find next |
| `Shift+F3` | find prev |
| `Ctrl+H` | replace |
| `Ctrl+F3` | find word next |
| `Ctrl+Shift+F3` | find word prev |
| `Ctrl+Shift+F` | find in files |
| `Ctrl+G` | go to line |
| `Ctrl+Shift+A` | replace all |
| `Ctrl+F2` | toggle bookmark |
| `F2` | next bookmark |
| `Shift+F2` | prev bookmark |
| `Ctrl+Shift+F2` | clear bookmarks |
| `Ctrl+D` | duplicate line |
| `Ctrl+L` | delete line |
| `Ctrl+Shift+Up` | move line up |
| `Ctrl+Shift+Down` | move line down |
| `Alt+Shift+S` | sort lines asc |
| `Alt+Shift+R` | sort lines desc |
| `Ctrl+Shift+U` | upper case |
| `Ctrl+U` | lower case |
| `Ctrl+Alt+U` | title case |
| `Ctrl+Shift+Q` | toggle comment |
| `F5` | insert date time |
| `Left` | move left |
| `Right` | move right |
| `Up` | move up |
| `Down` | move down |
| `Shift+Left` | move left select |
| `Shift+Right` | move right select |
| `Shift+Up` | move up select |
| `Shift+Down` | move down select |
| `Home` | line start |
| `End` | line end |
| `Shift+Home` | line start select |
| `Shift+End` | line end select |
| `Ctrl+Left` | word left |
| `Ctrl+Right` | word right |
| `Ctrl+Shift+Left` | word left select |
| `Ctrl+Shift+Right` | word right select |
| `PageUp, Ctrl+Home` | scroll page up |
| `PageDown, Ctrl+End` | scroll page down |
| `Tab` | indent |
| `Shift+Tab` | outdent |
| `Alt+Z` | toggle word wrap |
| `Alt+Shift+N` | toggle line numbers |
| `Alt+Shift+W` | toggle whitespace |
| `Alt+Shift+E` | toggle eol marks |
| `Alt+0` | fold all |
| `Alt+9` | unfold all |
| `Alt+1` | toggle fold |
| `Ctrl+Tab, Ctrl+PageDown` | next tab |
| `Ctrl+Shift+Tab, Ctrl+PageUp` | prev tab |
| `Ctrl+Alt+W` | close other tabs |
| `Ctrl+Shift+W` | close all tabs |
| `Ctrl+T` | reopen closed tab |
| `Ctrl+E` | recent files |
| `Ctrl+Shift+L` | language picker |
| `Ctrl+Shift+T` | theme picker |
| `Alt+Shift+U` | encoding menu |
| `Alt+Shift+D` | eol menu |
| `Alt+Shift+C` | convert encoding |
| `Alt+Shift+O` | toggle readonly |
| `Ctrl+Shift+C` | copy path |
| `Ctrl+Shift+F12` | copy filename |
| `Ctrl+,` | preferences |
| `Ctrl+Shift+P` | command palette |
| `Shift+F1` | help overlay |
| `F1` | about |
| `Ins` | toggle overtype |
| `F10` | menu file |

Menu accelerators (`Alt+F`, `Alt+E`, `Alt+S`, `Alt+V`, ...) are derived from the menu
definitions at startup rather than listed, so adding a menu cannot forget one. `Alt+Q`
is both "exit" and the Edit-menu accelerator by design; the conflict is resolved in
favour of the menu, as spec §6.10 requires.

Two chords are worth calling out because they are easy to guess wrong:

- **`F5` inserts the current date and time.** There is no Run dialog, so nothing else is
  shadowed by it.
- **`Ctrl+Q` exits**, and only when nothing is modified. Toggle-comment lives on
  `Ctrl+Shift+Q` and in the Edit menu, which keeps exit from destroying unsaved work.
- **Zoom** (`Ctrl++` / `Ctrl+-`) is not implemented. A terminal editor cannot resize
  its own font reliably; the terminal emulator owns zoom.
- **`Ctrl+Shift+Tab` switches to the previous tab**, as §6.10 states, so the document
  list is available from the View menu only.

## Mouse

Mouse support is full (selection, drag, gutter clicks, wheel scrolling, scrollbar
dragging, context menu, tab clicks), but the terminal has to hand the events over:

```sh
# tmux: mouse events otherwise never reach the program
set -g mouse on
```

Inside tmux with `mouse on`, hold **Shift** while dragging to use the terminal's native
selection and copy instead. Over **SSH**, the system clipboard is on the wrong machine;
set `clipboard = "osc52"` in `config.toml` to copy through the terminal protocol:

```toml
[ui]
clipboard = "osc52"   # auto | osc52 | none
```

`auto` tries the system clipboard first and falls back to OSC 52. OSC 52 also needs the
terminal emulator to accept it (alacritty, kitty, and WezTerm do by default; xterm needs
`allowWindowOps`).

## Quitting, and getting your terminal back

`Ctrl+Q` or File > Exit quits normally. If a document has unsaved changes, the prompt asks;
all three buttons answer it by mouse or keyboard (`1`/`2`/`3`, or arrows and Enter).

If the editor looks wedged, signal it and it will put your terminal back before it goes:

```sh
kill <pid>        # SIGTERM -- orderly exit, terminal restored
kill -HUP <pid>   # closing the terminal window -- same
```

`Ctrl+C` does the same. Send a second signal if the first is ignored: the first asks the
editor to shut down properly, the second restores the terminal and exits immediately, so an
insistent `kill` always lands.

`SIGKILL` cannot be caught by any program, so `kill -9` will still leave your shell in raw
mode. If the console starts printing `^[[?...` garbage and ignores what you type:

```sh
reset        # or, lighter:
stty sane
```

## Configuration

`~/.config/npt/config.toml`, created with defaults on first run; `--config` points
elsewhere. Config honors `$XDG_CONFIG_HOME`, and the session and log honor
`$XDG_DATA_HOME`:

| File | Default location |
| --- | --- |
| config | `~/.config/npt/config.toml` |
| session | `~/.local/share/npt/session.toml` |
| log | `~/.local/share/npt/npt.log` |

Every key and its default is in spec §7.1. A missing config is the normal first-run
state, not a warning; a bad value degrades to the default for that one item and the rest
of the file still applies. The knobs users reach for most often:

| Key | Default | Meaning |
| --- | --- | --- |
| `[editor] tab_width` | `4` | tab stop width and indent size |
| `[editor] insert_spaces` | `true` | Tab inserts spaces |
| `[editor] default_eol` | `"lf"` | encoding for new files |
| `[editor] eol_mode` | `"file"` | `file` preserves, or force `lf`/`crlf`/`cr` on save |
| `[editor] backup_on_save` | `false` | keep a `.bak` of the previous contents |
| `[editor] autosave_seconds` | `0` | `0` disables periodic autosave |
| `[ui] theme` | `"base16-ocean.dark"` | any theme name, e.g. `Dracula`, `Monokai`, `One Dark`, `Nord`, `Solarized Osaka (dark)`, `GitHub Light` |
| `[ui] word_wrap` | `false` | wrap long lines |
| `[ui] clipboard` | `"auto"` | `auto` / `osc52` / `none` |
| `[ui] mouse_scroll_lines` | `3` | wheel click distance |
| `[find] regex` | `false` | `find` uses regular expressions |
| `[find] wrap_around` | `true` | search continues past the ends |

`[keymap]` overrides any action by its kebab-case name, and one action accepts several
chords:

```toml
[keymap]
save = ["ctrl+s"]
find = ["ctrl+f", "f6"]
```

Unknown action names and unparseable chords are logged at startup rather than
silently dropped, and a malformed entry is ignored instead of guessed at.

## Features

Implemented: tabs with modified markers, close-others/close-all/reopen-closed, recent
files, read-only mode, encoding display and conversion, EOL display and conversion,
go-to-line, bookmarks with next/prev/clear, code folding with fold-all/unfold-all,
duplicate/move/delete line, sort lines, upper/lower/title case, toggle comment, insert
date/time, overtype mode, column (block) selection via `Alt`+mouse drag,
find/replace with regex, case, whole-word and wrap-around, Find in Files with
cancellable background search, background loading of very large files, word wrap,
whitespace and EOL markers, right margin,
13 syntax themes including Dracula, Monokai, One Dark, Nord and GitHub Light, command
palette, and a menu bar.

The theme controls every color on screen, including the editor chrome -- menu bar, tab bar,
status bar, service lines, dialogs and scrollbars. Nothing is left to the terminal's palette,
so switching your emulator between a dark and a light scheme changes only the colors you asked
it to. Picking a theme in the Themes dialog saves it; the next start opens on it.

On a light theme the text area's ground is softened away from pure white, and only as far as the
theme's own text stays comfortably readable. A light theme already near that limit, and every dark
theme, keeps the ground its author chose.

Band separation -- menu bar, tab bar, current line, focused field -- is tuned to read the same on
every theme rather than to a fixed color offset, because a fixed offset is worth far less near white
than near black. Where a theme's own colors cannot carry a surface npt derives, the ink is deepened
slightly; hue is never changed, and a theme with room to spare keeps its colors exactly.

Syntax colors are checked against the ground they are painted on. Some themes ship accents that are
barely visible on their own text area -- a light theme carrying accents chosen for a dark one -- and
those are darkened or lightened by a single amount for the whole palette, so the colors stay
distinguishable from each other rather than all converging on one weight. A dark theme whose
background is too light for its own palette is deepened slightly. Themes that already read well are
left exactly as their author wrote them.

Not implemented, and why: macro recording and playback, the plugin system, Run/
Document-Map/Function-List panels, and column-mode regex transform. The spec scopes
these out.

## Layout

```
LICENSE                 MIT license
README.md               this file
crates/np-core/         engine: buffer, undo, search, encoding, config, session (headless)
crates/np-tui/          binary: terminal, rendering, input, dialogs
```

`np-core` has no dependency on ratatui, crossterm, arboard, or anyhow, and `check.sh`
fails the build if that ever changes. The engine is fully testable without a terminal.

## Building from source

The toolchain is pinned in `rust-toolchain.toml`, so a plain clone builds with no setup
beyond a working Rust install:

```sh
git clone https://github.com/dpiotrov/notepad-tui
cd notepad-tui
cargo build --release
./target/release/npt
```

Optionally install it where your `PATH` points:

```sh
cargo install --path crates/np-tui
```

## Contributing

The quickest path to an accepted change is:

```sh
cargo fmt --all
./check.sh
```

`check.sh` is the whole quality gate -- formatting, `clippy -D warnings`, the test suite,
a release build, and the rule that `np-core` stays free of ratatui, crossterm, arboard and
anyhow. GitHub Actions runs the same script, so a red `check.sh` is a red CI.

## License

MIT. See [LICENSE](LICENSE).

Copyright (c) 2026 dpiotrov and inorganic beings.

### Color schemes

The bundled color schemes are published palettes reproduced from their own projects:
Dracula (dracula.io), Monokai (Wimer Hazenberg), One Dark (the Atom editor), Nord
(Arctic Ice Studio), a Solarized-revision dark, and GitHub Light. Only the color values
are used; each is written out as a plain TextMate `.tmTheme` by `tools/gen_themes.py`,
which is committed so the palettes are reviewable as data rather than as markup.

These color schemes are conventionally released under the MIT license, and the projects
named above do so. Add a `LICENSE` file beside any asset here if you redistribute a
scheme under different terms.

### Third-party grammar

CMake syntax highlighting uses the grammar from
[twxs/vs.language.cmake](https://github.com/twxs/vs.language.cmake), MIT licensed,
Copyright (c) 2015 Nicolas MARTIN. Its upstream text is retained in
[crates/np-core/assets/CMakeGrammar.LICENSE](crates/np-core/assets/CMakeGrammar.LICENSE).
The grammar is redistributed in converted form.
