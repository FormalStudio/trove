#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: file operations through the window, on `TestBackend`, each test on its own copy of
//! the demo fixture in a temp dir under the target folder. A fake platform keeps the context
//! menus it was asked to show and answers alerts with their first button.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use nd::components::services::{AlertCallback, Chrome, ChromeServices, OpenCallback, SaveCallback};
use nd::components::{
    Alert, AlertResponse, ContextMenu, ContextMenuRequested, Menu, MenuItem, OpenDialog, SaveDialog,
    SourceMenuRequested, TabMenuRequested, TextCancelled, TextEditingEnded,
};
use nd::core::{NodeId, WindowId};
use nd::prelude::*;
use nd::test::{Size, TestApp, by_test_id};
use trove::actions::*;
use trove::{Location, Root, Tab};
use trove_ops::{Resolution, ResolutionChoice};

#[derive(Default)]
struct Platform {
    menus: RefCell<Vec<Vec<String>>>,
    alerts: RefCell<Vec<String>>,
}

fn labels(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Action { label, enabled, .. } if *enabled => label.to_string(),
            MenuItem::Action { label, .. } => format!("({label})"),
            MenuItem::Submenu(m) => format!("{} >", m.title),
            MenuItem::Separator => "-".to_owned(),
            MenuItem::Role(_) => "?".to_owned(),
        })
        .collect()
}

impl ChromeServices for Platform {
    fn set_menus(&self, _: &mut App, _: Vec<Menu>) {}
    fn show_context_menu(&self, _: &mut App, _: WindowId, _: NodeId, _: f32, _: f32, menu: ContextMenu) {
        self.menus.borrow_mut().push(labels(&menu.items));
    }
    fn alert(&self, cx: &mut App, _: WindowId, alert: Alert, done: AlertCallback) {
        self.alerts.borrow_mut().push(alert.title.to_string());
        done(AlertResponse(0), cx);
    }
    fn open_dialog(&self, cx: &mut App, _: WindowId, _: OpenDialog, done: OpenCallback) {
        done(None, cx);
    }
    fn save_dialog(&self, cx: &mut App, _: WindowId, _: SaveDialog, done: SaveCallback) {
        done(None, cx);
    }
}

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
    platform: Rc<Platform>,
    home: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A window on a fresh fixture, showing `work`, a folder of its own holding `files`.
    fn new(files: &[&str]) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let home = trove::demo_root_in(dir.path());
        let work = home.join("work");
        fs::create_dir_all(&work).unwrap();
        for f in files {
            if let Some(d) = f.strip_suffix('/') {
                fs::create_dir_all(work.join(d)).unwrap();
            } else {
                fs::write(work.join(f), f.as_bytes()).unwrap();
            }
        }
        let platform = Rc::new(Platform::default());
        let mut cx = TestApp::new();
        cx.set_global(Chrome(platform.clone()));
        let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
            trove::init(cx);
            cx.new(|cx| Root::new(trove::Start::Path(work), window, cx))
        });
        let mut h = Self { cx, window, platform, home, _dir: dir };
        h.settle();
        h
    }

    fn work(&self) -> PathBuf {
        self.home.join("work")
    }

    fn root(&self) -> Entity<Root> {
        self.window.root().clone()
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.cx.run_until_idle();
            self.root().update(&mut self.cx, |r, cx| r.drain(Duration::from_secs(5), cx));
        }
        self.cx.run_until_idle();
    }

    /// Settles until `done` holds: a removal reaches a listing through its watch, which no
    /// job result waits for.
    fn eventually(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        for _ in 0..60 {
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
            self.settle();
        }
        panic!("{what}: still {:?}", self.names());
    }

    fn act(&mut self, action: impl Action) {
        let id = self.window.id();
        self.cx.dispatch_action(id, &action);
        self.settle();
    }

    fn tab(&self) -> Entity<Tab> {
        self.root().read(&self.cx).active_tab().clone()
    }

    fn names(&self) -> Vec<String> {
        self.tab().read(&self.cx).visible_names()
    }

    fn selected(&self) -> Vec<String> {
        self.tab().read(&self.cx).selected_names()
    }

    fn select(&mut self, names: &[&str]) {
        self.tab().update(&mut self.cx, |t, cx| t.select_names(names, cx));
        self.cx.run_until_idle();
        assert_eq!(self.selected(), names, "selecting {names:?} in {:?}", self.names());
    }

    fn go(&mut self, path: &Path) {
        self.act(GoTo(path.to_path_buf()));
    }

    fn on_disk(&self) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(self.work()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn node(&mut self, test_id: &str) -> NodeId {
        let id = self.window.id();
        by_test_id(&mut self.cx, id, test_id)
            .unwrap_or_else(|| panic!("no `{test_id}` in\n{}", self.cx.semantic_tree().snapshot()))
            .id
    }

    /// Return in the rename field, or focus leaving it: both end the edit with its text.
    fn rename_to(&mut self, text: &str) {
        let field = self.node(trove::RENAME_FIELD);
        self.cx.emit(field, TextEditingEnded { text: text.to_owned().into() });
        self.settle();
    }

    fn cancel_rename(&mut self) {
        let field = self.node(trove::RENAME_FIELD);
        self.cx.emit(field, TextCancelled);
        self.settle();
    }

    fn renaming(&self) -> Option<trove::Rename> {
        self.tab().read(&self.cx).renaming().cloned()
    }

    /// Right-clicks the tab with id `tab` and returns its menu.
    fn menu_on_tab(&mut self, tab: u64) -> Vec<String> {
        let node = self.node("tabs");
        self.cx.emit(node, TabMenuRequested { node, id: format!("tab-{tab}").into(), x: 4., y: 4. });
        self.settle();
        self.platform.menus.borrow_mut().pop().expect("a tab menu")
    }

    /// Right-clicks the sidebar row `id` and returns its menu.
    fn menu_on_place(&mut self, id: &str) -> Vec<String> {
        let node = self.node("places");
        self.cx.emit(node, SourceMenuRequested { node, id: id.to_owned().into(), x: 4., y: 4. });
        self.settle();
        self.platform.menus.borrow_mut().pop().expect("a sidebar menu")
    }

    /// Right-clicks the node with `test_id` and returns the menu the platform was asked for.
    fn menu_on(&mut self, test_id: &str) -> Vec<String> {
        let id = self.window.id();
        let node = by_test_id(&mut self.cx, id, test_id)
            .unwrap_or_else(|| panic!("no `{test_id}` in\n{}", self.cx.semantic_tree().snapshot()))
            .id;
        self.cx.emit(node, ContextMenuRequested { node, x: 4., y: 4. });
        self.settle();
        self.platform.menus.borrow_mut().pop().expect("a context menu")
    }
}

