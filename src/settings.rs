//! The user's preferences, through `nd-settings`: `config.toml` in the app's config folder,
//! layered with Nix, fspec and `TROVE_*` variables. The demo keeps its own inside the fixture.

use std::sync::Arc;

use nd::prelude::*;
use nd::settings::{Event, Options, SettingsHandle, WatchGuard};
use serde::{Deserialize, Serialize};

#[derive(Settings, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "nd::schemars")]
#[serde(default, deny_unknown_fields)]
pub struct TroveSettings {
    /// Show hidden files (Ctrl+H).
    pub show_hidden: bool,
    /// Folders before files, whatever the sort.
    pub folders_first: bool,
    /// The view a new tab opens in.
    pub default_view: DefaultView,
    /// Open folders and files with one click instead of two.
    pub single_click: bool,
    /// Ask before moving items to the Trash.
    pub confirm_trash: bool,
    /// Read files on network mounts to make their thumbnails.
    pub network_thumbnails: bool,
    /// Row size in the list and columns views, 0 to 2 (Ctrl+= and Ctrl+- there).
    pub list_zoom: u8,
    /// Tile size in the grid view, 0 to 4 (Ctrl+= and Ctrl+- there).
    pub grid_zoom: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "nd::schemars")]
#[serde(rename_all = "kebab-case")]
pub enum DefaultView {
    List,
    Grid,
}

impl Default for TroveSettings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            folders_first: true,
            default_view: DefaultView::List,
            single_click: false,
            confirm_trash: false,
            network_thumbnails: false,
            list_zoom: 0,
            grid_zoom: crate::zoom::GRID_DEFAULT as u8,
        }
    }
}

/// Notified after every change to the settings, from Preferences, a shortcut or the file edited
/// by hand, so each window brings its open tabs up to date.
pub struct Changes;

struct Loaded {
    handle: Option<SettingsHandle<TroveSettings>>,
    fallback: Arc<TroveSettings>,
    changes: Entity<Changes>,
    /// The handle's change reports, which the file watcher sends from its own thread.
    reports: flume::Receiver<()>,
    _watch: Option<WatchGuard>,
}

/// Reads the settings and watches their files; a file that does not load leaves the defaults
/// in force.
pub fn init(cx: &mut App) {
    if cx.has_global::<Loaded>() {
        return;
    }
    let env = crate::env::get();
    let mut opts = Options::new("trove");
    if env.demo {
        opts = opts.paths(nd::settings::Paths::in_dir(&env.home.join(".config/trove")));
    }
    let handle = match SettingsHandle::<TroveSettings>::load(opts) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("trove: settings: {e}");
            None
        }
    };
    let (tx, reports) = flume::unbounded();
    let mut watch = None;
    if let Some(h) = &handle {
        nd::components::forms::settings::provide(cx, h.clone());
        h.on_event(move |e| {
            if matches!(e, Event::Changed(_)) {
                let _ = tx.send(());
            }
        });
        watch = h.watch().map_err(|e| eprintln!("trove: settings: not watched: {e}")).ok();
    }
    let changes = cx.new(|_| Changes);
    crate::pump::pump(reports.clone(), changes.downgrade(), |_, _, cx| {
        cx.notify();
        true
    });
    cx.set_global(Loaded { handle, fallback: Arc::new(TroveSettings::default()), changes, reports, _watch: watch });
}

/// What to observe for changes to the settings.
pub fn changes(cx: &App) -> Option<Entity<Changes>> {
    cx.try_global::<Loaded>().map(|l| l.changes.clone())
}

/// Delivers the changes waiting where no host runs to pump them (tier 1 tests). Returns
/// whether there were any.
pub fn drain(cx: &mut App) -> bool {
    let Some(l) = cx.try_global::<Loaded>() else { return false };
    if l.reports.try_iter().count() == 0 {
        return false;
    }
    l.changes.clone().update(cx, |_, cx| cx.notify());
    true
}

pub fn get(cx: &App) -> Arc<TroveSettings> {
    match cx.try_global::<Loaded>() {
        Some(Loaded { handle: Some(h), .. }) => h.get(),
        Some(l) => l.fallback.clone(),
        None => Arc::new(TroveSettings::default()),
    }
}

/// Writes one key, keeping the file's comments. A key Nix or fspec manages stays as it is.
pub fn set<V: Serialize>(cx: &App, key: &str, value: &V) {
    if let Some(Loaded { handle: Some(h), .. }) = cx.try_global::<Loaded>()
        && let Err(e) = h.set(key, value)
    {
        eprintln!("trove: settings: {key}: {e}");
    }
}
