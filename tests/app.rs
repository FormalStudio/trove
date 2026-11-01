//! Tier 1: the window on `TestBackend`, over the demo fixture. No window server; listings run
//! on their real threads and each step drains them before reading.

use std::path::PathBuf;
use std::time::Duration;

use nd::prelude::*;
use nd::test::{Size, TestApp};
use trove::actions::*;
use trove::{Location, Root, Tab, View};
use trove_fs::{SortKey, SortOrder};

const ROOT_NAMES: &[&str] =
    &["Desktop", "Documents", "Downloads", "Logs", "Music", "Pictures", "Projects", "Videos", "Notes.md", "todo.txt"];

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
}

impl Harness {
    fn new() -> Self {
        let mut cx = TestApp::new();
        let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
            trove::init(cx);
            let home = trove::demo_root();
            cx.new(|cx| Root::new(trove::Start::Path(home), window, cx))
        });
        let mut h = Self { cx, window };
        h.settle();
        h
    }

    fn root(&self) -> Entity<Root> {
        self.window.root().clone()
    }

    fn settle(&mut self) {
        self.cx.run_until_idle();
        self.root().update(&mut self.cx, |r, cx| r.drain(Duration::from_secs(5), cx));
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

    fn names(&self) -> Vec<String> {
        self.tab().read(&self.cx).visible_names()
    }

    fn selected(&self) -> Vec<String> {
        self.tab().read(&self.cx).selected_names()
    }

    fn location(&self) -> Location {
        self.tab().read(&self.cx).location().clone()
    }

    fn demo(rel: &str) -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let root = base.join("trove-demo");
        if rel.is_empty() { root } else { root.join(rel) }
    }

    fn go(&mut self, rel: &str) {
        self.act(GoTo(Self::demo(rel)));
    }
}

#[test]
fn lists_folders_first_in_natural_order() {
    let h = Harness::new();
    assert_eq!(h.location(), Location::Dir(Harness::demo("")));
    assert_eq!(h.names(), ROOT_NAMES);
    let (left, _) = h.root().read(&h.cx).status().read(&h.cx).summary(&h.cx);
    assert_eq!(left, "10 items");
}

#[test]
fn natural_order_puts_2_before_10() {
    let mut h = Harness::new();
    h.go("Documents");
    let names = h.names();
    assert_eq!(names[0], "Taxes 2025");
    let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert!(pos("chapter1.md") < pos("chapter2.md") && pos("chapter2.md") < pos("chapter10.md"));
}

#[test]
fn back_forward_and_parent() {
    let mut h = Harness::new();
    h.go("Documents");
    h.go("Documents/Taxes 2025");
    assert_eq!(h.names(), ["Return.pdf"]);

    h.act(Back);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Documents")));
    assert_eq!(h.selected(), ["Taxes 2025"], "going back selects the folder just left");

    h.act(Forward);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Documents/Taxes 2025")));

    h.act(Parent);
    h.act(Parent);
    assert_eq!(h.location(), Location::Dir(Harness::demo("")));
    assert_eq!(h.selected(), ["Documents"]);
    assert!(h.tab().read(&h.cx).can_go_back());
}

#[test]
fn open_folder_and_into_folder() {
    let mut h = Harness::new();
    h.act(TypeSelect('m'));
    assert_eq!(h.selected(), ["Music"]);
    h.act(Open);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Music")));
    h.act(MoveDown);
    assert_eq!(h.selected(), ["Album"]);
    h.act(IntoFolder);
    assert_eq!(h.names()[0], "01 Opening.flac");
    assert_eq!(h.names()[3], "10 Encore.flac");
}

#[test]
fn tabs_keep_their_own_state() {
    let mut h = Harness::new();
    h.act(NewTab);
    assert_eq!(h.root().read(&h.cx).tabs().len(), 2);
    assert_eq!(h.root().read(&h.cx).active_index(), 1);
    h.go("Music");
    h.act(GridView);
    assert_eq!(h.tab().read(&h.cx).view(), View::Grid);

    h.act(SelectTab(0));
    assert_eq!(h.location(), Location::Dir(Harness::demo("")));
    assert_eq!(h.tab().read(&h.cx).view(), View::List);
    assert_eq!(h.names(), ROOT_NAMES);

    h.act(NextTab);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Music")));
    assert_eq!(h.tab().read(&h.cx).view(), View::Grid);
    h.act(PreviousTab);
    assert_eq!(h.root().read(&h.cx).active_index(), 0);

    h.act(SelectTab(1));
    h.act(CloseTab);
    assert_eq!(h.root().read(&h.cx).tabs().len(), 1);
    assert_eq!(h.location(), Location::Dir(Harness::demo("")));
}

#[test]
fn open_in_new_tab_stays_on_the_current_one() {
    let mut h = Harness::new();
    h.act(TypeSelect('p'));
    h.act(TypeSelect('r'));
    assert_eq!(h.selected(), ["Projects"]);
    h.act(OpenInNewTab);
    let tabs = h.root().read(&h.cx).tabs();
    assert_eq!(tabs.len(), 2);
    assert_eq!(h.root().read(&h.cx).active_index(), 0);
    h.settle();
    assert_eq!(tabs[1].read(&h.cx).location(), &Location::Dir(Harness::demo("Projects")));
    assert_eq!(tabs[1].read(&h.cx).title(), "Projects");
}

