//! Every command is an action, so a key, a toolbar button and a menu item never disagree. The
//! keys follow the Keys table in `docs/design.md`.

use std::path::PathBuf;

use nd::prelude::*;

actions!(
    trove,
    [
        Open,
        OpenInNewTab,
        Parent,
        Back,
        Forward,
        IntoFolder,
        Reload,
        ToggleHidden,
        ListView,
        GridView,
        TogglePreview,
        EditPath,
        CancelPathEdit,
        CompletePath,
        FocusSearch,
        ClearSearch,
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        MoveLeft,
        MoveRight,
        Deselect,
        FocusFiles,
        Copy,
        Cut,
        Paste,
        PasteInto,
        Undo,
        Redo,
        Trash,
        DeletePermanently,
        Rename,
        NewFolder,
        NewFile,
        Duplicate,
        Compress,
        ExtractHere,
        CopyPath,
        AddBookmark,
        Restore,
        EmptyTrash,
        ToggleFoldersFirst,
        SortAscending,
        SortDescending,
        OpenTerminal,
        Properties,
        BatchRename,
    ]
);

/// The context menu's "Open with": the app with this desktop id.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct OpenWith(pub String);

/// Makes the app with this desktop id the default for the selection's type.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct SetDefaultApp(pub String);

/// The context menu's "Sort by".
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct SortBy(pub trove_fs::SortKey);

/// A sidebar place or volume, from the sidebar's menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct OpenPlaceInNewTab(pub crate::location::Location);

/// A sidebar folder that is no bookmark yet, from its menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct BookmarkPlace(pub PathBuf);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct RemoveBookmark(pub PathBuf);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct Eject(pub PathBuf);

/// Files were dropped on a folder: dragged from a listing (the whole selection), or in from
/// another app.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct DropOnto {
    pub paths: Vec<PathBuf>,
    pub dest: PathBuf,
    pub mode: crate::fileops::Transfer,
}

/// A tab's menu: the commands on the tab it opened on, by the tab's id.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct DuplicateTab(pub u64);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct CloseOtherTabs(pub u64);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct CloseTabWithId(pub u64);

/// Alt+1..9: the tab at that position, Explorer's order.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct SelectTab(pub usize);

/// A character typed in the listing, for type-select.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct TypeSelect(pub char);

/// Go to a folder: the path bar's collapsed segments and its menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct GoTo(pub PathBuf);

/// A terminal in this folder: the path bar's menu on one of its segments.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct OpenTerminalAt(pub PathBuf);

/// Puts this path on the clipboard as text: the path bar's menu.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct CopyPathOf(pub PathBuf);

/// The whole window: everything inside the root view.
pub const WINDOW: &str = "Trove";
/// The listing's box: its keys match while the list inside it has focus.
pub const FILES: &str = "Files";
/// The columns view, around its lists: Left and Right move between columns, where every other
/// view leaves them to its list.
pub const COLUMNS: &str = "Columns";
pub const SEARCH: &str = "Search";
pub const PATH_EDIT: &str = "PathEdit";
/// The listing while one of its names is being edited: the listing's own keys (type-select,
/// Backspace, Delete) stand down so the field gets them.
pub const RENAMING: &str = "Renaming";
/// What a dragged row or tile carries.
pub const DRAG_FILES: &str = "trove/files";

/// What type-select answers to: letters of either case, digits and the punctuation names start
/// with. A capital binds as Shift and its letter, and "_" matches the character Shift+"-" types.
const TYPED: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_";

pub fn bind(cx: &mut App) {
    let w = Some(WINDOW);
    let f = Some(FILES);
    let mut keys = vec![
        KeyBinding::new("alt-left", Back, w),
        KeyBinding::new("alt-right", Forward, w),
        KeyBinding::new("alt-up", Parent, w),
        KeyBinding::new("alt-down", IntoFolder, w),
        KeyBinding::new("f5", Reload, w),
        KeyBinding::new("secondary-r", Reload, w),
        KeyBinding::new("secondary-h", ToggleHidden, w),
        KeyBinding::new("secondary-1", ListView, w),
        KeyBinding::new("secondary-2", GridView, w),
        KeyBinding::new("secondary-i", TogglePreview, w),
        KeyBinding::new("secondary-l", EditPath, w),
        KeyBinding::new("secondary-f", FocusSearch, w),
        KeyBinding::new("secondary-t", NewTab, w),
        KeyBinding::new("secondary-w", CloseTab, w),
        KeyBinding::new("ctrl-tab", NextTab, w),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, w),
        KeyBinding::new("enter", Open, f),
        KeyBinding::new("secondary-enter", OpenInNewTab, f),
        KeyBinding::new("backspace", Parent, f),
        KeyBinding::new("escape", Deselect, f),
        KeyBinding::new("left", MoveLeft, Some(COLUMNS)),
        KeyBinding::new("right", MoveRight, Some(COLUMNS)),
        KeyBinding::new("secondary-c", Copy, f),
        KeyBinding::new("secondary-x", Cut, f),
        KeyBinding::new("secondary-v", Paste, f),
        KeyBinding::new("secondary-shift-v", PasteInto, f),
        KeyBinding::new("secondary-z", Undo, f),
        KeyBinding::new("secondary-shift-z", Redo, f),
        KeyBinding::new("delete", Trash, f),
        KeyBinding::new("shift-delete", DeletePermanently, f),
        KeyBinding::new("secondary-backspace", Restore, f),
        KeyBinding::new("f2", Rename, f),
        KeyBinding::new("secondary-shift-n", NewFolder, w),
        KeyBinding::new("secondary-d", Duplicate, f),
        KeyBinding::new("escape", ClearSearch, Some(SEARCH)),
        KeyBinding::new("escape", CancelPathEdit, Some(PATH_EDIT)),
        KeyBinding::new("tab", CompletePath, Some(PATH_EDIT)),
    ];
    for n in 1..=9 {
        keys.push(KeyBinding::new(&format!("alt-{n}"), SelectTab(n - 1), w));
    }
    for c in TYPED.chars() {
        keys.push(KeyBinding::new(&c.to_string(), TypeSelect(c), f));
    }
    cx.bind_keys(nd::actions::standard_bindings());
    cx.bind_keys(keys);
}
