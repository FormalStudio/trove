#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: "New from template": the `XDG_TEMPLATES_DIR` line, the menu the templates folder makes,
//! a template copied in and renamed on the spot, and the empty state. Each test runs on a fixture
//! of its own with the templates folder inside it.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use nd::components::services::{AlertCallback, Chrome, ChromeServices, OpenCallback, SaveCallback};
use nd::components::{Alert, AlertResponse, ContextMenu, ContextMenuRequested, Menu, MenuItem, OpenDialog, SaveDialog};
use nd::core::{NodeId, WindowId};
use nd::prelude::*;
use nd::test::{Size, TestApp, by_test_id};
use trove::actions::*;
use trove::menus::{self, Here};
use trove::templates;
use trove::{Root, Tab, View};
use trove_fs::{SortKey, SortOrder, parse_templates_dir};

#[derive(Default)]
struct Platform {
    menus: RefCell<Vec<ContextMenu>>,
}

impl ChromeServices for Platform {
    fn set_menus(&self, _: &mut App, _: Vec<Menu>) {}
    fn show_context_menu(&self, _: &mut App, _: WindowId, _: NodeId, _: f32, _: f32, menu: ContextMenu) {
        self.menus.borrow_mut().push(menu);
    }
    fn alert(&self, cx: &mut App, _: WindowId, _: Alert, done: AlertCallback) {
        done(AlertResponse(0), cx);
    }
    fn open_dialog(&self, cx: &mut App, _: WindowId, _: OpenDialog, done: OpenCallback) {
        done(None, cx);
    }
    fn save_dialog(&self, cx: &mut App, _: WindowId, _: SaveDialog, done: SaveCallback) {
        done(None, cx);
    }
}

