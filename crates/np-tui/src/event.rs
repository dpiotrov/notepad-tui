//! The event loop (spec §5.1).
//!
//! # Split so it can be tested
//!
//! [`handle_event`] is a pure function of `(App, Event)`: no polling, no drawing, no clock
//! of its own. Every behavior the loop is supposed to have — Resize marks dirty, Paste
//! inserts once, an interrupted read does not kill the editor — is asserted against this
//! function with synthetic events. [`run`] is the thin shell that polls a real terminal and
//! calls it.
//!
//! # Draw only when dirty
//!
//! Spec §5.4. Drawing is skipped when `App::dirty` is false, which is what keeps a 100 ms
//! poll interval from costing CPU when nothing is happening. A burst of keypresses within
//! one poll still produces one frame, because all events readable without blocking are
//! drained before the draw.
//!
//! # Interrupt safety
//!
//! `poll` and `read` return `io::ErrorKind::Interrupted` when a signal arrives. That is
//! routine (SIGWINCH on some terminals, SIGINT from a user who means something else) and
//! must not terminate the editor, so it is treated as "nothing to do this tick". A hard I/O
//! error is different: stdout is gone, and continuing would spin.
//!
//! # Batch semantics
//!
//! `App::begin_batch` is called once per tick, not once per event, so that a burst of
//! typing shares one clock reading and coalesces into one undo group (FR-E4).

use std::io::{ErrorKind, Result as IoResult};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, MouseEvent};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::app::{App, Mode};

/// How long to wait for an event before checking other work.
///
/// 100 ms per the stage spec. It is also the ceiling on how long `drain_background` can be
/// delayed by an idle terminal, which matters for the S11 progress updates.
pub const POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Smallest terminal we render normally. Below this the layout has no room for the chrome
/// plus a usable text area, so a dedicated message replaces it (spec §8).
pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 8;

/// What the loop should do after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopSignal {
    /// Keep going.
    Continue,
    /// The user asked to quit; the loop exits after this batch.
    Quit,
}

/// Apply one event to `app`.
///
/// Pure: it never draws, never reads the clock beyond `App::begin_batch`, and never touches
/// the terminal. This is the function the tests drive.
pub fn handle_event(app: &mut App, event: &Event) -> LoopSignal {
    match event {
        Event::Key(key) => {
            // handle_key owns the mode-first routing, the Press filter and the
            // character fallback. Going through anything else would lose those.
            app.handle_key(*key);
            signal(app)
        }
        Event::Mouse(mouse) => {
            handle_mouse(app, *mouse);
            signal(app)
        }
        Event::Resize(width, height) => {
            // Recompute geometry so hit-testing and the next frame agree, then mark the
            // whole screen dirty: every cell may have moved.
            app.recompute_geometry(*width, *height);
            app.mark_all_dirty();
            LoopSignal::Continue
        }
        Event::Paste(text) => {
            paste_text(app, text);
            signal(app)
        }
        // FocusGained / FocusLost: nothing depends on focus yet. Marking dirty would cost a
        // redraw for no visual change, so they are ignored outright.
        Event::FocusGained | Event::FocusLost => LoopSignal::Continue,
    }
}

fn signal(app: &App) -> LoopSignal {
    if app.quit_requested {
        LoopSignal::Quit
    } else {
        LoopSignal::Continue
    }
}

/// Mouse input.
///
/// S6 replaces this with the routing table in `np-tui::mouse`. Until then the events are
/// accepted and dropped, which keeps the loop honest: an unhandled mouse event must not be
/// mistaken for an error, and the wiring is already in place so S6 only fills the body.
fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // The routing decision itself is `mouse::route`, which is pure and tested without a
    // terminal. This only supplies the batch clock and applies the result, so a mouse event
    // has exactly one mutation path.
    let now = app.clock;
    app.handle_mouse_at(&mouse, now);
}

