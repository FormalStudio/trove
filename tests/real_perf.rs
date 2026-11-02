//! The budgets in `docs/design.md`, measured on the release binary in a private headless
//! Hyprland. Each test prints its numbers; none asserts a budget, since the numbers mean
//! something only on the reference machine or pinned to cores like it.
//!
//! `TROVE_BIN` is the binary (build it with `cargo build --release`), `TROVE_CPUS` the cores it
//! runs on (`taskset -c`, default `8-15`: g815's first eight E-cores, the nearest thing to an
//! i3-N305). Run one at a time, with the machine otherwise quiet:
//! `ND_REAL=1 TROVE_BIN=target/release/trove cargo test --release --test real_perf -- --ignored
//! --nocapture --test-threads 1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nd_test::{LaunchOptions, launch_app};
use ndshell::hyprland::{Hyprland, Options};
use ndshell::wayland::VirtualInput;

fn bin() -> String {
    std::env::var("TROVE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_trove").to_owned())
}

fn cpus() -> String {
    std::env::var("TROVE_CPUS").unwrap_or_else(|_| "8-15".to_owned())
}

fn compositor() -> Hyprland {
    Hyprland::start(Options { width: 1400, height: 900, ..Default::default() }).unwrap()
}

fn unix_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.
}

/// A launch with its stdout and stderr lines, each stamped when it arrived.
struct Run {
    child: Child,
    lines: Arc<Mutex<Vec<(Instant, String)>>>,
    spawned_unix_ms: f64,
    _data: tempfile::TempDir,
}

impl Run {
    fn start(c: &Hyprland, args: &[&str], env: &[(&str, &str)]) -> Run {
        let data = tempfile::tempdir().unwrap();
        let mut cmd = c.command("taskset");
        cmd.arg("-c").arg(cpus()).arg(bin()).args(args);
        cmd.env("TROVE_TRACE", "1")
            .env("ND_APP_DATA_DIR", data.path().join("data"))
            .env("XDG_CACHE_HOME", data.path().join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let spawned_unix_ms = unix_ms();
        let mut child = cmd.spawn().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        collect(out, lines.clone());
        collect(err, lines.clone());
        Run { child, lines, spawned_unix_ms, _data: data }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_line(&self, needle: &str, timeout: Duration) -> Option<(Instant, String)> {
        let t0 = Instant::now();
        loop {
            if let Some(found) = self.lines.lock().unwrap().iter().find(|(_, l)| l.contains(needle)) {
                return Some(found.clone());
            }
            if t0.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn all(&self, needle: &str) -> Vec<String> {
        self.lines.lock().unwrap().iter().filter(|(_, l)| l.contains(needle)).map(|(_, l)| l.clone()).collect()
    }

    fn stop(mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn collect(pipe: impl Read + Send + 'static, lines: Arc<Mutex<Vec<(Instant, String)>>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            lines.lock().unwrap().push((Instant::now(), line));
        }
    });
}

/// The `+ms` of a `[trace +12.3ms] what` line.
fn at_ms(line: &str) -> Option<f64> {
    line.split("[trace +").nth(1)?.split_once("ms]")?.0.parse().ok()
}

/// The `n ms` of a `[trace ..] what took n ms` line.
fn took_ms(line: &str) -> Option<f64> {
    line.split(" took ").nth(1)?.strip_suffix("ms")?.parse().ok()
}

fn field(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

fn status_kb(pid: u32, key: &str) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status.lines().find_map(|l| l.strip_prefix(key)?.trim().strip_suffix("kB")?.trim().parse().ok()).unwrap_or(0)
}

/// utime + stime of every thread, in clock ticks.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = stat.rsplit_once(')').unwrap().1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()
}

/// A labelled launch: its arguments and extra environment.
type Launch<'a> = (&'a str, Vec<&'a str>, Vec<(&'a str, &'a str)>);

/// CPU ticks and voluntary switches per thread, keyed by tid, with the thread's name.
fn per_thread(pid: u32) -> std::collections::BTreeMap<String, (String, u64, u64)> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return out };
    for t in dir.flatten() {
        let tid = t.file_name().to_string_lossy().into_owned();
        let base = format!("/proc/{pid}/task/{tid}");
        let (Ok(stat), Ok(status)) =
            (std::fs::read_to_string(format!("{base}/stat")), std::fs::read_to_string(format!("{base}/status")))
        else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("{base}/comm")).unwrap_or_default().trim().to_owned();
        let f: Vec<&str> = stat.rsplit_once(')').unwrap().1.split_whitespace().collect();
        let ticks = f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap();
        let switches = status
            .lines()
            .find_map(|l| l.strip_prefix("voluntary_ctxt_switches:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        out.insert(tid, (comm, ticks, switches));
    }
    out
}

/// The threads that ran or woke between two `per_thread` reads.
fn busy(
    a: &std::collections::BTreeMap<String, (String, u64, u64)>,
    b: &std::collections::BTreeMap<String, (String, u64, u64)>,
) -> String {
    let mut parts = Vec::new();
    for (tid, (comm, ticks, switches)) in b {
        let (t0, s0) = a.get(tid).map_or((0, 0), |x| (x.1, x.2));
        if ticks > &t0 || switches > &s0 {
            parts.push(format!("{comm}[{tid}] {} ticks {} wakeups", ticks - t0, switches - s0));
        }
    }
    parts.join(", ")
}

fn threads(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task")).map(|d| d.count()).unwrap_or(0)
}

fn spread(v: &[f64]) -> String {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    format!("median {:.0}, min {:.0}, max {:.0} (n={})", s[s.len() / 2], s[0], s[s.len() - 1], s.len())
}

/// A folder of `n` entries: one in fifty a folder, the rest sparse files of mixed kinds, sizes
/// and dates, so every sort key gives a different order.
fn folder_of(n: usize) -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("trove-perf-").tempdir().unwrap();
    let kinds = ["txt", "jpg", "pdf", "rs", "md", "mp3", "zip"];
    for i in 0..n {
        if i % 50 == 0 {
            std::fs::create_dir(dir.path().join(format!("folder {i}"))).unwrap();
            continue;
        }
        let scrambled = (i as u64).wrapping_mul(2_654_435_761) % 1_000_003;
        let file = std::fs::File::create(dir.path().join(format!("file {i}.{}", kinds[i % kinds.len()]))).unwrap();
        file.set_len(scrambled * 13).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000 + scrambled * 97)).unwrap();
    }
    dir
}

