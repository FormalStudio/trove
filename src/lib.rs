//! Trove: a file manager on NativeDesktop. The window is `root::Root`; each tab is a
//! `tab::Tab` holding its own listing.

pub mod actions;
mod demo;
mod env;
mod format;
mod icons;
mod location;
mod pathbar;
mod preview;
mod pump;
mod root;
mod sidebar;
mod status;
mod tab;
mod trace;

use std::path::PathBuf;
use std::time::Duration;

use nd::prelude::*;

pub use location::Location;
pub use root::{Root, Start};
pub use tab::{Load, Tab, View};

pub fn app() -> Application {
    Application::new(nd::manifest!())
}

struct Bound;

/// Registers the icons and key bindings, once per app.
pub fn init(cx: &mut App) {
    icons::init();
    if !cx.has_global::<Bound>() {
        actions::bind(cx);
        cx.set_global(Bound);
    }
}

/// Opens a window. Under `TROVE_DEMO=1` it shows the demo fixture, wherever `start` points.
pub fn open(cx: &mut App, start: Start) -> WindowHandle<Root> {
    init(cx);
    let start = if demo::enabled() { demo_start(start) } else { start };
    let options =
        WindowOptions::main("main").title("Trove").size(size(px(1100.), px(700.))).min_size(size(px(640.), px(400.)));
    cx.open_window(options, move |window, cx| cx.new(|cx| Root::new(start, window, cx)))
}

/// A window on the demo fixture, for tests and the appearance audit, with the first listing
/// already applied.
pub fn open_demo(cx: &mut App) -> WindowHandle<Root> {
    let root = demo_root();
    let window = open(cx, Start::Path(root));
    window.root().clone().update(cx, |r, cx| r.drain(Duration::from_secs(5), cx));
    window
}

/// Writes the demo fixture and makes it the home, Recent and places for this process; returns
/// its root.
pub fn demo_root() -> PathBuf {
    let root = demo::ensure().unwrap_or_else(|e| {
        eprintln!("trove: writing the demo fixture: {e}");
        demo::root()
    });
    env::use_demo(&root);
    root
}

fn demo_start(start: Start) -> Start {
    let root = demo_root();
    match start {
        Start::Path(p) if p.starts_with(&root) => Start::Path(p),
        _ => Start::Path(root),
    }
}

pub fn run() -> nd::Result<()> {
    trace::init();
    pump::set_live();
    let start = match std::env::args_os().nth(1) {
        Some(arg) => Start::Path(std::path::absolute(PathBuf::from(arg)).unwrap_or_default()),
        None => Start::Home,
    };
    app().run(move |cx| {
        open(cx, start.clone());
    })
}