/// Insert pasted text as ONE undo unit (spec §5.1, §8).
///
/// A huge paste must be a single undo step; anything else turns one Ctrl+Z into a slow
/// animation. The chunked-with-progress path for 10 MB pastes is S7's; the undo grouping is
/// decided here so it is already right.
fn paste_text(app: &mut App, text: &str) {
    if text.is_empty() {
        return;
    }
    let now = app.clock;
    let Some(doc) = app.doc_mut() else {
        return;
    };
    // UndoGroup::Atomic: a paste is one user intent regardless of length.
    if let Err(err) = doc.insert_text(text, np_core::UndoGroup::Atomic, now) {
        app.push_toast(err.user_message(), true);
        return;
    }
    app.dirty = true;
}

/// Public entry for the paste path, so S7's clipboard and the tests can call it directly.
pub fn paste_into(app: &mut App, text: &str) {
    paste_text(app, text);
}

/// Background work hook. S11 (Find in Files) drains progress and results here.
///
/// Called once per tick with a time budget so a long queue cannot starve input.
pub fn drain_background(app: &mut App, budget: Duration) {
    // S11: drain app.fif results / loader messages, bounded by `budget`.
    let _ = (app, budget);
}

/// The result of a tick, used by [`run`] and by tests of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Nothing happened (poll timed out).
    Idle,
    /// One event was handled.
    Handled(LoopSignal),
    /// A read was interrupted by a signal; not an error.
    Interrupted,
    /// The terminal is gone. The loop must stop.
    Dead,
}

/// One poll/read/handle cycle. Testable: a caller can drive it with a fake source.
///
/// Background messages are drained once per tick, *before* the poll, rather than only when an
/// event arrives. A search delivering results while the user is not touching the keyboard is
/// the normal case, and draining only on input would leave the panel frozen at "searching"
/// until they pressed something. The budget bounds the work either way, so an idle tick cannot
/// stall: `POLL_TIMEOUT` is 100 ms and a full budget of appends is microseconds, which is how
/// the responsiveness claim in spec §9 holds.
pub fn try_tick(app: &mut App) -> Tick {
    crate::fif::drain(app, crate::fif::DRAIN_BUDGET);
    // Toast expiry runs next to the background drain, on the same tick. It has to happen even
    // when the terminal is idle -- a notification that only disappears when the user presses a
    // key is a notification that never disappears -- and it must happen *before* the poll so a
    // toast expiring during a 100 ms wait is gone on the frame after the wait rather than the
    // one after that. The clock is advanced first because the wait itself consumes time.
    app.begin_batch();
    app.expire_toasts();
    match wait_for_input(POLL_TIMEOUT) {
        Err(err) if err.kind() == ErrorKind::Interrupted => return Tick::Interrupted,
        Err(_) => return Tick::Dead,
        Ok(false) => return Tick::Idle,
        // A signal woke us. Consume the wake byte and report an idle tick; the loop's quit
        // check on the next iteration reads `signal::terminated()` and stops.
        Ok(true) if crate::signal::terminated() => {
            crate::signal::drain_wake();
            return Tick::Idle;
        }
        Ok(true) => {}
    }
    match read() {
        Err(err) if err.kind() == ErrorKind::Interrupted => Tick::Interrupted,
        Err(_) => Tick::Dead,
        Ok(event) => {
            app.begin_batch();
            Tick::Handled(handle_event(app, &event))
        }
    }
}

/// Wait for terminal input, or for a termination signal, up to `timeout`.
///
/// crossterm's `poll` watches the tty only. A signal that arrives while the editor is idle
/// does make the underlying `poll(2)` return `EINTR`, but crossterm retries it internally,
/// so the wake-up never reaches us -- and a handler that merely sets a flag would sit there
/// until the next keypress. The self-pipe in [`crate::signal`] turns the signal into
/// readable fd, and polling both descriptors is what makes "kill the editor" take effect
/// immediately instead of on the next keystroke.
///
/// `Ok(true)` means something is readable, which includes the wake byte: a caller that needs
/// to tell the two apart checks [`crate::signal::terminated`].
fn wait_for_input(timeout: Duration) -> IoResult<bool> {
    let Some(wake) = crate::signal::wake_fd() else {
        // No pipe (install never ran, or could not create one): the original behaviour.
        return poll(timeout);
    };
    match poll(timeout) {
        Ok(true) => return Ok(true),
        Err(err) if err.kind() == ErrorKind::Interrupted => {}
        Err(err) => return Err(err),
        Ok(false) => {}
    }
    // The poll produced nothing usable. Either a signal arrived -- announced by the wake
    // descriptor becoming readable, or by the flag alone if the byte has not landed yet --
    // or the timeout genuinely expired.
    if fd_is_readable(wake) || crate::signal::terminated() {
        return Ok(true);
    }
    // Nothing this tick. The next tick re-checks the flag, so the worst case a signal waits
    // for is one poll timeout -- 100 ms, which is imperceptible and far better than the
    // "nothing until the next keystroke" this function replaced.
    Ok(false)
}

