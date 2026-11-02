//! The sidebar: Favourites, Bookmarks, Devices and Network as one source list. Volumes follow
//! `watch_volumes`; a device's usage bar fills in when statvfs answers, and a network mount is
//! never touched until it is opened. Files dropped on a place go into it, and a folder dropped
//! between the bookmarks becomes one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use nd::prelude::*;
use trove_fs::{Place, PlaceIcon, Usage, Volume, VolumeKind};

use crate::actions::{DRAG_FILES, DropOnto};
use crate::env::is_device;
use crate::fileops::Transfer;
use crate::format;
use crate::icons;
use crate::location::Location;
use crate::menus;
use crate::pump::pump;

type OnOpen = Rc<dyn Fn(Location, &mut Window, &mut App)>;

const RECENT: &str = "recent";
const TRASH: &str = "trash";
const EJECT: &str = "eject";

pub struct Sidebar {
    places: Vec<Place>,
    bookmarks: Vec<Place>,
    volumes: Vec<Volume>,
    usage: HashMap<PathBuf, Usage>,
    /// Why the last eject of a volume failed, shown under it.
    eject_errors: HashMap<PathBuf, String>,
    current: Option<Location>,
    on_open: OnOpen,
}

impl Sidebar {
    pub fn new(on_open: impl Fn(Location, &mut Window, &mut App) + 'static, cx: &mut Context<Self>) -> Self {
        let env = crate::env::get();
        if let Some(watch) = crate::env::watch_volumes() {
            pump(watch, cx.weak_entity(), |this: &mut Sidebar, sets, cx| {
                if let Some(volumes) = sets.into_iter().last() {
                    this.set_volumes(volumes, cx);
                }
                true
            });
        }
        let mut this = Self {
            places: env.places.clone(),
            bookmarks: env.bookmarks.clone(),
            volumes: Vec::new(),
            usage: HashMap::new(),
            eject_errors: HashMap::new(),
            current: None,
            on_open: Rc::new(on_open),
        };
        this.set_volumes(crate::env::volumes(), cx);
        this
    }

    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    /// Highlights the row for the active tab's location, if it has one.
    pub fn set_current(&mut self, location: &Location, cx: &mut Context<Self>) {
        if self.current.as_ref() != Some(location) {
            self.current = Some(location.clone());
            cx.notify();
        }
    }

    fn set_volumes(&mut self, volumes: Vec<Volume>, cx: &mut Context<Self>) {
        let unknown: Vec<PathBuf> = volumes
            .iter()
            .filter(|v| is_device(v.kind) && !self.usage.contains_key(&v.mount_point))
            .map(|v| v.mount_point.clone())
            .collect();
        for mount in unknown {
            let answer = crate::env::usage(mount.clone(), Duration::from_secs(2));
            if !crate::pump::live() {
                if let Ok(Some(u)) = answer.try_recv() {
                    self.usage.insert(mount, u);
                }
                continue;
            }
            pump(answer, cx.weak_entity(), move |this: &mut Sidebar, answers, cx| {
                if let Some(u) = answers.into_iter().flatten().last() {
                    this.usage.insert(mount.clone(), u);
                    cx.notify();
                }
                false
            });
        }
        self.volumes = volumes;
        cx.notify();
    }

    fn id_for(&self, location: &Location) -> Option<SharedString> {
        let env = crate::env::get();
        match location {
            Location::Recent => Some(RECENT.into()),
            Location::Dir(p) if *p == env.trash => Some(TRASH.into()),
            Location::Dir(p) => {
                let place = self.places.iter().chain(&self.bookmarks).find(|pl| pl.path == *p);
                let volume = self.volumes.iter().find(|v| v.mount_point == *p);
                place.map(|pl| place_id(&pl.path)).or_else(|| volume.map(|v| volume_id(&v.mount_point)))
            }
        }
    }

    fn location_of(&self, id: &str) -> Option<Location> {
        if id == RECENT {
            return Some(Location::Recent);
        }
        if id == TRASH {
            return Some(Location::Dir(crate::env::get().trash.clone()));
        }
        let path = id.strip_prefix("place:").or_else(|| id.strip_prefix("volume:"))?;
        Some(Location::Dir(PathBuf::from(path)))
    }

