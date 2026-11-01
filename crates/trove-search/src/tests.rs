use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::thread::sleep;

use super::*;

fn tree(root: &Path, files: &[&str]) {
    for f in files {
        let p = root.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        if f.ends_with('/') {
            fs::create_dir_all(&p).unwrap();
        } else {
            fs::write(&p, b"x").unwrap();
        }
    }
}

fn wait_ready(s: &Search) -> u64 {
    let t = Instant::now();
    loop {
        if let IndexState::Ready { entries, .. } = s.index_state() {
            return entries;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "index never got ready");
        sleep(Duration::from_millis(5));
    }
}

fn run(s: &Search, text: &str, scope: &Path) -> (Vec<Hit>, bool) {
    let r = s.query(Query { text: text.into(), scope: scope.to_path_buf(), show_hidden: false, limit: 50 });
    let mut hits = Vec::new();
    for ev in r.events.iter() {
        match ev {
            SearchEvent::Hits(h) => hits = h,
            SearchEvent::Done { truncated, .. } => return (hits, truncated),
        }
    }
    panic!("no Done");
}

fn rel(hits: &[Hit], root: &Path) -> Vec<String> {
    hits.iter().map(|h| h.path.strip_prefix(root).unwrap().to_string_lossy().into_owned()).collect()
}

const FILES: &[&str] = &[
    "Documents/notes.md",
    "Documents/Meeting Notes.txt",
    "Projects/trove/Cargo.toml",
    "Projects/trove/src/main.rs",
    "Projects/trove/target/debug/notes.rlib",
    "Pictures/beach.jpg",
    ".config/notes/x.conf",
    "empty/",
];

#[test]
fn indexes_persists_and_queries() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    tree(home.path(), FILES);
    let s = Search::new(cache.path().to_path_buf(), home.path().to_path_buf());
    assert_eq!(s.index_state(), IndexState::Absent);
    assert_eq!(s.indexed_root(), home.path());
    s.start_indexing();
    let n = wait_ready(&s);
    assert_eq!(n, 14);
    assert!(cache.path().join(persist::file_name()).exists());

    let (hits, truncated) = run(&s, "notes", home.path());
    assert!(!truncated);
    assert_eq!(rel(&hits, home.path()), ["Documents/notes.md", "Documents/Meeting Notes.txt"]);
    assert_eq!(hits[0].name_matches, [(0, 5)]);
    assert_eq!(hits[1].name_matches, [(8, 13)]);
    assert_eq!(rel(&run(&s, ".config notes", home.path()).0, home.path()), [".config/notes"]);
    assert_eq!(rel(&run(&s, "main", &home.path().join("Projects")).0, home.path()), ["Projects/trove/src/main.rs"]);
    assert!(run(&s, "main", &home.path().join("Pictures")).0.is_empty());
    assert!(run(&s, "", home.path()).0.is_empty());
    drop(s);

    let again = Search::new(cache.path().to_path_buf(), home.path().to_path_buf());
    again.start_indexing();
    assert_eq!(wait_ready(&again), 14);
}

#[test]
fn corrupt_index_is_rebuilt() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    tree(home.path(), FILES);
    let file = cache.path().join(persist::file_name());
    fs::write(&file, b"TROVEIDX garbage that is not an index at all").unwrap();
    let s = Search::new(cache.path().to_path_buf(), home.path().to_path_buf());
    s.start_indexing();
    assert_eq!(wait_ready(&s), 14);
    assert!(persist::load(&file, home.path()).is_ok());
}

#[test]
fn notes_keep_results_right() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let h = home.path();
    tree(h, FILES);
    let s = Search::new(cache.path().to_path_buf(), h.to_path_buf());
    s.start_indexing();
    wait_ready(&s);

    fs::write(h.join("Documents/zebra.txt"), b"z").unwrap();
    s.note_created(h.join("Documents/zebra.txt"));
    assert_eq!(rel(&run(&s, "zebra", h).0, h), ["Documents/zebra.txt"]);

    fs::remove_file(h.join("Documents/notes.md")).unwrap();
    s.note_removed(h.join("Documents/notes.md"));
    assert_eq!(rel(&run(&s, "notes", h).0, h), ["Documents/Meeting Notes.txt"]);

    fs::rename(h.join("Projects/trove"), h.join("Projects/vault")).unwrap();
    s.note_renamed(h.join("Projects/trove"), h.join("Projects/vault"));
    assert_eq!(rel(&run(&s, "main", h).0, h), ["Projects/vault/src/main.rs"]);
    assert_eq!(rel(&run(&s, "vault", h).0, h), ["Projects/vault"]);
    assert!(run(&s, "trove", h).0.is_empty());

    fs::create_dir_all(h.join("Fresh/inner")).unwrap();
    fs::write(h.join("Fresh/inner/deep.txt"), b"d").unwrap();
    s.note_created(h.join("Fresh"));
    assert_eq!(rel(&run(&s, "deep", h).0, h), ["Fresh/inner/deep.txt"]);
    s.note_renamed(h.join("Documents/zebra.txt"), h.join("zebra2.txt"));
    assert_eq!(rel(&run(&s, "zebra", h).0, h), ["zebra2.txt"]);
}

