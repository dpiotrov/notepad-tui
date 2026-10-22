//! `np-core` — the terminal-independent engine behind `npt`.
//!
//! This crate MUST NOT depend on `ratatui`, `crossterm`, `arboard`, or `anyhow`.
//! Everything here is testable headless. See `docs/CONTEXT.md`.

pub mod buffer;
pub mod config;
pub mod cursor;
pub mod document;
pub mod encoding;
pub mod eol;
pub mod error;
pub mod fif;
pub mod file;
pub mod folding;
pub mod highlight;
pub mod palette;
pub mod search;
pub mod session;
pub mod style;
pub mod types;
pub mod undo;
pub mod view;

pub use buffer::{BlockRect, Buffer};
pub use config::{Config, EditorConfig, FindConfig, UiConfig};
pub use cursor::{Cursor, ScrollPos, Selection, SelectionKind};
pub use document::{CaseMode, Document, SortOrder};
pub use encoding::{decode, encode, looks_binary, DecodedText, BINARY_SNIFF_BYTES};
pub use eol::{
    detect_dominant_eol, join_for_save, normalize_for_save, split_lines_keep_eol, EolMode,
};
pub use error::{CoreError, CoreResult};
pub use fif::{
    query_error_message, search_files, zero_based, FifEvent, FifHit, FifRequest, FifSummary,
    GlobFilter, SkipReason, MAX_HITS_PER_FILE, MAX_SEARCH_BYTES,
};
pub use file::{
    changed_on_disk, load, open_policy, save_atomic, FileStamp, LoadedFile, OpenDecision, SaveInfo,
    SaveOptions,
};
pub use folding::{
    compute_folds, is_brace_language, merge_folds, merge_folds_preserving, set_all_collapsed,
    shift_folds_insert, shift_folds_remove, toggle_fold_at, unfold_containing,
};
pub use highlight::{
    HighlightCache, HighlightEngine, LineSource, CACHE_WINDOW, CHECKPOINT_EVERY, DEFAULT_THEME,
    PLAIN_TEXT, THEME_NONE,
};
pub use palette::{
    core as palette_core, dim_ink, luminance, mix, text_ground, tinted_contrast_floor, Rgb,
    ThemePalette, MIN_UI_CONTRAST,
};
pub use search::{
    count, expand_replacement, find_all, find_directional, find_next, find_prev, replace_all,
    validate, Match, Matcher, SearchError, SearchOpts,
};
pub use session::{Session, SessionSnapshot, SessionTab, UNTITLED_SESSION_MAX_BYTES};
pub use style::{CoreColor, CoreStyle};
pub use types::{EncodingId, Eol, LanguageId, LineCol};
pub use undo::{UndoEdit, UndoGroup, UndoStack, UNDO_GROUP_IDLE, UNDO_MAX_STEPS};
pub use view::{
    document_rows, hidden_rows_before, layout_visible_rows, line_at_row_offset, line_is_visible,
    next_visible_line, row_end_for, row_offset_of_line, total_row_count, BarOccupancy, FoldRange,
    RectCore, ViewGeometry, VisualRow,
};