#[test]
fn sort_by_size_descending() {
    let mut h = Harness::new();
    h.go("Downloads");
    h.tab().update(&mut h.cx, |t, cx| {
        t.set_sort(SortOrder { key: SortKey::Size, ascending: false, folders_first: true }, cx)
    });
    h.settle();
    assert_eq!(
        h.names(),
        ["trove-0.1.0.tar.gz", "photos.zip", "setup.AppImage", "openSUSE-Tumbleweed.iso", "Inter.ttf"]
    );
    h.act(NewTab);
    assert_eq!(h.tab().read(&h.cx).sort().key, SortKey::Name, "each tab sorts on its own");
}

#[test]
fn hidden_files_toggle() {
    let mut h = Harness::new();
    assert!(!h.names().iter().any(|n| n.starts_with('.')));
    h.act(ToggleHidden);
    let names = h.names();
    assert!(names.contains(&".config".to_owned()) && names.contains(&".bashrc".to_owned()));
    h.act(ToggleHidden);
    assert_eq!(h.names(), ROOT_NAMES);
}

#[test]
fn type_select_builds_a_prefix() {
    let mut h = Harness::new();
    h.go("Documents");
    h.act(TypeSelect('c'));
    h.act(TypeSelect('h'));
    assert_eq!(h.selected(), ["chapter1.md"]);
    h.act(TypeSelect('a'));
    h.act(TypeSelect('p'));
    h.act(TypeSelect('t'));
    h.act(TypeSelect('e'));
    h.act(TypeSelect('r'));
    h.act(TypeSelect('1'));
    h.act(TypeSelect('0'));
    assert_eq!(h.selected(), ["chapter10.md"]);
}

#[test]
fn keyboard_selection_and_select_all() {
    let mut h = Harness::new();
    h.act(MoveDown);
    assert_eq!(h.selected(), ["Desktop"]);
    h.act(MoveDown);
    h.act(ExtendDown);
    assert_eq!(h.selected(), ["Documents", "Downloads"]);
    h.act(MoveEnd);
    assert_eq!(h.selected(), ["todo.txt"]);
    h.act(SelectAll);
    assert_eq!(h.selected().len(), ROOT_NAMES.len());
    let (left, _) = h.root().read(&h.cx).status().read(&h.cx).summary(&h.cx);
    assert!(left.starts_with("10 items \u{b7} 10 selected ("), "{left}");
    h.act(Deselect);
    assert!(h.selected().is_empty());
}

#[test]
fn search_filters_by_name_and_escape_clears() {
    let mut h = Harness::new();
    let window = h.window.id();
    let field = nd::test::by_test_id(&mut h.cx, window, "search").expect("a search field");
    h.cx.emit(field.id, nd::components::TextChanged { text: "do".into() });
    h.settle();
    assert_eq!(h.names(), ["Documents", "Downloads", "todo.txt"]);
    let (left, _) = h.root().read(&h.cx).status().read(&h.cx).summary(&h.cx);
    assert_eq!(left, "3 of 10 items");

    h.act(ClearSearch);
    assert_eq!(h.names(), ROOT_NAMES);

    h.cx.emit(field.id, nd::components::TextChanged { text: "zzz".into() });
    h.settle();
    assert!(h.names().is_empty());
    h.go("Music");
    assert_eq!(h.tab().read(&h.cx).query(), "", "navigating clears the search");
}

#[test]
fn a_missing_folder_shows_why() {
    let mut h = Harness::new();
    h.go("Nowhere");
    assert!(matches!(h.tab().read(&h.cx).load_state(), trove::Load::Failed(trove_fs::ListError::NotFound)));
    let window = h.window.id();
    assert!(nd::test::by_test_id(&mut h.cx, window, "listing-error").is_some());
    assert!(nd::test::text(&mut h.cx, window).contains("Folder not found"));
}

#[test]
fn a_file_path_opens_its_folder_with_the_file_selected() {
    let mut cx = TestApp::new();
    let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
        trove::init(cx);
        let file = trove::demo_root().join("Documents/Roadmap.pptx");
        cx.new(|cx| Root::new(trove::Start::Path(file), window, cx))
    });
    let root = window.root().clone();
    for _ in 0..3 {
        cx.run_until_idle();
        root.update(&mut cx, |r, cx| r.drain(Duration::from_secs(5), cx));
    }
    let tab = root.read(&cx).active_tab().clone();
    assert_eq!(tab.read(&cx).location(), &Location::Dir(Harness::demo("Documents")));
    assert_eq!(tab.read(&cx).selected_names(), ["Roadmap.pptx"]);
}

#[test]
fn the_sidebar_opens_recent_newest_first() {
    let mut h = Harness::new();
    let window = h.window.id();
    let places = nd::test::by_test_id(&mut h.cx, window, "places").expect("the sidebar's list");
    h.cx.emit(places.id, nd::components::SourceSelected { id: "recent".into() });
    h.settle();
    assert_eq!(h.location(), Location::Recent);
    assert_eq!(h.names().len(), 5);
    assert_eq!(h.names()[0], "Quarterly report.docx");
    assert_eq!(h.tab().read(&h.cx).title(), "Recent");
}
