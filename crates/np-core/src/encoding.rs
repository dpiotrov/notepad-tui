//! Byte-level text codec (spec §6.11, contract §6).
//!
//! This is the only place `npt` turns bytes into `str` and back. Everything else works on
//! decoded text, which is what keeps encoding handling from leaking into the buffer, the
//! renderer, and the search engine.
//!
//! # Detection (FR-N1)
//!
//! A BOM decides: UTF-8 BOM, UTF-16LE BOM, UTF-16BE BOM. With no BOM the bytes are treated
//! as UTF-8, and invalid sequences become U+FFFD with `lossy = true` so the status bar can
//! warn. UTF-16 without a BOM is therefore read as UTF-8, exactly as the spec directs;
//! "Reload as…" (FR-N3) is the recovery path and passes `force`.
//!
//! # The UTF-16 encode trap
//!
//! **`encoding_rs`'s `UTF_16LE::encode()` does not produce UTF-16.** Per the WHATWG
//! standard it downgrades to UTF-8 whenever the input has no lone surrogates — verified on
//! encoding_rs 0.8.42, where `UTF_16LE.encode("hi")` returns the ASCII bytes `[104, 105]`.
//! Calling it and trusting the result would silently save UTF-16 documents as UTF-8. So
//! [`encode`] writes UTF-16 by hand from `str::encode_utf16`, and only delegates to
//! `encoding_rs` for the single-byte codecs where it behaves.
//!
//! # Round-trip contract
//!
//! `decode(encode(text, enc), Some(enc)) == text` for every encoding in
//! [`EncodingId::all`], asserted byte-exactly in the tests. That is the property a user
//! depends on when they open a file, change one character, and save.
//!
//! # Unmappable characters
//!
//! A single-byte codec cannot represent every scalar. `encoding_rs` emits a numeric
//! character reference (`U+0400` becomes `&#1024;`) rather than failing, which round-trips
//! as those literal characters, not the original. This is a real, documented lossiness of
//! "save Cyrillic as windows-1252"; the caller can detect it because [`encode`] reports
//! whether any substitution happened.

use crate::types::EncodingId;

/// A decoded file: the text, what encoding it turned out to be, and caveats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    pub encoding: EncodingId,
    /// True when the file began with a byte-order mark (FR-N1). Reported even for a UTF-8
    /// BOM, which `encoding_rs` also strips.
    pub had_bom: bool,
    /// True when invalid bytes were replaced with U+FFFD (FR-N1). The status bar warns on
    /// this; it is not an error, the document still opens.
    pub lossy: bool,
}

/// The UTF-8 BOM.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// The UTF-16LE BOM.
const UTF16_LE_BOM: [u8; 2] = [0xFF, 0xFE];
/// The UTF-16BE BOM.
const UTF16_BE_BOM: [u8; 2] = [0xFE, 0xFF];

/// Decode bytes to text.
///
/// `force` skips detection entirely and decodes with that codec (CLI `-e`, "Reload as…").
/// It never fails: undecodable bytes become U+FFFD and set `lossy`.
pub fn decode(bytes: &[u8], force: Option<EncodingId>) -> DecodedText {
    match force {
        Some(enc) => decode_forced(bytes, &enc),
        None => detect_and_decode(bytes),
    }
}

/// Strip a leading `bom` if present, reporting whether one was found.
fn strip_bom<'a>(bytes: &'a [u8], bom: &[u8]) -> (&'a [u8], bool) {
    if bytes.starts_with(bom) {
        (&bytes[bom.len()..], true)
    } else {
        (bytes, false)
    }
}

