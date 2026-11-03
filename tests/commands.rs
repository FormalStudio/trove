//! Tier 1: the commands around the listing: the folder, file and path bar menus, sorting,
//! type-select from real keystrokes, Open in terminal, Properties with its permissions, and
//! batch rename. Tests that change files run on a fixture of their own.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nd::prelude::gpui::Keystroke;
use nd::prelude::*;
use nd::test::{Size, TestApp};
use trove::actions::*;
use trove::menus::{self, Here, Target};
use trove::{Root, Tab, View};
use trove_fs::{SortKey, SortOrder};

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
    root: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

impl Harness {
    fn new() -> Self {
        Self::private()
    }

    /// Over a fixture of its own: the settings and state files live in it too.
    fn private() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = trove::demo_root_in(dir.path());
        Self::at(root, Some(dir))
    }

    fn at(root: PathBuf, dir: Option<tempfile::TempDir>) -> Self {
        let mut cx = TestApp::new();
        let start = root.clone();
        let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
            trove::init(cx);
            cx.new(|cx| Root::new(trove::Start::Path(start), window, cx))
        });
        let mut h = Self { cx, window, root, _dir: dir };
        h.settle();
        h
    }

    fn root_view(&self) -> Entity<Root> {
        self.window.root().clone()
    }

    fn settle(&mut self) {
        self.cx.run_until_idle();
        self.root_view().update(&mut self.cx, |r, cx| r.drain(Duration::from_secs(5), cx));
        self.cx.run_until_idle();
    }

    fn act(&mut self, action: impl Action) {
        let id = self.window.id();
        self.cx.dispatch_action(id, &action);
        self.settle();
    }

    fn tab(&self) -> Entity<Tab> {
        self.root_view().read(&self.cx).active_tab().clone()
    }

    fn selected(&self) -> Vec<String> {
        self.tab().read(&self.cx).selected_names()
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn go(&mut self, rel: &str) {
        let to = self.p(rel);
        self.act(GoTo(to));
    }

    /// Selects `names` in the table, as a click and Shift-clicks would.
    fn pick(&mut self, names: &[&str]) {
        let tab = self.tab();
        let rows = {
            let t = tab.read(&self.cx);
            let state = t.list_state();
            names
                .iter()
                .map(|name| {
                    let id = t.visible().iter().find(|id| t.entry(id).is_some_and(|e| e.name.as_ref() == *name));
                    id.and_then(|id| state.row_of(id)).unwrap_or_else(|| panic!("no row {name}"))
                })
                .collect::<Vec<_>>()
        };
        let cursor = rows.first().copied();
        let node = nd::test::by_test_id(&mut self.cx, self.window.id(), "files-table").expect("the table").id;
        self.cx.emit(node, nd::virtual_list::RowsSelected { rows, cursor });
        self.settle();
    }

    fn key(&mut self, keys: &str) {
        let id = self.window.id();
        let stroke = Keystroke::parse(keys).unwrap();
        self.cx.dispatch_keystroke(id, &stroke);
        self.settle();
    }

    /// The window opened last, other than the main one.
    fn sheet(&self) -> nd::core::WindowId {
        let main = self.window.id();
        self.cx.window_ids().filter(|w| *w != main).last().expect("a sheet")
    }

    fn emit_in<E: 'static>(&mut self, window: nd::core::WindowId, test_id: &str, event: E) {
        let node = nd::test::by_test_id(&mut self.cx, window, test_id)
            .unwrap_or_else(|| panic!("no `{test_id}` in\n{}", self.cx.semantic_tree().snapshot()))
            .id;
        self.cx.emit(node, event);
        self.settle();
    }
}

