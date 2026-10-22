//! The command vocabulary (contract §12, spec §5.3).
//!
//! # Why the whole enum lands here in S3
//!
//! Menus, the command palette, the keymap and the mouse all converge on `Action`, and
//! `App::execute` is the single place that interprets it. Declaring every variant now
//! means later stages add a match arm instead of restructuring an enum that a dozen files
//! already match on exhaustively. Variants whose behavior does not exist yet resolve to a
//! no-op arm, which is why `Action::Nop` is reachable and tested rather than vestigial.
//!
//! # `kebab()` is a storage format
//!
//! The kebab name is also the `[keymap]` key in `config.toml` (FR-CG4), so it is part of
//! the on-disk contract: renaming one silently orphans the user's binding. The uniqueness
//! and lowercase-kebab-shape tests in this module are what stop that from happening by
//! accident, and any new variant must be added to `kebab()` with a name that never changes.

use np_core::Eol;

/// The nine top-level menus (FR-U1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuId {
    File,
    Edit,
    Search,
    View,
    Encoding,
    Language,
    Settings,
    Window,
    Help,
}

/// Which submenu of a menu is open, identified by the row label it was opened from.
///
/// A submenu shares its parent's [`MenuId`], so `Encoding` inside `Encoding` was not
/// expressible and the renderer could not tell the two levels apart. An enum rather than a
/// `String` because [`crate::app::Mode`] is `Copy`, and because the set is closed: the labels
/// come from the same registry that draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmenuId {
    RecentFiles,
    EncodingConvertTo,
    EncodingReloadAs,
}

impl SubmenuId {
    /// The parent menu this submenu hangs off.
    pub fn parent(self) -> MenuId {
        match self {
            SubmenuId::RecentFiles => MenuId::File,
            SubmenuId::EncodingConvertTo | SubmenuId::EncodingReloadAs => MenuId::Encoding,
        }
    }

    /// The label of the row that opens it, which is the key `submenu_items` resolves on.
    pub fn label(self) -> &'static str {
        match self {
            SubmenuId::RecentFiles => "Recent Files",
            SubmenuId::EncodingConvertTo => "Convert to",
            SubmenuId::EncodingReloadAs => "Reload as",
        }
    }

    /// The submenu named `label` under `menu`, if that menu has one.
    pub fn find(menu: MenuId, label: &str) -> Option<SubmenuId> {
        match (menu, label) {
            (MenuId::File, "Recent Files") => Some(SubmenuId::RecentFiles),
            (MenuId::Encoding, "Convert to") => Some(SubmenuId::EncodingConvertTo),
            (MenuId::Encoding, "Reload as") => Some(SubmenuId::EncodingReloadAs),
            _ => None,
        }
    }
}

impl MenuId {
    /// All menus in bar order.
    pub const ALL: [MenuId; 9] = [
        MenuId::File,
        MenuId::Edit,
        MenuId::Search,
        MenuId::View,
        MenuId::Encoding,
        MenuId::Language,
        MenuId::Settings,
        MenuId::Window,
        MenuId::Help,
    ];

    /// Bar title.
    pub fn title(self) -> &'static str {
        match self {
            MenuId::File => "File",
            MenuId::Edit => "Edit",
            MenuId::Search => "Search",
            MenuId::View => "View",
            MenuId::Encoding => "Encoding",
            MenuId::Language => "Language",
            MenuId::Settings => "Settings",
            MenuId::Window => "Window",
            MenuId::Help => "Help",
        }
    }

    /// The underlined accelerator letter for `Alt+<letter>` (spec §6.10 "Alt+F/E/S/V/...").
    ///
    /// `Language` uses `L` rather than the `G` that would follow `Encoding`'s `E`, because
    /// the two menus must not share an accelerator or one becomes unreachable.
    pub fn accelerator(self) -> char {
        match self {
            MenuId::File => 'F',
            MenuId::Edit => 'D',
            MenuId::Search => 'S',
            MenuId::View => 'V',
            MenuId::Encoding => 'E',
            MenuId::Language => 'L',
            MenuId::Settings => 'T',
            MenuId::Window => 'W',
            MenuId::Help => 'H',
        }
    }
}

