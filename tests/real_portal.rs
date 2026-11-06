//! Tier 3: Trove behind the real xdg-desktop-portal and as `org.freedesktop.FileManager1`, in nd's
//! private headless Hyprland, on a session bus of the test's own that activates nothing, so no
//! call reaches the desktop's bus or starts one of its services.
//!
//! - `real_x11_caller_gets_a_modal_picker`: a GTK 3 app on the instance's XWayland
//!   (`tests/portal_caller.py`) asks the portal for a file over its window. The picker runs on
//!   XWayland too, names the caller's window in WM_TRANSIENT_FOR, Hyprland maps it as a modal X11
//!   window stacked over the caller, and Escape answers the caller "cancelled".
//! - `real_show_item_properties`: `FileManager1.ShowItemProperties` on a file, served by `trove
//!   --file-manager` (what D-Bus starts with no Trove running), opens a Trove window with the
//!   Properties sheet over it.
//! - `real_running_trove_shows_items`: with Trove already open, `ShowItems` lands in that process
//!   as a second window of its own (the frame shows the item selected).
//!
//! Frames and logs land in `artifacts/portal/`. Needs xdg-desktop-portal, python3-gobject with
//! GTK 3, xprop and grim. It opens real windows, so it runs only under nextest or with `ND_REAL=1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use ndshell::hyprland::{Hyprland, Options};
use ndshell::wayland::{VirtualInput, keys};
use trove::file_manager::{NAME as FILE_MANAGER_NAME, PATH as FILE_MANAGER_PATH};
use trove::portal::PORTAL_NAME;

const PORTAL_FRONTEND: &str = "org.freedesktop.portal.Desktop";
const APP_ID: &str = "dev.formalsnake.trove";

