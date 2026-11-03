//! The window's file commands: clipboard, undo, trash and delete, new items, rename, compress,
//! Open with, bookmarks, the Trash, and drops. Each acts on the active tab's selection and
//! submits to the app's `Ops`; what a job makes comes back selected (`ops::Jobs`).

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nd::prelude::*;
use trove_fs::SortKey;
use trove_ops::Op;

use crate::actions::*;
use crate::location::Location;
use crate::ops::{self, Origin, describe};
use crate::root::Root;

pub const NEW_FOLDER: &str = "untitled folder";
pub const NEW_FILE: &str = "untitled file";

pub fn register(root: Scaffold, cx: &mut Context<Root>) -> Scaffold {
    root.on_action(cx.listener(|r, _: &Copy, _, cx| copy(r, false, cx)))
        .on_action(cx.listener(|r, _: &Cut, _, cx| copy(r, true, cx)))
        .on_action(cx.listener(|r, _: &Paste, _, cx| paste(r, false, cx)))
        .on_action(cx.listener(|r, _: &PasteInto, _, cx| paste(r, true, cx)))
        .on_action(cx.listener(|r, _: &Undo, _, cx| {
            let o = origin(r, cx);
            ops::undo(cx, o);
        }))
        .on_action(cx.listener(|r, _: &Redo, _, cx| {
            let o = origin(r, cx);
            ops::redo(cx, o);
        }))
        .on_action(cx.listener(|r, _: &Trash, _, cx| trash(r, cx)))
        .on_action(cx.listener(|r, _: &DeletePermanently, _, cx| delete(r, cx)))
        .on_action(cx.listener(|r, _: &Rename, window, cx| {
            r.active_tab().clone().update(cx, |t, cx| t.start_rename(window, cx));
        }))
        .on_action(cx.listener(|r, _: &NewFolder, _, cx| new_item(r, true, cx)))
        .on_action(cx.listener(|r, _: &NewFile, _, cx| new_item(r, false, cx)))
        .on_action(cx.listener(|r, _: &Duplicate, _, cx| {
            let paths = selection(r, cx);
            if !paths.is_empty() && !in_trash(r, cx) {
                let o = origin(r, cx);
                ops::submit(cx, o, Op::Duplicate { paths });
            }
        }))
        .on_action(cx.listener(|r, _: &Compress, _, cx| compress(r, cx)))
        .on_action(cx.listener(|r, _: &ExtractHere, _, cx| {
            if let [archive] = selection(r, cx).as_slice()
                && let Some(dest_dir) = archive.parent()
            {
                let op = Op::Extract { archive: archive.clone(), dest_dir: dest_dir.to_path_buf() };
                let o = origin(r, cx);
                ops::submit(cx, o, op);
            }
        }))
        .on_action(cx.listener(|r, _: &CopyPath, _, cx| {
            let paths = selection(r, cx);
            if !paths.is_empty() {
                let text: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                nd::clipboard::write_text(cx, text.join("\n"));
            }
        }))
        .on_action(cx.listener(|r, _: &AddBookmark, _, cx| {
            let folder = r.active_tab().read(cx).selected_folder();
            if let Some(path) = folder {
                r.sidebar().clone().update(cx, |s, cx| s.add_bookmark(path, cx));
            }
        }))
        .on_action(cx.listener(|r, BookmarkPlace(path): &BookmarkPlace, _, cx| {
            let path = path.clone();
            r.sidebar().clone().update(cx, |s, cx| s.add_bookmark(path, cx));
        }))
        .on_action(cx.listener(|r, RemoveBookmark(path): &RemoveBookmark, _, cx| {
            let path = path.clone();
            r.sidebar().clone().update(cx, |s, cx| s.remove_bookmark(&path, cx));
        }))
        .on_action(cx.listener(|r, Eject(mount): &Eject, _, cx| {
            let mount = mount.clone();
            r.sidebar().clone().update(cx, |s, cx| s.eject_mount(&mount, cx));
        }))
        .on_action(cx.listener(|r, _: &Restore, _, cx| {
            let paths = selection(r, cx);
            if !paths.is_empty() && in_trash(r, cx) {
                let o = origin(r, cx);
                ops::submit(cx, o, Op::Restore { trashed: paths });
            }
        }))
        .on_action(cx.listener(|r, _: &EmptyTrash, _, cx| empty_trash(r, cx)))
        .on_action(cx.listener(|r, OpenWith(id): &OpenWith, _, cx| open_with(r, id, cx)))
        .on_action(cx.listener(|r, SetDefaultApp(id): &SetDefaultApp, _, cx| set_default(r, id, cx)))
        .on_action(cx.listener(|r, SortBy(key): &SortBy, _, cx| sort_by(r, *key, cx)))
        .on_action(cx.listener(|r, _: &ToggleFoldersFirst, _, cx| {
            r.active_tab().clone().update(cx, |t, cx| {
                let mut sort = t.sort();
                sort.folders_first = !sort.folders_first;
                t.set_sort(sort, cx);
            });
        }))
        .on_action(cx.listener(|r, _: &SortAscending, _, cx| set_ascending(r, true, cx)))
        .on_action(cx.listener(|r, _: &SortDescending, _, cx| set_ascending(r, false, cx)))
        .on_action(cx.listener(|r, e: &DropOnto, _, cx| drop_onto(r, e, cx)))
        .on_action(cx.listener(|r, _: &OpenTerminal, _, cx| {
            let dir = r.active_tab().read(cx).selected_folder().or_else(|| here(r, cx));
            if let Some(dir) = dir {
                open_terminal(r, dir, cx);
            }
        }))
        .on_action(cx.listener(|r, OpenTerminalAt(dir): &OpenTerminalAt, _, cx| open_terminal(r, dir.clone(), cx)))
        .on_action(cx.listener(|_, CopyPathOf(path): &CopyPathOf, _, cx| {
            nd::clipboard::write_text(cx, path.display().to_string());
        }))
        .on_action(cx.listener(|r, _: &Properties, _, cx| {
            let mut paths = selection(r, cx);
            if paths.is_empty() {
                paths.extend(r.active_tab().read(cx).location().dir().map(Path::to_path_buf));
            }
            if !paths.is_empty() {
                crate::properties::open(cx, r.window_id(), paths);
            }
        }))
        .on_action(cx.listener(|r, _: &BatchRename, _, cx| {
            let paths = selection(r, cx);
            if !paths.is_empty() && !in_trash(r, cx) {
                let o = origin(r, cx);
                crate::batch_rename::open(cx, o, paths);
            }
        }))
}