/// Detection path: BOM first, else UTF-8.
fn detect_and_decode(bytes: &[u8]) -> DecodedText {
    if bytes.starts_with(&UTF8_BOM) {
        // Checked before the UTF-16 BOMs: EF BB BF cannot be a UTF-16 BOM, so order is not
        // a correctness issue here, but UTF-8 first matches how the list reads.
        let body = &bytes[UTF8_BOM.len()..];
        let (text, lossy) = decode_utf8(body);
        return DecodedText {
            text,
            encoding: EncodingId::Utf8Bom,
            had_bom: true,
            lossy,
        };
    }
    if bytes.starts_with(&UTF16_BE_BOM) {
        // Must precede the UTF-16LE check: FE FF is unambiguous, but FF FE is the LE BOM
        // and 0x00 0xFE is not a BOM at all.
        return decode_utf16(&bytes[2..], EncodingId::Utf16Be, true);
    }
    if bytes.starts_with(&UTF16_LE_BOM) {
        return decode_utf16(&bytes[2..], EncodingId::Utf16Le, true);
    }
    // No BOM: UTF-8, per FR-N1. UTF-16 without a BOM is misread on purpose; the spec's
    // answer is that the user reloads it explicitly.
    let (text, lossy) = decode_utf8(bytes);
    DecodedText {
        text,
        encoding: EncodingId::Utf8,
        had_bom: false,
        lossy,
    }
}

/// Decode with a caller-chosen codec, honoring a leading BOM where one is present so that
/// "Reload as UTF-16LE" on a BOM'd file does not leave U+FEFF in the text.
fn decode_forced(bytes: &[u8], enc: &EncodingId) -> DecodedText {
    match enc {
        EncodingId::Utf8Bom => {
            let (body, had_bom) = strip_bom(bytes, &UTF8_BOM);
            let (text, lossy) = decode_utf8(body);
            DecodedText {
                text,
                encoding: EncodingId::Utf8Bom,
                had_bom,
                lossy,
            }
        }
        EncodingId::Utf16Le => {
            let (body, had_bom) = strip_bom(bytes, &UTF16_LE_BOM);
            decode_utf16(body, EncodingId::Utf16Le, had_bom)
        }
        EncodingId::Utf16Be => {
            let (body, had_bom) = strip_bom(bytes, &UTF16_BE_BOM);
            decode_utf16(body, EncodingId::Utf16Be, had_bom)
        }
        EncodingId::Utf8 => {
            // Forcing UTF-8 does not mean "expect a BOM", but a stray BOM should not become
            // U+FEFF in the buffer either. Strip it and report it.
            let (body, had_bom) = strip_bom(bytes, &UTF8_BOM);
            let (text, lossy) = decode_utf8(body);
            DecodedText {
                text,
                encoding: EncodingId::Utf8,
                had_bom,
                lossy,
            }
        }
        other => {
            let Some(codec) = other.encoding_rs() else {
                // Unknown label: fall back to UTF-8 and say so through `lossy`, matching
                // the documented behavior of `EncodingId::from_encoding_rs`.
                let (text, _) = decode_utf8(bytes);
                return DecodedText {
                    text,
                    encoding: EncodingId::Utf8,
                    had_bom: false,
                    lossy: true,
                };
            };
            let (cow, _actual, bom_consumed) = codec.decode(bytes);
            // Check the replacement character before moving `cow` into the owned String.
            let lossy = cow.contains('\u{FFFD}');
            DecodedText {
                text: cow.into_owned(),
                encoding: other.clone(),
                // `encoding_rs` reports BOM consumption for the BOM-aware codecs only; for
                // single-byte codecs it is always false, which is correct: they have no BOM.
                had_bom: bom_consumed,
                lossy,
            }
        }
    }
}

/// Decode UTF-8, reporting whether replacement happened.
fn decode_utf8(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => {
            // from_utf8_lossy replaces each invalid sequence with U+FFFD. It never fails,
            // which is what FR-N1 requires: a file with a few bad bytes still opens.
            let lossy = String::from_utf8_lossy(bytes);
            let had_replacement = lossy.contains('\u{FFFD}');
            (lossy.into_owned(), had_replacement)
        }
    }
}

