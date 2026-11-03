//! Tier 3: the README's images and the website screenshot, the demo fixture in a private headless
//! Hyprland with the window filling the output. The website's is the list view with the preview
//! pane open on a picture at 1920x1080, dark and light, in `artifacts/shots/readme-*.png`; the
//! README's are 1600x1000 and dark, in `artifacts/shots/images/`. `scripts/shots` copies both
//! into `docs/`.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::Path;
use std::time::Duration;

use nd_test::{LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

/// A picture's name, size and the two colours of its gradient.
type Shot = (&'static str, u32, u32, [f32; 3], [f32; 3]);

fn pictures(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let shots: [Shot; 12] = [
        ("Aurora.png", 1200, 800, [30., 200., 160.], [80., 40., 150.]),
        ("Canyon.png", 1200, 800, [200., 90., 50.], [250., 200., 120.]),
        ("Coral.png", 1000, 1000, [250., 120., 110.], [255., 210., 160.]),
        ("Dunes.png", 1200, 800, [244., 162., 97.], [120., 60., 140.]),
        ("Fjord.png", 1200, 800, [40., 90., 160.], [170., 220., 230.]),
        ("Glacier.png", 1200, 800, [190., 225., 240.], [60., 110., 170.]),
        ("Harbour.png", 1200, 800, [60., 70., 110.], [240., 170., 110.]),
        ("Meadow.png", 900, 1200, [90., 160., 70.], [230., 230., 150.]),
        ("Night.png", 1200, 800, [20., 24., 60.], [90., 60., 160.]),
        ("Orchard.png", 1200, 800, [120., 170., 60.], [230., 110., 90.]),
        ("Slate.png", 1200, 700, [70., 80., 90.], [180., 190., 200.]),
        ("Tundra.png", 1200, 800, [150., 160., 170.], [235., 240., 245.]),
    ];
    // Enough rows that the list fills a 1080p window.
    let more = [
        "Atoll", "Basalt", "Bluff", "Cliffs", "Dawn", "Estuary", "Fells", "Geyser", "Heath", "Isle", "Lagoon", "Moor",
        "Oasis", "Peaks", "Reef", "Steppe", "Thicket", "Vale",
    ];
    for (i, name) in more.iter().enumerate() {
        let hue = i as f32 * 0.37;
        let c = |k: f32| (128. + 110. * (hue * std::f32::consts::TAU + k).sin()) as u8;
        let img = image::RgbImage::from_fn(640, 420, |x, y| {
            let t = (x as f32 / 640. * 0.5 + y as f32 / 420. * 0.5).clamp(0., 1.);
            let m = |v: u8, w: u8| (v as f32 + (w as f32 - v as f32) * t) as u8;
            image::Rgb([m(c(0.), c(2.)), m(c(2.), c(4.)), m(c(4.), c(0.))])
        });
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
    for (name, w, h, from, to) in shots {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let t = (x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4).clamp(0., 1.);
            let c = |i: usize| (from[i] + (to[i] - from[i]) * t) as u8;
            image::Rgb([c(0), c(1), c(2)])
        });
        img.save(dir.join(name)).unwrap();
    }
}

#[test]
fn real_readme_screenshot() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    // The window rule runs after the harness's float rule, so the size and position win.
    let extra_config = format!(
        r#"hl.window_rule({{ name = "trove-fill", match = {{ class = "dev.formalsnake.trove" }}, size = "{WIDTH} {HEIGHT}", move = "0 0" }})"#
    );
    let compositor =
        Hyprland::start(Options { width: WIDTH, height: HEIGHT, extra_config, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    let root = Path::new(&runtime).join("trove-demo");
    pictures(&root.join("Gallery"));
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    std::fs::create_dir_all(&shots).unwrap();

    for scheme in ["dark", "light"] {
        let toml = shots.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let mut options = LaunchOptions::new(bin())
            .arg(root.join("Gallery").display().to_string())
            .env("TROVE_DEMO", "1")
            .env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in &env {
            options = options.env(*key, value.clone());
        }
        let app = launch_app(options).unwrap();
        expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        app.keyboard().press("down down down down ctrl-i").unwrap();
        std::thread::sleep(Duration::from_millis(1200));

        let client = compositor.wait_for_client(app.pid(), Duration::from_secs(10)).unwrap();
        println!("window {:?} at {:?}", client.size, client.at);
        let out = shots.join(format!("readme-{scheme}.png"));
        let status = compositor.command("grim").arg(&out).status().unwrap();
        assert!(status.success(), "grim failed");
        println!("screenshot {}", out.display());
        drop(app);
    }
}

/// The README's images: the demo fixture in a 1600x1000 output, dark, the window filling it.
/// Each lands in `artifacts/shots/images/<name>.png`; `scripts/shots` copies them to
/// `docs/images/`.
#[test]
fn real_docs_images() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    const W: u32 = 1600;
    const H: u32 = 1000;
    let extra_config = format!(
        r#"hl.window_rule({{ name = "trove-fill", match = {{ class = "dev.formalsnake.trove" }}, size = "{W} {H}", move = "0 0" }})"#
    );
    let compositor = Hyprland::start(Options { width: W, height: H, extra_config, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    pictures(&Path::new(&runtime).join("trove-demo").join("Gallery"));
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    let images = shots.join("images");
    std::fs::create_dir_all(&images).unwrap();
    let toml = shots.join("theme-dark.toml");
    std::fs::write(&toml, "appearance = \"dark\"\n").unwrap();
    let grab = |name: &str| {
        let out = images.join(format!("{name}.png"));
        let status = compositor.command("grim").arg("-l").arg("9").arg(&out).status().unwrap();
        assert!(status.success(), "grim failed");
        println!("screenshot {}", out.display());
    };

    let mut options = LaunchOptions::new(bin()).env("TROVE_DEMO", "1").env("ND_THEME_TOML", toml.display().to_string());
    for (key, value) in &env {
        options = options.env(*key, value.clone());
    }
    let app = launch_app(options).unwrap();
    let keys = |k: &str, settle: u64| {
        app.keyboard().press(k).unwrap();
        std::thread::sleep(Duration::from_millis(settle));
    };
    expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
    std::thread::sleep(Duration::from_millis(600));

    // A second tab, in the gallery, with a picture in the preview pane.
    keys("ctrl-t", 400);
    keys("g a l enter", 600);
    keys("f j o ctrl-i", 1500);
    grab("list");

    keys("ctrl-i ctrl-2", 1500);
    grab("grid");

    keys("ctrl-1 space", 1200);
    grab("quicklook");

    keys("space ctrl-3", 1500);
    grab("columns");

    // The gallery's search, switched to all of Home.
    keys("ctrl-1 ctrl-f", 400);
    app.get_by_test_id("search").type_text("re").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    app.get_by_test_id("search-scope-switch").fill("Home").unwrap();
    std::thread::sleep(Duration::from_millis(1000));
    grab("search");
}

/// The Properties and batch rename sheets over the demo fixture, dark, at 1600x1000, opened from
/// the files menu as a user would. They land in `artifacts/shots/sheets/` for review; nothing
/// copies them into `docs/`.
#[test]
fn real_sheet_shots() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    // No size rule: it would match the sheets too, which share the app's class.
    let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    let sheets = shots.join("sheets");
    std::fs::create_dir_all(&sheets).unwrap();
    let toml = shots.join("theme-dark.toml");
    std::fs::write(&toml, "appearance = \"dark\"\n").unwrap();
    let grab = |name: &str| {
        let out = sheets.join(format!("{name}.png"));
        let status = compositor.command("grim").arg("-l").arg("9").arg(&out).status().unwrap();
        assert!(status.success(), "grim failed");
        println!("screenshot {}", out.display());
    };

    let mut options = LaunchOptions::new(bin()).env("TROVE_DEMO", "1").env("ND_THEME_TOML", toml.display().to_string());
    for (key, value) in &env {
        options = options.env(*key, value.clone());
    }
    let app = launch_app(options).unwrap();
    let keys = |k: &str, settle: u64| {
        app.keyboard().press(k).unwrap();
        std::thread::sleep(Duration::from_millis(settle));
    };
    // The drawn menu is not in the semantic tree, so an item is reached on the keyboard: Up from
    // no row lands on the last one.
    let menu_up = |steps: usize| {
        std::thread::sleep(Duration::from_millis(400));
        keys(&vec!["up"; steps].join(" "), 200);
        keys("enter", 900);
    };
    expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
    std::thread::sleep(Duration::from_millis(600));

    keys("d o c enter", 800);
    app.get_by_text("Quarterly report.docx").right_click().unwrap();
    menu_up(1);
    grab("properties");
    keys("escape", 600);

    // A row is no control, so the pointer goes to its middle.
    let click_at = |locator: nd_test::Locator, fx: f32| {
        let b = locator.bounding_box().unwrap().unwrap();
        app.mouse().click(b.x + b.width * fx, b.y + b.height / 2.).unwrap();
    };
    click_at(app.get_by_test_id("cell-0").filter_has_text("chapter1.md").first(), 0.5);
    keys("shift-down shift-down", 400);
    app.get_by_text("chapter2.md").right_click().unwrap();
    // Properties, Move to Trash, Copy path, Compress, then "Rename 3 items…".
    menu_up(5);
    app.get_by_test_id("rename-find").type_text("chapter").unwrap();
    app.get_by_test_id("rename-with").type_text("Part ").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    grab("batch-rename");

    app.get_by_test_id("rename-regex").click().unwrap();
    app.get_by_test_id("rename-find").fill(r"chapter\d+").unwrap();
    app.get_by_test_id("rename-with").fill("Roadmap").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    grab("batch-rename-conflict");

    app.get_by_test_id("rename-mode").fill("Number").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    grab("batch-rename-number");
}

/// The window chrome, dark and light, in `artifacts/shots/chrome/`: one tab, three tabs, Quick
/// Look over them, the app menu, Preferences and the keyboard shortcuts.
#[test]
fn real_chrome_shots() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    const W: u32 = 1600;
    const H: u32 = 1000;
    let compositor = Hyprland::start(Options { width: W, height: H, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    let gallery = Path::new(&runtime).join("trove-demo").join("Gallery");
    pictures(&gallery);
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    let out = shots.join("chrome");
    std::fs::create_dir_all(&out).unwrap();

    for scheme in ["dark", "light"] {
        let toml = shots.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let grab = |name: &str| {
            let file = out.join(format!("{name}-{scheme}.png"));
            let status = compositor.command("grim").arg("-l").arg("9").arg(&file).status().unwrap();
            assert!(status.success(), "grim failed");
            println!("screenshot {}", file.display());
        };
        let mut options = LaunchOptions::new(bin())
            .arg(gallery.display().to_string())
            .env("TROVE_DEMO", "1")
            .env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in &env {
            options = options.env(*key, value.clone());
        }
        let app = launch_app(options).unwrap();
        let keys = |k: &str, settle: u64| {
            app.keyboard().press(k).unwrap();
            std::thread::sleep(Duration::from_millis(settle));
        };
        expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        keys("down down down down down", 800);
        grab("one-tab");

        // Home and Documents beside the gallery.
        keys("ctrl-t", 400);
        keys("backspace", 600);
        keys("ctrl-t", 400);
        keys("d o c enter", 800);
        grab("three-tabs");

        keys("alt-1", 600);
        keys("space", 1200);
        grab("quicklook");
        keys("space", 600);

        app.get_by_test_id("app-menu").click().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab("menu");
        keys("escape", 400);
        keys("ctrl-?", 800);
        grab("shortcuts");
        app.get_by_test_id("shortcuts-done").click().unwrap();
        std::thread::sleep(Duration::from_millis(400));

        keys("ctrl-,", 1200);
        grab("preferences");
        drop(app);
    }
}