/// Every command the editor can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NewFile,
    OpenFile,
    Save,
    SaveAs,
    CloseTab,
    Exit,
    ReloadFile,

    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Delete,

    Find,
    FindNext,
    FindPrev,
    Replace,
    FindInFiles,
    GoToLine,
    FindWordNext,
    FindWordPrev,
    ReplaceAll,

    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,

    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    SortLinesAsc,
    SortLinesDesc,

    UpperCase,
    LowerCase,
    TitleCase,
    ToggleComment,
    InsertDateTime,

    Indent,
    Outdent,
    ToggleWordWrap,
    ToggleLineNumbers,
    ToggleWhitespace,
    ToggleEolMarks,
    FoldAll,
    UnfoldAll,
    ToggleFold,

    NextTab,
    PrevTab,
    CloseOtherTabs,
    CloseAllTabs,
    ReopenClosedTab,

    DocumentList,
    RecentFiles,
    LanguagePicker,
    ThemePicker,

    EncodingMenu,
    EolMenu,
    ConvertEncoding,
    ReloadAsEncoding,
    SetEol(Eol),
    ToggleReadonly,
    CopyPath,
    CopyFilename,

    Preferences,
    CommandPalette,
    HelpOverlay,
    About,

    ToggleOvertype,

    // Movement (FR-E2) and its Shift-extending forms (FR-E3). Contract §12 predates these
    // and lists no movement variants even though the stage spec requires "all movement
    // actions"; see docs/DEVIATIONS.md. They are actions rather than direct view pokes
    // because the keymap, the palette and the menus all need to name them.
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveLeftSelect,
    MoveRightSelect,
    MoveUpSelect,
    MoveDownSelect,
    WordLeft,
    WordRight,
    WordLeftSelect,
    WordRightSelect,
    LineStart,
    LineEnd,
    LineStartSelect,
    LineEndSelect,
    DocStart,
    DocEnd,

    ScrollPageUp,
    ScrollPageDown,

    /// Open (or activate) a top-level menu.
    MenuBar(MenuId),
    /// Bound to a key that does nothing here, and the fallthrough for a variant whose
    /// implementation belongs to a later stage.
    Nop,
}

