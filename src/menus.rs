//! The context menus: on files, on a folder's empty space, on a sidebar row and on a tab.
//! Every item is an action, so a menu item, its key and its toolbar button never disagree; the
//! items act on the selection, which a right click on an unselected row moves to that row first.

use std::path::{Path, PathBuf};

use nd::prelude::*;
use trove_fs::SortKey;

use crate::actions::*;
use crate::keys::ColumnsView;
use crate::location::Location;
use crate::tab::View;
use crate::templates::Node;

/// What a menu on files acts on, read from the tab when the menu opens.
#[derive(Clone, Debug, Default)]
pub struct Target {
    pub paths: Vec<PathBuf>,
    pub folders: usize,
    /// The one archive selected, for Extract here.
    pub archive: bool,
    /// The listing is a trash.
    pub in_trash: bool,
    /// The listing is Recent, which has no folder to paste into.
    pub recent: bool,
}

/// The menu on a selection of files and folders.
pub fn files(menu: ContextMenu, t: &Target, cx: &mut App) -> ContextMenu {
    let one_folder = t.paths.len() == 1 && t.folders == 1;
    let files_only = t.folders == 0 && !t.paths.is_empty();
    let mut menu = menu.item("Open", Open);
    if one_folder {
        menu = menu.item("Open in new tab", OpenInNewTab);
        if !t.in_trash {
            menu = menu.item("Open in terminal", OpenTerminal);
        }
    }
    if files_only && !t.in_trash {
        menu = menu.submenu(open_with(t, cx));
    }
    menu = menu.separator();
    if t.in_trash {
        return menu
            .item("Restore", Restore)
            .item("Copy path", CopyPath)
            .separator()
            .item_destructive("Delete permanently", DeletePermanently);
    }
    menu = menu.item("Cut", Cut).item("Copy", Copy);
    if one_folder {
        menu = menu.item("Paste into folder", PasteInto);
    }
    menu = menu.item("Duplicate", Duplicate);
    if t.paths.len() == 1 {
        menu = menu.item("Rename\u{2026}", Rename);
    } else if t.paths.len() > 1 {
        menu = menu.item(format!("Rename {} items\u{2026}", t.paths.len()), BatchRename);
    }
    menu = menu.separator().item("Compress", Compress);
    if t.archive {
        menu = menu.item("Extract here", ExtractHere);
    }
    menu = menu.item("Copy path", CopyPath);
    if one_folder {
        menu = menu.item("Add to bookmarks", AddBookmark);
    }
    menu.separator().item_destructive("Move to Trash", Trash).separator().item("Properties\u{2026}", Properties)
}

/// "Open with": the apps for the first file's type, the default first, then a submenu that
/// makes another one the default.
fn open_with(t: &Target, cx: &mut App) -> Menu {
    let mut menu = Menu::new("Open with");
    let Some(first) = t.paths.first() else { return menu };
    let Some(apps) = crate::ops::apps_for(cx, first) else {
        return menu.item(MenuItem::action("Looking for apps\u{2026}", OpenWith(String::new())).enabled(false));
    };
    if apps.is_empty() {
        return menu.item(MenuItem::action("No apps for this type", OpenWith(String::new())).enabled(false));
    }
    for (i, app) in apps.iter().enumerate() {
        let label = if i == 0 { format!("{} (default)", app.name) } else { app.name.clone() };
        menu = menu.item(MenuItem::action(label, OpenWith(app.id.clone())));
    }
    if apps.len() > 1 && !crate::env::get().demo {
        let mut set = Menu::new("Set as default");
        for app in &apps[1..] {
            set = set.item(MenuItem::action(app.name.clone(), SetDefaultApp(app.id.clone())));
        }
        menu = menu.item(MenuItem::separator()).item(MenuItem::submenu(set));
    }
    menu
}

/// What the empty-space menu shows checked.
#[derive(Clone, Copy, Debug)]
pub struct Here {
    pub show_hidden: bool,
    pub sort: trove_fs::SortOrder,
    pub view: View,
    pub in_trash: bool,
    pub recent: bool,
}

