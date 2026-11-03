//! Trove: a file manager on NativeDesktop. The window is `root::Root`; each tab is a
//! `tab::Tab` holding its own listing.

pub mod actions;
mod columns;
mod conflict;
mod demo;
mod env;
mod extras;
mod fileops;
mod format;
mod icons;
pub mod keys;
mod location;
mod menus;
pub mod ops;
mod pathbar;
pub mod picker;
pub mod portal;
mod preview;
mod pump;
mod quicklook;
mod results;
mod root;
mod search;
mod session;
pub mod settings;
mod sidebar;
mod sound;
mod status;
mod tab;
mod thumbs;
mod trace;
mod trash;

use std::path::PathBuf;
use std::time::Duration;

use nd::prelude::*;

pub use location::Location;
pub use root::{Root, Start};
pub use tab::{Load, RENAME_FIELD, Rename, Tab, View, stem_chars};
pub use trash::TrashInfo;

pub use columns::Columns;
pub use preview::{Item, Loaded, Preview};
pub use quicklook::QuickLook;
pub use results::{Results, Scope};
pub use thumbs::{drain as drain_thumbnails, thumbnail};

pub fn app() -> Application {
    Application::new(nd::manifest!())
}

struct Bound;

/// Registers the icons and key bindings, once per app.
pub fn init(cx: &mut App) {
    icons::init();
    if !cx.has_global::<Bound>() {
        actions::bind(cx);
        keys::bind(cx);
        cx.set_global(Bound);
    }
}

/// Opens a window. Under `TROVE_DEMO=1` it shows the demo fixture, wherever `start` points.
pub fn open(cx: &mut App, start: Start) -> WindowHandle<Root> {
    init(cx);
    let start = if demo::enabled() { demo_start(start) } else { start };
    let (start, (w, h)) = restore(start, cx);
    let options =
        WindowOptions::main("main").title("Trove").size(size(px(w), px(h))).min_size(size(px(640.), px(400.)));
    let window = cx.open_window(options, move |window, cx| cx.new(|cx| Root::new(start, window, cx)));
    focus_listing(window.clone(), 30);
    window
}

/// A window picking files for another app; `done` gets the outcome once, when the user
/// accepts or cancels.
pub fn open_picker(
    cx: &mut App,
    request: picker::Request,
    done: impl FnOnce(picker::Outcome, &mut App) + 'static,
) -> WindowHandle<Root> {
    init(cx);
    let options = WindowOptions::main("picker")
        .title(request.window_title())
        .size(size(px(960.), px(620.)))
        .min_size(size(px(640.), px(400.)));
    let done: picker::Done = Box::new(done);
    let window = cx.open_window(options, move |window, cx| cx.new(|cx| Root::picking(request, done, window, cx)));
    focus_listing(window.clone(), 30);
    window
}

/// Gives the listing the keys once the window's first frames have mounted it: a focus asked
/// for while the window is being built finds nothing to land on yet.
fn focus_listing(window: WindowHandle<Root>, tries: u8) {
    if !pump::live() || tries == 0 {
        return;
    }
    nd::system::spawn(async {}, move |_, cx| {
        let target = window.root().read(cx).first_focus(cx);
        if !cx.focus_id(window.id(), target) {
            focus_listing(window, tries - 1);
        }
    });
}

/// A window on the demo fixture, for tests and the appearance audit, with the first listing
/// already applied.
pub fn open_demo(cx: &mut App) -> WindowHandle<Root> {
    let root = demo_root();
    let window = open(cx, Start::Path(root));
    window.root().clone().update(cx, |r, cx| r.drain(Duration::from_secs(5), cx));
    window
}

/// The conflict sheet over a demo window, for the appearance audit: a download meeting a
/// file of the same kind in Documents.
pub fn open_conflict_demo(cx: &mut App) -> nd::core::WindowId {
    let parent = open_demo(cx);
    let root = demo::root();
    let ask = ops::Ask {
        job: trove_ops::JobId(0),
        source: root.join("Downloads/trove-0.1.0.tar.gz"),
        existing: root.join("Documents/Roadmap.pptx"),
    };
    let sheet = conflict::open(cx, parent.id(), ask);
    ops::run_deferred(cx);
    sheet.id()
}

