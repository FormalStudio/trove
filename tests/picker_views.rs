#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: the view and zoom keys in a save picker, on its own copy of the demo fixture, since
//! zoom writes the settings file inside it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use nd::prelude::*;
use nd::test::{Size, TestApp, by_test_id};
use trove::actions::{GridView, ListView};
use trove::keys::{ColumnsView, ZoomIn};
use trove::picker::{Mode, NAME_FIELD, Outcome, Request};
use trove::{Root, View};

#[test]
fn the_view_and_zoom_keys_leave_the_name_field_its_keys() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let home = trove::demo_root_in(dir.path());
    let mut folder = home.join("Documents").into_os_string().into_encoded_bytes();
    folder.push(0);
    let request = Request { mode: Mode::Save, current_folder: Some(folder), ..Request::default() };
    let outcome = Rc::new(RefCell::new(Vec::new()));
    let out = outcome.clone();
    let mut cx = TestApp::new();
    let window = cx.open_window(Size { width: 1000., height: 700. }, move |window, cx| {
        trove::init(cx);
        let done = Box::new(move |o, _: &mut App| out.borrow_mut().push(o));
        cx.new(|cx| Root::picking(request, done, window, cx))
    });
    let id = window.id();
    let settle = |cx: &mut TestApp| {
        cx.run_until_idle();
        window.root().update(cx, |r, cx| r.drain(Duration::from_secs(5), cx));
        cx.run_until_idle();
    };
    let act = |cx: &mut TestApp, action: &dyn Action| {
        cx.dispatch_action(id, action);
        settle(cx);
    };
    settle(&mut cx);
    let tab = |cx: &TestApp| window.root().read(cx).active_tab().clone();
    let name = by_test_id(&mut cx, id, NAME_FIELD).expect("a name field").id;
    // The launcher focuses it as the window opens (`Root::first_focus`); the test opens the
    // root alone.
    assert!(cx.focus(id, name));
    settle(&mut cx);

    let zoom = tab(&cx).read(&cx).zoom();
    act(&mut cx, &GridView);
    act(&mut cx, &ZoomIn);
    assert_eq!(tab(&cx).read(&cx).view(), View::Grid);
    assert_ne!(tab(&cx).read(&cx).zoom(), zoom);
    act(&mut cx, &ColumnsView);
    act(&mut cx, &ListView);
    assert_eq!(cx.focused(id), Some(name), "the field still has the keys");

    let picker = window.root().read(&cx).picker().expect("a picker").clone();
    picker.update(&mut cx, |p, cx| p.set_name("fresh.txt".to_owned(), cx));
    settle(&mut cx);
    cx.emit(name, TextSubmitted { text: "fresh.txt".into() });
    settle(&mut cx);
    let uris = match outcome.borrow().as_slice() {
        [Outcome::Picked { uris, .. }] => uris.clone(),
        other => panic!("one pick, got {other:?}"),
    };
    assert_eq!(uris, [trove_fs::file_uri(&home.join("Documents/fresh.txt"))], "Return in the field saves");
}
