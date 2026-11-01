//! Index build, load and per-query latency over the real `$HOME` (read only; the index goes to a
//! temp dir) and over a generated 1M-entry index. Run it under `taskset -c 0-7` for an 8-thread
//! figure.
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use trove_search::{IndexState, Query, Search, SearchEvent, write_synthetic_index};

const QUERIES: &[&str] = &["cargo", "toml", "readme", "ext:png", "kind:image beach", "crgo", ".config hypr", "a"];
const RUNS: usize = 50;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 4 && args[1] == "load" {
        load_child(Path::new(&args[2]), Path::new(&args[3]));
        return;
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!("threads available: {threads}");

    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let cache = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let s = Search::new(cache.path().to_path_buf(), home.clone());
    s.start_indexing();
    let entries = wait_ready(&s);
    println!("\n== $HOME ({})", home.display());
    println!("build: {entries} entries in {:.2} s", t.elapsed().as_secs_f64());
    report_file(cache.path());
    drop(s);
    load_in_child(cache.path(), &home);
    bench_queries(cache.path(), &home);

    let syn = tempfile::tempdir().unwrap();
    let root = PathBuf::from("/nonexistent/trove-synthetic-home");
    let t = Instant::now();
    write_synthetic_index(syn.path(), &root, 1_000_000).unwrap();
    println!("\n== synthetic, 1M entries");
    println!("generate + lay out + save: {:.2} s", t.elapsed().as_secs_f64());
    report_file(syn.path());
    load_in_child(syn.path(), &root);
    bench_queries(syn.path(), &root);
}

fn wait_ready(s: &Search) -> u64 {
    loop {
        if let IndexState::Ready { entries, .. } = s.index_state() {
            return entries;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn report_file(cache: &Path) {
    for e in std::fs::read_dir(cache).unwrap().flatten() {
        let len = e.metadata().unwrap().len();
        println!("index file: {} {:.1} MB", e.file_name().to_string_lossy(), len as f64 / 1e6);
    }
}

fn rss_kb(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// In a fresh process, so the allocator holds nothing from the build.
fn load_in_child(cache: &Path, root: &Path) {
    let out = Command::new(std::env::current_exe().unwrap()).arg("load").arg(cache).arg(root).output().unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
}

fn load_child(cache: &Path, root: &Path) {
    let before = rss_kb("VmRSS:");
    let t = Instant::now();
    let s = Search::new(cache.to_path_buf(), root.to_path_buf());
    s.start_indexing();
    wait_ready(&s);
    let load = t.elapsed();
    let after = rss_kb("VmRSS:");
    println!("load (page cache warm): {:.1} ms", load.as_secs_f64() * 1e3);
    println!(
        "resident after load: +{:.1} MB (peak {:.1} MB)",
        (after - before) as f64 / 1024.0,
        rss_kb("VmHWM:") as f64 / 1024.0
    );
}

fn one(s: &Search, text: &str, root: &Path) -> (Duration, Duration, usize) {
    let t = Instant::now();
    let run = s.query(Query { text: text.into(), scope: root.to_path_buf(), show_hidden: false, limit: 100 });
    let mut first = None;
    let mut n = 0;
    for ev in run.events.iter() {
        match ev {
            SearchEvent::Hits(h) => {
                first.get_or_insert_with(|| t.elapsed());
                n = h.len();
            }
            SearchEvent::Done { .. } => break,
        }
    }
    (first.unwrap_or_default(), t.elapsed(), n)
}

fn bench_queries(cache: &Path, root: &Path) {
    let s = Search::new(cache.to_path_buf(), root.to_path_buf());
    s.start_indexing();
    wait_ready(&s);
    println!("{:<20} {:>6} {:>9} {:>9} {:>11}", "query", "hits", "median", "p95", "first hits");
    for q in QUERIES {
        for _ in 0..3 {
            one(&s, q, root);
        }
        let mut done = Vec::with_capacity(RUNS);
        let mut first = Vec::with_capacity(RUNS);
        let mut hits = 0;
        for _ in 0..RUNS {
            let (f, d, n) = one(&s, q, root);
            first.push(f);
            done.push(d);
            hits = n;
        }
        done.sort();
        first.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        println!(
            "{:<20} {:>6} {:>7.2}ms {:>7.2}ms {:>9.2}ms",
            q,
            hits,
            ms(done[RUNS / 2]),
            ms(done[RUNS * 95 / 100]),
            ms(first[RUNS / 2])
        );
    }
}