/// Every label in a menu, a submenu as "Title > item".
fn labels(items: &[MenuItem]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            MenuItem::Action { label, checked, .. } => {
                out.push(if *checked { format!("{label} \u{2713}") } else { label.to_string() })
            }
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

fn has(labels: &[String], label: &str) -> bool {
    labels.iter().any(|l| l == label)
}

#[test]
fn the_folder_menu_has_every_view_sort_and_direction() {
    let here = Here {
        show_hidden: false,
        sort: SortOrder { key: SortKey::Created, ascending: false, folders_first: true },
        view: View::Columns,
        in_trash: false,
        recent: false,
    };
    let l = labels(&menus::folder(ContextMenu::default(), &here, Some(&[])).items);
    for want in [
        "Open in terminal",
        "View > List",
        "View > Grid",
        "View > Columns \u{2713}",
        "Sort by > Created \u{2713}",
        "Sort by > Name",
        "Sort by > Ascending",
        "Sort by > Descending \u{2713}",
        "Sort by > Folders first \u{2713}",
        "Properties\u{2026}",
    ] {
        assert!(has(&l, want), "no {want:?} in {l:?}");
    }
    let recent = labels(&menus::folder(ContextMenu::default(), &Here { recent: true, ..here }, Some(&[])).items);
    assert!(!has(&recent, "Open in terminal") && !has(&recent, "Properties\u{2026}"), "{recent:?}");
}

#[test]
fn the_files_menu_offers_terminal_batch_rename_and_properties() {
    let mut cx = TestApp::new();
    trove::init(&mut cx);
    let one_folder = Target { paths: vec!["/x/Docs".into()], folders: 1, ..Target::default() };
    let l = labels(&menus::files(ContextMenu::default(), &one_folder, &mut cx).items);
    for want in ["Open in terminal", "Rename\u{2026}", "Copy path", "Properties\u{2026}"] {
        assert!(has(&l, want), "no {want:?} in {l:?}");
    }
    assert_eq!(l.last().map(String::as_str), Some("Properties\u{2026}"));

    let three = Target { paths: vec!["/x/a".into(), "/x/b".into(), "/x/c".into()], folders: 3, ..Target::default() };
    let l = labels(&menus::files(ContextMenu::default(), &three, &mut cx).items);
    assert!(has(&l, "Rename 3 items\u{2026}") && !has(&l, "Rename\u{2026}"), "{l:?}");
    assert!(!has(&l, "Open in terminal"), "a terminal opens on one folder");
}

#[test]
fn a_path_segment_menu_lists_the_folders_beside_it() {
    let path = Path::new("/home/me/Documents");
    let siblings = ["Desktop".to_owned(), "Documents".to_owned(), "Music".to_owned()];
    let l = labels(&menus::crumb(ContextMenu::default(), path, Some(&siblings)).items);
    assert_eq!(
        l,
        [
            "Open in new tab",
            "Open in terminal",
            "Copy path",
            "-",
            "Folders beside it > Desktop",
            "Folders beside it > Documents \u{2713}",
            "Folders beside it > Music",
        ]
    );
    let waiting = labels(&menus::crumb(ContextMenu::default(), path, None).items);
    assert!(has(&waiting, "Folders beside it > Looking\u{2026}"), "{waiting:?}");

    let mut h = Harness::new();
    h.go("Documents");
    let bar = h.root_view().read(&h.cx).pathbar().clone();
    let beside = bar.read(&h.cx).siblings(&h.p("Documents")).map(<[String]>::to_vec);
    assert_eq!(
        beside.as_deref(),
        Some(
            &["Desktop", "Documents", "Downloads", "Logs", "Music", "Pictures", "Projects", "Videos"].map(String::from)
                [..]
        ),
        "the fixture's folders, hidden ones left out"
    );
}

#[test]
fn sort_by_created_and_both_directions() {
    let mut h = Harness::new();
    h.act(SortBy(SortKey::Created));
    let sort = h.tab().read(&h.cx).sort();
    assert_eq!((sort.key, sort.ascending), (SortKey::Created, false), "newest first");
    h.act(SortAscending);
    assert!(h.tab().read(&h.cx).sort().ascending);
    h.act(SortDescending);
    assert!(!h.tab().read(&h.cx).sort().ascending);
    assert_eq!(h.tab().read(&h.cx).sort().key, SortKey::Created);
}

#[test]
fn type_select_takes_capitals_dashes_and_underscores() {
    let mut h = Harness::private();
    std::fs::write(h.p("snake_case.md"), "").unwrap();
    std::fs::write(h.p("snake-case.md"), "").unwrap();
    h.act(Reload);
    h.act(FocusFiles);
    for k in ["s", "n", "a", "k", "e", "_->_"] {
        h.key(k);
    }
    assert_eq!(h.selected(), ["snake_case.md"]);
    h.cx.advance_clock(2.);
    for k in ["s", "n", "a", "k", "e", "-"] {
        h.key(k);
    }
    assert_eq!(h.selected(), ["snake-case.md"]);

    h.go("Downloads");
    h.act(FocusFiles);
    for k in ["o", "p", "e", "n", "shift-s->S", "shift-u->U", "shift-s->S", "shift-e->E"] {
        h.key(k);
    }
    assert_eq!(h.selected(), ["openSUSE-Tumbleweed.iso"]);
    h.cx.advance_clock(2.);
    h.key("shift-t->T");
    assert_eq!(h.selected(), ["trove-0.1.0.tar.gz"], "a capital starts a name in either case");
}

#[test]
fn properties_change_permissions_and_undo_takes_them_back() {
    let mut h = Harness::private();
    let file = h.p("todo.txt");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    h.pick(&["todo.txt"]);
    h.act(Properties);
    let sheet = h.sheet();
    let text = nd::test::text(&mut h.cx, sheet);
    for want in ["todo.txt", "Kind", "Text", "Size", "Modified", "Owner", "Permissions"] {
        assert!(text.contains(want), "no {want:?} in {text}");
    }
    let exec = nd::test::by_test_id(&mut h.cx, sheet, "perm-owner-execute").expect("a checkbox");
    assert_eq!(exec.checked, Some(false));

    h.emit_in(sheet, "perm-owner-execute", Toggled { checked: true });
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&file), 0o744);
    let exec = nd::test::by_test_id(&mut h.cx, sheet, "perm-owner-execute").expect("a checkbox");
    assert_eq!(exec.checked, Some(true), "the box shows what the file has now");

    h.cx.close_window(sheet);
    h.settle();
    h.act(Undo);
    assert_eq!(mode(&file), 0o644);
}

