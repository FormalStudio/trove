//! Tier 3: the Disk Usage window on a real tree, in a private headless Hyprland. It measures
//! this checkout (sources, and build output when there is some), then goes into the largest
//! folder and back out. Screenshots land in `artifacts/shots/usage/` for review.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::Path;
use std::time::Duration;

use nd_test::{LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

#[test]
fn real_usage_measures_enters_and_leaves() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    const W: u32 = 1400;
    const H: u32 = 900;
    let extra_config = format!(
        r#"hl.window_rule({{ name = "trove-fill", match = {{ class = "dev.formalsnake.trove" }}, size = "{W} {H}", move = "0 0" }})"#
    );
    let compositor = Hyprland::start(Options { width: W, height: H, extra_config, ..Default::default() }).unwrap();
    let env = compositor.client_env();
    let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("shots").join("usage");
    std::fs::create_dir_all(&shots).unwrap();
    let target = std::env::var("TROVE_USAGE_OF").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());

    for scheme in ["dark", "light"] {
        let toml = shots.join(format!("theme-{scheme}.toml"));
        std::fs::write(&toml, format!("appearance = \"{scheme}\"\n")).unwrap();
        let mut options =
            LaunchOptions::new(bin()).arg("--usage").arg(&target).env("ND_THEME_TOML", toml.display().to_string());
        for (key, value) in &env {
            options = options.env(*key, value.clone());
        }
        let app = launch_app(options).unwrap();
        let grab = |name: &str| {
            std::thread::sleep(Duration::from_millis(700));
            let out = shots.join(format!("{name}-{scheme}.png"));
            let status = compositor.command("grim").arg("-l").arg("9").arg(&out).status().unwrap();
            assert!(status.success(), "grim failed");
            println!("screenshot {}", out.display());
        };
        expect(&app.get_by_test_id("usage-list")).to_be_visible().unwrap();
        expect(&app.get_by_test_id("usage-cell-0")).to_be_visible().unwrap();
        grab("root");

        let cell = app.get_by_test_id("usage-cell-0").bounding_box().unwrap().unwrap();
        app.mouse().double_click(cell.x + cell.width / 2., cell.y + cell.height / 2.).unwrap();
        expect(&app.get_by_test_id("usage-crumb-1")).to_be_visible().unwrap();
        grab("inside");

        app.get_by_test_id("usage-up").click().unwrap();
        expect(&app.get_by_test_id("usage-crumb-1")).to_have_count(0).unwrap();
        drop(app);
    }
}