/// Every label in a menu, a submenu as "Title > item" and a disabled item in parentheses.
fn labels(items: &[MenuItem]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            MenuItem::Action { label, enabled, .. } if *enabled => out.push(label.to_string()),
            MenuItem::Action { label, .. } => out.push(format!("({label})")),
            MenuItem::Submenu(menu) => {
                for inner in labels(&menu.items) {
                    out.push(format!("{} > {inner}", menu.title));
                }
            }
            MenuItem::Separator => out.push("-".into()),
            MenuItem::Role(_) => {}
        }
    }
    out
}

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
    platform: Rc<Platform>,
    home: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A window on a fresh fixture showing `work`, with `templates` (paths, a trailing slash
    /// for a folder) written into the fixture's `Templates`.
    fn new(templates: &[&str]) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let home = trove::demo_root_in(dir.path());
        let work = home.join("work");
        fs::create_dir_all(&work).unwrap();
        for t in templates {
            let path = home.join("Templates").join(t);
            if t.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, t.as_bytes()).unwrap();
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

    fn act(&mut self, action: impl Action) {
        let id = self.window.id();
        self.cx.dispatch_action(id, &action);
        self.settle();
    }

    fn tab(&self) -> Entity<Tab> {
        self.root().read(&self.cx).active_tab().clone()
    }

    fn on_disk(&self) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(self.work()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// Right-clicks the listing's empty space; the first menu after a fresh start is made before
    /// the folder has been read, so this opens it twice and returns the second.
    fn folder_menu(&mut self) -> Vec<String> {
        let mut last = Vec::new();
        for _ in 0..2 {
            let id = self.window.id();
            let node = by_test_id(&mut self.cx, id, "files")
                .unwrap_or_else(|| panic!("no files in\n{}", self.cx.semantic_tree().snapshot()))
                .id;
            self.cx.emit(node, ContextMenuRequested { node, x: 4., y: 4. });
            self.settle();
            last = labels(&self.platform.menus.borrow_mut().pop().expect("a context menu").items);
        }
        last
    }
}

fn here() -> Here {
    Here {
        show_hidden: false,
        sort: SortOrder { key: SortKey::Name, ascending: true, folders_first: true },
        view: View::List,
        in_trash: false,
        recent: false,
    }
}

#[test]
fn the_templates_line_follows_xdg_user_dirs() {
    let home = Path::new("/home/me");
    assert_eq!(parse_templates_dir("", home), Some(home.join("Templates")));
    assert_eq!(parse_templates_dir("XDG_TEMPLATES_DIR=\"$HOME/Vorlagen\"\n", home), Some(home.join("Vorlagen")));
    assert_eq!(parse_templates_dir("XDG_TEMPLATES_DIR=\"${HOME}/.tpl\"", home), Some(home.join(".tpl")));
    assert_eq!(parse_templates_dir("XDG_TEMPLATES_DIR=\"/srv/templates\"", home), Some("/srv/templates".into()));
    assert_eq!(parse_templates_dir("XDG_TEMPLATES_DIR=\"$HOME/\"", home), None, "$HOME means disabled");
    assert_eq!(parse_templates_dir("XDG_TEMPLATES_DIR=\"$HOME\"", home), None);
    let last_wins = "XDG_TEMPLATES_DIR=\"$HOME/a\"\nXDG_TEMPLATES_DIR=\"$HOME/b\"\n";
    assert_eq!(parse_templates_dir(last_wins, home), Some(home.join("b")));
}

#[test]
fn the_menu_lists_templates_by_name_with_subfolders_as_submenus() {
    let mut h = Harness::new(&["Letter.odt", "Notes.md", "Work/Invoice.ods", "Work/Quote.ods", "Empty/", ".hidden"]);
    let found = templates::read(&h.home.join("Templates"));
    let l = labels(&menus::folder(ContextMenu::default(), &here(), Some(&found)).items);
    let want = [
        "New from template > Work > Invoice",
        "New from template > Work > Quote",
        "New from template > Letter",
        "New from template > Notes",
    ];
    let got: Vec<&String> = l.iter().filter(|s| s.starts_with("New from template")).collect();
    assert_eq!(got, want, "folders first, no extensions, no hidden files, no empty folders");

    let through_the_window = h.folder_menu();
    assert!(through_the_window.iter().any(|s| s == "New from template > Letter"), "{through_the_window:?}");
}

#[test]
fn choosing_a_template_copies_it_and_starts_the_rename_on_the_stem() {
    let mut h = Harness::new(&["Letter.odt"]);
    let template = h.home.join("Templates/Letter.odt");
    h.act(NewFromTemplate(template.clone()));
    assert_eq!(h.on_disk(), ["Letter.odt"]);
    assert_eq!(fs::read(h.work().join("Letter.odt")).unwrap(), b"Letter.odt");
    let rename = h.tab().read(&h.cx).renaming().cloned().expect("renaming the copy");
    assert_eq!(rename.original, "Letter.odt");
    assert_eq!(trove::stem_chars(&rename.original, false), "Letter".len(), "the stem is selected");
    assert_eq!(h.tab().read(&h.cx).selected_names(), ["Letter.odt"]);

    h.act(NewFromTemplate(template.clone()));
    assert_eq!(h.on_disk(), ["Letter 2.odt", "Letter.odt"], "a taken name counts up");
    assert!(template.exists(), "the template stays where it was");

    h.act(Undo);
    assert_eq!(h.on_disk(), ["Letter.odt"], "undo takes the second copy away");
}

#[test]
fn no_templates_says_so_and_opens_the_folder_made_on_the_spot() {
    let mut h = Harness::new(&[]);
    let templates_dir = h.home.join("Templates");
    assert!(!templates_dir.exists());
    let l = h.folder_menu();
    assert!(l.iter().any(|s| s == "New from template > (No templates)"), "{l:?}");
    assert!(l.iter().any(|s| s == "New from template > Open templates folder"), "{l:?}");

    let id = h.window.id();
    h.cx.dispatch_action(id, &OpenTemplatesFolder(templates_dir.clone()));
    // The folder is made off the main thread; its follow-up lands outside any root update.
    for _ in 0..20 {
        if trove::ops::run_deferred(&mut h.cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    h.settle();
    assert!(templates_dir.is_dir(), "the folder is made when missing");
    assert_eq!(h.tab().read(&h.cx).location().dir(), Some(templates_dir.as_path()));
}

#[test]
fn an_empty_templates_folder_is_the_same_empty_state() {
    let mut h = Harness::new(&["Empty/"]);
    let l = h.folder_menu();
    assert!(l.iter().any(|s| s == "New from template > (No templates)"), "{l:?}");
}
