//! The places the app starts from: the real home, its XDG folders, bookmarks and Recent, or
//! the demo fixture's, so tests and screenshots see the same window on every machine. Chosen
//! once at startup; everything here reads small config files only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use trove_fs::{Place, PlaceIcon, Usage, Volume, VolumeKind};

pub struct Env {
    pub home: PathBuf,
    /// The folder of trashed files themselves (`Trash/files`).
    pub trash: PathBuf,
    pub places: Vec<Place>,
    pub bookmarks: Vec<Place>,
    /// Recent's paths, fixed for the demo; `None` reads the system's list when asked.
    pub recent: Option<Vec<PathBuf>>,
    /// The demo's mounts and what each has free, inside the fixture; `None` reads the system's.
    pub volumes: Option<Vec<(Volume, Option<Usage>)>>,
    /// The demo fixture: nothing outside its root is written (no bookmarks file, no
    /// mimeapps.list, no volume trashes).
    pub demo: bool,
}

static ENV: OnceLock<Env> = OnceLock::new();

pub fn get() -> &'static Env {
    ENV.get_or_init(system)
}

/// Uses the demo fixture rooted at `root` for the rest of the process. Returns false when the
/// environment was already chosen.
pub fn use_demo(root: &Path) -> bool {
    let at = |p: &str| root.join(p);
    let place = |label: &str, icon| Place { label: label.to_owned(), path: at(label), icon };
    let mut places = vec![Place { label: "Home".into(), path: root.to_path_buf(), icon: PlaceIcon::Home }];
    places.extend([
        place("Desktop", PlaceIcon::Desktop),
        place("Documents", PlaceIcon::Documents),
        place("Downloads", PlaceIcon::Downloads),
        place("Music", PlaceIcon::Music),
        place("Pictures", PlaceIcon::Pictures),
        place("Videos", PlaceIcon::Videos),
    ]);
    let env = Env {
        home: root.to_path_buf(),
        trash: at(".local/share/Trash/files"),
        places,
        bookmarks: vec![
            Place { label: "trove".into(), path: at("Projects/trove"), icon: PlaceIcon::Folder },
            Place { label: "Holiday".into(), path: at("Pictures/Holiday"), icon: PlaceIcon::Folder },
        ],
        recent: Some(crate::demo::RECENT.iter().map(|p| at(p)).collect()),
        volumes: Some(crate::demo::volumes(root)),
        demo: true,
    };
    ENV.set(env).is_ok()
}

fn system() -> Env {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    Env {
        trash: trove_fs::trash_dir().join("files"),
        places: trove_fs::user_dirs(),
        bookmarks: trove_fs::bookmarks(),
        recent: None,
        volumes: None,
        demo: false,
        home,
    }
}

/// Recent's paths, newest first. Reads `recently-used.xbel`, so call it off the main thread.
pub fn recent(limit: usize) -> Vec<PathBuf> {
    match &get().recent {
        Some(fixed) => fixed.iter().take(limit).cloned().collect(),
        None => trove_fs::recent(limit),
    }
}

/// The mounted volumes: the demo's fixed set, else the system's from procfs.
pub fn volumes() -> Vec<Volume> {
    match &get().volumes {
        Some(fixed) => fixed.iter().map(|(v, _)| v.clone()).collect(),
        None => trove_fs::volumes(),
    }
}

/// Mount changes as they happen; the demo's set never changes.
pub fn watch_volumes() -> Option<flume::Receiver<Vec<Volume>>> {
    get().volumes.is_none().then(trove_fs::watch_volumes)
}

/// What the volume holding `path` has free: the demo answers from its fixed set at once (the
/// deepest volume holding the path, else the first), anything else asks statvfs off the main
/// thread.
pub fn usage(path: PathBuf, timeout: Duration) -> flume::Receiver<Option<Usage>> {
    let Some(fixed) = &get().volumes else { return trove_fs::usage(path, timeout) };
    let found = fixed
        .iter()
        .filter(|(v, _)| path.starts_with(&v.mount_point))
        .max_by_key(|(v, _)| v.mount_point.components().count())
        .or_else(|| fixed.first())
        .and_then(|(_, u)| *u);
    let (tx, rx) = flume::bounded(1);
    let _ = tx.send(found);
    rx
}

/// Whether `kind` is a disk of this computer or one plugged into it, rather than a share.
pub fn is_device(kind: VolumeKind) -> bool {
    matches!(kind, VolumeKind::Root | VolumeKind::Internal | VolumeKind::Removable | VolumeKind::Phone)
}
