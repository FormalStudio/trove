//! The commands of the preview, Quick Look, search results, the columns view and the views'
//! zoom, their keys, and the window's handlers for them.

use std::path::Path;

use nd::prelude::*;

use crate::actions::{FILES, SEARCH, WINDOW};
use crate::location::Location;
use crate::root::Root;
use crate::tab::View;
use crate::zoom::Zoom;

actions!(
    trove,
    [
        QuickLook,
        ColumnsView,
        ShowInFolder,
        FocusResults,
        QuickLookNext,
        QuickLookPrevious,
        CloseQuickLook,
        QuickLookOpen,
        ZoomIn,
        ZoomOut,
        ZoomReset
    ]
);

/// Quick Look's own keys, while it is open.
pub const QUICKLOOK: &str = "QuickLook";
/// The listing while it shows search results.
pub const RESULTS: &str = "Results";

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", QuickLook, Some(FILES)),
        KeyBinding::new("secondary-3", ColumnsView, Some(WINDOW)),
        KeyBinding::new("alt-enter", ShowInFolder, Some(FILES)),
        KeyBinding::new("alt-enter", ShowInFolder, Some(SEARCH)),
        KeyBinding::new("secondary-enter", ShowInFolder, Some(RESULTS)),
        KeyBinding::new("secondary-enter", ShowInFolder, Some(SEARCH)),
        KeyBinding::new("down", FocusResults, Some(SEARCH)),
        KeyBinding::new("enter", crate::actions::Open, Some(SEARCH)),
        KeyBinding::new("space", CloseQuickLook, Some(QUICKLOOK)),
        KeyBinding::new("escape", CloseQuickLook, Some(QUICKLOOK)),
        KeyBinding::new("right", QuickLookNext, Some(QUICKLOOK)),
        KeyBinding::new("down", QuickLookNext, Some(QUICKLOOK)),
        KeyBinding::new("left", QuickLookPrevious, Some(QUICKLOOK)),
        KeyBinding::new("up", QuickLookPrevious, Some(QUICKLOOK)),
        KeyBinding::new("secondary-o", QuickLookOpen, Some(QUICKLOOK)),
        KeyBinding::new("enter", QuickLookOpen, Some(QUICKLOOK)),
        KeyBinding::new("secondary-=", ZoomIn, Some(WINDOW)),
        KeyBinding::new("secondary-+", ZoomIn, Some(WINDOW)),
        KeyBinding::new("secondary--", ZoomOut, Some(WINDOW)),
        KeyBinding::new("secondary-0", ZoomReset, Some(WINDOW)),
    ]);
}

/// The window's handlers for these commands.
pub fn on_root(root: Scaffold, cx: &mut Context<Root>) -> Scaffold {
    root.on_action(cx.listener(|this, _: &QuickLook, window, cx| {
        let tab = this.active_tab().clone();
        let ql = this.quicklook().clone();
        ql.update(cx, |q, cx| q.toggle(&tab, window, cx));
    }))
    .on_action(cx.listener(|this, _: &ColumnsView, _, cx| {
        this.active_tab().clone().update(cx, |t, cx| t.set_view(View::Columns, cx));
    }))
    .on_action(cx.listener(|this, _: &ShowInFolder, window, cx| {
        let tab = this.active_tab().clone();
        tab.update(cx, |t, cx| show_in_folder(t, window, cx));
        this.focus_files(window, cx);
    }))
    .on_action(cx.listener(|this, _: &ZoomIn, _, cx| zoom(this, |z, view| z.step(view, 1), cx)))
    .on_action(cx.listener(|this, _: &ZoomOut, _, cx| zoom(this, |z, view| z.step(view, -1), cx)))
    .on_action(cx.listener(|this, _: &ZoomReset, _, cx| zoom(this, Zoom::reset, cx)))
    .on_action(cx.listener(|this, _: &FocusResults, window, cx| {
        let tab = this.active_tab().clone();
        tab.update(cx, |t, cx| {
            if t.showing_results() {
                t.extras.results.select_first(cx);
                cx.notify();
            }
        });
        this.focus_files(window, cx);
    }))
}

/// Resizes the active tab's view, in every tab of the window, and keeps the size for the next
/// launch.
fn zoom(root: &mut Root, to: impl Fn(Zoom, View) -> Zoom, cx: &mut Context<Root>) {
    let (now, view) = {
        let tab = root.active_tab().read(cx);
        (tab.zoom(), tab.view())
    };
    let next = to(now, view);
    if next == now {
        return;
    }
    for tab in root.tabs() {
        tab.update(cx, |t, cx| t.set_zoom(next, cx));
    }
    if next.list != now.list {
        crate::settings::set(cx, "list_zoom", &next.list);
    }
    if next.grid != now.grid {
        crate::settings::set(cx, "grid_zoom", &next.grid);
    }
}

/// "Show in enclosing folder": the result's folder, with the result selected.
fn show_in_folder(tab: &mut crate::tab::Tab, window: &mut Window, cx: &mut Context<crate::tab::Tab>) {
    let Some(path) = tab.target().and_then(|id| tab.path_of(&id)) else { return };
    let Some(dir) = path.parent() else { return };
    tab.clear_search(window, cx);
    tab.navigate(Location::Dir(dir.to_path_buf()), window, cx);
    tab.reveal_in(&path, cx);
}

/// Opens a file in its default app, saying so when that fails.
pub fn open_file(path: &Path, window: &mut Window, cx: &mut App) {
    if let Err(e) = trove_ops::open(&[path.to_path_buf()]) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let alert = Alert::new(format!("Could not open \u{201c}{name}\u{201d}")).message(e);
        window.alert(cx, alert, |_, _| {});
    }
}