/// One non-blocking `poll(2)` on a single descriptor.
fn fd_is_readable(fd: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    rc > 0 && (pfd.revents & libc::POLLIN) != 0
}

// Indirection over crossterm so a test could substitute a source if needed later, while the
// production path stays a direct call.
fn poll(timeout: Duration) -> IoResult<bool> {
    event::poll(timeout)
}

fn read() -> IoResult<Event> {
    event::read()
}

/// Draw `app` if it is dirty.
///
/// Split out so the "only when dirty" rule has one home and one test.
///
/// The size comes from `frame.area()` inside the closure rather than a `terminal.size()`
/// pre-read: the frame is authoritative at draw time, and a pre-read would need error
/// handling for a value that is then thrown away (Backend::Error is not guaranteed to be a
/// `std::error::Error`, so `anyhow::Context` does not even apply to it).
pub fn draw_if_dirty<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<bool> {
    if !app.dirty {
        return Ok(false);
    }
    terminal
        .draw(|frame| {
            let w = frame.area().width;
            let h = frame.area().height;
            if is_too_small(w, h) {
                draw_too_small(frame.buffer_mut(), w, h, &app.palette());
                return;
            }
            // Keep App's geometry in step with the real terminal before painting.
            app.recompute_geometry(w, h);
            crate::ui::draw(app, frame.buffer_mut());
            // `terminal_cursor`, not `caret_position`. The helper that decides which widget owns
            // the terminal's one cursor was added in S20 and this line kept calling the editor
            // caret directly, so the strips still showed no caret and the whole fix was dead code.
            if let Some(pos) = terminal_cursor(app) {
                frame.set_cursor_position(pos);
            }
        })
        .map_err(|err| anyhow::anyhow!("draw failed: {err}"))?;
    app.dirty = false;
    Ok(true)
}

/// Where the terminal's own cursor goes for this frame.
///
/// The terminal has exactly one real cursor. It used to be pointed at the document caret and
/// nowhere else, which is right while the editor has the keyboard and wrong for every chrome
/// mode that takes it: the caret sits in the file the user is not looking at, while the field
/// they are typing into shows no cursor at all. Both search strips were reported that way --
/// "cursor dont appear in search string field".
///
/// The rule is whichever widget currently owns the keyboard, asked in order, and the editor is
/// the fallback. A chrome mode that reports no position (a toggle or action word focused, or a
/// results list being driven by arrows) deliberately leaves nothing on screen rather than
/// inventing a text position on a word that is not a field.
pub fn terminal_cursor(app: &App) -> Option<(u16, u16)> {
    if crate::ui::find_bar::is_open(app) {
        return crate::ui::find_bar::cursor_position(app);
    }
    if matches!(app.mode(), Mode::FindPanel) {
        return crate::ui::find_in_files::cursor_position(app);
    }
    let pos = crate::ui::editor_view::caret_position(app)?;
    Some((pos.col as u16, pos.line as u16))
}

/// True when the terminal cannot host the normal layout.
pub fn is_too_small(width: u16, height: u16) -> bool {
    width < MIN_WIDTH || height < MIN_HEIGHT
}

