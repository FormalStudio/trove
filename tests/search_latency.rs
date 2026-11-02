//! Keystroke to results over the real home, read-only: the index is built into a temp cache,
//! then a word is typed one key at a time, each keystroke dropping the run before it as the
//! results view does. Prints the time to the first hits and to done for each prefix.
//!
//! Run in release with `cargo test --release --test search_latency -- --ignored --nocapture`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::time::{Duration, Instant};

use trove_search::{IndexState, Query, Search, SearchEvent};

#[test]
#[ignore = "reads the real home; a benchmark, not a check"]
fn keystroke_to_results_on_the_real_home() {
    let home = std::env::home_dir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let search = Search::new(cache.path().to_path_buf(), home.clone());
    let t0 = Instant::now();
    search.start_indexing();
    let entries = loop {
        if let IndexState::Ready { entries, .. } = search.index_state() {
            break entries;
        }
        assert!(t0.elapsed() < Duration::from_secs(600), "the index never finished");
        std::thread::sleep(Duration::from_millis(50));
    };
    println!("index: {entries} entries in {:.1}s", t0.elapsed().as_secs_f64());

    for word in ["report", "config", "main.rs", "ext:pdf invoice"] {
        let mut runs = Vec::new();
        for n in 2..=word.chars().count() {
            let text: String = word.chars().take(n).collect();
            let start = Instant::now();
            let run = search.query(Query { text: text.clone(), scope: home.clone(), show_hidden: false, limit: 500 });
            let mut first = None;
            let mut hits = 0;
            let done = loop {
                match run.events.recv_timeout(Duration::from_secs(30)).unwrap() {
                    SearchEvent::Hits(h) => {
                        first.get_or_insert(start.elapsed());
                        hits = h.len();
                    }
                    SearchEvent::Done { .. } => break start.elapsed(),
                }
            };
            let first = first.unwrap_or(done);
            println!(
                "{text:>16}: first hits {:>6.1} ms, done {:>6.1} ms, {hits} hits",
                first.as_secs_f64() * 1e3,
                done.as_secs_f64() * 1e3
            );
            runs.push(first);
        }
        runs.sort();
        println!("{word}: median first hits {:.1} ms", runs[runs.len() / 2].as_secs_f64() * 1e3);
    }
}
