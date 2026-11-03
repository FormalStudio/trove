//! The session: the open tabs with their folders, views and sorts, the window size, the pane
//! widths and whether the preview shows. Kept in a `Store` (JSON in the state folder, written debounced on its own
//! thread) and read back at launch without touching any of the paths it names.

use std::path::PathBuf;

use nd::prelude::*;
use nd::settings::Store;
use serde::{Deserialize, Serialize};
use trove_fs::{SortKey, SortOrder};

use crate::location::Location;
use crate::tab::View;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub width: f32,
    pub height: f32,
    pub show_preview: bool,
    /// The sidebar's and the preview pane's widths, as the user last dragged them; 0 is unset.
    pub sidebar_width: f32,
    pub preview_width: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    /// `None` is Recent.
    pub path: Option<PathBuf>,
    pub view: SavedView,
    pub sort: SavedSort,
    pub ascending: bool,
    pub folders_first: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SavedView {
    List,
    Grid,
    Columns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SavedSort {
    Name,
    Size,
    Kind,
    Modified,
    Created,
}

impl TabState {
    pub fn new(location: &Location, view: View, sort: SortOrder) -> Self {
        Self {
            path: location.dir().map(PathBuf::from),
            view: match view {
                View::List => SavedView::List,
                View::Grid => SavedView::Grid,
                View::Columns => SavedView::Columns,
            },
            sort: match sort.key {
                SortKey::Size => SavedSort::Size,
                SortKey::Kind => SavedSort::Kind,
                SortKey::Modified => SavedSort::Modified,
                SortKey::Created => SavedSort::Created,
                _ => SavedSort::Name,
            },
            ascending: sort.ascending,
            folders_first: sort.folders_first,
        }
    }

    pub fn location(&self) -> Location {
        self.path.clone().map_or(Location::Recent, Location::Dir)
    }

    pub fn view(&self) -> View {
        match self.view {
            SavedView::List => View::List,
            SavedView::Grid => View::Grid,
            SavedView::Columns => View::Columns,
        }
    }

    pub fn sort(&self) -> SortOrder {
        let key = match self.sort {
            SavedSort::Name => SortKey::Name,
            SavedSort::Size => SortKey::Size,
            SavedSort::Kind => SortKey::Kind,
            SavedSort::Modified => SortKey::Modified,
            SavedSort::Created => SortKey::Created,
        };
        SortOrder { key, ascending: self.ascending, folders_first: self.folders_first }
    }
}

struct Saved(Store<Session>);

/// Where the app keeps state file `name`: `$XDG_STATE_HOME/trove`, inside `ND_APP_DATA_DIR`
/// when a test isolates the app, inside the fixture for the demo.
pub(crate) fn state_file(name: &str) -> PathBuf {
    let env = crate::env::get();
    if env.demo {
        return env.home.join(".local/state/trove").join(name);
    }
    if let Some(root) = std::env::var_os("ND_APP_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(root).join("state").join(name);
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".local/state"));
    state.join("trove").join(name)
}

fn store(cx: &mut App) -> &Store<Session> {
    if !cx.has_global::<Saved>() {
        let path = state_file("session.json");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        cx.set_global(Saved(Store::open(path, 1, None)));
    }
    &cx.global::<Saved>().0
}

/// The session the last run left.
pub fn load(cx: &mut App) -> Session {
    (*store(cx).get()).clone()
}

/// Records `session`; the store writes it a moment later, off the main thread.
pub fn save(cx: &mut App, session: Session) {
    let store = store(cx);
    if *store.get() != session {
        store.update(|s| *s = session);
    }
}
