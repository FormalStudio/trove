//! Tier 3: the real app in a private headless Hyprland on the demo fixture. Screenshots in light
//! and dark, list and grid, into `artifacts/shots/`, and process start to the first frame over
//! ten launches. `TROVE_BIN` picks the binary (a release build for the timing).
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use nd_test::{LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

#[test]
fn real_light_and_dark_list_and_grid() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    std::fs::create_dir_all(&dir).unwrap();
    // GPUI on Linux reads no frame back from a real window, so the compositor's screencopy
    // takes it.
    let grab = |compositor: &Hyprland, pid: u32, out: &Path| {
        let client = compositor.wait_for_client(pid, Duration::from_secs(10)).unwrap();
        let geometry = format!("{},{} {}x{}", client.at.0, client.at.1, client.size.0, client.size.1);
        let status = compositor.command("grim").arg("-g").arg(&geometry).arg(out).status().unwrap();
        assert!(status.success(), "grim -g {geometry} failed");
        println!("screenshot {}", out.display());
    };
    for scheme in ["light", "dark"] {
        let toml = dir.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let mut options =
            LaunchOptions::new(bin()).env("TROVE_DEMO", "1").env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in compositor.client_env() {
            options = options.env(key, value);
        }
        let app = launch_app(options).unwrap();
        expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        grab(&compositor, app.pid(), &dir.join(format!("list-{scheme}.png")));

        app.keyboard().press("down down ctrl-i").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab(&compositor, app.pid(), &dir.join(format!("preview-{scheme}.png")));

        app.keyboard().press("ctrl-i enter ctrl-f").unwrap();
        app.get_by_test_id("search").type_text("chap").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab(&compositor, app.pid(), &dir.join(format!("search-{scheme}.png")));

        app.keyboard().press("escape ctrl-t ctrl-2").unwrap();
        expect(&app.get_by_test_id("files-grid")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        grab(&compositor, app.pid(), &dir.join(format!("grid-{scheme}.png")));

        app.keyboard().press("home right").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        grab(&compositor, app.pid(), &dir.join(format!("grid-selected-{scheme}.png")));
    }
}

/// A context menu, an inline rename, a running copy and the conflict sheet, each a whole-output
/// grab since the menu and the sheet are surfaces of their own. The fixture lives in the private
/// compositor's runtime dir, so the extra files the test writes go with it.
#[test]
fn real_file_operations() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots");
    std::fs::create_dir_all(&dir).unwrap();
    let env = compositor.client_env();
    let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
    let root = Path::new(&runtime).join("trove-demo");
    // Enough files that the copy is still running when the frame is taken.
    let bulk = root.join("Bulk");
    std::fs::create_dir_all(&bulk).unwrap();
    for n in 0..40_000 {
        std::fs::File::create(bulk.join(format!("item {n}"))).unwrap();
    }
    std::fs::create_dir_all(root.join("Documents")).unwrap();
    std::fs::write(root.join("Documents/Notes.md"), "an older copy\n").unwrap();
    let grab = |out: &Path| {
        let status = compositor.command("grim").arg(out).status().unwrap();
        assert!(status.success(), "grim failed");
        println!("screenshot {}", out.display());
    };
    for scheme in ["light", "dark"] {
        let toml = dir.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let mut options =
            LaunchOptions::new(bin()).env("TROVE_DEMO", "1").env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in &env {
            options = options.env(*key, value.clone());
        }
        let app = launch_app(options).unwrap();
        let files = app.get_by_test_id("files-table");
        expect(&files).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(600));

        let row = |name: &str| app.get_by_test_id("cell-0").filter_has_text(name).first();
        // A row is no control, so the pointer goes to its middle.
        let centre = |name: &str| {
            let b = row(name).bounding_box().unwrap().unwrap();
            (b.x + b.width / 2., b.y + b.height / 2.)
        };
        let click = |name: &str| {
            let (x, y) = centre(name);
            app.mouse().click(x, y).unwrap();
        };
        row("todo.txt").right_click().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab(&dir.join(format!("menu-{scheme}.png")));
        app.keyboard().press("escape").unwrap();

        click("todo.txt");
        app.keyboard().press("f2").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        grab(&dir.join(format!("rename-{scheme}.png")));
        app.get_by_test_id("rename").press("escape").unwrap();

        click("Bulk");
        app.keyboard().press("ctrl-c ctrl-v").unwrap();
        expect(&app.get_by_test_id("job-1")).to_be_visible().unwrap();
        grab(&dir.join(format!("copy-job-{scheme}.png")));

        click("Notes.md");
        app.keyboard().press("ctrl-c").unwrap();
        let (x, y) = centre("Documents");
        app.mouse().double_click(x, y).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        app.keyboard().press("ctrl-v").unwrap();
        std::thread::sleep(Duration::from_millis(900));
        grab(&dir.join(format!("conflict-{scheme}.png")));
        app.get_by_test_id("conflict-cancel").click().unwrap();
    }
}

/// The `+ms` of a `[trace +12.3ms] what` line.
fn ms(line: &str) -> Option<f64> {
    line.strip_prefix("[trace +")?.split_once("ms]")?.0.parse().ok()
}

#[test]
fn real_cold_start() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
    let mut first = Vec::new();
    for run in 0..10 {
        let mut child = compositor
            .command(bin())
            .env("TROVE_DEMO", "1")
            .env("TROVE_TRACE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut f = None;
        for line in stderr.lines().map_while(Result::ok) {
            if line.ends_with("] first render") {
                f = ms(&line);
                break;
            }
            if Instant::now() > deadline {
                break;
            }
        }
        compositor.wait_for_client(child.id(), Duration::from_secs(10)).unwrap();
        child.kill().ok();
        child.wait().ok();
        println!("run {run}: first render {f:?} ms");
        first.extend(f);
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(!first.is_empty(), "the trace printed its mark");
    first.sort_by(f64::total_cmp);
    println!("first render {:.0} to {:.0} ms, median {:.0}", first[0], first[first.len() - 1], first[first.len() / 2]);
}