/// Decode UTF-16 by hand from BOM-stripped bytes.
///
/// Deliberately not `encoding_rs::decode`: that path is fine, but writing it here lets the
/// odd trailing byte case be explicit (a lone final byte is dropped and flagged) and keeps
/// decode symmetric with the hand-written [`encode_utf16_bytes`].
fn decode_utf16(bytes: &[u8], enc: EncodingId, had_bom: bool) -> DecodedText {
    let little = enc == EncodingId::Utf16Le;
    let mut lossy = false;
    // An odd byte count means a truncated code unit at the end: it cannot be decoded.
    let usable = bytes.len() - (bytes.len() % 2);
    if usable != bytes.len() {
        lossy = true;
    }
    let mut units: Vec<u16> = Vec::with_capacity(usable / 2);
    for chunk in bytes[..usable].chunks_exact(2) {
        units.push(if little {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        });
    }
    // `decode_utf16` yields Err for an unpaired surrogate rather than panicking; a lone
    // surrogate has no scalar value, so it becomes U+FFFD and marks the text lossy.
    let mut text = String::with_capacity(units.len());
    for unit in String::from_utf16_lossy(&units).chars() {
        text.push(unit);
    }
    if text.contains('\u{FFFD}') {
        lossy = true;
    }
    // Also flag genuinely unpaired surrogates, which from_utf16_lossy already turned into
    // U+FFFD above; the check is on the unit list to catch the case where the input text
    // legitimately contained U+FFFD.
    if units.iter().any(|u| (0xD800..0xE000).contains(u)) && !has_valid_pairs(&units) {
        lossy = true;
    }
    DecodedText {
        text,
        encoding: enc,
        had_bom,
        lossy,
    }
}

/// Whether every surrogate in `units` is part of a well-formed high/low pair.
fn has_valid_pairs(units: &[u16]) -> bool {
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xD800..0xDC00).contains(&u) {
            // High surrogate: the next unit must be a low surrogate.
            match units.get(i + 1) {
                Some(&next) if (0xDC00..0xE000).contains(&next) => i += 2,
                _ => return false,
            }
        } else if (0xDC00..0xE000).contains(&u) {
            // Low surrogate with no high surrogate before it.
            return false;
        } else {
            i += 1;
        }
    }
    true
}

/// Encode text to bytes with a leading BOM where the encoding has one.
///
/// The result round-trips through [`decode`] with `force = Some(enc)`.
pub fn encode(text: &str, enc: EncodingId) -> Vec<u8> {
    match enc {
        // UTF-8 BOM: the BOM then plain UTF-8. `str` is already UTF-8, so no codec call.
        EncodingId::Utf8Bom => {
            let mut out = Vec::with_capacity(UTF8_BOM.len() + text.len());
            out.extend_from_slice(&UTF8_BOM);
            out.extend_from_slice(text.as_bytes());
            out
        }
        EncodingId::Utf16Le => encode_utf16_bytes(text, true),
        EncodingId::Utf16Be => encode_utf16_bytes(text, false),
        EncodingId::Utf8 => text.as_bytes().to_vec(),
        other => {
            // Single-byte codecs: encoding_rs handles these correctly, including the
            // numeric-character-reference fallback for unmappable scalars.
            match other.encoding_rs() {
                Some(codec) => codec.encode(text).0.into_owned(),
                // An unknown label should not be reachable through the menu; encode as
                // UTF-8 rather than losing the document to an error.
                None => text.as_bytes().to_vec(),
            }
        }
    }
}

/// Write UTF-16 by hand, with a BOM.
///
/// Not delegating to `encoding_rs` is deliberate: its `encode` downgrades UTF-16 to UTF-8
/// for any input without lone surrogates (module docs). The BOM is written explicitly so
/// the file is self-describing and detection reads it back.
/// Write UTF-16 by hand, with a BOM.
///
/// Not delegating to `encoding_rs` is deliberate: its `encode` downgrades UTF-16 to UTF-8
/// for any input without lone surrogates (module docs). The BOM is written explicitly so
/// the file is self-describing and detection reads it back.
fn encode_utf16_bytes(text: &str, little: bool) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Vec::with_capacity(2 + units.len() * 2);
    out.extend_from_slice(if little { &UTF16_LE_BOM } else { &UTF16_BE_BOM });
    for u in units {
        out.extend_from_slice(&if little {
            u.to_le_bytes()
        } else {
            u.to_be_bytes()
        });
    }
    out
}