/// The "terminal too small" screen (spec §8).
///
/// Rendered instead of the editor, and the loop keeps polling so a resize restores normal
/// drawing without any user action. Crashing here would be absurd: the user can simply make
/// the window bigger.
pub fn draw_too_small(
    buf: &mut ratatui::buffer::Buffer,
    width: u16,
    height: u16,
    palette: &np_core::ThemePalette,
) {
    // Both channels come from the theme, like every other surface. This screen is drawn when the
    // window is too small for the editor, which is exactly when the user is most likely to be
    // looking at a terminal they have just resized -- and possibly restyled. Text that names no
    // background here would sit on the emulator's default and can land on any color at all.
    let style = crate::style::ink(palette.foreground, palette.background);
    let msg = "Terminal too small";
    // ASCII only and short: at 20 columns an em dash or a long sentence gets clipped
    // mid-word, which reads as a bug. The size lines carry the information that matters.
    let sizes = format!("{width}x{height}");
    let need = format!("min {MIN_WIDTH}x{MIN_HEIGHT}");
    let help = "Resize to continue";

    // Fit to rows. The headline and the recovery hint outrank the diagnostics: with a fixed
    // ladder the hint appeared only from row 6 up, which meant a 4- or 5-row terminal was
    // told it was too small with nothing on screen saying what to do about it. So the hint is
    // kept as long as there are two rows, and the current size is the first thing dropped.
    let mut lines: Vec<String> = vec![msg.to_string()];
    match height {
        0 => {}
        1 => lines.push(help.to_string()),
        2 => {
            lines.push(help.to_string());
            lines.push(need);
        }
        3 => {
            lines.push(need);
            lines.push(help.to_string());
        }
        _ => {
            lines.push(sizes);
            lines.push(need);
            lines.push(help.to_string());
        }
    }

    // Center the block, then clamp it back inside the screen. Centering alone is not enough:
    // `height / 2 - len / 2` leaves the tail past the bottom edge whenever the row count and
    // the height interact badly, and the tail is the recovery hint. The clamp keeps every row
    // visible, which is always possible because `lines` never exceeds `height` rows.
    let start = (height / 2)
        .saturating_sub(lines.len() as u16 / 2)
        .min(height.saturating_sub(lines.len() as u16));
    for (i, line) in lines.iter().enumerate() {
        let y = start + i as u16;
        if y >= height {
            break;
        }
        // Skip rather than clip a line that cannot fit horizontally.
        if (line.chars().count() as u16) > width {
            continue;
        }
        crate::ui::menu_bar::draw_str(buf, centered(line, width), y, line, style);
    }
}

/// Column that centers `text` in `width`, clamped so it never wraps.
fn centered(text: &str, width: u16) -> u16 {
    let len = text.chars().count() as u16;
    width.saturating_sub(len) / 2
}