fn real_allowed() -> bool {
    std::env::var_os("ND_REAL").is_some_and(|v| v == "1")
}

/// Process start (the spawn) to the end of the first display frame the window presented:
/// nd's `ND_EXIT_AFTER_FIRST_FRAME` prints the wall clock of that frame callback.
#[test]
#[ignore = "a benchmark"]
fn perf_cold_launch() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let home = std::env::home_dir().unwrap();
    let home = home.to_str().unwrap();
    for (label, env) in [("demo", vec![("TROVE_DEMO", "1")]), ("real home", vec![("HOME", home)])] {
        let mut frame = Vec::new();
        let mut render = Vec::new();
        for _ in 0..10 {
            let mut env = env.clone();
            env.push(("ND_EXIT_AFTER_FIRST_FRAME", "1"));
            let run = Run::start(&c, &[], &env);
            let line = run.wait_line("nd-budget: first_frame", Duration::from_secs(20)).expect("first frame").1;
            frame.push(field(&line, "unix_ms").unwrap() - run.spawned_unix_ms);
            if let Some((_, l)) = run.wait_line("] first render", Duration::from_secs(1)) {
                render.push(at_ms(&l).unwrap());
            }
            run.stop();
            std::thread::sleep(Duration::from_millis(300));
        }
        println!("{label}: spawn to first presented frame {}", spread(&frame));
        println!("{label}: process start to first render {}", spread(&render));
    }
}

