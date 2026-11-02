//! Tier 3: the preview pane, Quick Look, grid thumbnails, search results and the columns view
//! in the real app, in a private headless Hyprland on the demo fixture, light and dark, into
//! `artifacts/shots/`. Real pictures and text are written into the fixture first; thumbnails
//! go to the fixture's own cache beside it.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use nd_test::{AppHandle, LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

/// A picture's name, size and the two colours of its gradient.
type Shot = (&'static str, u32, u32, [f32; 3], [f32; 3]);

/// Gradients, so a thumbnail and a fitted picture read as pictures.
fn pictures(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let shots: [Shot; 6] = [
        ("Dunes.png", 1200, 800, [244., 162., 97.], [120., 60., 140.]),
        ("Fjord.png", 1200, 800, [40., 90., 160.], [170., 220., 230.]),
        ("Meadow.png", 900, 1200, [90., 160., 70.], [230., 230., 150.]),
        ("Night.png", 1200, 800, [20., 24., 60.], [90., 60., 160.]),
        ("Coral.png", 1000, 1000, [250., 120., 110.], [255., 210., 160.]),
        ("Slate.png", 1200, 700, [70., 80., 90.], [180., 190., 200.]),
    ];
    for (name, w, h, from, to) in shots {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let t = (x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4).clamp(0., 1.);
            let c = |i: usize| (from[i] + (to[i] - from[i]) * t) as u8;
            image::Rgb([c(0), c(1), c(2)])
        });
        img.save(dir.join(name)).unwrap();
    }
}

const PLAN: &str = "# Trove roadmap

Trove is a file manager for people who keep a lot of files.

## Now

- Instant search across Home, with the query language
- Previews for pictures, text, sound and archives
- Thumbnails in the grid, made off the main thread

## Next

- Tags and smart folders
- A faster network listing
- Sync status in the sidebar

## Later

- Plugins for previews
";

struct Rig {
    compositor: Hyprland,
    root: PathBuf,
    shots: PathBuf,
}

impl Rig {
    fn start() -> Self {
        let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
        let env = compositor.client_env();
        let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
        let root = Path::new(&runtime).join("trove-demo");
        pictures(&root.join("Gallery"));
        std::fs::create_dir_all(root.join("Documents")).unwrap();
        std::fs::write(root.join("Documents/Roadmap.md"), PLAN).unwrap();
        let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
        std::fs::create_dir_all(&shots).unwrap();
        Self { compositor, root, shots }
    }

    fn launch(&self, scheme: &str, at: &str) -> AppHandle {
        let toml = self.shots.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let mut options = LaunchOptions::new(bin())
            .arg(self.root.join(at).display().to_string())
            .env("TROVE_DEMO", "1")
            .env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in self.compositor.client_env() {
            options = options.env(key, value);
        }
        let app = launch_app(options).unwrap();
        expect(&app.get_by_test_id("files")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        app
    }

    /// GPUI on Linux reads no frame back from a real window, so the compositor's screencopy
    /// takes it.
    fn grab(&self, app: &AppHandle, name: &str) {
        let out = self.shots.join(format!("{name}.png"));
        let client = self.compositor.wait_for_client(app.pid(), Duration::from_secs(10)).unwrap();
        let geometry = format!("{},{} {}x{}", client.at.0, client.at.1, client.size.0, client.size.1);
        let status = self.compositor.command("grim").arg("-g").arg(&geometry).arg(&out).status().unwrap();
        assert!(status.success(), "grim -g {geometry} failed");
        println!("screenshot {}", out.display());
    }
}

fn settle(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

#[test]
fn real_previews_quick_look_thumbnails_search_and_columns() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let rig = Rig::start();
    for scheme in ["light", "dark"] {
        let app = rig.launch(scheme, "Gallery");
        app.keyboard().press("down ctrl-i").unwrap();
        settle(900);
        rig.grab(&app, &format!("pane-image-{scheme}"));

        app.keyboard().press("space").unwrap();
        expect(&app.get_by_test_id("quicklook")).to_be_visible().unwrap();
        settle(900);
        rig.grab(&app, &format!("quicklook-{scheme}"));
        app.get_by_test_id("quicklook").press("escape").unwrap();
        settle(300);

        app.keyboard().press("ctrl-i ctrl-2").unwrap();
        expect(&app.get_by_test_id("files-grid")).to_be_visible().unwrap();
        settle(1500);
        rig.grab(&app, &format!("grid-thumbnails-{scheme}"));

        app.keyboard().press("ctrl-3").unwrap();
        expect(&app.get_by_test_id("files-columns")).to_be_visible().unwrap();
        app.keyboard().press("down").unwrap();
        expect(&app.get_by_test_id("files-columns")).to_be_visible().unwrap();
        settle(900);
        rig.grab(&app, &format!("columns-{scheme}"));
        drop(app);

        let app = rig.launch(scheme, "Documents");
        app.keyboard().press("r o ctrl-i").unwrap();
        settle(900);
        rig.grab(&app, &format!("pane-text-{scheme}"));
        app.keyboard().press("t a").unwrap();
        settle(900);
        rig.grab(&app, &format!("pane-folder-{scheme}"));
        drop(app);

        let app = rig.launch(scheme, "");
        app.keyboard().press("ctrl-i ctrl-f").unwrap();
        app.get_by_test_id("search").type_text("chap").unwrap();
        expect(&app.get_by_test_id("results-table")).to_be_visible().unwrap();
        settle(700);
        app.get_by_test_id("search").press("down").unwrap();
        settle(900);
        rig.grab(&app, &format!("results-{scheme}"));
        drop(app);
    }
}

/// The states with nothing to list, and a window holding several tabs.
#[test]
fn real_empty_states_and_tabs() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let rig = Rig::start();
    std::fs::create_dir_all(rig.root.join("Empty")).unwrap();
    let locked = rig.root.join("Locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    for scheme in ["light", "dark"] {
        let app = rig.launch(scheme, "Empty");
        rig.grab(&app, &format!("empty-{scheme}"));
        drop(app);

        let app = rig.launch(scheme, "Locked");
        rig.grab(&app, &format!("denied-{scheme}"));
        drop(app);

        let app = rig.launch(scheme, "");
        app.keyboard().press("ctrl-f").unwrap();
        app.get_by_test_id("search").type_text("zzqx").unwrap();
        settle(1200);
        rig.grab(&app, &format!("no-results-{scheme}"));
        app.keyboard().press("escape").unwrap();

        for keys in ["ctrl-t", "m u", "enter", "ctrl-t", "p r", "enter"] {
            app.keyboard().press(keys).unwrap();
            settle(400);
        }
        settle(500);
        rig.grab(&app, &format!("tabs-{scheme}"));
        drop(app);
    }
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}