#[test]
fn live_walk_outside_the_index() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    tree(other.path(), FILES);
    std::os::unix::fs::symlink(other.path(), other.path().join("Documents/loop")).unwrap();
    let s = Search::new(cache.path().to_path_buf(), home.path().to_path_buf());
    let (hits, truncated) = run(&s, "notes", other.path());
    assert!(!truncated);
    assert_eq!(rel(&hits, other.path()), ["Documents/notes.md", "Documents/Meeting Notes.txt"]);
    assert_eq!(hits[1].name_matches, [(8, 13)]);
    let (hits, _) = run(&s, "loop", other.path());
    assert_eq!(rel(&hits, other.path()), ["Documents/loop"]);
    assert!(!hits[0].is_dir);
    assert_eq!(rel(&run(&s, "kind:image", other.path()).0, other.path()), ["Pictures/beach.jpg"]);
    assert_eq!(rel(&run(&s, "man", other.path()).0, other.path()), ["Projects/trove/src/main.rs"]);
    assert_eq!(
        rel(&run(&s, "notes", &other.path().join("Projects/trove/target")).0, other.path()),
        ["Projects/trove/target/debug/notes.rlib"]
    );
}

#[test]
fn stale_index_walks_the_top_levels() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let h = home.path();
    tree(h, FILES);
    let mut ix = walk::build_index(h, &AtomicU64::new(0), &AtomicBool::new(false)).unwrap();
    ix.built_at = now_secs() - 10 * 60;
    persist::save(&ix, &cache.path().join(persist::file_name())).unwrap();
    tree(h, &["Documents/new notes.md", "a/b/c/d/deep notes.md"]);
    let s = Search::new(cache.path().to_path_buf(), h.to_path_buf());
    s.start_indexing();
    wait_ready(&s);
    let mut got = rel(&run(&s, "notes", h).0, h);
    assert_eq!(got.remove(0), "Documents/notes.md");
    got.sort();
    assert_eq!(got, ["Documents/Meeting Notes.txt", "Documents/new notes.md"]);
}

#[test]
fn caps_cancel_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..2000).map(|i| format!("d{}/f{i}.txt", i % 40)).collect();
    tree(dir.path(), &names.iter().map(String::as_str).collect::<Vec<_>>());
    let m = Matcher::parse("f", false, 0);
    let mounts = mounts::read_mounts();
    let cut = LiveOpts { max_depth: Some(4), budget: Some(100), report_cut: true, limit: 10 };
    let r = live::search(dir.path(), &m, cut, &[], &mounts, &AtomicBool::new(false), |_| true);
    assert!(r.truncated);
    assert!(r.scanned <= 100 + 64);
    let shallow = LiveOpts { max_depth: Some(1), budget: None, report_cut: true, limit: 10 };
    let r = live::search(dir.path(), &m, shallow, &[], &mounts, &AtomicBool::new(false), |_| true);
    assert!(r.truncated);
    assert_eq!(r.scanned, 40);

    let all = LiveOpts { max_depth: None, budget: None, report_cut: false, limit: 10 };
    let r = live::search(dir.path(), &m, all, &[], &mounts, &AtomicBool::new(true), |_| true);
    assert!(r.scanned < 2040, "a cancelled walk ran to the end");
    let mut calls = 0;
    let r = live::search(dir.path(), &m, all, &[], &mounts, &AtomicBool::new(false), |_| {
        calls += 1;
        false
    });
    assert!(calls >= 1);
    assert!(r.scanned <= 2040);

    let s = Search::new(dir.path().join("cache"), dir.path().to_path_buf());
    let q = s.query(Query { text: "f".into(), scope: dir.path().to_path_buf(), show_hidden: false, limit: 10 });
    drop(q);
}
