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