fn origin(r: &Root, _cx: &App) -> Origin {
    Origin { window: r.window_id(), tab: Some(r.active_tab().downgrade()) }
}

fn selection(r: &Root, cx: &App) -> Vec<PathBuf> {
    r.active_tab().read(cx).selected_paths()
}

fn in_trash(r: &Root, cx: &App) -> bool {
    r.active_tab().read(cx).is_trash()
}

/// The folder new items, pastes and archives go to: the tab's own, never Recent or a trash.
fn here(r: &Root, cx: &App) -> Option<PathBuf> {
    let tab = r.active_tab().read(cx);
    if tab.is_trash() {
        return None;
    }
    tab.location().dir().map(Path::to_path_buf)
}

fn copy(r: &mut Root, cut: bool, cx: &mut Context<Root>) {
    let paths = selection(r, cx);
    if paths.is_empty() || cut && in_trash(r, cx) {
        return;
    }
    ops::copy_to_clipboard(cx, paths, cut);
}

/// Ctrl+V into the folder shown, Ctrl+Shift+V into the selected folder. A cut moves (and a
/// cut pasted where it already is changes nothing); a copy pasted beside its original makes
/// "name copy".
fn paste(r: &mut Root, into: bool, cx: &mut Context<Root>) {
    let dest = if into { r.active_tab().read(cx).selected_folder() } else { here(r, cx) };
    let Some(dest) = dest else { return };
    let origin = origin(r, cx);
    ops::read_clipboard(cx, move |found, cx| {
        let Some((paths, cut)) = found else { return };
        if cut {
            let sources: Vec<PathBuf> = paths.into_iter().filter(|p| p.parent() != Some(dest.as_path())).collect();
            if !sources.is_empty() {
                ops::submit(cx, origin, Op::Move { sources, dest_dir: dest });
            }
        } else {
            ops::submit(cx, origin, Op::Copy { sources: paths, dest_dir: dest });
        }
    });
}

