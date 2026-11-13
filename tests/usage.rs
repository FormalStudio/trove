#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: the Disk Usage view over a tree of this binary's own: what it measures, going into a
//! folder and back out, and an item leaving the map once it is in the Trash.

use std::fs;
use std::time::Duration;

use nd::test::{Size, TestApp};
use trove::usage::{Scan, UsageView};

const WAIT: Duration = Duration::from_secs(10);

fn names(view: &UsageView) -> Vec<String> {
    view.folder().map(|f| f.children.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
}

#[test]
fn measures_a_tree_enters_folders_and_forgets_what_is_trashed() {
    let dir = tempfile::Builder::new().prefix("trove-usage").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
    let home = trove::demo_root_in(&dir);
    let tree = home.join("Measured");
    fs::create_dir_all(tree.join("big/deep")).unwrap();
    fs::create_dir_all(tree.join("small")).unwrap();
    fs::write(tree.join("big/deep/a.mkv"), vec![1u8; 300_000]).unwrap();
    fs::write(tree.join("big/b.iso"), vec![1u8; 100_000]).unwrap();
    fs::write(tree.join("small/c.txt"), vec![1u8; 30_000]).unwrap();
    fs::write(tree.join("notes"), vec![1u8; 9_000]).unwrap();

    let mut cx = TestApp::new();
    let root = tree.clone();
    let window = cx.open_window(Size { width: 1000., height: 700. }, |window, cx| {
        trove::init(cx);
        let id = window.id();
        cx.new(|cx| UsageView::new(root, id, cx))
    });
    let view = window.root().clone();
    view.update(&mut cx, |v, cx| v.drain(WAIT, cx));
    cx.run_until_idle();

    {
        let v = view.read(&cx);
        assert_eq!(v.state(), &Scan::Done);
        assert_eq!(names(v), ["big", "small", "notes"]);
        assert_eq!(v.crumbs(), ["Measured"]);
        assert!(v.detail().ends_with("7 items"), "{}", v.detail());
    }
    let whole = view.read(&cx).folder().unwrap().size;

    view.update(&mut cx, |v, cx| v.enter(0, cx));
    {
        let v = view.read(&cx);
        assert_eq!(v.crumbs(), ["Measured", "big"]);
        assert_eq!(names(v), ["deep", "b.iso"]);
        assert_eq!(v.path_of(1), Some(tree.join("big/b.iso")));
    }
    // A file is no folder to go into.
    view.update(&mut cx, |v, cx| v.enter(1, cx));
    assert_eq!(view.read(&cx).crumbs().len(), 2);
    view.update(&mut cx, |v, cx| v.select(1, cx));
    assert!(view.read(&cx).detail().contains("b.iso"));

    view.update(&mut cx, |v, cx| v.trash(tree.join("big/b.iso"), cx));
    let jobs = trove::ops::jobs(&cx);
    jobs.update(&mut cx, |j, cx| j.drain(WAIT, cx));
    cx.run_until_idle();
    assert!(!tree.join("big/b.iso").exists());
    assert_eq!(names(view.read(&cx)), ["deep"]);

    view.update(&mut cx, |v, cx| v.go_to(0, cx));
    let v = view.read(&cx);
    assert_eq!(v.crumbs(), ["Measured"]);
    let left = v.folder().unwrap().size;
    assert!(whole - left >= 100_000, "{whole} {left}");
    assert_eq!(v.folder().unwrap().items, 6);
}

#[test]
fn a_folder_that_is_gone_says_so() {
    let dir = tempfile::Builder::new().prefix("trove-usage").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
    let home = trove::demo_root_in(&dir);
    let mut cx = TestApp::new();
    let window = cx.open_window(Size { width: 1000., height: 700. }, |window, cx| {
        trove::init(cx);
        let id = window.id();
        cx.new(|cx| UsageView::new(home.join("no-such-folder"), id, cx))
    });
    let view = window.root().clone();
    view.update(&mut cx, |v, cx| v.drain(WAIT, cx));
    cx.run_until_idle();
    assert_eq!(view.read(&cx).state(), &Scan::Failed);
    assert!(view.read(&cx).folder().is_none());
}

#[test]
fn the_developer_view_lists_what_can_be_rebuilt_and_shows_where_it_is() {
    let dir = tempfile::Builder::new().prefix("trove-usage").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
    let home = trove::demo_root_in(&dir);
    let tree = home.join("Code");
    fs::create_dir_all(tree.join("app/node_modules/left-pad")).unwrap();
    fs::create_dir_all(tree.join("app/src")).unwrap();
    fs::create_dir_all(tree.join("engine/target/debug")).unwrap();
    fs::write(tree.join("app/node_modules/left-pad/index.js"), vec![1u8; 200_000]).unwrap();
    fs::write(tree.join("app/src/main.ts"), vec![1u8; 20_000]).unwrap();
    fs::write(tree.join("engine/target/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
    fs::write(tree.join("engine/target/debug/engine"), vec![1u8; 400_000]).unwrap();
    fs::write(tree.join("engine/lib.rs"), vec![1u8; 10_000]).unwrap();

    let mut cx = TestApp::new();
    let root = tree.clone();
    let window = cx.open_window(Size { width: 1000., height: 700. }, |window, cx| {
        trove::init(cx);
        let id = window.id();
        cx.new(|cx| UsageView::new(root, id, cx))
    });
    let view = window.root().clone();
    view.update(&mut cx, |v, cx| v.drain(WAIT, cx));
    view.update(&mut cx, |v, cx| v.set_developer(true, cx));
    cx.run_until_idle();

    let found = |v: &UsageView| -> Vec<(String, &'static str)> {
        v.finds().iter().map(|f| (f.path.display().to_string(), f.tag.id())).collect()
    };
    assert_eq!(found(view.read(&cx)), [("engine/target".to_owned(), "build"), ("app/node_modules".to_owned(), "deps")]);

    // A row leads to the folder the find is in, with the find picked out in the map.
    view.update(&mut cx, |v, cx| v.reveal(1, cx));
    {
        let v = view.read(&cx);
        assert_eq!(v.crumbs(), ["Code", "app"]);
        assert_eq!(v.selected().and_then(|i| v.path_of(i)), Some(tree.join("app/node_modules")));
    }

    // Trashed from anywhere under the root, it leaves the list and every total above it.
    let whole = view.read(&cx).finds().iter().map(|f| f.size).sum::<u64>();
    view.update(&mut cx, |v, cx| v.trash(tree.join("engine/target"), cx));
    let jobs = trove::ops::jobs(&cx);
    jobs.update(&mut cx, |j, cx| j.drain(WAIT, cx));
    cx.run_until_idle();
    assert!(!tree.join("engine/target").exists());
    let v = view.read(&cx);
    assert_eq!(found(v), [("app/node_modules".to_owned(), "deps")]);
    assert!(whole - v.finds()[0].size >= 400_000);
    assert_eq!(v.crumbs(), ["Code", "app"]);
}
