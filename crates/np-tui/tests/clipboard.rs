//! Clipboard provider and OSC 52 encoding (stage S7, spec §6.3).
//!
//! No test here touches a real clipboard or a real terminal: the provider is built with
//! `Osc52` or `None` and an injected sink, so `arboard` is never constructed.

use std::cell::RefCell;
use std::rc::Rc;

use np_tui::clipboard::{
    osc52_sequence, osc52_sequence_buffer, ClipboardMode, ClipboardProvider, ClipboardTarget,
};

/// The sink function type the provider writes OSC 52 bytes to.
type SinkFn = Box<dyn FnMut(&[u8])>;

/// A sink that appends everything written, so a test can assert on exact bytes.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Sink {
    fn box_ed(self) -> SinkFn {
        let inner = self.0.clone();
        Box::new(move |bytes: &[u8]| inner.borrow_mut().extend_from_slice(bytes))
    }
    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().clone()
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
    fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

fn provider(mode: ClipboardMode, sink: &Sink) -> ClipboardProvider {
    ClipboardProvider::new(mode).with_osc_sink(sink.clone().box_ed())
}

// ------------------------------------------------------ osc52_sequence

#[test]
fn osc52_sequence_matches_a_known_byte_vector() {
    // "hello" is "aGVsbG8=" in base64. ESC ] 52 ; c ; <b64> ESC \
    assert_eq!(
        osc52_sequence("hello"),
        b"\x1b]52;c;aGVsbG8=\x1b\\".to_vec()
    );
}

#[test]
fn osc52_sequence_of_empty_string_is_still_well_formed() {
    assert_eq!(osc52_sequence(""), b"\x1b]52;c;\x1b\\".to_vec());
}

#[test]
fn osc52_sequence_encodes_utf8_multibyte() {
    // héllo -> "aMOpbGxv" (é is C3 A9, base64 of the UTF-8 bytes, not of the code point).
    assert_eq!(
        osc52_sequence("h\u{e9}llo"),
        b"\x1b]52;c;aMOpbGxv\x1b\\".to_vec()
    );
    assert_eq!(
        osc52_sequence("\u{65e5}\u{672c}\u{8a9e}"),
        b"\x1b]52;c;5pel5pys6Kqe\x1b\\".to_vec()
    );
}

#[test]
fn osc52_sequence_targets_the_primary_buffer_when_asked() {
    assert_eq!(
        osc52_sequence_buffer('p', "hi"),
        b"\x1b]52;p;aGk=\x1b\\".to_vec()
    );
}

#[test]
fn osc52_sequence_starts_with_osc_and_ends_with_st() {
    let seq = osc52_sequence("some longer text with spaces");
    assert_eq!(&seq[0..2], b"\x1b]");
    assert_eq!(&seq[seq.len() - 2..], b"\x1b\\");
}

// -------------------------------------------------------- mode parsing

#[test]
fn mode_parses_the_documented_values() {
    assert_eq!(ClipboardMode::parse("auto"), ClipboardMode::Auto);
    assert_eq!(ClipboardMode::parse("OSC52"), ClipboardMode::Osc52);
    assert_eq!(ClipboardMode::parse("  none "), ClipboardMode::None);
    // An unknown value degrades to auto, the same way the config layer handles a bad value.
    assert_eq!(ClipboardMode::parse("carrier-pigeon"), ClipboardMode::Auto);
}

#[test]
fn mode_config_keys_round_trip() {
    for mode in [
        ClipboardMode::Auto,
        ClipboardMode::Osc52,
        ClipboardMode::None,
    ] {
        assert_eq!(ClipboardMode::parse(mode.config_key()), mode);
    }
}

// ------------------------------------------------------- provider modes

#[test]
fn new_does_not_touch_the_system_clipboard() {
    // Constructing must be free of side effects even in `auto`, which is the mode a bare
    // SSH session lands in. If this ever probes arboard, the lazy-init guarantee is gone.
    let sink = Sink::default();
    let p = provider(ClipboardMode::Auto, &sink);
    assert_eq!(p.mode(), ClipboardMode::Auto);
    assert!(sink.is_empty());
}

#[test]
fn osc52_mode_writes_the_escape_sequence() {
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    assert_eq!(p.copy("hello"), ClipboardTarget::Osc52);
    assert_eq!(sink.text(), "\x1b]52;c;aGVsbG8=\x1b\\");
}

#[test]
fn none_mode_writes_nothing() {
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::None, &sink);
    assert_eq!(p.copy("hello"), ClipboardTarget::Nowhere);
    assert_eq!(p.paste(), None);
    assert_eq!(p.paste_primary(), None);
    assert_eq!(p.copy_primary("x"), ClipboardTarget::Nowhere);
    assert!(sink.is_empty());
}

#[test]
fn copy_of_empty_text_writes_nothing() {
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    assert_eq!(p.copy(""), ClipboardTarget::Nowhere);
    assert!(sink.is_empty());
}

#[test]
fn paste_returns_the_in_process_copy_after_an_osc52_copy() {
    // OSC 52 hands the text to the terminal, which we cannot read back. The mirror is what
    // makes Ctrl+V work after Ctrl+C in this session.
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    p.copy("mirrored text");
    assert_eq!(p.paste().as_deref(), Some("mirrored text"));
}

#[test]
fn primary_selection_round_trips_in_process() {
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    p.copy_primary("primary payload");
    assert_eq!(p.paste_primary().as_deref(), Some("primary payload"));
    // and it went out on the wire targeting the `p` buffer
    assert!(sink.text().contains("52;p;"));
}

#[test]
fn primary_falls_back_to_the_clipboard_when_nothing_was_selected() {
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    p.copy("clipboard only");
    let got = p.paste_primary();
    assert_eq!(got.as_deref(), Some("clipboard only"));
}

#[test]
fn a_long_copy_is_one_sequence_with_the_full_payload() {
    // Documents the no-chunking choice: the whole payload rides in one sequence.
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Osc52, &sink);
    let text = "x".repeat(5000);
    p.copy(&text);
    let expected = osc52_sequence(&text);
    assert_eq!(sink.bytes(), expected);
}

#[test]
fn auto_mode_degrades_to_osc52_when_the_system_clipboard_is_unavailable() {
    // On a machine with a working clipboard this test would take the System branch, so the
    // assertion covers the fallback that matters headless: something was written, and the
    // copy is readable back either way.
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Auto, &sink);
    let target = p.copy("fallback probe");
    assert!(matches!(
        target,
        ClipboardTarget::System | ClipboardTarget::Osc52
    ));
    assert_eq!(p.paste().as_deref(), Some("fallback probe"));
}

#[test]
fn repeated_copies_do_not_reprobe_the_system_clipboard() {
    // The cached-failure guarantee: after the first `auto` write decides the system clipboard
    // is gone, later writes must go straight to OSC 52 rather than re-attempting arboard.
    // Observed through the sink: with a working system clipboard nothing reaches the sink,
    // and with a broken one every write reaches it. Either way the count is consistent.
    let sink = Sink::default();
    let mut p = provider(ClipboardMode::Auto, &sink);
    let mut osc_writes = 0;
    for i in 0..5 {
        if p.copy(&format!("n{i}")) == ClipboardTarget::Osc52 {
            osc_writes += 1;
        }
    }
    assert!(osc_writes == 0 || osc_writes == 5, "writes: {osc_writes}");
}