/// Delete: to the Trash, or, inside a trash, for good after asking.
fn trash(r: &mut Root, cx: &mut Context<Root>) {
    if in_trash(r, cx) {
        delete(r, cx);
        return;
    }
    let paths = selection(r, cx);
    if paths.is_empty() || r.active_tab().read(cx).location() == &Location::Recent && paths.is_empty() {
        return;
    }
    let origin = origin(r, cx);
    if crate::settings::get(cx).confirm_trash {
        let title = format!("Move {} to the Trash?", describe(&paths));
        nd::chrome::alert(cx, origin.window, Alert::confirm(title, "Move to Trash"), move |res, cx| {
            if res.0 == 0 {
                ops::submit(cx, origin, Op::Trash { paths });
            }
        });
        return;
    }
    ops::submit(cx, origin, Op::Trash { paths });
}

/// Shift+Delete, or Delete in the Trash: asks first, naming the item or the count.
fn delete(r: &mut Root, cx: &mut Context<Root>) {
    let paths = selection(r, cx);
    if paths.is_empty() {
        return;
    }
    let origin = origin(r, cx);
    let title = format!("Delete {} permanently?", describe(&paths));
    let mut alert = Alert::confirm(title, "Delete");
    alert.message = Some("This can\u{2019}t be undone.".into());
    nd::chrome::alert(cx, origin.window, alert, move |res, cx| {
        if res.0 == 0 {
            ops::submit(cx, origin, Op::Delete { paths });
        }
    });
}

fn empty_trash(r: &mut Root, cx: &mut Context<Root>) {
    let origin = origin(r, cx);
    let mut alert = Alert::confirm("Empty the Trash?", "Empty Trash");
    alert.message = Some("Everything in the Trash is deleted permanently.".into());
    nd::chrome::alert(cx, origin.window, alert, move |res, cx| {
        if res.0 == 0 {
            ops::submit(cx, origin, Op::EmptyTrash);
        }
    });
}

/// Ctrl+Shift+N: "untitled folder" (or the next free "untitled folder 2"), selected and
/// already being renamed once it lands.
fn new_item(r: &mut Root, folder: bool, cx: &mut Context<Root>) {
    let Some(parent) = here(r, cx) else { return };
    let op = if folder {
        Op::NewFolder { parent, name: NEW_FOLDER.into() }
    } else {
        Op::NewFile { parent, name: NEW_FILE.into() }
    };
    let o = origin(r, cx);
    ops::submit(cx, o, op);
}

/// "name.zip" for one item, "Archive.zip" for more, beside them.
fn compress(r: &mut Root, cx: &mut Context<Root>) {
    let paths = selection(r, cx);
    let Some(dest) = paths.first().and_then(|p| p.parent()).map(Path::to_path_buf) else { return };
    if in_trash(r, cx) {
        return;
    }
    let o = origin(r, cx);
    ops::submit(cx, o, Op::Compress { paths, dest });
}

fn open_with(r: &mut Root, id: &str, cx: &mut Context<Root>) {
    let paths = selection(r, cx);
    let Some(app) = paths.first().and_then(|p| ops::app_by_id(p, id)) else { return };
    let window = r.window_id();
    ops::background(
        cx.weak_entity(),
        move || trove_ops::open_with(&app, &paths).err().map(|e| (app.name.clone(), e)),
        move |_: &mut Root, failed, cx| {
            if let Some((name, why)) = failed {
                nd::chrome::alert(
                    cx,
                    window,
                    Alert::new(format!("Could not open with {name}")).message(why),
                    |_, _| {},
                );
            }
        },
    );
}

