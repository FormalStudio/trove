//! Tier 3: the header in a narrow window with the preview pane open, the shape of a laptop
//! half-screen, on a folder of pictures. Every header control has to sit clear of the preview pane
//! and answer a real click, and the search field folds into a toggle whose bar Ctrl+F opens.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::Path;
use std::time::{Duration, Instant};

use nd_test::{AppHandle, LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};
use ndshell::wayland::VirtualInput;

const W: u32 = 880;
const H: u32 = 1000;

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

fn bounds(app: &AppHandle, id: &str) -> (f32, f32, f32, f32) {
    let b = app
        .get_by_test_id(id)
        .first()
        .bounding_box()
        .unwrap_or_else(|e| panic!("{id}: {e}"))
        .unwrap_or_else(|| panic!("{id} is not on screen"));
    (b.x, b.y, b.width, b.height)
}

fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn real_header_in_a_narrow_window() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let extra_config = format!(
        r#"hl.window_rule({{ name = "trove-fill", match = {{ class = "dev.formalsnake.trove" }}, size = "{W} {H}", move = "0 0" }})"#
    );
    let compositor = Hyprland::start(Options { width: W, height: H, extra_config, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    std::fs::create_dir_all(&shots).unwrap();
    let toml = shots.join("theme-light.toml");
    std::fs::write(&toml, "appearance = \"light\"\n").unwrap();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    let gallery = Path::new(&runtime).join("trove-demo").join("Gallery");
    std::fs::create_dir_all(&gallery).unwrap();
    for (i, name) in ["Dunes", "Fjord", "Meadow", "Night"].iter().enumerate() {
        let tint = 40 + 50 * i as u8;
        let img = image::RgbImage::from_fn(600, 400, |x, y| image::Rgb([tint, (x / 3) as u8, (y / 2) as u8]));
        img.save(gallery.join(format!("{name}.png"))).unwrap();
    }
    let mut options = LaunchOptions::new(bin())
        .arg(gallery.display().to_string())
        .env("TROVE_DEMO", "1")
        .env("ND_THEME_TOML", toml.display().to_string());
    for (key, value) in &env {
        options = options.env(*key, value.clone());
    }
    let app = launch_app(options).unwrap();
    expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
    let client = compositor.wait_for_client(app.pid(), Duration::from_secs(10)).unwrap();
    let origin = (f64::from(client.at.0), f64::from(client.at.1));
    let mut input = VirtualInput::connect(&compositor).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    app.keyboard().press("down ctrl-i").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let grab = |name: &str| {
        let out = shots.join(format!("{name}.png"));
        assert!(compositor.command("grim").arg(&out).status().unwrap().success(), "grim failed");
        println!("screenshot {}", out.display());
    };
    grab("header-narrow");

    let pane = bounds(&app, "nd-inspector");
    println!("inspector {pane:?}");
    for id in ["back", "pathbar", "view", "preview-toggle"] {
        let b = bounds(&app, id);
        println!("{id} {b:?}");
        assert!(b.0 + b.2 <= pane.0 + 0.5, "{id} at {b:?} runs under the preview pane at {pane:?}");
        assert!(b.2 >= 20., "{id} at {b:?} has no room");
    }

    let click = |input: &mut VirtualInput, id: &str, dx: f32| {
        let (x, y, w, h) = bounds(&app, id);
        let at = (origin.0 + f64::from(x + w * dx), origin.1 + f64::from(y + h / 2.));
        input.click(at.0, at.1).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    };
    // The search field folds into the header's toggle here; Ctrl+F opens its bar.
    app.keyboard().press("ctrl-f").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let field = bounds(&app, "search");
    assert!(field.0 + field.2 <= pane.0 + 0.5, "the search bar at {field:?} runs under the preview pane");
    grab("header-narrow-search");
    app.keyboard().press("escape").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // Here the view switch is folded into one menu button; Grid is its second entry.
    click(&mut input, "view", 0.5);
    // Evdev Down twice (List, Grid), then Return.
    for code in [108, 108, 28] {
        input.key(code, &[]).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }
    eventually("a click on Grid shows the grid", || app.get_by_test_id("files-grid").count().unwrap_or(0) > 0);
    click(&mut input, "preview-toggle", 0.5);
    eventually("a click on the preview toggle hides the pane", || {
        app.get_by_test_id("nd-inspector").bounding_box().ok().flatten().is_none_or(|b| b.width < 1.)
    });
    grab("header-narrow-closed");
}
