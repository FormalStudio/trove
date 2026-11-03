//! Tier 1: the window on `TestBackend`, over the demo fixture. No window server; listings run
//! on their real threads and each step drains them before reading.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use nd::prelude::*;
use nd::test::{Size, TestApp};
use trove::actions::*;
use trove::{Location, Root, Tab, View};
use trove_fs::{SortKey, SortOrder};

const ROOT_NAMES: &[&str] =
    &["Desktop", "Documents", "Downloads", "Logs", "Music", "Pictures", "Projects", "Videos", "Notes.md", "todo.txt"];

/// The fixture this process runs on, in its own folder: `nd test` gives every test a process,
/// and the fixture's settings and state files (Ctrl+H writes `show_hidden`) must not reach a
/// test beside it.
fn fixture() -> &'static PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = tempfile::Builder::new().prefix("trove-app").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
        trove::demo_root_in(&dir)
    })
}

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
}

impl Harness {
    fn new() -> Self {
        let home = fixture().clone();
        let mut cx = TestApp::new();
        let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
            trove::init(cx);
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

    /// What a click, a Shift range or a key in the list `test_id` reports: `names` selected,
    /// the cursor on `cursor`.
    fn pick(&mut self, test_id: &str, names: &[&str], cursor: Option<&str>) {
        let tab = self.tab();
        let (rows, cursor) = {
            let t = tab.read(&self.cx);
            let state = t.list_state();
            let row = |name: &str| {
                let id = t.visible().iter().find(|id| t.entry(id).is_some_and(|e| e.name.as_ref() == name));
                id.and_then(|id| state.row_of(id)).unwrap_or_else(|| panic!("no row {name}"))
            };
            (names.iter().map(|n| row(n)).collect::<Vec<_>>(), cursor.map(row))
        };
        let id = self.window.id();
        let node = nd::test::by_test_id(&mut self.cx, id, test_id)
            .unwrap_or_else(|| panic!("no `{test_id}` in\n{}", self.cx.semantic_tree().snapshot()))
            .id;
        self.cx.emit(node, nd::virtual_list::RowsSelected { rows, cursor });
        self.settle();
    }

    fn location(&self) -> Location {
        self.tab().read(&self.cx).location().clone()
    }

    fn demo(rel: &str) -> PathBuf {
        let root = fixture().clone();
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
    h.pick("files-table", &["Album"], Some("Album"));
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
fn the_list_owns_the_selection_and_the_cursor() {
    let mut h = Harness::new();
    h.pick("files-table", &["Documents", "Downloads"], Some("Documents"));
    assert_eq!(h.selected(), ["Documents", "Downloads"]);
    let cursor = h.tab().read(&h.cx).cursor();
    assert_eq!(
        cursor.and_then(|c| h.tab().read(&h.cx).entry(&c).map(|e| e.name.to_string())).as_deref(),
        Some("Documents")
    );
    h.act(Open);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Documents")), "Return opens the cursor's row");

    h.act(Back);
    h.pick("files-table", ROOT_NAMES, None);
    let (left, _) = h.root().read(&h.cx).status().read(&h.cx).summary(&h.cx);
    assert!(left.starts_with("10 of 10 selected, "), "{left}");
    h.pick("files-table", &[], None);
    assert!(h.selected().is_empty(), "a click on empty space clears it");
    assert_eq!(h.tab().read(&h.cx).cursor(), None);

    h.pick("files-table", &["Music"], Some("Music"));
    h.act(Deselect);
    assert!(h.selected().is_empty());
}

#[test]
fn the_list_shown_has_the_keys() {
    let mut h = Harness::new();
    let window = h.window.id();
    let focused = |h: &mut Harness, id: &str| nd::test::by_test_id(&mut h.cx, window, id).is_some_and(|n| n.focused);
    h.act(FocusFiles);
    assert!(focused(&mut h, "files-table"), "{}", h.cx.semantic_tree().snapshot());
    h.act(GridView);
    assert!(focused(&mut h, "files-grid"), "the grid takes the keys from the table it replaced");
    h.act(trove::keys::ColumnsView);
    assert!(focused(&mut h, "files-columns"), "the tab's own column has them in the columns view");
    h.act(ListView);
    assert!(focused(&mut h, "files-table"));
}

#[test]
fn the_grid_and_columns_select_through_their_own_lists() {
    let mut h = Harness::new();
    h.act(GridView);
    h.pick("files-grid", &["Pictures"], Some("Pictures"));
    assert_eq!(h.selected(), ["Pictures"]);
    h.act(trove::keys::ColumnsView);
    h.pick("files-columns", &["Videos"], Some("Videos"));
    assert_eq!(h.selected(), ["Videos"]);
}

#[test]
fn search_filters_by_name_and_escape_clears() {
    let mut h = Harness::new();
    let window = h.window.id();
    assert!(nd::test::by_test_id(&mut h.cx, window, "search").is_some(), "the search field always shows");
    h.act(FocusSearch);
    let field = nd::test::by_test_id(&mut h.cx, window, "search").expect("a search field");
    h.cx.emit(field.id, nd::components::TextChanged { text: "do".into() });
    h.settle();
    assert_eq!(h.names(), ["Documents", "Downloads", "todo.txt"]);
    let (left, _) = h.root().read(&h.cx).status().read(&h.cx).summary(&h.cx);
    assert_eq!(left, "3 results");

    h.act(ClearSearch);
    assert_eq!(h.names(), ROOT_NAMES);
    assert_eq!(h.tab().read(&h.cx).query(), "", "escape clears the search");
    assert!(
        nd::test::by_test_id(&mut h.cx, window, "files-table").is_some_and(|n| n.focused),
        "and the list has the keys"
    );

    h.act(FocusSearch);
    let field = nd::test::by_test_id(&mut h.cx, window, "search").expect("a search field");
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
        let file = Harness::demo("Documents/Roadmap.pptx");
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

#[test]
fn ctrl_f_focuses_the_search_field() {
    let mut h = Harness::new();
    h.act(FocusSearch);
    let window = h.window.id();
    let field = nd::test::by_test_id(&mut h.cx, window, "search").expect("a search field");
    assert!(field.focused, "{}", h.cx.semantic_tree().snapshot());
}

// Picker mode: the window another app's file chooser opens.

mod picking {
    use std::cell::RefCell;
    use std::rc::Rc;

    use nd::chrome::services::{AlertCallback, Chrome, ChromeServices, OpenCallback, SaveCallback};
    use nd::chrome::{Alert, AlertResponse, ContextMenu, Menu, OpenDialog, SaveDialog};
    use nd::core::{NodeId, WindowId};
    use trove::picker::{Filter, Mode, Outcome, PickAccept, PickCancel, Request};

    use super::*;

    /// A platform that keeps each alert's title and answers when the test says.
    #[derive(Default)]
    struct Alerts {
        shown: RefCell<Vec<SharedString>>,
        pending: RefCell<Vec<AlertCallback>>,
    }

    impl ChromeServices for Alerts {
        fn set_menus(&self, _: &mut App, _: Vec<Menu>) {}
        fn show_context_menu(&self, _: &mut App, _: WindowId, _: NodeId, _: f32, _: f32, _: ContextMenu) {}
        fn alert(&self, _: &mut App, _: WindowId, alert: Alert, done: AlertCallback) {
            self.shown.borrow_mut().push(alert.title);
            self.pending.borrow_mut().push(done);
        }
        fn open_dialog(&self, cx: &mut App, _: WindowId, _: OpenDialog, done: OpenCallback) {
            done(None, cx);
        }
        fn save_dialog(&self, cx: &mut App, _: WindowId, _: SaveDialog, done: SaveCallback) {
            done(None, cx);
        }
    }

    struct Picking {
        h: Harness,
        outcome: Rc<RefCell<Vec<Outcome>>>,
        alerts: Rc<Alerts>,
    }

    fn folder(rel: &str) -> Option<Vec<u8>> {
        let mut bytes = Harness::demo(rel).into_os_string().into_encoded_bytes();
        bytes.push(0);
        Some(bytes)
    }

    fn uri(rel: &str) -> String {
        trove_fs::file_uri(&Harness::demo(rel))
    }

    fn filter(name: &str, patterns: &[(u32, &str)]) -> Filter {
        Filter { name: name.into(), patterns: patterns.iter().map(|(k, p)| (*k, (*p).to_owned())).collect() }
    }

    impl Picking {
        fn new(request: Request) -> Self {
            fixture();
            let mut cx = TestApp::new();
            let alerts = Rc::new(Alerts::default());
            cx.set_global(Chrome(alerts.clone()));
            let outcome = Rc::new(RefCell::new(Vec::new()));
            let out = outcome.clone();
            let window = cx.open_window(Size { width: 1000., height: 700. }, move |window, cx| {
                trove::init(cx);
                let done = Box::new(move |o, _: &mut App| out.borrow_mut().push(o));
                cx.new(|cx| Root::picking(request, done, window, cx))
            });
            let mut h = Harness { cx, window };
            h.settle();
            Self { h, outcome, alerts }
        }

        fn outcome(&self) -> Vec<Outcome> {
            self.outcome.borrow().clone()
        }

        fn picked(&self) -> Vec<String> {
            match self.outcome().as_slice() {
                [Outcome::Picked { uris, .. }] => uris.clone(),
                other => panic!("one pick, got {other:?}"),
            }
        }

        fn name(&self) -> String {
            let root = self.h.root();
            let picker = root.read(&self.h.cx).picker().unwrap_or_else(|| panic!("a picker")).clone();
            picker.read(&self.h.cx).name().to_owned()
        }
    }

    #[test]
    fn accept_returns_the_selected_files_as_uris() {
        let request = Request { multiple: true, current_folder: folder("Documents"), ..Request::default() };
        let mut p = Picking::new(request);
        assert_eq!(p.h.location(), Location::Dir(Harness::demo("Documents")));
        p.h.pick("files-table", &["chapter1.md", "Taxes 2025", "chapter2.md"], Some("chapter2.md"));
        p.h.act(PickAccept);
        assert_eq!(p.picked(), [uri("Documents/chapter1.md"), uri("Documents/chapter2.md")], "folders stay out");
    }

    #[test]
    fn one_file_at_a_time_takes_the_cursor_and_a_folder_opens() {
        let mut p = Picking::new(Request { current_folder: folder("Documents"), ..Request::default() });
        p.h.pick("files-table", &["Taxes 2025"], Some("Taxes 2025"));
        p.h.act(Open);
        assert_eq!(p.h.location(), Location::Dir(Harness::demo("Documents/Taxes 2025")), "Enter goes into a folder");
        assert!(p.outcome().is_empty());
        p.h.pick("files-table", &["Return.pdf"], Some("Return.pdf"));
        p.h.act(Open);
        assert_eq!(p.picked(), [uri("Documents/Taxes 2025/Return.pdf")], "Enter on a file accepts it");
        p.h.act(PickCancel);
        assert_eq!(p.outcome().len(), 1, "a pick ends once");
    }

    #[test]
    fn filters_hide_files_by_glob_and_mime_type_but_never_folders() {
        let request = Request {
            current_folder: folder("Documents"),
            filters: vec![filter("Markdown", &[(0, "*.md")]), filter("Images", &[(1, "image/*")])],
            ..Request::default()
        };
        let mut p = Picking::new(request);
        assert_eq!(p.h.names(), ["Taxes 2025", "chapter1.md", "chapter2.md", "chapter10.md"]);
        let picker = p.h.root().read(&p.h.cx).picker().expect("a picker").clone();
        picker.update(&mut p.h.cx, |pk, cx| pk.set_filter(1, cx));
        p.h.settle();
        assert_eq!(p.h.names(), ["Taxes 2025"]);
        p.h.go("Pictures");
        assert_eq!(p.h.names(), ["Holiday", "Screenshots", "diagram.svg", "sunset.jpg"]);
        p.h.pick("files-table", &["sunset.jpg"], Some("sunset.jpg"));
        p.h.act(PickAccept);
        let outcome = p.outcome();
        let [Outcome::Picked { filter: Some(chosen), .. }] = outcome.as_slice() else { panic!("{outcome:?}") };
        assert_eq!(chosen.name, "Images", "the filter in force goes back with the pick");
    }

    #[test]
    fn folder_mode_lists_folders_and_returns_one() {
        let request = Request { directory: true, current_folder: folder("Documents"), ..Request::default() };
        let mut p = Picking::new(request);
        assert_eq!(p.h.names(), ["Taxes 2025"], "files do not list");
        p.h.act(PickAccept);
        assert_eq!(p.picked(), [uri("Documents")], "nothing selected picks the folder shown");

        let request = Request { directory: true, current_folder: folder("Documents"), ..Request::default() };
        let mut p = Picking::new(request);
        p.h.pick("files-table", &["Taxes 2025"], Some("Taxes 2025"));
        p.h.act(PickAccept);
        assert_eq!(p.picked(), [uri("Documents/Taxes 2025")]);
    }

    #[test]
    fn save_returns_the_folder_and_name_and_asks_before_replacing() {
        let request = Request {
            mode: Mode::Save,
            current_folder: folder("Documents"),
            current_name: Some("draft.md".into()),
            ..Request::default()
        };
        let mut p = Picking::new(request.clone());
        assert_eq!(p.name(), "draft.md");
        p.h.act(PickAccept);
        assert_eq!(p.picked(), [uri("Documents/draft.md")]);

        let mut p = Picking::new(request);
        p.h.pick("files-table", &["chapter1.md"], Some("chapter1.md"));
        assert_eq!(p.name(), "chapter1.md", "selecting a file takes its name");
        p.h.act(PickAccept);
        assert!(p.outcome().is_empty(), "nothing is returned before the answer");
        assert_eq!(p.alerts.shown.borrow().as_slice(), ["\u{201c}chapter1.md\u{201d} already exists. Replace it?"]);
        let keep = p.alerts.pending.borrow_mut().remove(0);
        keep(AlertResponse(1), &mut p.h.cx);
        p.h.settle();
        assert!(p.outcome().is_empty(), "Cancel in the alert keeps the picker open");
        p.h.act(PickAccept);
        let replace = p.alerts.pending.borrow_mut().remove(0);
        replace(AlertResponse(0), &mut p.h.cx);
        p.h.settle();
        assert_eq!(p.picked(), [uri("Documents/chapter1.md")]);
    }

    #[test]
    fn save_files_into_the_folder_shown() {
        let request = Request {
            mode: Mode::SaveFiles,
            current_folder: folder("Documents"),
            files: vec![b"a.txt\0".to_vec(), b"b.txt\0".to_vec()],
            ..Request::default()
        };
        let mut p = Picking::new(request);
        p.h.act(PickAccept);
        assert_eq!(p.picked(), [uri("Documents/a.txt"), uri("Documents/b.txt")]);
    }

    #[test]
    fn escape_and_cancel_answer_cancelled() {
        let mut p = Picking::new(Request { current_folder: folder("Documents"), ..Request::default() });
        p.h.act(Deselect);
        assert_eq!(p.outcome(), [Outcome::Cancelled]);

        let mut p = Picking::new(Request::default());
        p.h.act(PickCancel);
        assert_eq!(p.outcome(), [Outcome::Cancelled]);
    }
}

#[test]
fn reopen_closed_tab_brings_back_its_folder_view_and_selection() {
    let mut h = Harness::new();
    h.act(NewTab);
    h.go("Music");
    h.act(GridView);
    h.pick("files-grid", &["Album"], Some("Album"));
    h.act(CloseTab);
    assert_eq!(h.root().read(&h.cx).tabs().len(), 1);
    assert!(h.root().read(&h.cx).can_reopen());

    h.act(trove::app_menu::ReopenClosedTab);
    assert_eq!(h.root().read(&h.cx).tabs().len(), 2);
    assert_eq!(h.root().read(&h.cx).active_index(), 1, "where it stood in the strip");
    assert_eq!(h.location(), Location::Dir(Harness::demo("Music")));
    assert_eq!(h.tab().read(&h.cx).view(), View::Grid);
    assert_eq!(h.selected(), ["Album"]);
    assert!(!h.root().read(&h.cx).can_reopen());

    h.act(trove::app_menu::ReopenClosedTab);
    assert_eq!(h.root().read(&h.cx).tabs().len(), 2, "nothing left to reopen");
}

#[test]
fn close_other_tabs_reopen_one_at_a_time() {
    let mut h = Harness::new();
    h.go("Music");
    h.act(NewTab);
    h.go("Pictures");
    h.act(NewTab);
    h.go("Videos");
    let keep = h.root().read(&h.cx).tab_ids()[2];
    h.act(CloseOtherTabs(keep));
    h.act(trove::app_menu::ReopenClosedTab);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Music")), "the last one closed comes back first");
    h.act(trove::app_menu::ReopenClosedTab);
    assert_eq!(h.location(), Location::Dir(Harness::demo("Pictures")));
    let tabs: Vec<Location> = h.root().read(&h.cx).tabs().iter().map(|t| t.read(&h.cx).location().clone()).collect();
    assert_eq!(tabs, [Harness::demo("Music"), Harness::demo("Pictures"), Harness::demo("Videos")].map(Location::Dir));
}

fn menu_rows(menu: ContextMenu) -> Vec<(String, bool, bool)> {
    menu.items
        .iter()
        .filter_map(|item| match item {
            MenuItem::Action { label, enabled, checked, .. } => Some((label.to_string(), *enabled, *checked)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_app_menu_holds_the_window_commands() {
    let mut h = Harness::new();
    let window = h.window.id();
    if !nd::platform::has(nd::platform::Feature::MenuBar) {
        assert!(nd::test::by_test_id(&mut h.cx, window, "app-menu").is_some(), "the header has the menu button");
    }
    // Show hidden files is a setting other tests flip, so its tick is read off the window.
    let rows = |h: &Harness| {
        let r = h.root().read(&h.cx);
        menu_rows(trove::app_menu::items(ContextMenu::default(), r.shows_hidden(), r.can_reopen()))
    };
    let hidden = h.root().read(&h.cx).shows_hidden();
    let row = |label: &str| (label.to_owned(), true, false);
    assert_eq!(
        rows(&h),
        [
            row("New window"),
            row("New tab"),
            ("Reopen closed tab".to_owned(), false, false),
            ("Show hidden files".to_owned(), true, hidden),
            row("Preferences"),
            row("Keyboard shortcuts"),
            row("About Trove"),
        ]
    );
    h.act(NewTab);
    h.act(CloseTab);
    assert!(rows(&h).contains(&row("Reopen closed tab")), "a closed tab can come back");
    let ticked = menu_rows(trove::app_menu::items(ContextMenu::default(), true, false));
    assert!(ticked.contains(&("Show hidden files".to_owned(), true, true)), "{ticked:?}");

    let windows = h.cx.window_ids().count();
    h.act(trove::app_menu::NewWindow);
    assert_eq!(h.cx.window_ids().count(), windows + 1, "New window opens a window");
    h.act(trove::app_menu::Preferences);
    assert_eq!(h.cx.window_ids().count(), windows + 2, "Preferences opens its window");
    h.act(trove::app_menu::Preferences);
    assert_eq!(h.cx.window_ids().count(), windows + 2, "and brings it forward the second time");
}

#[test]
fn the_shortcuts_sheet_reads_the_bindings() {
    let mut h = Harness::new();
    let groups = trove::app_menu::shortcuts(&h.cx);
    let keys = |label: &str| {
        groups.iter().flat_map(|(_, rows)| rows).find(|(l, _)| *l == label).map(|(_, k)| k.clone()).unwrap_or_default()
    };
    let mac = nd::platform::target() == nd::platform::Target::MacOs;
    let secondary = if mac { "Cmd" } else { "Ctrl" };
    assert_eq!(keys("Reopen closed tab"), format!("{secondary}+Shift+T"));
    assert_eq!(keys("Reload"), format!("F5, {secondary}+R"));
    assert_eq!(keys("Parent folder"), "Alt+Up, Backspace");
    assert_eq!(keys("Quick Look"), "Space");
    let titles: Vec<&str> = groups.iter().map(|(t, _)| *t).collect();
    assert_eq!(titles, ["Navigation", "Files", "View", "Windows and tabs"]);
    assert_eq!(keys("Preferences"), format!("{secondary}+,"));

    let windows = h.cx.window_ids().count();
    h.act(trove::app_menu::KeyboardShortcuts);
    assert_eq!(h.cx.window_ids().count(), windows + 1, "the sheet opens over the window");
}