/// Run the loop against a live terminal until the user quits.
///
/// The caller owns the `TerminalGuard`; this function does not enter or leave the terminal,
/// so the guard's lifetime can wrap the loop plus any post-run file work.
///
/// A panic inside the loop propagates to the hook, which restores the terminal. Under
/// unwinding builds, [`crate::panic_hook::guard_running`] at the top of `main::run` turns it
/// into a clean exit instead.
pub fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    // Paint once before waiting, so the editor is on screen even if the first input takes a
    // while to arrive.
    draw_if_dirty(terminal, app)?;
    loop {
        // Check quit BEFORE polling. A quit can be set by anything other than a key event
        // (a signal handler, a background message in S11, or a caller that pre-sets it), and
        // if the terminal is unreadable the poll would report that instead of honoring the
        // request the user already made.
        if app.quit_requested || crate::signal::terminated() {
            // A termination signal is a request to leave, not a request to save. The loop
            // exits through the same path a quit takes, which writes the session and draws
            // the last frame, and the guard restores the terminal on the way out. A second
            // signal skips all of that -- see `signal::handle`.
            break;
        }
        match try_tick(app) {
            Tick::Dead => {
                anyhow::bail!(
                    "the terminal is no longer readable; exiting so your shell comes back"
                );
            }
            Tick::Handled(LoopSignal::Quit) => break,
            Tick::Handled(LoopSignal::Continue) | Tick::Idle | Tick::Interrupted => {}
        }
        drain_background(app, Duration::from_millis(2));
        // Timers run once per iteration, not per event: that is what bounds a burst of a
        // thousand keystrokes to a single session write instead of a thousand.
        let now = std::time::Instant::now();
        app.run_timers(now);
        draw_if_dirty(terminal, app)?;
    }
    // Session state is written on the way out as well as on the debounce, so quitting
    // immediately after a change still records it (FR-CG3). Autosave runs first so any
    // document due for a save reaches disk before the session that describes it.
    app.autosave_now();
    app.flush_session();
    // One last frame so the final state (a saved indicator, a toast) is visible before the
    // alternate screen goes away.
    let _ = draw_if_dirty(terminal, app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    };
    use np_core::Config;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        App::new_headless(Config::default(), (80, 24))
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    /// Whole buffer as one string, for "does this text appear anywhere" assertions.
    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        let area = buf.area();
        (0..area.height)
            .map(|y| crate::ui::row_text(buf, y, 0, area.width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ------------------------------------------------------------- keys

    #[test]
    fn key_event_routes_through_the_app() {
        let mut app = app();
        handle_event(&mut app, &press(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.doc().unwrap().buffer.text_string(), "a");
    }

    #[test]
    fn key_repeat_and_release_are_not_doubled() {
        let mut app = app();
        let mut ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        ev.kind = KeyEventKind::Press;
        handle_event(&mut app, &Event::Key(ev));
        ev.kind = KeyEventKind::Repeat;
        handle_event(&mut app, &Event::Key(ev));
        assert_eq!(app.doc().unwrap().buffer.text_string(), "x");
    }

    #[test]
    fn quit_key_stops_the_loop() {
        let mut app = app();
        let sig = handle_event(&mut app, &press(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert_eq!(sig, LoopSignal::Quit);
        assert!(app.quit_requested);
    }

    // ----------------------------------------------------------- resize

    #[test]
    fn resize_recomputes_geometry_and_marks_dirty() {
        let mut app = app();
        app.dirty = false;
        let sig = handle_event(&mut app, &Event::Resize(120, 40));
        assert_eq!(sig, LoopSignal::Continue);
        assert!(app.dirty, "a resize must schedule a redraw");
        assert_eq!(app.regions.status.y, 39);
        assert_eq!(app.regions.editor.h, 37);
    }

    #[test]
    fn resize_to_a_tiny_terminal_degrades_without_panicking() {
        let mut app = app();
        handle_event(&mut app, &Event::Resize(1, 1));
        assert_eq!(app.regions.editor.h, 0);
    }

    // ------------------------------------------------------------ paste

    #[test]
    fn paste_inserts_exactly_once() {
        let mut app = app();
        handle_event(&mut app, &Event::Paste("hello world".into()));
        assert_eq!(app.doc().unwrap().buffer.text_string(), "hello world");
    }

    #[test]
    fn paste_is_a_single_undo_unit() {
        let mut a = app();
        a.dirty = false;
        paste_into(&mut a, "one\ntwo\nthree");
        assert_eq!(a.doc().unwrap().undo.len(), 1, "a paste is one undo step");
        a.execute(crate::Action::Undo);
        assert_eq!(a.doc().unwrap().buffer.text_string(), "");
    }

    #[test]
    fn paste_of_nothing_does_nothing() {
        let mut app = app();
        app.dirty = false;
        let before = app.doc().unwrap().undo.len();
        handle_event(&mut app, &Event::Paste(String::new()));
        assert_eq!(app.doc().unwrap().undo.len(), before);
        assert!(!app.dirty);
    }

    #[test]
    fn paste_marks_dirty() {
        let mut app = app();
        app.dirty = false;
        handle_event(&mut app, &Event::Paste("x".into()));
        assert!(app.dirty);
    }

    #[test]
    fn paste_into_a_readonly_document_toasts() {
        let mut app = app();
        if let Some(doc) = app.doc_mut() {
            doc.readonly = true;
        }
        handle_event(&mut app, &Event::Paste("nope".into()));
        assert!(!app.toasts.is_empty(), "the failure must be visible");
        assert_eq!(app.doc().unwrap().buffer.text_string(), "");
    }

    // ------------------------------------------------------------ mouse

    #[test]
    fn mouse_events_are_accepted_and_do_not_panic() {
        let mut app = app();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
        ] {
            let ev = Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(handle_event(&mut app, &ev), LoopSignal::Continue);
        }
    }

    #[test]
    fn focus_events_are_ignored_without_a_redraw() {
        let mut app = app();
        app.dirty = false;
        assert_eq!(
            handle_event(&mut app, &Event::FocusGained),
            LoopSignal::Continue
        );
        assert_eq!(
            handle_event(&mut app, &Event::FocusLost),
            LoopSignal::Continue
        );
        assert!(!app.dirty);
    }

    // --------------------------------------------------------- minimum size

    #[test]
    fn too_small_threshold_is_forty_by_eight() {
        assert!(!is_too_small(40, 8));
        assert!(is_too_small(39, 8));
        assert!(is_too_small(40, 7));
        assert!(is_too_small(1, 1));
    }

    #[test]
    fn too_small_screen_renders_and_does_not_panic() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).expect("test terminal");
        app.dirty = true;
        let drew = draw_if_dirty(&mut terminal, &mut app).expect("draws");
        assert!(drew);
        let buf = terminal.backend().buffer().clone();
        let text = buffer_text(&buf);
        assert!(text.contains("Terminal too small"), "buffer was {text:?}");
        // The current and required sizes must both survive at this width, not be clipped.
        assert!(
            text.contains("20x5"),
            "should show the current size: {text:?}"
        );
        assert!(text.contains("40x8"), "should show the minimum: {text:?}");
    }

    #[test]
    fn normal_size_renders_the_frame_not_the_warning() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");
        app.dirty = true;
        draw_if_dirty(&mut terminal, &mut app).expect("draws");
        let buf = terminal.backend().buffer().clone();
        let row0 = crate::ui::row_text(&buf, 0, 0, 60);
        assert!(row0.contains("File"), "menu bar expected, got {row0:?}");
        let all = buffer_text(&buf);
        assert!(!all.contains("Terminal too small"));
    }

    #[test]
    fn drawing_clears_dirty_and_a_second_draw_is_a_no_op() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");
        assert!(draw_if_dirty(&mut terminal, &mut app).expect("first draws"));
        assert!(!app.dirty);
        assert!(!draw_if_dirty(&mut terminal, &mut app).expect("second skips"));
    }

    #[test]
    fn resize_then_draw_recovers_from_too_small() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).expect("test terminal");
        app.dirty = true;
        draw_if_dirty(&mut terminal, &mut app).expect("draws warning");
        // A resize both marks dirty and gives room; the next draw is the normal frame.
        handle_event(&mut app, &Event::Resize(80, 24));
        let mut big = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        assert!(draw_if_dirty(&mut big, &mut app).expect("draws frame"));
        let buf = big.backend().buffer().clone();
        assert!(crate::ui::row_text(&buf, 0, 0, 80).contains("File"));
    }

    // --------------------------------------------------------- tick / loop

    #[test]
    fn tick_on_a_dead_terminal_reports_dead() {
        // No terminal exists in a test process, so poll() fails; the loop must classify that
        // as Dead rather than spin or panic.
        let mut app = app();
        let tick = try_tick(&mut app);
        assert!(
            matches!(tick, Tick::Dead | Tick::Idle | Tick::Interrupted),
            "unexpected {tick:?}"
        );
    }

    #[test]
    fn run_reports_an_error_when_there_is_no_terminal() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");
        // TestBackend never produces crossterm events, and no real stdin is attached, so run
        // must exit with a clear error rather than hang or panic.
        let result = run(&mut terminal, &mut app);
        assert!(result.is_err(), "expected the 'terminal is gone' path");
        assert!(
            result.unwrap_err().to_string().contains("terminal"),
            "message should name the problem"
        );
    }

    #[test]
    fn run_returns_ok_when_quit_is_already_requested() {
        let mut app = app();
        app.quit_requested = true;
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");
        // The initial draw happens, then the loop's quit check fires before any read.
        assert!(run(&mut terminal, &mut app).is_ok());
    }

    #[test]
    fn drain_background_is_safe_to_call() {
        let mut app = app();
        drain_background(&mut app, Duration::from_millis(1));
    }

    #[test]
    fn centered_never_produces_a_wrapping_column() {
        assert_eq!(centered("abc", 10), 3);
        assert_eq!(centered("a much longer message than the screen", 4), 0);
        assert_eq!(centered("", 0), 0);
    }
}