/// A save picker over the demo fixture, for the appearance audit: the name field, the filter
/// and the buttons under the listing.
pub fn open_picker_demo(cx: &mut App) -> nd::core::WindowId {
    let root = demo_root();
    let mut folder = root.join("Documents").into_os_string().into_encoded_bytes();
    folder.push(0);
    let filter = |name: &str, glob: &str| picker::Filter { name: name.into(), patterns: vec![(0, glob.into())] };
    let request = picker::Request {
        mode: picker::Mode::Save,
        current_folder: Some(folder),
        current_name: Some("Notes.md".into()),
        filters: vec![filter("Markdown", "*.md"), filter("All files", "*")],
        ..picker::Request::default()
    };
    let window = open_picker(cx, request, |_, _| {});
    window.root().clone().update(cx, |r, cx| r.drain(Duration::from_secs(5), cx));
    window.id()
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

/// The demo fixture written under `dir` and made this process's home, for tests that change
/// files and so cannot share the fixture other tests read.
pub fn demo_root_in(dir: &std::path::Path) -> PathBuf {
    let root = dir.join("trove-demo");
    if let Err(e) = demo::ensure_at(root.clone()) {
        eprintln!("trove: writing the demo fixture: {e}");
    }
    env::use_demo(&root);
    root
}

/// A launch with no path reopens the last session's tabs and window size. The demo always
/// starts fresh.
fn restore(start: Start, cx: &mut App) -> (Start, (f32, f32)) {
    const SIZE: (f32, f32) = (1100., 700.);
    if start != Start::Home || env::get().demo {
        return (start, SIZE);
    }
    let session = session::load(cx);
    if session.tabs.is_empty() {
        return (start, SIZE);
    }
    let size = if session.width >= 640. && session.height >= 400. { (session.width, session.height) } else { SIZE };
    (Start::Restore(session), size)
}

fn demo_start(start: Start) -> Start {
    let root = demo_root();
    match start {
        Start::Path(p) if p.starts_with(&root) => Start::Path(p),
        _ => Start::Path(root),
    }
}

/// A launcher passes `%U` as `file://` URIs; a shell passes plain paths.
fn arg_path(arg: std::ffi::OsString) -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let Some(rest) = arg.as_bytes().strip_prefix(b"file://") else { return PathBuf::from(arg) };
    let rest = rest.strip_prefix(b"localhost").unwrap_or(rest);
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        match (rest[i], rest.get(i + 1).copied().and_then(hex), rest.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(hi << 4 | lo);
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

pub fn run() -> nd::Result<()> {
    match std::env::args_os().nth(1).as_deref().and_then(|a| a.to_str()) {
        Some("--pick") => return run_picker(),
        Some("--portal") => {
            if let Err(e) = portal::run() {
                eprintln!("trove: portal: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        _ => {}
    }
    trace::init();
    pump::set_live();
    let start = match std::env::args_os().nth(1) {
        Some(arg) => Start::Path(std::path::absolute(arg_path(arg)).unwrap_or_default()),
        None => Start::Home,
    };
    app().run(move |cx| {
        open(cx, start.clone());
    })
}

/// `trove --pick`: the request as JSON on stdin, the outcome as one JSON line on stdout.
fn run_picker() -> nd::Result<()> {
    use std::io::{Read, Write};
    let mut input = Vec::new();
    let request = std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_slice::<picker::Request>(&input).map_err(|e| e.to_string()));
    let request = match request {
        Ok(r) => r,
        Err(e) => {
            eprintln!("trove: reading the pick request: {e}");
            std::process::exit(2);
        }
    };
    trace::init();
    pump::set_live();
    app().run(move |cx| {
        let window = std::rc::Rc::new(std::cell::Cell::new(None));
        let id = window.clone();
        let handle = open_picker(cx, request, move |outcome, cx| {
            let mut out = std::io::stdout().lock();
            let written = serde_json::to_string(&outcome)
                .map_err(std::io::Error::other)
                .and_then(|line| writeln!(out, "{line}").and_then(|_| out.flush()));
            if let Err(e) = written {
                eprintln!("trove: answering the pick: {e}");
            }
            if let Some(id) = id.get() {
                cx.close_window(id);
            }
        });
        window.set(Some(handle.id()));
    })
}

#[cfg(test)]
mod arg_tests {
    use super::arg_path;
    use std::path::PathBuf;

    #[test]
    fn file_uris_decode_and_paths_pass_through() {
        assert_eq!(arg_path("file:///home/a%20b/%C3%A9".into()), PathBuf::from("/home/a b/é"));
        assert_eq!(arg_path("file://localhost/tmp/x".into()), PathBuf::from("/tmp/x"));
        assert_eq!(arg_path("/tmp/100%".into()), PathBuf::from("/tmp/100%"));
        assert_eq!(arg_path("file:///tmp/100%".into()), PathBuf::from("/tmp/100%"));
    }
}