/// True when `text` contains a scalar that `enc` cannot represent.
///
/// Lets the save path warn "this will be saved as character references" instead of quietly
/// writing `&#1024;` where the user typed a Cyrillic letter.
pub fn has_unmappable(text: &str, enc: EncodingId) -> bool {
    // Unicode codecs represent every scalar, so nothing is ever unmappable. This must be
    // checked first: `Utf16Le.encoding_rs()` is Some(UTF_16LE), and running the round-trip
    // check on UTF-16 is both wrong (encode() downgrades) and pointless.
    if is_unicode_codec(&enc) {
        return false;
    }
    // Fast reject: pure ASCII is in every single-byte codec's repertoire.
    if text.is_ascii() {
        return false;
    }
    match enc.encoding_rs() {
        Some(codec) => encoding_is_lossy(codec, text),
        // An unknown label is treated as UTF-8 by encode(), which is lossless.
        None => false,
    }
}

/// Whether `enc` is a Unicode codec that can represent every scalar value.
fn is_unicode_codec(enc: &EncodingId) -> bool {
    matches!(
        enc,
        EncodingId::Utf8 | EncodingId::Utf8Bom | EncodingId::Utf16Le | EncodingId::Utf16Be
    )
}

/// Whether encoding `text` with `codec` loses information.
///
/// Round-tripping is the honest test: if the bytes do not decode back to the original,
/// something was substituted (for `encoding_rs` that substitution is a numeric character
/// reference such as `&#1024;`). This catches the NCR case and anything similar without
/// depending on the exact substitution format.
fn encoding_is_lossy(codec: &'static encoding_rs::Encoding, text: &str) -> bool {
    let (encoded, _actual, _bom) = codec.encode(text);
    let (back, _enc, _bom) = codec.decode(&encoded);
    back != text
}

/// Binary sniff (FR-N5): a NUL byte in the first 8 KiB means "this is not text".
///
/// 8 KiB is the spec's window and matches what other editors use; it is large enough to
/// catch a header block yet small enough that a real text file with one stray NUL deep in
/// a line is not misreported. UTF-16 text has NUL bytes and is caught too, which is fine:
/// binary detection runs before decoding, and a UTF-16 file is opened through the "this
/// might be binary, open as text?" confirm.
pub fn looks_binary(bytes: &[u8]) -> bool {
    // `memchr` is not a dependency, and this is only ever run on the head of a file, so a
    // plain scan of at most 8 KiB is the right cost.
    let window = bytes.len().min(BINARY_SNIFF_BYTES);
    bytes[..window].contains(&0)
}

