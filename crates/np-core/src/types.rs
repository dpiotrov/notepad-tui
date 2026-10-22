//! Shared identity types: positions, line endings, encodings, languages.
//!
//! These carry no behavior beyond mapping to their external representations, so that
//! every other module can depend on them without creating cycles.

use std::path::Path;

/// A position in the document: `line` is a 0-based logical line, `col` is a 0-based
/// **character** offset within that line.
///
/// Display columns are a separate concept computed through
/// [`Buffer::display_col`](crate::buffer::Buffer::display_col). Never store a display
/// column in a `LineCol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

impl LineCol {
    pub const fn new(line: usize, col: usize) -> Self {
        LineCol { line, col }
    }
}

/// Line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Eol {
    #[default]
    Lf,
    Crlf,
    Cr,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::Crlf => "\r\n",
            Eol::Cr => "\r",
        }
    }

    /// Short label for the status bar (spec FR-U3).
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
            Eol::Cr => "CR",
        }
    }

    /// Parse the config spellings used by `default_eol` / `eol_mode` (spec §7.1).
    pub fn parse(s: &str) -> Option<Eol> {
        // Only surrounding spaces/tabs are trimmed: the raw escapes "\n" / "\r" are
        // valid spellings and must survive.
        let key = s
            .trim_matches(|c: char| c == ' ' || c == '\t')
            .to_ascii_lowercase();
        match key.as_str() {
            "lf" | "unix" | "\n" => Some(Eol::Lf),
            "crlf" | "windows" | "dos" | "\r\n" => Some(Eol::Crlf),
            "cr" | "mac" | "macos" | "\r" => Some(Eol::Cr),
            _ => None,
        }
    }

    /// The config spelling, for writing config back out.
    pub fn config_key(self) -> &'static str {
        match self {
            Eol::Lf => "lf",
            Eol::Crlf => "crlf",
            Eol::Cr => "cr",
        }
    }
}

/// Stable identifier for a character encoding (spec FR-N2).
///
/// Equality is by label, so `Utf8` and `Label("UTF-8")` would be distinct by design:
/// only the listed variants are produced by detection and by the Encoding menu.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EncodingId {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// A named codec from `encoding_rs`, e.g. `"ISO-8859-1"`.
    Label(&'static str),
}

/// The full set offered by the Encoding menu, in menu order.
static ALL_ENCODINGS: &[EncodingId] = &[
    EncodingId::Utf8,
    EncodingId::Utf8Bom,
    EncodingId::Utf16Le,
    EncodingId::Utf16Be,
    EncodingId::Label("ISO-8859-1"),
    EncodingId::Label("windows-1251"),
    EncodingId::Label("windows-1252"),
];