#[test]
fn properties_over_several_items_show_mixed_bits_and_the_total() {
    let mut h = Harness::private();
    std::fs::set_permissions(h.p("todo.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(h.p("Notes.md"), std::fs::Permissions::from_mode(0o644)).unwrap();
    h.pick(&["Notes.md", "todo.txt"]);
    h.act(Properties);
    let sheet = h.sheet();
    let text = nd::test::text(&mut h.cx, sheet);
    assert!(text.contains("2 items") && text.contains("2 files"), "{text}");
    let read = nd::test::by_test_id(&mut h.cx, sheet, "perm-group-read").expect("a checkbox");
    assert_eq!(read.value.as_deref(), Some("mixed"), "{read:?}");
    h.emit_in(sheet, "perm-group-read", Toggled { checked: true });
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(h.p("Notes.md")), mode(h.p("todo.txt"))), (0o644, 0o640));
}

#[test]
fn batch_rename_previews_flags_conflicts_and_undoes_as_one() {
    let mut h = Harness::private();
    h.go("Documents");
    h.pick(&["chapter1.md", "chapter2.md", "chapter10.md"]);
    h.act(BatchRename);
    let sheet = h.sheet();
    h.emit_in(sheet, "rename-find", TextChanged { text: "chapter".into() });
    h.emit_in(sheet, "rename-with", TextChanged { text: "Part ".into() });
    let text = nd::test::text(&mut h.cx, sheet);
    assert!(text.contains("Part 1.md") && text.contains("Part 2.md"), "{text}");
    assert!(text.contains("3 of 3 names change"), "{text}");

    h.emit_in(sheet, "rename-with", TextChanged { text: "Roadmap.pptx".into() });
    h.emit_in(sheet, "rename-regex", Toggled { checked: true });
    h.emit_in(sheet, "rename-find", TextChanged { text: r"^chapter1\.md$".into() });
    let text = nd::test::text(&mut h.cx, sheet);
    assert!(text.contains("Already exists") && text.contains("1 name can\u{2019}t be used"), "{text}");
    let commit = nd::test::by_test_id(&mut h.cx, sheet, "rename-commit").expect("a button");
    assert!(!commit.enabled, "a taken name stops the rename");

    h.emit_in(sheet, "rename-mode", SelectChanged { index: 1 });
    h.emit_in(sheet, "rename-base", TextChanged { text: "Chapter".into() });
    let text = nd::test::text(&mut h.cx, sheet);
    assert!(text.contains("Chapter 01.md") && text.contains("3 of 3 names change"), "{text}");
    h.emit_in(sheet, "rename-commit", ClickEvent);

    let names = |h: &Harness| {
        let mut n: Vec<String> = std::fs::read_dir(h.p("Documents"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase().starts_with("chapter"))
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(&h), ["Chapter 01.md", "Chapter 02.md", "Chapter 03.md"]);
    assert_eq!(h.selected().len(), 3, "the renamed items stay selected");
    h.act(Undo);
    assert_eq!(names(&h), ["chapter1.md", "chapter10.md", "chapter2.md"], "one undo puts every name back");
}