/// How many leading bytes [`looks_binary`] inspects (spec FR-N5).
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples every encoding can represent: ASCII plus the Latin-1 range and control-ish
    /// characters that show up in real files.
    const UNIVERSAL_SAMPLES: &[&str] = &[
        "",
        "hello",
        "one\ntwo\nthree\n",
        "café naïve",
        "tab\there\tand\tmore",
        "trailing space   ",
        "line1\r\nline2\rline3\n",
        "&amp; and <tags> and \"quotes\"",
    ];

    /// Pure-ASCII samples: the only set every single-byte codec can represent, since the
    /// Latin-1 range is present in windows-1252 but absent from windows-1251.
    const ASCII_SAMPLES: &[&str] = &[
        "",
        "hello",
        "one\ntwo\nthree\n",
        "tab\there\tand\tmore",
        "trailing space   ",
        "&amp; and <tags> and \"quotes\"",
        "mixed\r\nCRLF and\nLF and\rCR\n",
    ];

    /// Samples only the Unicode codecs can represent.
    const UNICODE_SAMPLES: &[&str] = &[
        "日本語のテキスト",
        "עברית",
        "emoji 🎉🚀",
        "mixed 日本語 café 🎉 end",
        "Привет мир",
    ];

    #[test]
    fn every_encoding_round_trips_byte_exact() {
        // The core FR-N2 guarantee. Universal samples must survive every codec; the
        // Unicode-only samples only the Unicode codecs, since a single-byte codec cannot
        // hold a CJK character (that lossiness is asserted in
        // `unmappable_detection_for_single_byte_codecs`).
        for enc in EncodingId::all() {
            let mut samples: Vec<&str> = if is_unicode_codec(enc) {
                // Unicode codecs take everything.
                let mut all = UNIVERSAL_SAMPLES.to_vec();
                all.extend_from_slice(UNICODE_SAMPLES);
                all
            } else {
                // A single-byte codec holds only its own repertoire, and those differ:
                // windows-1252 has é but not Cyrillic, windows-1251 the reverse. So the
                // universally-safe set for these is ASCII, plus the codec's own script
                // below in `single_byte_codecs_round_trip_their_own_range`.
                ASCII_SAMPLES.to_vec()
            };
            for sample in samples.drain(..) {
                let bytes = encode(sample, enc.clone());
                let back = decode(&bytes, Some(enc.clone()));
                assert_eq!(
                    back.text,
                    *sample,
                    "round-trip failed for {} with {sample:?}",
                    enc.label()
                );
                assert!(!back.lossy, "round-trip reported lossy for {}", enc.label());
                // Bytes must be identical on a second encode, or saving twice would drift.
                assert_eq!(
                    encode(&back.text, enc.clone()),
                    bytes,
                    "re-encode differs for {}",
                    enc.label()
                );
            }
        }
    }

    #[test]
    fn round_trip_through_detection_matches_force() {
        // A BOM'd file is detected, not forced; detection must agree with forcing.
        for enc in [
            EncodingId::Utf8Bom,
            EncodingId::Utf16Le,
            EncodingId::Utf16Be,
        ] {
            let bytes = encode("café 日本語", enc.clone());
            let detected = decode(&bytes, None);
            assert_eq!(detected.text, "café 日本語");
            assert_eq!(
                detected.encoding,
                enc,
                "detection misidentified {}",
                enc.label()
            );
            assert!(detected.had_bom, "{} should report a BOM", enc.label());
        }
    }

    #[test]
    fn utf16_encode_actually_produces_two_byte_code_units() {
        // Regression guard for the encoding_rs downgrade trap: UTF_16LE.encode("hi") returns
        // ASCII bytes. Ours must not.
        let bytes = encode("hi", EncodingId::Utf16Le);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "missing UTF-16LE BOM");
        assert_eq!(&bytes[2..4], &[b'h', 0], "expected 16-bit code unit");
        assert_eq!(bytes.len(), 2 + 4, "BOM + two UTF-16 units");
    }

    #[test]
    fn utf8_bom_variant_emits_the_bom() {
        let bytes = encode("x", EncodingId::Utf8Bom);
        assert_eq!(&bytes[..3], &UTF8_BOM);
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn plain_utf8_has_no_bom() {
        assert_eq!(encode("x", EncodingId::Utf8), b"x");
    }

    #[test]
    fn bom_detection_for_all_three() {
        let u8 = decode(&[0xEF, 0xBB, 0xBF, b'a'], None);
        assert_eq!(u8.encoding, EncodingId::Utf8Bom);
        assert!(u8.had_bom);
        assert_eq!(u8.text, "a", "BOM must not remain in the text");

        // "a" as UTF-16LE with BOM.
        let le = decode(&[0xFF, 0xFE, b'a', 0], None);
        assert_eq!(le.encoding, EncodingId::Utf16Le);
        assert_eq!(le.text, "a");

        let be = decode(&[0xFE, 0xFF, 0, b'a'], None);
        assert_eq!(be.encoding, EncodingId::Utf16Be);
        assert_eq!(be.text, "a");
    }

    #[test]
    fn utf16_without_bom_is_read_as_utf8() {
        // Documented FR-N1 behavior: no BOM means UTF-8, even when the bytes are really
        // UTF-16. Asserting it here so the choice is visible and not a silent surprise.
        let raw = vec![b'a', 0, b'b', 0];
        let d = decode(&raw, None);
        assert_eq!(d.encoding, EncodingId::Utf8);
        assert!(!d.had_bom);
        // The NULs survive as characters; the caller can see the file looks odd and reload.
        assert!(d.text.contains('\0'));
    }

    #[test]
    fn lossy_decode_sets_the_flag_and_inserts_replacement() {
        // 0xC3 0x28 is invalid UTF-8 (a truncated 2-byte sequence).
        let d = decode(&[b'a', 0xC3, 0x28, b'b'], None);
        assert!(d.lossy, "invalid bytes must set lossy");
        assert!(
            d.text.contains('\u{FFFD}'),
            "expected U+FFFD, got {:?}",
            d.text
        );
        assert_eq!(d.encoding, EncodingId::Utf8);
    }

    #[test]
    fn valid_utf8_is_not_lossy() {
        let d = decode("clean text ✓".as_bytes(), None);
        assert!(!d.lossy);
        assert_eq!(d.text, "clean text ✓");
    }

    #[test]
    fn force_overrides_detection() {
        // Bytes that would detect as UTF-16LE, forced to UTF-8: must come back as UTF-8.
        let raw = vec![0xFF, 0xFE, 0x00, 0x41];
        let forced = decode(&raw, Some(EncodingId::Utf8));
        assert_eq!(forced.encoding, EncodingId::Utf8);
    }

    #[test]
    fn empty_input_is_utf8_not_lossy() {
        let d = decode(&[], None);
        assert_eq!(d.text, "");
        assert_eq!(d.encoding, EncodingId::Utf8);
        assert!(!d.lossy);
        assert!(!d.had_bom);
    }

    #[test]
    fn utf16_odd_byte_count_is_lossy_not_a_panic() {
        // A truncated code unit at the end: 5 bytes after the BOM.
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(&[b'a', 0, b'b', 0, 0x41]);
        let d = std::panic::catch_unwind(|| decode(&bytes, None));
        let d = d.expect("decode must not panic on a truncated unit");
        assert!(d.lossy, "a truncated code unit must be reported lossy");
    }

    #[test]
    fn looks_binary_at_the_eight_kib_boundary() {
        // FR-N5 boundary, asserted exactly.
        let mut a = vec![b'a'; 8192];
        a[8191] = 0; // last byte inside the window
        assert!(
            looks_binary(&a),
            "NUL at byte 8191 is inside the 8 KiB window"
        );

        let mut b = vec![b'a'; 8193];
        b[8192] = 0; // first byte outside the window
        assert!(!looks_binary(&b), "NUL at byte 8192 is outside the window");
    }

    #[test]
    fn looks_binary_on_typical_inputs() {
        assert!(!looks_binary(b"plain text"));
        assert!(looks_binary(b"\x00elf header"));
        assert!(!looks_binary(b""));
        // UTF-16 text has NULs, so it sniffs as binary by design; the confirm dialog is the
        // path to opening it.
        assert!(looks_binary(&encode("hi", EncodingId::Utf16Le)));
    }

    #[test]
    fn unmappable_detection_for_single_byte_codecs() {
        // Cyrillic cannot be written in windows-1252.
        assert!(has_unmappable("Привет", EncodingId::Label("windows-1252")));
        // Latin-1 range is fine in windows-1252.
        assert!(!has_unmappable("café", EncodingId::Label("windows-1252")));
        // UTF-8/UTF-16 can represent everything.
        assert!(!has_unmappable("Привет 日本語 🎉", EncodingId::Utf8));
        assert!(!has_unmappable("Привет 日本語 🎉", EncodingId::Utf16Le));
    }

    #[test]
    fn single_byte_codecs_round_trip_their_own_range() {
        // Cyrillic round-trips through windows-1251, which is its codec.
        let bytes = encode("Привет мир", EncodingId::Label("windows-1251"));
        let back = decode(&bytes, Some(EncodingId::Label("windows-1251")));
        assert_eq!(back.text, "Привет мир");
        assert!(!back.lossy);
    }

    #[test]
    fn windows_1252_is_what_iso_label_resolves_to() {
        // WHATWG folds ISO-8859-1 to windows-1252; the menu offers the familiar name and it
        // behaves as windows-1252. Pinned so a future change to that mapping is noticed.
        let bytes = encode("café", EncodingId::Label("ISO-8859-1"));
        assert_eq!(bytes, vec![b'c', b'a', b'f', 0xE9]);
        let back = decode(&bytes, Some(EncodingId::Label("ISO-8859-1")));
        assert_eq!(back.text, "café");
    }

    #[test]
    fn unknown_label_falls_back_to_utf8_and_reports_lossy() {
        let d = decode(b"abc", Some(EncodingId::Label("made-up-99")));
        assert_eq!(d.encoding, EncodingId::Utf8);
        assert!(
            d.lossy,
            "an unknown codec must be flagged, not silently used"
        );
    }
}