    /// Adds `path` to Bookmarks, after the ones already there. The demo keeps it in memory:
    /// the real bookmarks file is never written from the fixture.
    pub fn add_bookmark(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.bookmarks.iter().any(|b| b.path == path) {
            return;
        }
        let label = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.bookmarks.push(Place { label, path: path.clone(), icon: PlaceIcon::Folder });
        cx.notify();
        if crate::env::get().demo {
            return;
        }
        crate::ops::background(
            cx.weak_entity(),
            move || trove_fs::add_bookmark(&path, None).map_err(|e| e.to_string()),
            |_, result, _| {
                if let Err(e) = result {
                    eprintln!("trove: adding a bookmark: {e}");
                }
            },
        );
    }

    pub fn remove_bookmark(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|b| b.path != path);
        if self.bookmarks.len() == before {
            return;
        }
        cx.notify();
        if crate::env::get().demo {
            return;
        }
        let path = path.to_path_buf();
        crate::ops::background(
            cx.weak_entity(),
            move || trove_fs::remove_bookmark(&path).map_err(|e| e.to_string()),
            |_, result, _| {
                if let Err(e) = result {
                    eprintln!("trove: removing a bookmark: {e}");
                }
            },
        );
    }

    pub fn eject_mount(&mut self, mount: &std::path::Path, cx: &mut Context<Self>) {
        self.eject(&volume_id(mount), cx);
    }

    /// What the menu acts on: the row the active tab shows.
    fn menu_place(&self) -> Option<menus::Place> {
        let env = crate::env::get();
        match self.current.as_ref()? {
            Location::Recent => Some(menus::Place::Recent),
            Location::Dir(p) if *p == env.trash => Some(menus::Place::Trash),
            Location::Dir(p) => {
                if let Some(v) = self.volumes.iter().find(|v| v.mount_point == *p) {
                    return Some(menus::Place::Volume { mount: p.clone(), ejectable: ejectable(v.kind) });
                }
                let bookmark = self.bookmarks.iter().any(|b| b.path == *p);
                let place = bookmark || self.places.iter().any(|pl| pl.path == *p);
                place.then(|| menus::Place::Folder { path: p.clone(), bookmark })
            }
        }
    }

    /// Files dropped on a place go into it (into the Trash, trashed); folders dropped between
    /// the bookmarks become bookmarks.
    fn dropped(&mut self, e: &Dropped, window: &mut Window, cx: &mut Context<Self>) {
        let kind = e.item.kind.as_ref();
        if kind != DRAG_FILES && kind != DragItem::FILES {
            return;
        }
        let paths: Vec<PathBuf> = e.item.ids.iter().map(|id| PathBuf::from(id.as_ref())).collect();
        if let Some(on) = e.slot.as_ref().and_then(|s| s.on.as_deref()) {
            if let Some(Location::Dir(dest)) = self.location_of(on) {
                let mode = Transfer::from_modifiers(e.modifiers);
                cx.dispatch_action(window.id(), &DropOnto { paths, dest, mode });
            }
            return;
        }
        let into_bookmarks = e.slot.as_ref().and_then(|s| s.parent.as_deref()) == Some("bookmarks")
            || self.bookmarks.is_empty() && e.slot.is_some();
        if !into_bookmarks {
            return;
        }
        crate::ops::background(
            cx.weak_entity(),
            move || paths.into_iter().filter(|p| p.is_dir()).collect::<Vec<_>>(),
            move |this, folders, cx| {
                for path in folders {
                    this.add_bookmark(path, cx);
                }
            },
        );
    }

    fn eject(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(mount) = id.strip_prefix("volume:").map(PathBuf::from) else { return };
        let Some(volume) = self.volumes.iter().find(|v| v.mount_point == mount).cloned() else { return };
        self.eject_errors.remove(&mount);
        if crate::env::get().demo {
            self.volumes.retain(|v| v.mount_point != mount);
            cx.notify();
            return;
        }
        pump(trove_fs::unmount(&volume), cx.weak_entity(), move |this: &mut Sidebar, results, cx| {
            if let Some(Err(why)) = results.into_iter().last() {
                this.eject_errors.insert(mount.clone(), why);
                cx.notify();
            }
            false
        });
    }

    fn nodes(&self) -> Vec<SourceNode> {
        let mut nodes = vec![source_section("favourites", "Favourites")];
        let place = |p: &Place, section: &str| {
            source_node(place_id(&p.path), p.label.clone()).icon(icons::for_place(p.icon)).parent(section.to_owned())
        };
        let mut places = self.places.iter();
        if let Some(home) = places.next() {
            nodes.push(place(home, "favourites"));
        }
        nodes.push(source_node(RECENT, "Recent").icon(icons::recent()).parent("favourites"));
        nodes.extend(places.map(|p| place(p, "favourites")));
        nodes.push(source_node(TRASH, "Trash").icon(Icon::Trash).parent("favourites"));

        if !self.bookmarks.is_empty() {
            nodes.push(source_section("bookmarks", "Bookmarks"));
            nodes.extend(self.bookmarks.iter().map(|p| place(p, "bookmarks")));
        }
        for (section, title, wanted) in [("devices", "Devices", true), ("network", "Network", false)] {
            let vols: Vec<&Volume> = self.volumes.iter().filter(|v| is_device(v.kind) == wanted).collect();
            if vols.is_empty() {
                continue;
            }
            nodes.push(source_section(section, title));
            for v in vols {
                let mut node = source_node(volume_id(&v.mount_point), volume_label(v))
                    .icon(icons::for_volume(v.kind))
                    .parent(section);
                if let Some(why) = self.eject_errors.get(&v.mount_point) {
                    node = node.caption(why.clone());
                } else if let Some(u) = self.usage.get(&v.mount_point).filter(|u| u.total > 0) {
                    // nd: source rows share one fixed pitch, so a caption over a meter runs
                    // into the next row; the free space sits beside the name instead.
                    let used = u.total.saturating_sub(u.free) as f64 / u.total as f64;
                    node = node.badge(format!("{} free", format::size(u.free))).meter(used as f32);
                }
                if ejectable(v.kind) {
                    node = node.actions([EJECT]);
                }
                nodes.push(node);
            }
        }
        nodes
    }
}