impl Action {
    /// kebab-case name, and the `[keymap]` key (FR-CG4). Stable once shipped.
    pub fn kebab(self) -> &'static str {
        match self {
            Action::NewFile => "new-file",
            Action::OpenFile => "open-file",
            Action::Save => "save",
            Action::SaveAs => "save-as",
            Action::CloseTab => "close-tab",
            Action::Exit => "exit",
            Action::ReloadFile => "reload-file",

            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::Cut => "cut",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::SelectAll => "select-all",
            Action::Delete => "delete",

            Action::Find => "find",
            Action::FindNext => "find-next",
            Action::FindPrev => "find-prev",
            Action::Replace => "replace",
            Action::FindInFiles => "find-in-files",
            Action::GoToLine => "go-to-line",
            Action::FindWordNext => "find-word-next",
            Action::FindWordPrev => "find-word-prev",
            Action::ReplaceAll => "replace-all",

            Action::ToggleBookmark => "toggle-bookmark",
            Action::NextBookmark => "next-bookmark",
            Action::PrevBookmark => "prev-bookmark",
            Action::ClearBookmarks => "clear-bookmarks",

            Action::DuplicateLine => "duplicate-line",
            Action::DeleteLine => "delete-line",
            Action::MoveLineUp => "move-line-up",
            Action::MoveLineDown => "move-line-down",
            Action::SortLinesAsc => "sort-lines-asc",
            Action::SortLinesDesc => "sort-lines-desc",

            Action::UpperCase => "upper-case",
            Action::LowerCase => "lower-case",
            Action::TitleCase => "title-case",
            Action::ToggleComment => "toggle-comment",
            Action::InsertDateTime => "insert-date-time",

            Action::Indent => "indent",
            Action::Outdent => "outdent",
            Action::ToggleWordWrap => "toggle-word-wrap",
            Action::ToggleLineNumbers => "toggle-line-numbers",
            Action::ToggleWhitespace => "toggle-whitespace",
            Action::ToggleEolMarks => "toggle-eol-marks",
            Action::FoldAll => "fold-all",
            Action::UnfoldAll => "unfold-all",
            Action::ToggleFold => "toggle-fold",

            Action::NextTab => "next-tab",
            Action::PrevTab => "prev-tab",
            Action::CloseOtherTabs => "close-other-tabs",
            Action::CloseAllTabs => "close-all-tabs",
            Action::ReopenClosedTab => "reopen-closed-tab",

            Action::DocumentList => "document-list",
            Action::RecentFiles => "recent-files",
            Action::LanguagePicker => "language-picker",
            Action::ThemePicker => "theme-picker",

            Action::EncodingMenu => "encoding-menu",
            Action::EolMenu => "eol-menu",
            Action::ConvertEncoding => "convert-encoding",
            Action::ReloadAsEncoding => "reload-as-encoding",
            // Parameterized variants are not keymap-addressable by name alone; they get a
            // kebab stem so `label()`/menus still work, and the keymap binds the three
            // concrete forms through `SetEol`'s own arm in `Keymap::action_for`.
            Action::SetEol(Eol::Lf) => "set-eol-lf",
            Action::SetEol(Eol::Crlf) => "set-eol-crlf",
            Action::SetEol(Eol::Cr) => "set-eol-cr",
            Action::ToggleReadonly => "toggle-readonly",
            Action::CopyPath => "copy-path",
            Action::CopyFilename => "copy-filename",

            Action::Preferences => "preferences",
            Action::CommandPalette => "command-palette",
            Action::HelpOverlay => "help-overlay",
            Action::About => "about",

            Action::ToggleOvertype => "toggle-overtype",
            Action::MoveLeft => "move-left",
            Action::MoveRight => "move-right",
            Action::MoveUp => "move-up",
            Action::MoveDown => "move-down",
            Action::MoveLeftSelect => "move-left-select",
            Action::MoveRightSelect => "move-right-select",
            Action::MoveUpSelect => "move-up-select",
            Action::MoveDownSelect => "move-down-select",
            Action::WordLeft => "word-left",
            Action::WordRight => "word-right",
            Action::WordLeftSelect => "word-left-select",
            Action::WordRightSelect => "word-right-select",
            Action::LineStart => "line-start",
            Action::LineEnd => "line-end",
            Action::LineStartSelect => "line-start-select",
            Action::LineEndSelect => "line-end-select",
            Action::DocStart => "doc-start",
            Action::DocEnd => "doc-end",
            Action::ScrollPageUp => "scroll-page-up",
            Action::ScrollPageDown => "scroll-page-down",

            Action::MenuBar(MenuId::File) => "menu-file",
            Action::MenuBar(MenuId::Edit) => "menu-edit",
            Action::MenuBar(MenuId::Search) => "menu-search",
            Action::MenuBar(MenuId::View) => "menu-view",
            Action::MenuBar(MenuId::Encoding) => "menu-encoding",
            Action::MenuBar(MenuId::Language) => "menu-language",
            Action::MenuBar(MenuId::Settings) => "menu-settings",
            Action::MenuBar(MenuId::Window) => "menu-window",
            Action::MenuBar(MenuId::Help) => "menu-help",
            Action::Nop => "nop",
        }
    }

    /// Human label for menus and the command palette.
    pub fn label(self) -> &'static str {
        match self {
            Action::NewFile => "New",
            Action::OpenFile => "Open...",
            Action::Save => "Save",
            Action::SaveAs => "Save As...",
            Action::CloseTab => "Close",
            Action::Exit => "Exit",
            Action::ReloadFile => "Reload",

            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::Cut => "Cut",
            Action::Copy => "Copy",
            Action::Paste => "Paste",
            Action::SelectAll => "Select All",
            Action::Delete => "Delete",

            Action::Find => "Find...",
            Action::FindNext => "Find Next",
            Action::FindPrev => "Find Previous",
            Action::Replace => "Replace...",
            Action::FindInFiles => "Find in Files...",
            Action::GoToLine => "Go to Line...",
            Action::FindWordNext => "Find Word Next",
            Action::FindWordPrev => "Find Word Previous",
            Action::ReplaceAll => "Replace All",

            Action::ToggleBookmark => "Toggle Bookmark",
            Action::NextBookmark => "Next Bookmark",
            Action::PrevBookmark => "Previous Bookmark",
            Action::ClearBookmarks => "Clear Bookmarks",

            Action::DuplicateLine => "Duplicate Line",
            Action::DeleteLine => "Delete Line",
            Action::MoveLineUp => "Move Line Up",
            Action::MoveLineDown => "Move Line Down",
            Action::SortLinesAsc => "Sort Lines Ascending",
            Action::SortLinesDesc => "Sort Lines Descending",

            Action::UpperCase => "UPPERCASE",
            Action::LowerCase => "lowercase",
            Action::TitleCase => "Title Case",
            Action::ToggleComment => "Toggle Comment",
            Action::InsertDateTime => "Insert Date/Time",

            Action::Indent => "Indent",
            Action::Outdent => "Outdent",
            Action::ToggleWordWrap => "Toggle Word Wrap",
            Action::ToggleLineNumbers => "Toggle Line Numbers",
            Action::ToggleWhitespace => "Toggle Whitespace",
            Action::ToggleEolMarks => "Toggle EOL Marks",
            Action::FoldAll => "Fold All",
            Action::UnfoldAll => "Unfold All",
            Action::ToggleFold => "Toggle Fold",

            Action::NextTab => "Next Tab",
            Action::PrevTab => "Previous Tab",
            Action::CloseOtherTabs => "Close Other Tabs",
            Action::CloseAllTabs => "Close All Tabs",
            Action::ReopenClosedTab => "Reopen Closed Tab",

            Action::DocumentList => "Document List",
            Action::RecentFiles => "Recent Files",
            Action::LanguagePicker => "Languages",
            Action::ThemePicker => "Themes",

            Action::EncodingMenu => "Encoding",
            Action::EolMenu => "EOL",
            Action::ConvertEncoding => "Convert Encoding",
            Action::ReloadAsEncoding => "Reload as Encoding",
            Action::SetEol(e) => match e {
                Eol::Lf => "Convert to UNIX (LF)",
                Eol::Crlf => "Convert to Windows (CR LF)",
                Eol::Cr => "Convert to Macintosh (CR)",
            },
            Action::ToggleReadonly => "Toggle Read-Only",
            Action::CopyPath => "Copy Full Path",
            Action::CopyFilename => "Copy Filename",

            Action::Preferences => "Preferences...",
            Action::CommandPalette => "Command Palette",
            Action::HelpOverlay => "Keyboard Help",
            Action::About => "About npt",

            Action::ToggleOvertype => "Toggle Overtype",
            Action::MoveLeft => "Move Left",
            Action::MoveRight => "Move Right",
            Action::MoveUp => "Move Up",
            Action::MoveDown => "Move Down",
            Action::MoveLeftSelect => "Select Left",
            Action::MoveRightSelect => "Select Right",
            Action::MoveUpSelect => "Select Up",
            Action::MoveDownSelect => "Select Down",
            Action::WordLeft => "Previous Word",
            Action::WordRight => "Next Word",
            Action::WordLeftSelect => "Select to Previous Word",
            Action::WordRightSelect => "Select to Next Word",
            Action::LineStart => "Line Start",
            Action::LineEnd => "Line End",
            Action::LineStartSelect => "Select to Line Start",
            Action::LineEndSelect => "Select to Line End",
            Action::DocStart => "Document Start",
            Action::DocEnd => "Document End",
            Action::ScrollPageUp => "Scroll Page Up",
            Action::ScrollPageDown => "Scroll Page Down",

            Action::MenuBar(m) => m.title(),
            Action::Nop => "No-op",
        }
    }

    /// Every variant that `kebab()`/`label()` must handle, for exhaustive metadata tests.
    ///
    /// Hand-maintained on purpose: the tests that walk it are what catch a new variant
    /// added without a kebab name, and a list generated by a macro would still need the
    /// same manual edit while hiding the omission behind a compile error far from the
    /// cause. `SetEol` and `MenuBar` contribute all their parameterizations.
    pub const ALL: &'static [Action] = &[
        Action::NewFile,
        Action::OpenFile,
        Action::Save,
        Action::SaveAs,
        Action::CloseTab,
        Action::Exit,
        Action::ReloadFile,
        Action::Undo,
        Action::Redo,
        Action::Cut,
        Action::Copy,
        Action::Paste,
        Action::SelectAll,
        Action::Delete,
        Action::Find,
        Action::FindNext,
        Action::FindPrev,
        Action::Replace,
        Action::FindInFiles,
        Action::GoToLine,
        Action::FindWordNext,
        Action::FindWordPrev,
        Action::ReplaceAll,
        Action::ToggleBookmark,
        Action::NextBookmark,
        Action::PrevBookmark,
        Action::ClearBookmarks,
        Action::DuplicateLine,
        Action::DeleteLine,
        Action::MoveLineUp,
        Action::MoveLineDown,
        Action::SortLinesAsc,
        Action::SortLinesDesc,
        Action::UpperCase,
        Action::LowerCase,
        Action::TitleCase,
        Action::ToggleComment,
        Action::InsertDateTime,
        Action::Indent,
        Action::Outdent,
        Action::ToggleWordWrap,
        Action::ToggleLineNumbers,
        Action::ToggleWhitespace,
        Action::ToggleEolMarks,
        Action::FoldAll,
        Action::UnfoldAll,
        Action::ToggleFold,
        Action::NextTab,
        Action::PrevTab,
        Action::CloseOtherTabs,
        Action::CloseAllTabs,
        Action::ReopenClosedTab,
        Action::DocumentList,
        Action::RecentFiles,
        Action::LanguagePicker,
        Action::ThemePicker,
        Action::EncodingMenu,
        Action::EolMenu,
        Action::ConvertEncoding,
        Action::ReloadAsEncoding,
        Action::SetEol(Eol::Lf),
        Action::SetEol(Eol::Crlf),
        Action::SetEol(Eol::Cr),
        Action::ToggleReadonly,
        Action::CopyPath,
        Action::CopyFilename,
        Action::Preferences,
        Action::CommandPalette,
        Action::HelpOverlay,
        Action::About,
        Action::ToggleOvertype,
        Action::MoveLeft,
        Action::MoveRight,
        Action::MoveUp,
        Action::MoveDown,
        Action::MoveLeftSelect,
        Action::MoveRightSelect,
        Action::MoveUpSelect,
        Action::MoveDownSelect,
        Action::WordLeft,
        Action::WordRight,
        Action::WordLeftSelect,
        Action::WordRightSelect,
        Action::LineStart,
        Action::LineEnd,
        Action::LineStartSelect,
        Action::LineEndSelect,
        Action::DocStart,
        Action::DocEnd,
        Action::ScrollPageUp,
        Action::ScrollPageDown,
        Action::MenuBar(MenuId::File),
        Action::MenuBar(MenuId::Edit),
        Action::MenuBar(MenuId::Search),
        Action::MenuBar(MenuId::View),
        Action::MenuBar(MenuId::Encoding),
        Action::MenuBar(MenuId::Language),
        Action::MenuBar(MenuId::Settings),
        Action::MenuBar(MenuId::Window),
        Action::MenuBar(MenuId::Help),
        Action::Nop,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_is_unique_across_every_variant() {
        // A duplicate would silently orphan one action's [keymap] binding.
        let mut seen: Vec<(&str, Action)> = Vec::new();
        for &a in Action::ALL {
            let k = a.kebab();
            if let Some((_, other)) = seen.iter().find(|(k2, _)| *k2 == k) {
                panic!("kebab collision {k}: {a:?} vs {other:?}");
            }
            seen.push((k, a));
        }
        assert_eq!(seen.len(), Action::ALL.len());
    }

    #[test]
    fn every_action_is_listed_in_all() {
        // Guard the hand-maintained list against the enum by count and by kebab set.
        assert!(Action::ALL.len() >= 75, "expected the full §12 enum");
        assert!(Action::ALL.contains(&Action::SetEol(Eol::Crlf)));
        assert!(Action::ALL.contains(&Action::MenuBar(MenuId::Window)));
        assert!(Action::ALL.contains(&Action::Nop));
    }

    #[test]
    fn kebab_names_are_lowercase_kebab_case() {
        for &a in Action::ALL {
            let k = a.kebab();
            assert!(!k.is_empty(), "{a:?} has an empty kebab name");
            assert!(
                k.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{a:?} kebab {k:?} has characters outside [a-z0-9-]"
            );
            assert!(!k.starts_with('-'), "{a:?} kebab {k:?} starts with a dash");
            assert!(!k.ends_with('-'), "{a:?} kebab {k:?} ends with a dash");
            assert!(!k.contains("--"), "{a:?} kebab {k:?} has a doubled dash");
        }
    }

    #[test]
    fn kebab_names_are_stable_for_the_ones_already_in_config_examples() {
        // These appear in spec §7.1's [keymap] example and must never be renamed (FR-CG4).
        assert_eq!(Action::Save.kebab(), "save");
        assert_eq!(Action::Find.kebab(), "find");
        assert_eq!(Action::Undo.kebab(), "undo");
        assert_eq!(Action::ToggleComment.kebab(), "toggle-comment");
        assert_eq!(Action::DuplicateLine.kebab(), "duplicate-line");
    }

    #[test]
    fn labels_are_present_and_not_just_the_kebab_name() {
        for &a in Action::ALL {
            let l = a.label();
            assert!(!l.is_empty(), "{a:?} has an empty label");
            if a != Action::Nop {
                assert_ne!(l, a.kebab(), "{a:?} label looks unreviewed");
            }
        }
    }

    #[test]
    fn menu_titles_match_fr_u1_exactly() {
        let titles: Vec<&str> = MenuId::ALL.iter().map(|m| m.title()).collect();
        assert_eq!(
            titles,
            vec![
                "File", "Edit", "Search", "View", "Encoding", "Language", "Settings", "Window",
                "Help"
            ]
        );
    }

    #[test]
    fn menu_accelerators_are_unique() {
        let mut seen = Vec::new();
        for m in MenuId::ALL {
            let c = m.accelerator();
            assert!(
                !seen.contains(&c),
                "menu {m:?} accelerator {c} collides with another menu"
            );
            seen.push(c);
        }
    }

    #[test]
    fn set_eol_variants_each_get_their_own_kebab() {
        assert_eq!(Action::SetEol(Eol::Lf).kebab(), "set-eol-lf");
        assert_eq!(Action::SetEol(Eol::Crlf).kebab(), "set-eol-crlf");
        assert_eq!(Action::SetEol(Eol::Cr).kebab(), "set-eol-cr");
    }

    #[test]
    fn action_is_copy_and_comparable() {
        // The enum must stay `Copy` for the hot path in App::execute.
        let a = Action::Save;
        let b = a;
        assert_eq!(a, b);
        let set: std::collections::HashSet<Action> = [Action::Save, Action::Save, Action::Undo]
            .into_iter()
            .collect();
        assert_eq!(set.len(), 2);
    }
}
