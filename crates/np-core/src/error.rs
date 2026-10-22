//! Crate-wide error type for `np-core`.
//!
//! `np-core` never panics on user input or I/O failure: fallible operations return
//! `CoreResult` and the UI layer surfaces the message as a toast (spec FR-U6).
//! `anyhow` is deliberately not used here — the engine's error surface is closed and
//! small so callers can pattern-match on it.

use std::io;
use std::path::PathBuf;

/// Every recoverable failure produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// File read/write failure. The path is carried so the UI can name the file in the
    /// error toast instead of showing a bare OS message (spec FR-U6, §8).
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Text could not be decoded or encoded with the requested codec (spec §6.11).
    #[error("encoding error: {0}")]
    Encoding(String),

    /// Caller supplied a value the engine cannot act on: out-of-range line, read-only
    /// document, malformed pattern, and so on.
    #[error("invalid argument: {0}")]
    Invalid(String),
}

/// Shorthand for engine results.
pub type CoreResult<T> = Result<T, CoreError>;

/// Attach a path to an `io::Error` at the boundary where the path is known.
impl CoreError {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        CoreError::Io {
            path: path.into(),
            source,
        }
    }

    /// Human-readable message for toasts. Distinct from `Display` only in that callers
    /// should use this rather than formatting the error themselves, so wording stays
    /// consistent across the app.
    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_names_the_path() {
        let err = CoreError::io(
            "/tmp/does-not-exist",
            io::Error::new(io::ErrorKind::NotFound, "no such file"),
        );
        let msg = err.to_string();
        assert!(msg.contains("/tmp/does-not-exist"), "got: {msg}");
        assert!(msg.contains("no such file"), "got: {msg}");
    }

    #[test]
    fn encoding_and_invalid_messages_render() {
        assert_eq!(
            CoreError::Encoding("bad utf-16".into()).to_string(),
            "encoding error: bad utf-16"
        );
        assert_eq!(
            CoreError::Invalid("read-only".into()).to_string(),
            "invalid argument: read-only"
        );
    }

    #[test]
    fn user_message_matches_display() {
        let err = CoreError::Invalid("nope".into());
        assert_eq!(err.user_message(), err.to_string());
    }

    #[test]
    fn source_chain_is_preserved_for_io() {
        let root = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let err = CoreError::io("/x", root);
        let mut chain: &dyn std::error::Error = &err;
        let mut found = false;
        while let Some(src) = chain.source() {
            if let Some(io_err) = src.downcast_ref::<io::Error>() {
                assert_eq!(io_err.kind(), io::ErrorKind::PermissionDenied);
                found = true;
            }
            chain = src;
        }
        assert!(found, "io::Error must remain reachable in the source chain");
    }

    #[test]
    fn core_result_is_usable_with_question_mark() {
        fn inner() -> CoreResult<u32> {
            let _ = std::fs::read("/definitely/not/a/real/path/npt-s00")
                .map_err(|e| CoreError::io("/definitely/not/a/real/path/npt-s00", e))?;
            Ok(1)
        }
        // Propagates as an Io variant carrying the path.
        match inner() {
            Err(CoreError::Io { path, .. }) => {
                assert_eq!(
                    path,
                    std::path::Path::new("/definitely/not/a/real/path/npt-s00")
                );
            }
            other => panic!("expected Io variant, got {other:?}"),
        }
    }
}