impl EncodingId {
    pub fn all() -> &'static [EncodingId] {
        ALL_ENCODINGS
    }

    /// Display name shown in the status bar and Encoding menu.
    pub fn label(&self) -> &str {
        match self {
            EncodingId::Utf8 => "UTF-8",
            EncodingId::Utf8Bom => "UTF-8 BOM",
            EncodingId::Utf16Le => "UTF-16LE",
            EncodingId::Utf16Be => "UTF-16BE",
            EncodingId::Label(s) => s,
        }
    }

    /// The underlying codec, or `None` for `Utf8Bom`, which is UTF-8 plus a BOM that
    /// S5's encoder writes explicitly.
    ///
    /// `ISO-8859-1` maps to `WINDOWS_1252`: `encoding_rs` implements the WHATWG label
    /// algorithm, in which "iso-8859-1" is an alias for windows-1252, so the crate has
    /// no `ISO_8859_1` static at all. Consequences for later stages:
    ///  - detection can never report ISO-8859-1; it reports windows-1252
    ///  - `from_encoding_rs` therefore never yields `Label("ISO-8859-1")`
    ///  - the menu label is still offered so users can pick the familiar name
    pub fn encoding_rs(&self) -> Option<&'static encoding_rs::Encoding> {
        match self {
            EncodingId::Utf8 => Some(encoding_rs::UTF_8),
            EncodingId::Utf8Bom => None,
            EncodingId::Utf16Le => Some(encoding_rs::UTF_16LE),
            EncodingId::Utf16Be => Some(encoding_rs::UTF_16BE),
            EncodingId::Label("ISO-8859-1") => Some(encoding_rs::WINDOWS_1252),
            EncodingId::Label("windows-1251") => Some(encoding_rs::WINDOWS_1251),
            EncodingId::Label("windows-1252") => Some(encoding_rs::WINDOWS_1252),
            EncodingId::Label(_) => None,
        }
    }

    pub fn from_encoding_rs(enc: &'static encoding_rs::Encoding) -> Self {
        if enc == encoding_rs::UTF_8 {
            EncodingId::Utf8
        } else if enc == encoding_rs::UTF_16LE {
            EncodingId::Utf16Le
        } else if enc == encoding_rs::UTF_16BE {
            EncodingId::Utf16Be
        } else if enc == encoding_rs::WINDOWS_1251 {
            EncodingId::Label("windows-1251")
        } else if enc == encoding_rs::WINDOWS_1252 {
            EncodingId::Label("windows-1252")
        } else {
            // Unknown codec: fall back to UTF-8 rather than inventing a label we cannot
            // round-trip. S5 surfaces this through its `lossy` flag.
            EncodingId::Utf8
        }
    }

    /// Parse a config value or CLI `--encoding` argument.
    pub fn parse(s: &str) -> Option<EncodingId> {
        let norm: String = s
            .trim()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
            .replace('_', "-");
        match norm.as_str() {
            "utf-8" | "utf8" => Some(EncodingId::Utf8),
            "utf-8bom" | "utf-8-bom" | "utf8bom" | "utf-8-with-bom" => Some(EncodingId::Utf8Bom),
            "utf-16le" | "utf16le" | "utf-16-le" | "utf-16" | "utf16" => Some(EncodingId::Utf16Le),
            "utf-16be" | "utf16be" | "utf-16-be" => Some(EncodingId::Utf16Be),
            // BOM-less spellings that users type when they mean a specific endianness.
            "utf-16l" | "utf-16b" => {
                if norm.ends_with('l') {
                    Some(EncodingId::Utf16Le)
                } else {
                    Some(EncodingId::Utf16Be)
                }
            }
            "iso-8859-1" | "latin1" | "latin-1" | "8859-1" => Some(EncodingId::Label("ISO-8859-1")),
            "windows-1251" | "cp1251" | "1251" => Some(EncodingId::Label("windows-1251")),
            "windows-1252" | "cp1252" | "1252" => Some(EncodingId::Label("windows-1252")),
            _ => None,
        }
    }

    /// The config spelling for `default_encoding`.
    pub fn config_key(&self) -> String {
        self.label().to_lowercase()
    }
}

/// Language selection for syntax highlighting (spec FR-H2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum LanguageId {
    /// Resolve from file extension, then shebang (spec FR-H1).
    #[default]
    Auto,
    /// Forced syntect syntax name, e.g. `"Rust"`.
    Named(String),
}

impl LanguageId {
    /// Session files store `""` for auto-detect (spec §7.2).
    pub fn from_session_str(s: &str) -> Self {
        if s.trim().is_empty() {
            LanguageId::Auto
        } else {
            LanguageId::Named(s.to_string())
        }
    }

    pub fn to_session_str(&self) -> String {
        match self {
            LanguageId::Auto => String::new(),
            LanguageId::Named(n) => n.clone(),
        }
    }

