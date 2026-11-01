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

        app.get_by_test_id("files").press("down down ctrl-i").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab(&compositor, app.pid(), &dir.join(format!("preview-{scheme}.png")));

        app.get_by_test_id("files").press("ctrl-i enter").unwrap();
        app.get_by_test_id("files").press("ctrl-f").unwrap();
        app.get_by_test_id("search").type_text("chap").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        grab(&compositor, app.pid(), &dir.join(format!("search-{scheme}.png")));

        app.get_by_test_id("files").press("ctrl-t").unwrap();
        app.get_by_test_id("view-grid").click().unwrap();
        expect(&app.get_by_test_id("files-grid")).to_be_visible().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        grab(&compositor, app.pid(), &dir.join(format!("grid-{scheme}.png")));
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
