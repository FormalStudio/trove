//! Tier 3: frames of each view for reading by eye, the demo fixture in a private headless
//! Hyprland at 1920x1080, dark: the list across a wide window, the grid at its default and its
//! largest tiles, the columns view with a picture's preview, the search scope row, an empty
//! Trash, and the file picker over the grid. They land in `artifacts/shots/views/`, which no
//! script copies anywhere.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use nd_test::{LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};

const W: u32 = 1920;
const H: u32 = 1080;

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

fn pictures(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let names = [
        "Atoll", "Basalt", "Bluff", "Cliffs", "Dawn", "Estuary", "Fells", "Geyser", "Heath", "Isle", "Lagoon", "Moor",
        "Oasis", "Peaks", "Reef", "Steppe", "Thicket", "Vale", "Fjord", "Glacier",
    ];
    for (i, name) in names.iter().enumerate() {
        let hue = i as f32 * 0.37;
        let c = |k: f32| (128. + 110. * (hue * std::f32::consts::TAU + k).sin()) as u8;
        let img = image::RgbImage::from_fn(1200, 800, |x, y| {
            let t = (x as f32 / 1200. * 0.5 + y as f32 / 800. * 0.5).clamp(0., 1.);
            let m = |v: u8, w: u8| (v as f32 + (w as f32 - v as f32) * t) as u8;
            image::Rgb([m(c(0.), c(2.)), m(c(2.), c(4.)), m(c(4.), c(0.))])
        });
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

#[test]
fn real_view_frames() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let extra_config = format!(
        r#"hl.window_rule({{ name = "trove-fill", match = {{ class = "dev.formalsnake.trove" }}, size = "{W} {H}", move = "0 0" }})"#
    );
    let compositor = Hyprland::start(Options { width: W, height: H, extra_config, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    let root = Path::new(&runtime).join("trove-demo");
    pictures(&root.join("Gallery"));
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots").join("views");
    std::fs::create_dir_all(&out).unwrap();
    let toml = out.join("theme-dark.toml");
    std::fs::write(&toml, "appearance = \"dark\"\n").unwrap();
    let grab = |name: &str| {
        let file = out.join(format!("{name}.png"));
        let status = compositor.command("grim").arg("-l").arg("9").arg(&file).status().unwrap();
        assert!(status.success(), "grim failed");
        println!("screenshot {}", file.display());
    };
    let launch = |at: &Path| {
        let mut options = LaunchOptions::new(bin())
            .arg(at.display().to_string())
            .env("TROVE_DEMO", "1")
            .env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in &env {
            options = options.env(*key, value.clone());
        }
        launch_app(options).unwrap()
    };

    let app = launch(&root.join("Gallery"));
    let keys = |k: &str, settle: u64| {
        app.keyboard().press(k).unwrap();
        std::thread::sleep(Duration::from_millis(settle));
    };
    expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
    std::thread::sleep(Duration::from_millis(800));
    keys("down down", 600);
    grab("list-wide");

    keys("ctrl-2", 2000);
    grab("grid");
    keys("ctrl-= ctrl-= ctrl-=", 3000);
    grab("grid-large");
    keys("ctrl-0 ctrl-1 ctrl-= ctrl-=", 1500);
    grab("list-large");
    keys("ctrl-0 ctrl-3", 1500);
    grab("columns");

    keys("ctrl-1 ctrl-f", 400);
    app.get_by_test_id("search").type_text("ee").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    grab("search");
    app.get_by_test_id("search-scope-switch").fill("Home").unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    grab("search-home");
    drop(app);

    let app = launch(&root.join(".local/share/Trash/files"));
    expect(&app.get_by_test_id("trash-empty")).to_be_visible().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    grab("trash-empty");
    drop(app);

    // The picker reads its request on stdin, which the automation launch does not give, so it
    // runs as a plain child with a data and config home of its own.
    let own = out.join("picker-home");
    std::fs::create_dir_all(&own).unwrap();
    let mut folder = root.join("Gallery").into_os_string().into_encoded_bytes();
    folder.push(0);
    let request = format!(r#"{{"mode":"open","title":"Open","multiple":true,"current_folder":{folder:?}}}"#);
    let mut picker = Command::new(bin())
        .arg("--pick")
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .env("ND_THEME_TOML", toml.display().to_string())
        .env("ND_APP_DATA_DIR", own.join("data"))
        .env("XDG_CONFIG_HOME", own.join("config"))
        .env("XDG_STATE_HOME", own.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    picker.stdin.take().unwrap().write_all(request.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    grab("picker");
    let wtype = |args: &[&str]| compositor.command("wtype").args(args).status().is_ok_and(|s| s.success());
    if wtype(&["-M", "ctrl", "2", "-m", "ctrl"]) {
        std::thread::sleep(Duration::from_millis(1500));
        grab("picker-grid");
    } else {
        println!("no wtype: the picker's grid frame is skipped");
    }
    let _ = picker.kill();
    let _ = picker.wait();
}