#[test]
fn copy_and_paste_beside_the_original_makes_a_copy_and_selects_it() {
    let mut h = Harness::new(&["a.txt", "b.txt"]);
    h.select(&["a.txt"]);
    h.act(Copy);
    h.act(Paste);
    assert_eq!(h.on_disk(), ["a copy.txt", "a.txt", "b.txt"]);
    assert_eq!(h.selected(), ["a copy.txt"], "the copy is selected once it lands");
}

#[test]
fn cut_dims_until_pasted_and_then_moves() {
    let mut h = Harness::new(&["a.txt", "dest/"]);
    h.select(&["a.txt"]);
    h.act(Cut);
    assert_eq!(h.tab().read(&h.cx).cut_names(), ["a.txt"]);
    let dest = h.work().join("dest");
    h.go(&dest);
    h.act(Paste);
    assert_eq!(h.names(), ["a.txt"]);
    assert!(!h.work().join("a.txt").exists());
    assert!(h.tab().read(&h.cx).cut_names().is_empty(), "a pasted cut is spent");
}

#[test]
fn rename_inline_commits_cancels_and_reports_errors() {
    let mut h = Harness::new(&["notes.txt", "taken.txt"]);
    h.select(&["notes.txt"]);
    h.act(Rename);
    let r = h.renaming().expect("renaming");
    assert_eq!(r.text.as_ref(), "notes.txt");

    h.cancel_rename();
    assert!(h.renaming().is_none());
    assert_eq!(h.on_disk(), ["notes.txt", "taken.txt"]);

    h.act(Rename);
    h.rename_to("notes.txt");
    assert!(h.renaming().is_none(), "an unchanged name ends the rename");
    assert_eq!(h.on_disk(), ["notes.txt", "taken.txt"]);

    h.act(Rename);
    h.rename_to("a/b");
    assert!(h.renaming().and_then(|r| r.error).is_some(), "a slash keeps the field with a reason");
    h.rename_to("taken.txt");
    assert!(h.renaming().and_then(|r| r.error).is_some(), "a taken name keeps the field with a reason");
    assert_eq!(h.on_disk(), ["notes.txt", "taken.txt"]);

    h.rename_to("ideas.txt");
    assert!(h.renaming().is_none());
    assert_eq!(h.on_disk(), ["ideas.txt", "taken.txt"]);
    assert_eq!(h.selected(), ["ideas.txt"]);

    h.act(Undo);
    assert_eq!(h.on_disk(), ["notes.txt", "taken.txt"]);
    h.act(Redo);
    assert_eq!(h.on_disk(), ["ideas.txt", "taken.txt"]);
}

#[test]
fn new_folder_lands_selected_and_in_rename() {
    let mut h = Harness::new(&["a.txt"]);
    h.act(NewFolder);
    assert!(h.work().join("untitled folder").is_dir());
    assert_eq!(h.selected(), ["untitled folder"]);
    let r = h.renaming().expect("the new folder is in rename");
    assert_eq!(r.text.as_ref(), "untitled folder");
    h.rename_to("Plans");
    assert!(h.work().join("Plans").is_dir());
}

