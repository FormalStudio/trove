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
        SelectAll,
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
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        ExtendUp,
        ExtendDown,
        MoveHome,
        MoveEnd,
        Deselect,
        Copy,
        Cut,
        Paste,
        PasteInto,
        Undo,
        Redo,
        Trash,
        DeletePermanently,
        Rename,
        CommitRename,
        CancelRename,
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
        CloseOtherTabs,
        DuplicateTab,
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
pub struct OpenPlaceInNewTab(pub PathBuf);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct RemoveBookmark(pub PathBuf);

#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct Eject(pub PathBuf);

/// Files dragged inside the app were dropped on a folder: `dragged` is the path the drag
/// started on, `dest` the folder.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct DropOnto {
    pub dragged: PathBuf,
    pub dest: PathBuf,
}

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

/// The whole window: everything inside the root view.
pub const WINDOW: &str = "Trove";
/// The listing, while it has focus.
pub const FILES: &str = "Files";
pub const SEARCH: &str = "Search";
pub const PATH_EDIT: &str = "PathEdit";
/// The listing while one of its names is being edited: the listing's own keys (type-select,
/// Backspace, Delete) stand down so the field gets them.
pub const RENAMING: &str = "Renaming";
/// What a dragged row or tile carries.
pub const DRAG_FILES: &str = "trove/files";

/// What type-select answers to: letters, digits and the punctuation names start with.
const TYPED: &str = "abcdefghijklmnopqrstuvwxyz0123456789.";

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
        KeyBinding::new("secondary-a", SelectAll, f),
        KeyBinding::new("up", MoveUp, f),
        KeyBinding::new("down", MoveDown, f),
        KeyBinding::new("left", MoveLeft, f),
        KeyBinding::new("right", MoveRight, f),
        KeyBinding::new("shift-up", ExtendUp, f),
        KeyBinding::new("shift-down", ExtendDown, f),
        KeyBinding::new("home", MoveHome, f),
        KeyBinding::new("end", MoveEnd, f),
        KeyBinding::new("escape", Deselect, f),
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
        KeyBinding::new("enter", CommitRename, Some(RENAMING)),
        KeyBinding::new("escape", CancelRename, Some(RENAMING)),
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