/// What startup touches: every path syscall of the first five seconds on the real home, with
/// the owner's sshfs mount (`~/.macbook`) present. None may name a network mount.
#[test]
#[ignore = "a benchmark"]
fn perf_startup_touches_no_network_mount() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let home = std::env::home_dir().unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut child = c
        .command("strace")
        .args(std::env::var("TROVE_STRACE_STACK").map(|_| "-k"))
        .args(["-f", "-qq", "-e", "trace=%file,%desc,statfs,fstatfs", "-o"])
        .arg(log.path())
        .arg(bin())
        .env("HOME", &home)
        .env("ND_APP_DATA_DIR", data.path())
        .env("XDG_CACHE_HOME", data.path().join("cache"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let seconds = std::env::var("TROVE_STRACE_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
    std::thread::sleep(Duration::from_secs(seconds));
    let children = std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", child.id())).unwrap_or_default();
    for pid in children.split_whitespace() {
        let _ = std::process::Command::new("kill").arg(pid).status();
    }
    std::thread::sleep(Duration::from_millis(500));
    child.kill().ok();
    child.wait().ok();
    let text = std::fs::read_to_string(log.path()).unwrap();
    if let Ok(keep) = std::env::var("TROVE_STRACE_LOG") {
        std::fs::write(keep, &text).unwrap();
    }
    let network: Vec<PathBuf> = trove_fs::volumes()
        .into_iter()
        .filter(|v| matches!(v.kind, trove_fs::VolumeKind::Network))
        .map(|v| v.mount_point)
        .collect();
    println!("network mounts: {network:?}");
    let mut hits = 0;
    for line in text.lines() {
        if network.iter().any(|m| line.contains(&format!("\"{}", m.display()))) {
            hits += 1;
            if hits < 10 {
                println!("touched: {line}");
                let after = text.split(line).nth(1).unwrap_or_default();
                for frame in after.lines().skip(1).take_while(|l| l.starts_with(" >")).take(40) {
                    println!("{frame}");
                }
            }
        }
    }
    println!("syscalls naming a network mount in the first {seconds} s: {hits} (of {} traced)", text.lines().count());
}

/// A tab opened on a mount that takes ten seconds to answer: the window keeps answering and
/// shows the waiting state after a second.
#[test]
#[ignore = "a benchmark"]
fn perf_dead_mount_tab() {
    if !real_allowed() {
        return;
    }
    let mount = std::env::var("TROVE_SLOW_MOUNT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::home_dir().unwrap().join(".macbook"));
    let c = compositor();
    let cache = tempfile::tempdir().unwrap();
    let mut options = LaunchOptions::new("/usr/bin/taskset")
        .arg("-c")
        .arg(cpus())
        .arg(bin())
        .arg(mount.display().to_string())
        .env("HOME", std::env::home_dir().unwrap().display().to_string())
        .env("XDG_CACHE_HOME", cache.path().display().to_string());
    for (k, v) in c.client_env() {
        options = options.env(k, v);
    }
    let t0 = Instant::now();
    let app = launch_app(options).unwrap();
    println!("automation up after {:.0} ms", t0.elapsed().as_secs_f64() * 1000.);
    let waiting = app.get_by_test_id("waiting");
    let mut shown = None;
    let mut round_trips = Vec::new();
    while t0.elapsed() < Duration::from_secs(8) {
        let r0 = Instant::now();
        let visible = waiting.is_visible().unwrap_or(false);
        round_trips.push(r0.elapsed().as_secs_f64() * 1000.);
        if visible && shown.is_none() {
            shown = Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let k0 = Instant::now();
    app.keyboard().press("ctrl-t").unwrap();
    let tabs = app.get_by_test_id("tabs");
    while !tabs.is_visible().unwrap_or(false) && k0.elapsed() < Duration::from_secs(5) {}
    println!("waiting state shown after {:?}", shown.map(|d| format!("{:.0} ms", d.as_secs_f64() * 1000.)));
    println!("automation round trips while the mount hangs: {}", spread(&round_trips));
    println!("ctrl-t to a second tab on screen: {:.0} ms", k0.elapsed().as_secs_f64() * 1000.);
}

/// Resident memory and CPU while nothing changes, focused and behind another window.
#[test]
#[ignore = "a benchmark"]
fn perf_idle() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let big = folder_of(100_000);
    let path = big.path().display().to_string();
    // An empty home, so the index the app starts two seconds in has nothing to walk.
    let home = tempfile::tempdir().unwrap();
    let home = home.path().display().to_string();
    let runs: [Launch; 2] = [
        ("demo home", vec![], vec![("TROVE_DEMO", "1")]),
        ("100k folder", vec![path.as_str()], vec![("HOME", home.as_str())]),
    ];
    for (label, args, env) in runs {
        let run = Run::start(&c, &args, &env);
        c.wait_for_client(run.pid(), Duration::from_secs(10)).unwrap();
        run.wait_line("] complete", Duration::from_secs(60));
        // Past the index's start (two seconds after the first frame) and its build of the demo.
        std::thread::sleep(Duration::from_secs(10));
        let pid = run.pid();
        println!(
            "{label}: VmRSS {} MB, VmHWM {} MB, {} threads",
            status_kb(pid, "VmRSS:") / 1024,
            status_kb(pid, "VmHWM:") / 1024,
            threads(pid)
        );
        let seconds = std::env::var("TROVE_IDLE_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
        let (a, ta) = (cpu_ticks(pid), per_thread(pid));
        std::thread::sleep(Duration::from_secs(seconds));
        let (b, tb) = (cpu_ticks(pid), per_thread(pid));
        println!("{label}: focused, {} ticks of CPU over {seconds} s: {}", b - a, busy(&ta, &tb));
        let mut other = c.command("foot").arg("sleep").arg("1000").stderr(Stdio::null()).spawn().unwrap();
        c.wait_for_client(other.id(), Duration::from_secs(10)).ok();
        std::thread::sleep(Duration::from_secs(2));
        let (a, ta) = (cpu_ticks(pid), per_thread(pid));
        std::thread::sleep(Duration::from_secs(seconds));
        let (b, tb) = (cpu_ticks(pid), per_thread(pid));
        println!("{label}: unfocused, {} ticks of CPU over {seconds} s: {}", b - a, busy(&ta, &tb));
        other.kill().ok();
        other.wait().ok();
        run.stop();
    }
}

/// A 100k-entry folder: load to the first names, the longest batch the main thread applied,
/// to complete; then a whole-window re-render every frame with the wheel turning over it.
#[test]
#[ignore = "a benchmark"]
fn perf_big_folder() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let n: usize = std::env::var("TROVE_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let big = folder_of(n);
    let path = big.path().display().to_string();
    let view = std::env::var("TROVE_VIEW").unwrap_or_else(|_| "list".to_owned());
    for _ in 0..3 {
        let run = Run::start(&c, &[&path], &[("TROVE_DEFAULT_VIEW", &view)]);
        let load = run.wait_line("] load", Duration::from_secs(20)).unwrap().1;
        let names = run.wait_line("] names", Duration::from_secs(20)).unwrap().1;
        let done = run.wait_line("] complete", Duration::from_secs(60)).unwrap().1;
        std::thread::sleep(Duration::from_millis(1500));
        let applies: Vec<f64> = run.all("] apply took").iter().filter_map(|l| took_ms(l)).collect();
        println!(
            "{n} entries ({view}): load at {:.0} ms, first names {:.0} ms after, complete {:.0} ms after; {} batches, longest {:.1} ms, total {:.0} ms; VmRSS {} MB, VmHWM {} MB",
            at_ms(&load).unwrap(),
            at_ms(&names).unwrap() - at_ms(&load).unwrap(),
            at_ms(&done).unwrap() - at_ms(&load).unwrap(),
            applies.len(),
            applies.iter().copied().fold(0., f64::max),
            applies.iter().sum::<f64>(),
            status_kb(run.pid(), "VmRSS:") / 1024,
            status_kb(run.pid(), "VmHWM:") / 1024,
        );
        run.stop();
    }

    // Scrolling: the wheel turned every 16 ms once the folder is in, while nd re-renders the
    // whole window every frame and reports the frame intervals.
    let seconds = 12.;
    let run = Run::start(&c, &[&path], &[("TROVE_DEFAULT_VIEW", &view), ("ND_ANIMATE", &format!("{seconds}"))]);
    let client = c.wait_for_client(run.pid(), Duration::from_secs(10)).unwrap();
    run.wait_line("] complete", Duration::from_secs(60)).unwrap();
    let mut input = VirtualInput::connect(&c).unwrap();
    let (x, y) = (f64::from(client.at.0 + client.size.0 * 2 / 3), f64::from(client.at.1 + client.size.1 / 2));
    let t0 = Instant::now();
    while run.all("nd-budget: frames").is_empty() && t0.elapsed() < Duration::from_secs(30) {
        input.scroll(x, y, 4).unwrap();
    }
    for line in run.all("nd-budget: frames") {
        println!("{n} entries ({view}) scrolling: {line}");
    }
    run.stop();
}

/// Sort changes and filter typing on a 100k-entry folder: how long each held the main thread.
#[test]
#[ignore = "a benchmark"]
fn perf_sort_and_filter() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let big = folder_of(100_000);
    let home = tempfile::tempdir().unwrap();
    let mut options = LaunchOptions::new("/usr/bin/taskset")
        .arg("-c")
        .arg(cpus())
        .arg(bin())
        .env("HOME", home.path().display().to_string())
        .env("TROVE_TRACE", "1");
    for (k, v) in c.client_env() {
        options = options.env(k, v);
    }
    let app = launch_app(options).unwrap();
    app.wait_for_stderr("] complete", Duration::from_secs(60)).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    // Navigation from a running window: the path bar's field, the folder, Enter.
    app.keyboard().press("ctrl-l").unwrap();
    app.get_by_test_id("path-edit").fill(&big.path().display().to_string()).unwrap();
    app.keyboard().press("enter").unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let tail = app.stderr_tail(200);
    let last = |what: &str| tail.iter().rev().find(|l| l.ends_with(what)).and_then(|l| at_ms(l));
    let load = last("] load").unwrap();
    println!(
        "navigation to 100k: first names {:.0} ms after the load started, complete {:.0} ms after",
        last("] names").unwrap() - load,
        last("] complete").unwrap() - load
    );
    let applies: Vec<f64> = tail
        .iter()
        .filter(|l| l.contains("] apply took") && at_ms(l).is_some_and(|t| t > load))
        .filter_map(|l| took_ms(l))
        .collect();
    println!("navigation to 100k: batches applied on the main thread {applies:.1?} ms");
    // The header's columns sit at the table's right edge: Size, Kind, Modified at 96, 132 and
    // 168 points; Name fills the rest.
    let client = c.wait_for_client(app.pid(), Duration::from_secs(10)).unwrap();
    let table = app.get_by_test_id("files-table").bounding_box().unwrap().unwrap();
    let (x0, y0) = (f64::from(client.at.0) + f64::from(table.x), f64::from(client.at.1) + f64::from(table.y) + 12.);
    let right = x0 + f64::from(table.width);
    let size = right - 168. - 132. - 48.;
    let kind = right - 168. - 66.;
    let modified = right - 84.;
    let name = x0 + 60.;
    let mut input = VirtualInput::connect(&c).unwrap();
    for x in [size, modified, kind, name, name] {
        input.click(x, y0).unwrap();
        std::thread::sleep(Duration::from_millis(800));
    }
    app.keyboard().press("ctrl-f").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    for key in ["f", "i", "l", "e", "space", "1", "2", "backspace", "backspace", "backspace"] {
        app.keyboard().press(key).unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }
    std::thread::sleep(Duration::from_millis(800));
    let tail = app.stderr_tail(400);
    for l in tail.iter().filter(|l| l.contains("DBG")) {
        println!("{l}");
    }
    let sorts: Vec<f64> = tail.iter().filter(|l| l.contains("] sort took")).filter_map(|l| took_ms(l)).collect();
    let filters: Vec<f64> = tail.iter().filter(|l| l.contains("] filter took")).filter_map(|l| took_ms(l)).collect();
    println!("sort changes on 100k: {sorts:.1?} ms");
    println!("filter keystrokes on 100k: {filters:.1?} ms");
}

/// 200 large JPEGs in the grid: the switch to the grid to every visible tile's thumbnail made,
/// into an empty thumbnail cache.
#[test]
#[ignore = "a benchmark"]
fn perf_thumbnails() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    let dir = tempfile::Builder::new().prefix("trove-jpegs-").tempdir().unwrap();
    let source = dir.path().join("source.jpg");
    let ok = std::process::Command::new("magick")
        .args(["-size", "4000x3000", "plasma:", "-quality", "90"])
        .arg(&source)
        .status()
        .unwrap()
        .success();
    assert!(ok, "magick made the source picture");
    for i in 0..200 {
        std::fs::copy(&source, dir.path().join(format!("IMG_{i:04}.jpg"))).unwrap();
    }
    std::fs::remove_file(&source).unwrap();
    let bytes = std::fs::metadata(dir.path().join("IMG_0000.jpg")).unwrap().len();
    println!("200 JPEGs of 4000x3000, {:.1} MB each", bytes as f64 / 1e6);
    for _ in 0..2 {
        let run = Run::start(&c, &[&dir.path().display().to_string()], &[("TROVE_DEFAULT_VIEW", "grid")]);
        let names = run.wait_line("] names", Duration::from_secs(20)).unwrap();
        let idle = run.wait_line("] thumbnails idle", Duration::from_secs(120));
        match idle {
            Some((t, _)) => println!(
                "first names to the visible tiles' thumbnails: {:.0} ms; VmHWM {} MB",
                (t - names.0).as_secs_f64() * 1000.,
                status_kb(run.pid(), "VmHWM:") / 1024
            ),
            None => println!("thumbnails never went idle"),
        }
        run.stop();
    }
}

/// One launch's trace lines, to see where the time before the first frame goes.
#[test]
#[ignore = "a benchmark"]
fn perf_launch_trace() {
    if !real_allowed() {
        return;
    }
    let c = compositor();
    if let Ok(out) = std::env::var("TROVE_STRACE") {
        let status = c
            .command("strace")
            .args(["-f", "-tt", "-qq", "-o", &out])
            .arg(bin())
            .env("TROVE_DEMO", "1")
            .env("TROVE_TRACE", "1")
            .env("ND_EXIT_AFTER_FIRST_FRAME", "1")
            .status()
            .unwrap();
        println!("strace run: {status}");
        return;
    }
    for _ in 0..3 {
        let run = Run::start(&c, &[], &[("TROVE_DEMO", "1"), ("ND_EXIT_AFTER_FIRST_FRAME", "1")]);
        run.wait_line("nd-budget: first_frame", Duration::from_secs(20)).expect("first frame");
        for line in run.all("") {
            println!("{line}");
        }
        run.stop();
    }
}
