//! Tier 3: the real app in a private headless Hyprland on the demo fixture, driven by a virtual
//! pointer and keyboard the compositor treats as hardware: click, rubber band, a drag onto a
//! folder, rename, copy and paste, trash and undo, Quick Look, search and Escape. Each step is
//! read back off the window's semantic tree or the fixture on disk.
//!
//! It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nd_test::{AppHandle, LaunchOptions, expect, launch_app};
use ndshell::hyprland::{Hyprland, Options};
use ndshell::wayland::{Modifier, VirtualInput};

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

/// Evdev codes for the keys the test types.
mod key {
    pub const ESC: u32 = 1;
    pub const ENTER: u32 = 28;
    pub const SPACE: u32 = 57;
    pub const F2: u32 = 60;
    pub const DELETE: u32 = 111;
    pub const DOWN: u32 = 108;
    pub const C: u32 = 46;
    pub const F: u32 = 33;
    pub const V: u32 = 47;
    pub const Z: u32 = 44;

    pub fn letter(c: char) -> u32 {
        const ROW: &str = "qwertyuiop";
        const HOME: &str = "asdfghjkl";
        const BOTTOM: &str = "zxcvbnm";
        if let Some(i) = ROW.find(c) {
            16 + i as u32
        } else if let Some(i) = HOME.find(c) {
            30 + i as u32
        } else if let Some(i) = BOTTOM.find(c) {
            44 + i as u32
        } else {
            panic!("no key for {c:?}")
        }
    }
}

struct Rig {
    compositor: Hyprland,
    input: VirtualInput,
    app: AppHandle,
    root: PathBuf,
    origin: (f64, f64),
}

impl Rig {
    fn start() -> Self {
        let compositor = Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap();
        let env = compositor.client_env();
        let runtime = env.iter().find(|(k, _)| *k == "XDG_RUNTIME_DIR").map(|(_, v)| v.clone()).unwrap();
        let root = Path::new(&runtime).join("trove-demo");
        let mut options = LaunchOptions::new(bin()).env("TROVE_DEMO", "1");
        for (key, value) in env {
            options = options.env(key, value);
        }
        let app = launch_app(options).unwrap();
        expect(&app.get_by_test_id("files-table")).to_be_visible().unwrap();
        let client = compositor.wait_for_client(app.pid(), Duration::from_secs(10)).unwrap();
        let origin = (f64::from(client.at.0), f64::from(client.at.1));
        let input = VirtualInput::connect(&compositor).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        Self { compositor, input, app, root, origin }
    }

    /// The middle of the Name cell of the row showing `name`, on the output.
    fn row(&self, name: &str) -> (f64, f64) {
        let cell = self.app.get_by_test_id("cell-0").filter_has_text(name).first();
        let b = cell
            .bounding_box()
            .unwrap_or_else(|e| panic!("{e}\n{}", self.app.stderr_tail(400).join("\n")))
            .unwrap_or_else(|| panic!("no row {name}"));
        (self.origin.0 + f64::from(b.x + b.width / 2.), self.origin.1 + f64::from(b.y + b.height / 2.))
    }

    fn click(&mut self, at: (f64, f64)) {
        self.input.click(at.0, at.1).unwrap();
        settle(250);
    }

    fn key(&mut self, code: u32, mods: &[Modifier]) {
        self.input.key(code, mods).unwrap();
        settle(150);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let code = key::letter(c.to_ascii_lowercase());
            let mods: &[Modifier] = if c.is_uppercase() { &[Modifier::Shift] } else { &[] };
            self.input.key(code, mods).unwrap();
            settle(60);
        }
        settle(250);
    }

    fn status(&self) -> String {
        self.app.get_by_test_id("status-items").text().unwrap()
    }

    /// Polls until `done` holds, for what a job or a listing's watch delivers.
    fn eventually(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}: the status reads {:?}", self.status());
            settle(100);
        }
    }

    fn has_file(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }
}