    /// Whether `name` looks like the file belongs to this language, used by tests and
    /// by the Language menu's checkmark. Deliberately cheap: extension-only, no
    /// syntect lookup (S12 owns that).
    pub fn matches_path(&self, path: &Path) -> bool {
        match self {
            LanguageId::Auto => false,
            LanguageId::Named(name) => {
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    return false;
                };
                ext.eq_ignore_ascii_case(name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_spellings_round_trip() {
        for eol in [Eol::Lf, Eol::Crlf, Eol::Cr] {
            assert_eq!(Eol::parse(eol.config_key()), Some(eol));
            assert_eq!(Eol::parse(eol.as_str()), Some(eol));
        }
        assert_eq!(Eol::parse("windows"), Some(Eol::Crlf));
        assert_eq!(Eol::parse("  CRLF  "), Some(Eol::Crlf));
        assert_eq!(Eol::parse("nope"), None);
        assert_eq!(Eol::default(), Eol::Lf);
    }

    #[test]
    fn eol_labels_are_the_status_bar_ones() {
        assert_eq!(Eol::Lf.label(), "LF");
        assert_eq!(Eol::Crlf.label(), "CRLF");
        assert_eq!(Eol::Cr.label(), "CR");
    }

    #[test]
    fn encoding_list_matches_spec_fr_n2() {
        let labels: Vec<&str> = EncodingId::all().iter().map(|e| e.label()).collect();
        assert_eq!(
            labels,
            vec![
                "UTF-8",
                "UTF-8 BOM",
                "UTF-16LE",
                "UTF-16BE",
                "ISO-8859-1",
                "windows-1251",
                "windows-1252",
            ]
        );
    }

    #[test]
    fn every_listed_encoding_except_bom_maps_to_a_codec() {
        for enc in EncodingId::all() {
            if matches!(enc, EncodingId::Utf8Bom) {
                assert!(enc.encoding_rs().is_none());
            } else {
                assert!(enc.encoding_rs().is_some(), "{enc:?} has no codec");
            }
        }
    }

    #[test]
    fn encoding_round_trips_through_encoding_rs_except_the_latin1_alias() {
        for enc in EncodingId::all() {
            if enc.label() == "ISO-8859-1" {
                continue;
            }
            if let Some(codec) = enc.encoding_rs() {
                assert_eq!(&EncodingId::from_encoding_rs(codec), enc, "{enc:?}");
            }
        }
    }

    #[test]
    fn iso_8859_1_is_the_whatwg_alias_for_windows_1252() {
        let iso = EncodingId::Label("ISO-8859-1");
        let cp = EncodingId::Label("windows-1252");
        assert_eq!(iso.encoding_rs(), cp.encoding_rs());
        assert_eq!(
            encoding_rs::Encoding::for_label(b"iso-8859-1"),
            Some(encoding_rs::WINDOWS_1252)
        );
        assert_eq!(EncodingId::from_encoding_rs(encoding_rs::WINDOWS_1252), cp);
    }

    #[test]
    fn unknown_codec_falls_back_to_utf8() {
        assert_eq!(
            EncodingId::from_encoding_rs(encoding_rs::SHIFT_JIS),
            EncodingId::Utf8
        );
    }

    #[test]
    fn encoding_parse_accepts_common_aliases() {
        assert_eq!(EncodingId::parse("utf8"), Some(EncodingId::Utf8));
        assert_eq!(EncodingId::parse("UTF-8"), Some(EncodingId::Utf8));
        assert_eq!(EncodingId::parse("utf_16_le"), Some(EncodingId::Utf16Le));
        assert_eq!(
            EncodingId::parse("latin1"),
            Some(EncodingId::Label("ISO-8859-1"))
        );
        assert_eq!(
            EncodingId::parse("cp1251"),
            Some(EncodingId::Label("windows-1251"))
        );
        assert_eq!(EncodingId::parse(""), None);
        assert_eq!(EncodingId::parse("klingon"), None);
    }

    #[test]
    fn language_session_strings_use_empty_for_auto() {
        assert_eq!(LanguageId::from_session_str(""), LanguageId::Auto);
        assert_eq!(LanguageId::from_session_str("   "), LanguageId::Auto);
        assert_eq!(
            LanguageId::from_session_str("Rust"),
            LanguageId::Named("Rust".into())
        );
        assert_eq!(LanguageId::Auto.to_session_str(), "");
        assert_eq!(
            LanguageId::from_session_str(&LanguageId::Named("C".into()).to_session_str()),
            LanguageId::Named("C".into())
        );
        assert_eq!(LanguageId::default(), LanguageId::Auto);
    }

    #[test]
    fn linecol_orders_lexicographically() {
        assert!(LineCol::new(0, 9) < LineCol::new(1, 0));
        assert!(LineCol::new(1, 0) < LineCol::new(1, 1));
        assert_eq!(LineCol::default(), LineCol::new(0, 0));
    }
}
