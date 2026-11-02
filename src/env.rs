//! The places the app starts from: the real home, its XDG folders, bookmarks and Recent, or
//! the demo fixture's, so tests and screenshots see the same window on every machine. Chosen
//! once at startup; everything here reads small config files only.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use trove_fs::{Place, PlaceIcon};

pub struct Env {
    pub home: PathBuf,
    /// The folder of trashed files themselves (`Trash/files`).
    pub trash: PathBuf,
    pub places: Vec<Place>,
    pub bookmarks: Vec<Place>,
    /// Recent's paths, fixed for the demo; `None` reads the system's list when asked.
    pub recent: Option<Vec<PathBuf>>,
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