#[test]
fn trash_and_undo_and_the_trash_view() {
    let mut h = Harness::new(&["old.txt", "keep.txt"]);
    h.select(&["old.txt"]);
    h.act(Trash);
    assert_eq!(h.on_disk(), ["keep.txt"]);
    h.act(Undo);
    assert_eq!(h.on_disk(), ["keep.txt", "old.txt"]);

    h.select(&["old.txt"]);
    h.act(Trash);
    let trash = h.home.join(".local/share/Trash/files");
    h.go(&trash);
    assert!(h.tab().read(&h.cx).is_trash());
    assert_eq!(h.names(), ["old.txt"]);
    let info = h.tab().read(&h.cx).trash_info("old.txt").cloned().expect("its .trashinfo");
    assert_eq!(info.origin.as_deref(), Some(h.work().as_path()));
    assert!(info.deleted.is_some());

    h.select(&["old.txt"]);
    h.act(Restore);
    h.eventually("the restored item leaves the Trash", |h| h.names().is_empty());
    assert!(h.work().join("old.txt").exists());
}

#[test]
fn delete_permanently_asks_first() {
    let mut h = Harness::new(&["gone.txt", "keep.txt"]);
    h.select(&["gone.txt"]);
    h.act(DeletePermanently);
    assert_eq!(h.platform.alerts.borrow().as_slice(), ["Delete \u{201c}gone.txt\u{201d} permanently?"]);
    assert_eq!(h.on_disk(), ["keep.txt"]);
}

#[test]
fn a_conflict_waits_for_an_answer() {
    let mut h = Harness::new(&["a.txt", "other/", "other/a.txt"]);
    h.select(&["a.txt"]);
    h.act(Copy);
    let other = h.work().join("other");
    h.go(&other);
    h.act(Paste);
    let jobs = trove::ops::jobs(&h.cx);
    let ask = jobs.read(&h.cx).asking().cloned().expect("a conflict to answer");
    assert_eq!(ask.existing, other.join("a.txt"));
    jobs.update(&mut h.cx, |j, cx| {
        j.answer(ResolutionChoice { resolution: Resolution::KeepBoth, apply_to_all: false }, cx)
    });
    h.settle();
    assert_eq!(h.names(), ["a 2.txt", "a.txt"]);
    assert!(jobs.read(&h.cx).asking().is_none());
}

#[test]
fn each_tab_has_its_own_menu() {
    let mut h = Harness::new(&["a.txt", "b/"]);
    let root = h.root();
    let first = root.read(&h.cx).tab_ids()[0];
    h.act(DuplicateTab(first));
    assert_eq!(root.read(&h.cx).tabs().len(), 2);
    assert_eq!(h.tab().read(&h.cx).location(), &Location::Dir(h.work()));
    let second = root.read(&h.cx).tab_ids()[1];
    assert_eq!(h.menu_on_tab(first), ["Duplicate tab", "-", "Close tab", "Close other tabs"]);
    assert_eq!(root.read(&h.cx).active_index(), 1, "the menu leaves the tab it opened on unselected");

    h.act(CloseOtherTabs(first));
    assert_eq!(root.read(&h.cx).tab_ids(), [first], "the tab the menu opened on stays, not the active one");
    h.act(DuplicateTab(first));
    h.act(CloseTabWithId(first));
    assert_eq!(root.read(&h.cx).tab_ids().len(), 1);
    assert_ne!(root.read(&h.cx).tab_ids()[0], second);
}

#[test]
fn each_sidebar_row_has_its_own_menu_and_a_middle_click_opens_a_tab() {
    let mut h = Harness::new(&["a.txt", "b/"]);
    let documents = h.home.join("Documents");
    let id = format!("place:{}", documents.display());
    assert_eq!(h.menu_on_place(&id), ["Open in new tab", "-", "Add to bookmarks"]);
    assert_eq!(h.menu_on_place("trash"), ["Open in new tab", "-", "Empty Trash"]);
    assert_eq!(h.menu_on_place("recent"), ["Open in new tab"]);
    assert_eq!(h.tab().read(&h.cx).location(), &Location::Dir(h.work()), "a right click opens no place");

    h.act(BookmarkPlace(documents.clone()));
    let bookmark = h.menu_on_place(&id);
    assert_eq!(bookmark, ["Open in new tab", "-", "Remove from bookmarks"], "{bookmark:?}");

    let places = h.node("places");
    h.cx.emit(places, nd::components::SourceMiddleClicked { id: id.into() });
    h.settle();
    let root = h.root();
    let tabs = root.read(&h.cx).tabs();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1].read(&h.cx).location(), &Location::Dir(documents));
    assert_eq!(root.read(&h.cx).active_index(), 0, "the new tab opens behind the active one");
}

#[test]
fn the_empty_space_menu_offers_new_items_and_view_options() {
    let mut h = Harness::new(&["a.txt"]);
    let menu = h.menu_on("files");
    assert_eq!(menu, ["New folder", "New file", "Paste", "-", "Show hidden files", "Sort by >", "View >"], "{menu:?}");
}
