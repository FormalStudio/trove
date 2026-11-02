//! The user's preferences, through `nd-settings`: `config.toml` in the app's config folder,
//! layered with Nix, fspec and `TROVE_*` variables. The demo keeps its own inside the fixture.

use std::sync::Arc;

use nd::prelude::*;
use nd::settings::{Options, SettingsHandle};
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
    /// Make thumbnails for files on network mounts, which reads every file over the network.
    pub network_thumbnails: bool,
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
        }
    }
}

struct Loaded {
    handle: Option<SettingsHandle<TroveSettings>>,
    fallback: Arc<TroveSettings>,
}

/// Reads the settings once; a file that does not load leaves the defaults in force.
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
    cx.set_global(Loaded { handle, fallback: Arc::new(TroveSettings::default()) });
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
