//! Per-document highlight caches, owned by `App` (spec §6.6, FR-H4).
//!
//! The engine itself is `np_core::highlight`. This module is the UI-side ownership problem:
//! one cache per open document, kept in step with its buffer, and thrown away when the
//! document goes away or the theme changes.
//!
//! # Why a side table rather than a field on `Document`
//!
//! Contract §8 says the cache "is owned here but reached through `HighlightEngine`", i.e. on
//! `Document`. Putting it there means `Document` grows syntect parse state, which then flows
//! into `Document: Clone`, into undo snapshots that clone documents, and into anything that
//! formats one. Contract §7 also requires that this state never be serialized, and a field on
//! a type that is serialized is a field someone will eventually serialize by accident.
//!
//! A table keyed by tab index has none of those consequences. Its cost is that it must be
//! kept aligned with `App::documents` when tabs open, close and reorder, which is what
//! [`HighlightStore::retain`] and [`HighlightStore::shift`] are for. See `docs/DEVIATIONS.md`.
//!
//! # Invalidation is by content revision, not by call site
//!
//! A document changes through a dozen paths -- typing, paste, sort, comment, case, indent,
//! undo, redo, reload, and the FiF jump that opens a file. If each had to remember to
//! invalidate, the failure mode would be a stale color that looks like a theme bug and cannot
//! be reproduced. Instead [`HighlightStore::for_line`] compares [`np_core::Buffer::revision`]
//! and drops the cache when it moved. The cost is one integer compare per line per frame.
//!
//! A revision change invalidates the whole cache rather than the edited line range. That is
//! deliberately coarser than FR-H4's "from the edited line to the next checkpoint": the
//! document does not report an edit range, and the replay is cheap enough that a full drop
//! costs one viewport plus a checkpoint gap. The fine-grained path is still there in
//! [`np_core::HighlightEngine::invalidate_from`] for a caller that does know the range.

use std::collections::HashMap;

use np_core::{CoreStyle, HighlightCache, HighlightEngine};

/// What a highlight cache was built from, so staleness is a comparison rather than a
/// notification.
///
/// Grouped because the three together are the cache's identity -- a change to any of them
/// invalidates it -- and passing them separately turned the render entry point into an
/// eight-parameter function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Which tab the cache belongs to.
    pub tab: usize,
    /// `Buffer::revision()` at build time.
    pub revision: u64,
    /// Resolved syntax name (`""` for Plain Text).
    pub language: String,
    /// `HighlightEngine::theme_name()` at build time.
    pub theme: String,
}

/// Interior-mutable on purpose.
///
/// The renderer takes `&App`: `Terminal::draw`'s callback is an `FnMut` that also calls
/// `caret_position(app)`, so taking `&mut App` to draw would conflict with the next `&App` use
/// in the same closure. Caches are pure derived state, so a cell here buys a shared borrow at
/// no observable cost.
#[derive(Default)]
pub struct HighlightStore {
    inner: std::cell::RefCell<HashMap<usize, Entry>>,
}

impl std::fmt::Debug for HighlightStore {
    /// Counts only. A cache holds per-line styles for every document, which is far too much
    /// for a `{:?}` and says nothing a count would not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighlightStore")
            .field("documents", &self.inner.borrow().len())
            .finish()
    }
}

struct Entry {
    cache: HighlightCache,
    /// The buffer revision the cache was built against.
    revision: u64,
    /// The language the cache was built for, so a per-document override takes effect without
    /// waiting for a revision bump.
    language: String,
    /// The theme the cache was built for (FR-H3). A theme switch invalidates everything.
    theme: String,
}