/// The menu on a folder's empty space.
/// `templates` is the last read of the templates folder, `None` while the first is under way.
pub fn folder(menu: ContextMenu, h: &Here, templates: Option<&[Node]>) -> ContextMenu {
    let mut menu = menu;
    if h.in_trash {
        menu = menu.item_destructive("Empty Trash", EmptyTrash).separator();
    } else if !h.recent {
        menu = menu
            .item("New folder", NewFolder)
            .item("New file", NewFile)
            .submenu(from_template(templates))
            .item("Paste", Paste)
            .separator()
            .item("Open in terminal", OpenTerminal)
            .separator();
    }
    let key = |label: &'static str, k: SortKey| MenuItem::action(label, SortBy(k)).checked(h.sort.key == k);
    let sort = Menu::new("Sort by").items([
        key("Name", SortKey::Name),
        key("Size", SortKey::Size),
        key("Kind", SortKey::Kind),
        key("Modified", SortKey::Modified),
        key("Created", SortKey::Created),
        MenuItem::separator(),
        MenuItem::action("Ascending", SortAscending).checked(h.sort.ascending),
        MenuItem::action("Descending", SortDescending).checked(!h.sort.ascending),
        MenuItem::separator(),
        MenuItem::action("Folders first", ToggleFoldersFirst).checked(h.sort.folders_first),
    ]);
    let view = Menu::new("View").items([
        MenuItem::action("List", ListView).checked(h.view == View::List),
        MenuItem::action("Grid", GridView).checked(h.view == View::Grid),
        MenuItem::action("Columns", ColumnsView).checked(h.view == View::Columns),
    ]);
    let menu = menu
        .entry(MenuItem::action("Show hidden files", ToggleHidden).checked(h.show_hidden))
        .submenu(sort)
        .submenu(view);
    if h.recent { menu } else { menu.separator().item("Properties\u{2026}", Properties) }
}

/// "New from template": the templates folder's files, a subfolder as a submenu of its own.
fn from_template(templates: Option<&[Node]>) -> Menu {
    let mut menu = Menu::new("New from template");
    let none = |label: &'static str| MenuItem::action(label, NewFromTemplate(PathBuf::new())).enabled(false);
    match templates {
        None => menu.item(none("Looking\u{2026}")),
        Some([]) => {
            menu = menu.item(none("No templates"));
            match &crate::env::get().templates {
                Some(dir) => menu
                    .item(MenuItem::separator())
                    .item(MenuItem::action("Open templates folder", OpenTemplatesFolder(dir.clone()))),
                None => menu,
            }
        }
        Some(nodes) => menu.items(nodes.iter().map(template_item)),
    }
}

fn template_item(node: &Node) -> MenuItem {
    match &node.children {
        Some(children) => MenuItem::submenu(Menu::new(node.label.clone()).items(children.iter().map(template_item))),
        None => MenuItem::action(node.label.clone(), NewFromTemplate(node.path.clone())),
    }
}

/// The menu on a path bar segment: the folder it names, then the folders beside it to jump to.
/// `siblings` is `None` while they are still being read.
pub fn crumb(menu: ContextMenu, path: &Path, siblings: Option<&[String]>) -> ContextMenu {
    let dir = path.to_path_buf();
    let menu = menu
        .item("Open in new tab", OpenPlaceInNewTab(Location::Dir(dir.clone())))
        .item("Open in terminal", OpenTerminalAt(dir.clone()))
        .item("Copy path", CopyPathOf(dir));
    let Some(parent) = path.parent() else { return menu };
    let own = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut beside = Menu::new("Folders beside it");
    match siblings {
        None => beside = beside.item(MenuItem::action("Looking\u{2026}", GoTo(parent.to_path_buf())).enabled(false)),
        Some([]) => {
            beside = beside.item(MenuItem::action("No other folders", GoTo(parent.to_path_buf())).enabled(false))
        }
        Some(names) => {
            for name in names {
                beside = beside.item(MenuItem::action(name.clone(), GoTo(parent.join(name))).checked(*name == own));
            }
        }
    }
    menu.separator().submenu(beside)
}

/// What a sidebar row's menu acts on.
#[derive(Clone, Debug)]
pub enum Place {
    Folder { path: PathBuf, bookmark: bool },
    Volume { mount: PathBuf, ejectable: bool },
    Trash,
    Recent,
}

pub fn sidebar(menu: ContextMenu, place: &Place) -> ContextMenu {
    match place {
        Place::Folder { path, bookmark } => {
            let menu = menu.item("Open in new tab", OpenPlaceInNewTab(Location::Dir(path.clone()))).separator();
            if *bookmark {
                menu.item("Remove from bookmarks", RemoveBookmark(path.clone()))
            } else {
                menu.item("Add to bookmarks", BookmarkPlace(path.clone()))
            }
        }
        Place::Volume { mount, ejectable } => {
            let menu = menu.item("Open in new tab", OpenPlaceInNewTab(Location::Dir(mount.clone())));
            if *ejectable { menu.separator().item("Eject", Eject(mount.clone())) } else { menu }
        }
        Place::Trash => menu
            .item("Open in new tab", OpenPlaceInNewTab(Location::Dir(crate::env::get().trash.clone())))
            .separator()
            .item_destructive("Empty Trash", EmptyTrash),
        Place::Recent => menu.item("Open in new tab", OpenPlaceInNewTab(Location::Recent)),
    }
}

/// The menu on the tab with id `tab`.
pub fn tab(menu: ContextMenu, tab: u64, count: usize) -> ContextMenu {
    let menu = menu.item("Duplicate tab", DuplicateTab(tab)).separator().item("Close tab", CloseTabWithId(tab));
    if count > 1 { menu.item("Close other tabs", CloseOtherTabs(tab)) } else { menu }
}
