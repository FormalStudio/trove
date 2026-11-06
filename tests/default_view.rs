#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: a new tab opens on the default sort and view from the settings. A fixture of this
//! binary's own, since a drop-in setting them reaches every window of the process.

use std::time::Duration;

use nd::test::{Size, TestApp};
use trove::{Root, View};
use trove_fs::{SortKey, SortOrder};

#[test]
fn a_new_tab_opens_on_the_default_sort_and_view() {
    let dir = tempfile::Builder::new().prefix("trove-defaults").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
    let home = trove::demo_root_in(&dir);
    // Sorts after the fixture's own drop-in, so it wins.
    std::fs::write(
        home.join(".config/trove/config.d/zz-defaults.toml"),
        "default_sort = \"size\"\ndefault_sort_order = \"descending\"\ndefault_view = \"grid\"\n",
    )
    .unwrap();
    let mut cx = TestApp::new();
    let logs = home.join("Logs");
    let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
        trove::init(cx);
        cx.new(|cx| Root::new(trove::Start::Path(logs), window, cx))
    });
    cx.run_until_idle();
    window.root().clone().update(&mut cx, |r, cx| r.drain(Duration::from_secs(5), cx));
    cx.run_until_idle();
    let tab = window.root().read(&cx).active_tab().clone();
    let tab = tab.read(&cx);
    assert_eq!(tab.sort(), SortOrder { key: SortKey::Size, ascending: false, folders_first: true });
    assert_eq!(tab.view(), View::Grid);
    // The logs grow with their number: largest first is the last one written.
    assert_eq!(tab.visible_names().first().map(String::as_str), Some("build 120.log"));
}

#[test]
fn the_shipped_default_is_newest_modified_first() {
    let prefs = trove::settings::TroveSettings::default();
    assert_eq!(prefs.sort(), SortOrder { key: SortKey::Modified, ascending: false, folders_first: true });
}
