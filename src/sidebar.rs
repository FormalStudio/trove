//! The sidebar: Favourites, Bookmarks, Devices and Network as one source list. Volumes follow
//! `watch_volumes`; a device's free space fills in when statvfs answers, and a network mount is
//! never touched until it is opened.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use nd::prelude::*;
use trove_fs::{Place, Usage, Volume, VolumeKind};

use crate::format;
use crate::icons;
use crate::location::Location;
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
        pump(trove_fs::watch_volumes(), cx.weak_entity(), |this: &mut Sidebar, sets, cx| {
            if let Some(volumes) = sets.into_iter().last() {
                this.set_volumes(volumes, cx);
            }
            true
        });
        let mut this = Self {
            places: env.places.clone(),
            bookmarks: env.bookmarks.clone(),
            volumes: Vec::new(),
            usage: HashMap::new(),
            eject_errors: HashMap::new(),
            current: None,
            on_open: Rc::new(on_open),
        };
        this.set_volumes(trove_fs::volumes(), cx);
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
        for v in volumes.iter().filter(|v| is_device(v.kind) && !self.usage.contains_key(&v.mount_point)) {
            let mount = v.mount_point.clone();
            pump(
                trove_fs::usage(mount.clone(), Duration::from_secs(2)),
                cx.weak_entity(),
                move |this: &mut Sidebar, answers, cx| {
                    if let Some(u) = answers.into_iter().flatten().last() {
                        this.usage.insert(mount.clone(), u);
                        cx.notify();
                    }
                    false
                },
            );
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

    fn eject(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(mount) = id.strip_prefix("volume:").map(PathBuf::from) else { return };
        let Some(volume) = self.volumes.iter().find(|v| v.mount_point == mount).cloned() else { return };
        self.eject_errors.remove(&mount);
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
                } else if let Some(u) = self.usage.get(&v.mount_point) {
                    node = node.caption(format!("{} free of {}", format::size(u.free), format::size(u.total)));
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

fn is_device(kind: VolumeKind) -> bool {
    matches!(kind, VolumeKind::Root | VolumeKind::Internal | VolumeKind::Removable | VolumeKind::Phone)
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
        let (open, act) = (cx.weak_entity(), cx.weak_entity());
        source_tree("places")
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
    }
}