fn real_allowed() -> bool {
    std::env::var_os("NEXTEST").is_some() || std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

fn artifacts() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts").join("portal");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One run: the compositor, a private bus, Trove's portal service on it and whatever the test
/// starts, each child stopped by its pid when the run ends.
struct Desktop {
    hyprland: Hyprland,
    dir: tempfile::TempDir,
    bus: String,
    children: Vec<Child>,
}

impl Desktop {
    fn start(xwayland: bool) -> Self {
        // Hyprland reads `_NET_WM_STATE_MODAL` into the window; the tag shows it in `j/clients`.
        let extra_config = r#"hl.window_rule({ name = "nd-modal", match = { modal = true }, tag = "+modal" })"#;
        let hyprland =
            Hyprland::start(Options { xwayland, extra_config: extra_config.into(), ..Default::default() }).unwrap();
        let dir = tempfile::tempdir().unwrap();
        for sub in ["home/Reports", "config", "data", "state", "cache", "portals", "services"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("home/Reports/q3.txt"), "Q3\n").unwrap();
        let portal = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/linux/trove.portal");
        std::fs::copy(portal, dir.path().join("portals/trove.portal")).unwrap();
        // With XDG_DESKTOP_PORTAL_DIR set, xdg-desktop-portal reads its portals.conf there too.
        std::fs::write(dir.path().join("portals/portals.conf"), "[preferred]\ndefault=trove\n").unwrap();
        let socket = dir.path().join("bus");
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={socket}</listen>
  <servicedir>{services}</servicedir>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                socket = socket.display(),
                services = dir.path().join("services").display(),
            ),
        )
        .unwrap();
        let mut this = Self { hyprland, bus: format!("unix:path={}", socket.display()), dir, children: Vec::new() };
        let mut daemon = Command::new("dbus-daemon");
        daemon.arg(format!("--config-file={}", config.display())).args(["--nofork", "--nopidfile"]);
        this.spawn(daemon, "dbus-daemon");
        let end = Instant::now() + Duration::from_secs(5);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < end, "the private bus never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut portal = this.command(bin());
        portal.arg("--portal");
        this.spawn(portal, "trove-portal");
        this.wait_name(PORTAL_NAME);
        this
    }

    /// `program` as a client of the compositor and the private bus, with a home and XDG dirs of
    /// the run's own, so nothing reads or writes the user's.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = self.hyprland.command(program);
        let dir = self.dir.path();
        command
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus)
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("XDG_CURRENT_DESKTOP", "nd-test")
            .env("NO_AT_BRIDGE", "1");
        command
    }

    /// Starts `command` with its output in `artifacts/portal/<name>.log`.
    fn spawn(&mut self, command: Command, name: &str) -> &mut Child {
        let log = std::fs::File::create(artifacts().join(format!("{name}.log"))).unwrap();
        self.spawn_with(command, name, log.try_clone().unwrap().into(), log)
    }

    fn spawn_with(&mut self, mut command: Command, name: &str, stdout: Stdio, log: std::fs::File) -> &mut Child {
        command.stdin(Stdio::null()).stdout(stdout).stderr(log);
        let child = command.spawn().unwrap_or_else(|e| panic!("{name}: {e}"));
        self.children.push(child);
        self.children.last_mut().unwrap()
    }

    fn connection(&self) -> zbus::Connection {
        async_io::block_on(zbus::connection::Builder::address(self.bus.as_str()).unwrap().build()).unwrap()
    }

    fn wait_name(&self, name: &str) {
        let conn = self.connection();
        let proxy = async_io::block_on(zbus::fdo::DBusProxy::new(&conn)).unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let bus_name = zbus::names::BusName::try_from(name).unwrap();
        while !async_io::block_on(proxy.name_has_owner(bus_name.clone())).unwrap() {
            assert!(Instant::now() < end, "nothing took {name} on the private bus");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// xdg-desktop-portal on the private bus, with Trove as its only backend.
    fn start_portal_frontend(&mut self) {
        let dir = self.dir.path();
        let mut frontend = self.command("/usr/libexec/xdg-desktop-portal");
        frontend.arg("--verbose").env("XDG_DESKTOP_PORTAL_DIR", dir.join("portals"));
        self.spawn(frontend, "xdg-desktop-portal");
        self.wait_name(PORTAL_FRONTEND);
    }

    /// `FileManager1.<method>` on `item`, as a browser's "Show in folder" sends it.
    fn show(&self, method: &str, item: &Path) {
        let uri = format!("file://{}", item.display());
        async_io::block_on(self.connection().call_method(
            Some(FILE_MANAGER_NAME),
            FILE_MANAGER_PATH,
            Some(FILE_MANAGER_NAME),
            method,
            &(vec![uri.as_str()], ""),
        ))
        .unwrap();
    }

    fn clients(&self) -> Vec<serde_json::Value> {
        let clients: serde_json::Value = serde_json::from_str(&self.hyprland.request("j/clients").unwrap()).unwrap();
        clients.as_array().cloned().unwrap_or_default()
    }

    /// The mapped windows of class `class` once there are `count` of them and their geometry has
    /// settled.
    fn wait_windows(&self, class: &str, count: usize, timeout: Duration) -> Vec<serde_json::Value> {
        let mapped = || -> Vec<serde_json::Value> {
            self.clients()
                .into_iter()
                .filter(|c| c["class"] == class && c["mapped"] == true && c["size"][0].as_i64().unwrap_or(0) > 0)
                .collect()
        };
        let end = Instant::now() + timeout;
        while mapped().len() < count {
            assert!(Instant::now() < end, "{count} {class} windows did not map in {timeout:?}: {:#?}", self.clients());
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(800));
        mapped()
    }

    fn screenshot(&self, name: &str) -> PathBuf {
        let out = artifacts().join(format!("{name}.png"));
        let status = self.hyprland.command("grim").arg(&out).status().expect("grim runs (grim package)");
        assert!(status.success(), "grim failed");
        println!("screenshot {}", out.display());
        out
    }

    /// `xprop` against the instance's X server.
    fn xprop(&self, args: &[&str]) -> String {
        let display = self.hyprland.x11_display().unwrap();
        let out = Command::new("xprop").arg("-display").arg(display).args(args).output().expect("xprop runs (xprop)");
        assert!(out.status.success(), "xprop {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The X window ids in a root window list property, bottom first for the stacking one.
    fn x_list(&self, property: &str) -> Vec<u32> {
        let line = self.xprop(&["-root", property]);
        line.split_once('#')
            .map(|(_, ids)| {
                ids.split(',').filter_map(|id| u32::from_str_radix(id.trim().trim_start_matches("0x"), 16).ok())
            })
            .into_iter()
            .flatten()
            .collect()
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A window as one line: class, title, box, and how Hyprland holds it.
fn summary(c: &serde_json::Value) -> String {
    format!(
        "{} {:?} at {} size {} xwayland={} tags={} focusHistoryID={}",
        c["class"], c["title"], c["at"], c["size"], c["xwayland"], c["tags"], c["focusHistoryID"]
    )
}

/// `(x, y, w, h)` of a window in layout pixels.
fn bounds(c: &serde_json::Value) -> (i64, i64, i64, i64) {
    let n = |v: &serde_json::Value| v.as_i64().unwrap_or(0);
    (n(&c["at"][0]), n(&c["at"][1]), n(&c["size"][0]), n(&c["size"][1]))
}

/// Kills the process `pid` of a window that a child of the run started (a Trove window the
/// portal service spawned), by its pid alone.
fn kill_window_process(pid: i64) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// Reads lines from `stdout` until one starts with `prefix`.
fn read_until(stdout: &mut BufReader<ChildStdout>, prefix: &str) -> String {
    let mut line = String::new();
    loop {
        line.clear();
        let n = stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "the caller exited before printing {prefix:?}");
        if line.starts_with(prefix) {
            return line.trim().to_owned();
        }
    }
}

#[test]
fn real_x11_caller_gets_a_modal_picker() {
    if !real_allowed() {
        println!("skipped: opens real windows; run with ND_REAL=1");
        return;
    }
    let mut desktop = Desktop::start(true);
    desktop.start_portal_frontend();
    let mut keyboard = VirtualInput::connect(&desktop.hyprland).unwrap();

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/portal_caller.py");
    let mut caller = desktop.command("python3");
    caller.arg(script).env("GDK_BACKEND", "x11").env("GTK_USE_PORTAL", "1");
    let log = std::fs::File::create(artifacts().join("caller.log")).unwrap();
    let stdout = desktop.spawn_with(caller, "caller", Stdio::piped(), log).stdout.take().unwrap();
    let mut stdout = BufReader::new(stdout);
    let line = read_until(&mut stdout, "caller xid=");
    let caller_xid = u32::from_str_radix(line.trim_start_matches("caller xid=0x"), 16).unwrap();

    let picker = desktop.wait_windows(APP_ID, 1, Duration::from_secs(30)).remove(0);
    let parent = desktop.clients().into_iter().find(|c| c["title"] == "Portal caller").expect("the caller's window");
    println!("caller {}", summary(&parent));
    println!("picker {}", summary(&picker));
    let frame = desktop.screenshot("x11-picker");

    assert_eq!(parent["xwayland"], true, "the caller is not on XWayland");
    assert_eq!(picker["xwayland"], true, "the picker for an x11: parent did not run on XWayland");
    assert!(
        picker["tags"].as_array().is_some_and(|t| t.iter().any(|t| t.as_str().is_some_and(|t| t.starts_with("modal")))),
        "Hyprland does not see the picker as modal: {}",
        picker["tags"]
    );
    assert_eq!(picker["focusHistoryID"], 0, "the picker does not have the focus");
    let ((px, py, pw, ph), (cx, cy, cw, ch)) = (bounds(&picker), bounds(&parent));
    assert!(px < cx + cw && cx < px + pw && py < cy + ch && cy < py + ph, "the picker does not overlap the caller");

    let windows = desktop.x_list("_NET_CLIENT_LIST");
    let class_of = |id: u32| desktop.xprop(&["-id", &format!("0x{id:x}"), "WM_CLASS"]);
    let picker_xid = *windows.iter().find(|id| class_of(**id).contains(APP_ID)).expect("the picker's X window");
    let props = desktop.xprop(&["-id", &format!("0x{picker_xid:x}"), "WM_TRANSIENT_FOR", "_NET_WM_STATE"]);
    println!("picker 0x{picker_xid:x}: {props}");
    assert!(
        props.contains(&format!("window id # 0x{caller_xid:x}")),
        "WM_TRANSIENT_FOR does not name the caller 0x{caller_xid:x}: {props}"
    );
    let stacking = desktop.x_list("_NET_CLIENT_LIST_STACKING");
    let above = |a: u32, b: u32| stacking.iter().position(|id| *id == a) > stacking.iter().position(|id| *id == b);
    assert!(above(picker_xid, caller_xid), "the picker is not stacked over the caller: {stacking:x?}");
    let log = std::fs::read_to_string(desktop.hyprland.runtime_dir().join("hyprland.log")).unwrap_or_default();
    assert!(!log.contains("Denying transient"), "Hyprland refused the transient");

    keyboard.key(keys::ESC, &[]).unwrap();
    assert_eq!(read_until(&mut stdout, "cancelled"), "cancelled");
    println!("frame {}", frame.display());
}

#[test]
fn real_show_item_properties() {
    if !real_allowed() {
        println!("skipped: opens real windows; run with ND_REAL=1");
        return;
    }
    let mut desktop = Desktop::start(false);
    let mut service = desktop.command(bin());
    service.arg("--file-manager");
    desktop.spawn(service, "trove-file-manager");
    desktop.wait_name(FILE_MANAGER_NAME);
    let item = desktop.dir.path().join("home/Reports/q3.txt");
    desktop.show("ShowItemProperties", &item);
    // The window and the Properties sheet, a window of its own over it.
    let mut windows = desktop.wait_windows(APP_ID, 2, Duration::from_secs(30));
    windows.sort_by_key(|w| -(bounds(w).2 * bounds(w).3));
    for w in &windows {
        println!("window {}", summary(w));
    }
    desktop.screenshot("show-item-properties");
    for w in &windows {
        kill_window_process(w["pid"].as_i64().unwrap_or(-1));
    }
    assert_eq!(windows.len(), 2, "one window and its sheet");
    assert_eq!(windows[0]["pid"], windows[1]["pid"], "the sheet belongs to another process");
    let ((x, y, w, h), (sx, sy, sw, sh)) = (bounds(&windows[0]), bounds(&windows[1]));
    assert!(sx >= x && sy >= y && sx + sw <= x + w && sy + sh <= y + h, "the sheet is not over its window");
}

#[test]
fn real_running_trove_shows_items() {
    if !real_allowed() {
        println!("skipped: opens real windows; run with ND_REAL=1");
        return;
    }
    let mut desktop = Desktop::start(false);
    let home = desktop.dir.path().join("home");
    let mut trove = desktop.command(bin());
    trove.arg(&home);
    desktop.spawn(trove, "trove-running");
    desktop.wait_name(FILE_MANAGER_NAME);
    let first = desktop.wait_windows(APP_ID, 1, Duration::from_secs(30));
    desktop.show("ShowItems", &home.join("Reports/q3.txt"));
    let windows = desktop.wait_windows(APP_ID, 2, Duration::from_secs(30));
    for w in &windows {
        println!("window {}", summary(w));
    }
    desktop.screenshot("show-items-running");
    let pid = first[0]["pid"].clone();
    for w in &windows {
        kill_window_process(w["pid"].as_i64().unwrap_or(-1));
    }
    assert_eq!(windows.len(), 2, "the running window and one for the call");
    assert!(windows.iter().all(|w| w["pid"] == pid), "the call started another process: {windows:#?}");
}