fn settle(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// nd: VirtualInput has no middle button and no modifier held across a click or a drag, so
// Shift-click, middle-click and a Ctrl or Shift drag are tier 1 tests on the events nd emits.
#[test]
fn real_input_on_the_demo_fixture() {
    if !real_allowed() {
        println!("skipped: opens a real window; run with ND_REAL=1");
        return;
    }
    let mut rig = Rig::start();

    // The sidebar scrolls under the wheel: its places run past the window's foot at this size.
    // Its rows are no nodes of their own, so the check is the sidebar's pixels moving.
    let places = rig.app.get_by_test_id("places").bounding_box().unwrap().unwrap();
    let (x, y) = (rig.origin.0 + f64::from(places.x), rig.origin.1 + f64::from(places.y));
    let (w, h) = (f64::from(places.width), f64::from(places.height));
    let sidebar = |rig: &Rig| {
        let frame = ndshell::wayland::screenshot(&rig.compositor).unwrap();
        let mut column = Vec::new();
        for dy in (0..h as u32).step_by(2) {
            column.push(frame.rgb((x + w / 3.) as u32, y as u32 + dy));
        }
        column
    };
    let before = sidebar(&rig);
    rig.input.scroll(x + w / 2., y + h / 2., 10).unwrap();
    settle(400);
    assert_ne!(sidebar(&rig), before, "the sidebar did not scroll");

    // A click selects, and the list then takes the arrows itself.
    let notes = rig.row("Notes.md");
    rig.click(notes);
    rig.eventually("a click selects the row", |r| r.status().ends_with("1 selected (2.0 kB)"));
    rig.key(key::DOWN, &[]);
    rig.eventually("Down moves to the next row", |r| r.status().ends_with("1 selected (312 bytes)"));
    rig.key(key::ESC, &[]);
    rig.eventually("Escape deselects", |r| !r.status().contains("selected"));

    // A rubber band from the empty space under the rows up over the last two.
    let todo = rig.row("todo.txt");
    let below = (todo.0 + 200., todo.1 + 120.);
    rig.input.drag(below, 0., notes.1 - below.1, 12, Duration::from_millis(16)).unwrap();
    settle(300);
    rig.eventually("the band selects both rows it crossed", |r| r.status().contains("2 selected"));

    // Escape clears the selection.
    rig.key(key::ESC, &[]);
    rig.eventually("Escape deselects", |r| !r.status().contains("selected"));

    // todo.txt dragged onto Projects moves it there: one file system.
    let projects = rig.row("Projects");
    let todo = rig.row("todo.txt");
    rig.click(todo);
    // Past the double-click interval, so the press that starts the drag is a press of its own.
    settle(600);
    rig.input.drag(todo, projects.0 - todo.0, projects.1 - todo.1, 16, Duration::from_millis(16)).unwrap();
    rig.eventually("the drop moves the file", |r| r.has_file("Projects/todo.txt") && !r.has_file("todo.txt"));

    // F2 preselects the name without its extension, so typing keeps ".md".
    let notes = rig.row("Notes.md");
    rig.click(notes);
    rig.key(key::F2, &[]);
    expect(&rig.app.get_by_test_id("rename")).to_be_visible().unwrap();
    rig.type_text("Plan");
    rig.key(key::ENTER, &[]);
    rig.eventually("the rename lands", |r| r.has_file("Plan.md") && !r.has_file("Notes.md"));

    // Ctrl+C, Ctrl+V beside the original makes a copy.
    rig.eventually("the renamed row shows", |r| {
        r.app.get_by_test_id("cell-0").filter_has_text("Plan.md").count().unwrap_or(0) > 0
    });
    let plan = rig.row("Plan.md");
    rig.click(plan);
    rig.key(key::C, &[Modifier::Control]);
    rig.key(key::V, &[Modifier::Control]);
    rig.eventually("the copy lands", |r| r.has_file("Plan copy.md"));

    // Delete trashes the selected copy, Ctrl+Z puts it back.
    rig.eventually("the copy is selected", |r| r.status().ends_with("1 selected (2.0 kB)"));
    rig.key(key::DELETE, &[]);
    rig.eventually("Delete trashes it", |r| !r.has_file("Plan copy.md"));
    rig.key(key::Z, &[Modifier::Control]);
    rig.eventually("Ctrl+Z restores it", |r| r.has_file("Plan copy.md"));

    // Space opens Quick Look on the selection, Escape closes it.
    let plan = rig.row("Plan.md");
    rig.click(plan);
    rig.key(key::SPACE, &[]);
    expect(&rig.app.get_by_test_id("quicklook")).to_be_visible().unwrap();
    rig.key(key::ESC, &[]);
    expect(&rig.app.get_by_test_id("quicklook")).to_be_hidden().unwrap();

    // Ctrl+F and typing search below the folder; Escape puts the folder back.
    rig.key(key::F, &[Modifier::Control]);
    rig.type_text("chap");
    expect(&rig.app.get_by_test_id("results-table")).to_be_visible().unwrap();
    rig.eventually("the status counts results", |r| r.status().starts_with("3 results"));
    rig.key(key::ESC, &[]);
    expect(&rig.app.get_by_test_id("files-table")).to_be_visible().unwrap();
}