fn ejectable(kind: VolumeKind) -> bool {
    matches!(kind, VolumeKind::Removable | VolumeKind::Phone | VolumeKind::Network | VolumeKind::Gvfs)
}

fn place_id(path: &std::path::Path) -> SharedString {
    format!("place:{}", path.display()).into()
}

fn volume_id(path: &std::path::Path) -> SharedString {
    format!("volume:{}", path.display()).into()
}

fn volume_label(v: &Volume) -> String {
    if !v.label.is_empty() {
        return v.label.clone();
    }
    if v.kind == VolumeKind::Root {
        return "Computer".to_owned();
    }
    v.mount_point.file_name().map_or_else(|| v.mount_point.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open, act, drop) = (cx.weak_entity(), cx.weak_entity(), cx.weak_entity());
        let place = self.menu_place();
        // nd: source_tree rows take no context menu and no middle click of their own, so the
        // menu acts on the row the active tab shows and a middle click opens nothing.
        let tree = source_tree("places")
            .size_full()
            .nodes(self.nodes())
            .actions([source_action(EJECT, icons::eject()).tooltip("Eject")])
            .selected_opt(self.current.as_ref().and_then(|l| self.id_for(l)))
            .on_select(move |e: &SourceSelected, window, cx| {
                let found = open.update(cx, |this, _| (this.location_of(&e.id), this.on_open.clone()));
                if let Some((Some(location), on_open)) = found {
                    on_open(location, window, cx);
                }
            })
            .on_action(move |e: &SourceActionClicked, _, cx| {
                if e.action.as_ref() == EJECT {
                    let _ = act.update(cx, |this, cx| this.eject(&e.id, cx));
                }
            })
            .accepts_drops([DRAG_FILES, DragItem::FILES])
            .drops_onto_rows()
            .on_drop(move |e: &Dropped, window, cx| {
                let _ = drop.update(cx, |this, cx| this.dropped(e, window, cx));
            });
        div()
            .id("places-menu")
            .size_full()
            .on_navigate(|e: &NavigateEvent, window, cx| crate::root::history(e, window, cx))
            .child(tree)
            .context_menu(move |menu, _, _| menus::sidebar(menu, place.as_ref()))
    }
}