impl HighlightStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Narrow the cache after an edit, from the line the buffer reports as dirty.
    ///
    /// Called once per frame before any line is drawn. A revision change used to rebuild the
    /// whole cache, which is free on a small file and cost 333 ms per keystroke on a 10 MB one
    /// -- the coarser rule S12 chose was justified by "the replay is cheap enough", and that
    /// stopped being true once documents got large. The buffer now reports the lowest line any
    /// mutation touched, so the drop is exactly FR-H4: from the edited line to the next
    /// checkpoint, with checkpoints below it kept so the replay stays short.
    ///
    /// Correctness does not depend on the reported line being right. A value that is too low
    /// drops more cache than necessary; a path that never reported anything leaves the default
    /// of 0, which is the old whole-cache behavior. The revision comparison in [`Self::ensure`]
    /// stays as the backstop for language, theme and out-of-band changes.
    pub fn invalidate_edited_range(
        &self,
        tab: usize,
        engine: &HighlightEngine,
        buffer: &np_core::Buffer,
    ) {
        let Some(from) = buffer.dirty_from() else {
            // Nothing edited since the caches were last brought up to date.
            return;
        };
        let mut inner = self.inner.borrow_mut();
        if let Some(entry) = inner.get_mut(&tab) {
            engine.invalidate_from(&mut entry.cache, from);
        }
    }

    /// Ensure a cache exists for `key.tab`, rebuilding if its buffer, language or theme moved.
    fn ensure(&self, key: &CacheKey, engine: &HighlightEngine) {
        let mut inner = self.inner.borrow_mut();
        let stale = match inner.get(&key.tab) {
            None => true,
            Some(e) => {
                e.revision != key.revision || e.language != key.language || e.theme != key.theme
            }
        };
        if stale {
            let lang = (!key.language.is_empty()).then_some(key.language.as_str());
            inner.insert(
                key.tab,
                Entry {
                    cache: engine.new_cache(lang),
                    revision: key.revision,
                    language: key.language.clone(),
                    theme: key.theme.clone(),
                },
            );
        }
    }

    /// Syntax styles for `line` of `tab`'s document, rebuilding the cache first if stale.
    ///
    /// This is the renderer's only entry point. The borrow of the cell is scoped to the
    /// `line_at` call, so nothing here holds it across engine work.
    pub fn styles_for_line(
        &self,
        key: &CacheKey,
        engine: &HighlightEngine,
        line: usize,
        src: &dyn np_core::LineSource,
    ) -> Vec<(CoreStyle, String)> {
        self.ensure(key, engine);
        let mut inner = self.inner.borrow_mut();
        let entry = inner.get_mut(&key.tab).expect("ensured above");
        engine.line_at(&mut entry.cache, line, src)
    }

    /// Drop caches for tabs that no longer exist. Called after a close.
    pub fn retain(&self, live: impl Fn(usize) -> bool) {
        self.inner.borrow_mut().retain(|tab, _| live(*tab));
    }

    /// Shift caches after a tab was inserted at or before `at`, so a cache follows its
    /// document instead of its old index.
    ///
    /// A cache attached to the wrong document is worse than no cache: it shows another file's
    /// colors. Inserting then dropping the new index is the boring, correct version.
    pub fn shift_for_insert(&self, at: usize) {
        for (k, e) in self.take_where(|k| *k >= at) {
            self.inner.borrow_mut().insert(k + 1, e);
        }
    }

    /// Shift caches after a tab was removed at `at`.
    pub fn shift_for_remove(&self, at: usize) {
        self.inner.borrow_mut().remove(&at);
        for (k, e) in self.take_where(|k| *k > at) {
            self.inner.borrow_mut().insert(k - 1, e);
        }
    }

    /// Remove and return every entry whose key satisfies `pred`.
    ///
    /// Keys are collected before anything is removed: `remove` inside an iterator over
    /// `self.inner` is a borrow error, and this shape avoids re-borrowing mid-walk.
    fn take_where(&self, pred: impl Fn(&usize) -> bool) -> Vec<(usize, Entry)> {
        let keys: Vec<usize> = self
            .inner
            .borrow()
            .keys()
            .filter(|k| pred(k))
            .copied()
            .collect();
        let mut inner = self.inner.borrow_mut();
        keys.into_iter()
            .filter_map(|k| inner.remove(&k).map(|e| (k, e)))
            .collect()
    }

    /// Drop every cache. Required on a theme switch (FR-H3), since styles are theme-derived
    /// and a revision change says nothing about the theme.
    pub fn clear(&self) {
        self.inner.borrow_mut().clear();
    }

    pub fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }

    /// Cached style count for `tab`, for the memory and staleness tests.
    pub fn cached_lines(&self, tab: usize) -> usize {
        self.inner
            .borrow()
            .get(&tab)
            .map(|e| e.cache.cached_line_count())
            .unwrap_or(0)
    }

    /// The revision `tab`'s cache was built against, or `None` when there is no cache.
    pub fn cached_revision(&self, tab: usize) -> Option<u64> {
        self.inner.borrow().get(&tab).map(|e| e.revision)
    }
}