fn set_default(r: &mut Root, id: &str, cx: &mut Context<Root>) {
    let paths = selection(r, cx);
    let Some(first) = paths.first() else { return };
    let (Some(app), Some(mime)) = (ops::app_by_id(first, id), ops::mime_of(first)) else { return };
    ops::forget_apps(&mime);
    ops::background(cx.weak_entity(), move || trove_ops::set_default(&app, &mime), |_: &mut Root, _, _| {});
}

/// Sort by a key: the same key again flips the order; a new one starts the way people read it
/// (names A to Z, the biggest and the newest first).
fn sort_by(r: &mut Root, key: SortKey, cx: &mut Context<Root>) {
    r.active_tab().clone().update(cx, |t, cx| {
        let mut sort = t.sort();
        if sort.key == key {
            sort.ascending = !sort.ascending;
        } else {
            sort.key = key;
            sort.ascending = matches!(key, SortKey::Name | SortKey::Kind);
        }
        t.set_sort(sort, cx);
    });
}

fn set_ascending(r: &mut Root, ascending: bool, cx: &mut Context<Root>) {
    r.active_tab().clone().update(cx, |t, cx| {
        let mut sort = t.sort();
        sort.ascending = ascending;
        t.set_sort(sort, cx);
    });
}

/// The user's terminal in `dir`, saying so when none starts.
fn open_terminal(r: &mut Root, dir: PathBuf, cx: &mut Context<Root>) {
    let window = r.window_id();
    ops::background(
        cx.weak_entity(),
        move || trove_ops::open_terminal(&dir).err(),
        move |_: &mut Root, failed, cx| {
            if let Some(why) = failed {
                nd::chrome::alert(cx, window, Alert::new("Could not open a terminal").message(why), |_, _| {});
            }
        },
    );
}

/// How a drop moves its files: by the file system rule, or as the held key asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transfer {
    /// One file system moves, another copies, as Finder and Files do.
    #[default]
    Auto,
    Copy,
    Move,
}

impl Transfer {
    /// Control (or Alt) copies and Shift moves, whatever the file systems.
    pub fn from_modifiers(m: KeyModifiers) -> Self {
        if m.control || m.alt {
            Self::Copy
        } else if m.shift {
            Self::Move
        } else {
            Self::Auto
        }
    }
}

/// Files dropped on a folder. Dropping into the Trash trashes them; dropping where they already
/// are does nothing unless the drop asks for a copy.
fn drop_onto(r: &mut Root, e: &DropOnto, cx: &mut Context<Root>) {
    let dest = e.dest.clone();
    let sources: Vec<PathBuf> =
        e.paths.iter().filter(|s| **s != dest && !dest.starts_with(s.as_path())).cloned().collect();
    let o = origin(r, cx);
    if dest == crate::env::get().trash {
        if !sources.is_empty() {
            ops::submit(cx, o, Op::Trash { paths: sources });
        }
        return;
    }
    let all_here = sources.iter().all(|s| s.parent() == Some(dest.as_path()));
    if sources.is_empty() || all_here && e.mode != Transfer::Copy {
        return;
    }
    let root = cx.weak_entity();
    transfer(cx, root, o, sources, dest, e.mode);
}

/// Copies or moves `sources` into `dest`: `Auto` moves when they share its file system.
pub fn transfer(
    cx: &mut App,
    root: WeakEntity<Root>,
    origin: Origin,
    sources: Vec<PathBuf>,
    dest: PathBuf,
    mode: Transfer,
) {
    let submit = move |cx: &mut App, sources, dest, copy: bool| {
        let op = if copy { Op::Copy { sources, dest_dir: dest } } else { Op::Move { sources, dest_dir: dest } };
        ops::submit(cx, origin, op);
    };
    if mode != Transfer::Auto {
        submit(cx, sources, dest, mode == Transfer::Copy);
        return;
    }
    let (s, d) = (sources.clone(), dest.clone());
    ops::background(
        root,
        move || {
            let dev = |p: &Path| std::fs::symlink_metadata(p).ok().map(|m| m.dev());
            let to = dev(&d);
            to.is_some() && s.iter().all(|p| dev(p) == to)
        },
        move |_: &mut Root, same, cx| submit(cx, sources, dest, !same),
    );
}
