//! Tier 1: the view on `TestBackend`. No window server, runs on every OS.

use trove::app::Counter;
use nd::prelude::*;
use nd::test::{ButtonProps, NodeId, Size, TestApp, TextProps};

fn text_node(cx: &TestApp, wanted: &str) -> Option<NodeId> {
    cx.backend()
        .find(|_, props| props.downcast_ref::<TextProps>().is_some_and(|t| t.text.as_ref() == wanted))
        .first()
        .copied()
}

#[test]
fn add_one_counts_up() {
    let mut cx = TestApp::new();
    cx.open_window(Size { width: 800., height: 600. }, |_, cx| cx.new(|_| Counter::default()));
    assert!(text_node(&cx, "0").is_some());

    let add = cx
        .backend()
        .find(|_, props| props.downcast_ref::<ButtonProps>().is_some_and(|b| b.label == "Add one"))
        .first()
        .copied()
        .expect("the toolbar has an Add one button");
    cx.emit(add, ClickEvent);

    assert!(text_node(&cx, "1").is_some());
    assert!(text_node(&cx, "0").is_none());
}