/// Resolve a document's language to a syntax name, honoring the per-document override.
///
/// `LanguageId::Auto` runs detection against the path and the first line (FR-H1); `Named(s)`
/// forces it (FR-H2). The empty string means Plain Text, which is what `HighlightStore`
/// understands as "no syntax".
pub fn resolve_language(
    engine: &HighlightEngine,
    language: &np_core::LanguageId,
    path: Option<&std::path::Path>,
    first_line: &str,
) -> String {
    match language {
        np_core::LanguageId::Auto => engine.detect_syntax(path, first_line).unwrap_or_default(),
        np_core::LanguageId::Named(name) => {
            // "Plain Text" is passed through as a name, not as absence. The engine maps it
            // to no styling, which keeps one authority for what "unstyled" means instead of
            // two representations (`""` here and `None` there) that a later stage could
            // confuse.
            if engine.has_language(name) || name == np_core::highlight::PLAIN_TEXT {
                name.clone()
            } else {
                // Unknown name: fall back to detection rather than silently rendering plain,
                // so a session from another machine degrades to "best guess".
                engine.detect_syntax(path, first_line).unwrap_or_default()
            }
        }
    }
}

/// Fuzzy-filter language or theme names for a picker (FR-H2).
///
/// Empty query returns everything in the given order, which is what makes "Plain Text" the
/// first thing the user sees when they open the picker with nothing typed.
pub fn fuzzy_filter(names: &[String], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..names.len()).collect();
    }
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| matcher.fuzzy_match(n, query).map(|s| (s, i)))
        .collect();
    // Best first, then by original order so the result is deterministic for equal scores.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_has_no_caches() {
        let store = HighlightStore::new();
        assert!(store.is_empty());
    }

    #[test]
    fn an_unknown_query_returns_everything_in_order() {
        let names = vec!["Plain Text".to_string(), "Rust".to_string()];
        assert_eq!(fuzzy_filter(&names, ""), vec![0, 1]);
        assert_eq!(fuzzy_filter(&names, "   "), vec![0, 1]);
    }

    #[test]
    fn a_query_filters_and_ranks() {
        let names = vec![
            "Plain Text".to_string(),
            "Rust".to_string(),
            "Ruby".to_string(),
            "Markdown".to_string(),
        ];
        let out = fuzzy_filter(&names, "ru");
        assert!(out.contains(&1), "Rust should match 'ru'");
        assert!(out.contains(&2), "Ruby should match 'ru'");
        assert!(!out.contains(&0), "Plain Text should not");
    }

    #[test]
    fn a_query_with_no_match_returns_nothing() {
        let names = vec!["Rust".to_string()];
        assert!(fuzzy_filter(&names, "zzzznotathing").is_empty());
    }

    #[test]
    fn plain_text_override_beats_the_extension() {
        // Plain Text resolves to its own name rather than to the empty string: the engine
        // recognizes the name and produces no styling for it, which keeps one authority for
        // what "unstyled" means. The observable contract is on the engine, asserted in
        // `crates/np-tui/tests/highlight_s12.rs` and `crates/np-core/tests/highlight_s12.rs`.
        let engine = HighlightEngine::load();
        assert_eq!(
            resolve_language(
                &engine,
                &np_core::LanguageId::Named(np_core::highlight::PLAIN_TEXT.into()),
                Some(std::path::Path::new("x.rs")),
                "",
            ),
            np_core::highlight::PLAIN_TEXT,
            "forcing Plain Text must beat the .rs extension"
        );
        let mut cache = engine.new_cache(Some(np_core::highlight::PLAIN_TEXT));
        let src = np_core::Buffer::from_str_lossy("let x = 1;");
        let styles = engine.line_at(&mut cache, 0, &src);
        assert!(
            styles.iter().all(|(st, _)| *st == CoreStyle::neutral()),
            "Plain Text must produce no syntax styling"
        );
    }

    #[test]
    fn auto_resolves_from_extension_then_shebang() {
        let engine = HighlightEngine::load();
        assert_eq!(
            resolve_language(
                &engine,
                &np_core::LanguageId::Auto,
                Some(std::path::Path::new("a.rs")),
                "",
            ),
            "Rust"
        );
        assert_eq!(
            resolve_language(
                &engine,
                &np_core::LanguageId::Auto,
                None,
                "#!/usr/bin/python3"
            ),
            "Python"
        );
        assert_eq!(
            resolve_language(&engine, &np_core::LanguageId::Auto, None, "hi"),
            ""
        );
    }

    #[test]
    fn a_named_override_beats_the_extension() {
        let engine = HighlightEngine::load();
        assert_eq!(
            resolve_language(
                &engine,
                &np_core::LanguageId::Named("C".into()),
                Some(std::path::Path::new("a.rs")),
                "",
            ),
            "C"
        );
    }

    #[test]
    fn an_unknown_named_language_falls_back_to_detection() {
        let engine = HighlightEngine::load();
        assert_eq!(
            resolve_language(
                &engine,
                &np_core::LanguageId::Named("Nonsense".into()),
                Some(std::path::Path::new("a.rs")),
                "",
            ),
            "Rust",
            "a stale session value should degrade to a guess, not to plain text"
        );
    }
}
