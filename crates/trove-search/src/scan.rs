use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use memchr::memmem::Finder;
use rayon::prelude::*;

use crate::index::{HIDDEN, HIDDEN_PATH, Index, NONE};
use crate::matcher::{Matcher, Term};

/// Sort key: quality, then shallower, then newer. Higher is better.
pub(crate) fn rank_key(quality: u32, rel_depth: usize, mtime: u32) -> u64 {
    let shallow = 255 - rel_depth.min(255) as u64;
    (u64::from(quality) << 40) | (shallow << 32) | u64::from(mtime)
}

/// A hit before it becomes a `Hit`: ordered best first by key, then by path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ranked {
    pub key: u64,
    pub path: PathBuf,
    pub is_dir: bool,
    pub fuzzy: bool,
}

impl Ord for Ranked {
    fn cmp(&self, o: &Self) -> Ordering {
        self.key.cmp(&o.key).then_with(|| o.path.cmp(&self.path))
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cand {
    pub key: u64,
    pub idx: u32,
}

impl Ord for Cand {
    fn cmp(&self, o: &Self) -> Ordering {
        self.key.cmp(&o.key).then_with(|| o.idx.cmp(&self.idx))
    }
}

impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// The best `k` seen, by a min-heap whose top is the worst kept.
pub(crate) struct TopK<T: Ord> {
    heap: BinaryHeap<Reverse<T>>,
    k: usize,
}

impl<T: Ord> TopK<T> {
    pub fn new(k: usize) -> Self {
        TopK { heap: BinaryHeap::with_capacity(k.min(4096) + 1), k }
    }

    pub fn push(&mut self, c: T) {
        if self.heap.len() < self.k {
            self.heap.push(Reverse(c));
        } else if self.heap.peek().is_some_and(|w| c > w.0) {
            self.heap.pop();
            self.heap.push(Reverse(c));
        }
    }

    pub fn extend(&mut self, other: TopK<T>) {
        for c in other.heap {
            self.push(c.0);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.heap.iter().map(|r| &r.0)
    }

    /// Best first.
    pub fn into_sorted(self) -> Vec<T> {
        let mut v: Vec<T> = self.heap.into_iter().map(|r| r.0).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v
    }
}

pub(crate) fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().thread_name(|i| format!("trove-search-{i}")).build().ok())
        .as_ref()
}

fn threads() -> usize {
    pool().map_or(1, rayon::ThreadPool::current_num_threads)
}

/// One query against the index under `scope`.
pub(crate) struct Scan<'a> {
    pub ix: &'a Index,
    pub m: &'a Matcher,
    pub scope: u32,
    /// Sorted, disjoint record ranges removed since the index was built.
    pub tomb: &'a [(u32, u32)],
    pub cancel: &'a AtomicBool,
    within: Vec<Vec<(u32, u32)>>,
    scope_hidden: bool,
    base_depth: u8,
}

impl<'a> Scan<'a> {
    pub fn new(ix: &'a Index, m: &'a Matcher, scope: u32, tomb: &'a [(u32, u32)], cancel: &'a AtomicBool) -> Self {
        let mut s = Scan {
            ix,
            m,
            scope,
            tomb,
            cancel,
            within: Vec::new(),
            scope_hidden: ix.flags[scope as usize] & HIDDEN_PATH != 0,
            base_depth: ix.depth[scope as usize],
        };
        let scope_name = if scope == 0 {
            crate::text::fold(ix.root.file_name().map_or(&b""[..], std::os::unix::ffi::OsStrExt::as_bytes))
        } else {
            ix.lower_name(scope).to_vec()
        };
        s.within = m.within_left(&scope_name).iter().map(|w| s.folders_named(w)).collect();
        s
    }

    fn range(&self) -> (u32, u32) {
        (self.scope + 1, self.ix.end[self.scope as usize])
    }

    /// Subtrees under folders called `name` inside the scope, outermost only, so the list is
    /// disjoint and a binary search answers "is under one of them".
    fn folders_named(&self, name: &[u8]) -> Vec<(u32, u32)> {
        let (lo, hi) = self.range();
        let mut needle = name.to_vec();
        needle.push(b'/');
        let f = Finder::new(&needle);
        let mut out: Vec<(u32, u32)> = Vec::new();
        self.find_records(&f, false, lo, hi, |r| {
            if self.ix.is_dir(r) && self.ix.lower_name(r) == name && out.last().is_none_or(|l| r >= l.1) {
                out.push((r + 1, self.ix.end[r as usize]));
            }
        });
        out
    }

    /// Calls `f` with each record in `lo..hi` whose name holds the needle, in order, once each.
    fn find_records(&self, f: &Finder, sensitive: bool, lo: u32, hi: u32, mut each: impl FnMut(u32)) {
        let ix = self.ix;
        let blob = if sensitive { &ix.names } else { &ix.lower };
        let start = ix.off[lo as usize] as usize;
        let stop = ix.off[hi as usize] as usize;
        let mut from = start;
        let mut r = lo;
        while from < stop {
            let Some(p) = f.find(&blob[from..stop]) else { break };
            let at = from + p;
            let offs = &ix.off[r as usize..hi as usize];
            r += offs.partition_point(|&o| o as usize <= at) as u32 - 1;
            each(r);
            r += 1;
            from = ix.off[r as usize] as usize;
        }
    }

    /// Top `limit` matches and how many matched in all. `fuzzy` runs the subsequence pass.
    pub fn run(&self, fuzzy: bool, limit: usize) -> (Vec<Cand>, u64) {
        let (lo, hi) = self.range();
        let total = (hi - lo) as usize;
        let chunk = (total / (threads() * 4)).max(16_384) as u32;
        let mut ranges = Vec::new();
        let mut a = lo;
        while a < hi {
            let b = a.saturating_add(chunk).min(hi);
            ranges.push((a, b));
            a = b;
        }
        let ext = self.m.ext_needle();
        let needles: Vec<(&Finder<'static>, bool)> = if fuzzy {
            Vec::new()
        } else if let Some(e) = &ext {
            vec![(e, false)]
        } else {
            self.m.needles()
        };
        let work = |&(a, b): &(u32, u32)| self.chunk(a, b, &needles, fuzzy, limit);
        let parts: Vec<(TopK<Cand>, u64)> = match pool() {
            Some(p) => p.install(|| ranges.par_iter().map(work).collect()),
            None => ranges.iter().map(work).collect(),
        };
        let mut top = TopK::new(limit);
        let mut matched = 0;
        for (t, n) in parts {
            top.extend(t);
            matched += n;
        }
        (top.into_sorted(), matched)
    }

    fn chunk(
        &self,
        lo: u32,
        hi: u32,
        needles: &[(&Finder<'static>, bool)],
        fuzzy: bool,
        limit: usize,
    ) -> (TopK<Cand>, u64) {
        let mut top = TopK::new(limit);
        let mut matched = 0;
        if self.cancel.load(Relaxed) {
            return (top, 0);
        }
        if needles.is_empty() {
            let skip_hidden = !self.m.allow_hidden && !self.scope_hidden;
            let mut i = lo;
            let mut tick = 0u32;
            while i < hi {
                tick += 1;
                if tick & 0x3fff == 0 && self.cancel.load(Relaxed) {
                    break;
                }
                if skip_hidden && self.ix.flags[i as usize] & HIDDEN != 0 {
                    i = self.ix.end[i as usize];
                    continue;
                }
                self.consider(i, fuzzy, &mut top, &mut matched);
                i += 1;
            }
        } else {
            let mut cands = Vec::new();
            for (f, sensitive) in needles {
                self.find_records(f, *sensitive, lo, hi, |r| cands.push(r));
            }
            if needles.len() > 1 {
                cands.sort_unstable();
                cands.dedup();
            }
            for (n, &i) in cands.iter().enumerate() {
                if n & 0xfff == 0xfff && self.cancel.load(Relaxed) {
                    break;
                }
                self.consider(i, fuzzy, &mut top, &mut matched);
            }
        }
        (top, matched)
    }

    fn hidden_ok(&self, i: u32) -> bool {
        let ix = self.ix;
        if self.m.allow_hidden {
            return true;
        }
        if !self.scope_hidden {
            return ix.flags[i as usize] & HIDDEN_PATH == 0;
        }
        let mut j = i;
        while j != self.scope && j != NONE {
            if ix.flags[j as usize] & HIDDEN != 0 {
                return false;
            }
            j = ix.parent[j as usize];
        }
        true
    }

    fn in_ranges(list: &[(u32, u32)], i: u32) -> bool {
        let n = list.partition_point(|&(s, _)| s <= i);
        n > 0 && i < list[n - 1].1
    }

    fn consider(&self, i: u32, fuzzy: bool, top: &mut TopK<Cand>, matched: &mut u64) {
        let ix = self.ix;
        let iu = i as usize;
        if !self.hidden_ok(i) || (!self.tomb.is_empty() && Self::in_ranges(self.tomb, i)) {
            return;
        }
        let lower = ix.lower_name(i);
        let is_dir = ix.is_dir(i);
        if !self.m.cheap_ok(lower, is_dir, ix.kind(i)) || !self.m.meta_ok(ix.size[iu], u64::from(ix.mtime[iu])) {
            return;
        }
        if !self.within.iter().all(|l| Self::in_ranges(l, i)) {
            return;
        }
        let in_ancestor = |t: &Term| {
            let mut j = ix.parent[iu];
            while j != self.scope && j != NONE {
                if t.in_ancestor(ix.name(j), ix.lower_name(j)) {
                    return true;
                }
                j = ix.parent[j as usize];
            }
            false
        };
        let Some(q) = self.m.eval(ix.name(i), lower, is_dir, fuzzy, in_ancestor) else { return };
        *matched += 1;
        let depth = usize::from(ix.depth[iu].saturating_sub(self.base_depth));
        top.push(Cand { key: rank_key(q, depth, ix.mtime[iu]), idx: i });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::raw;
    use std::path::{Path, PathBuf};

    const NOW: u32 = 1_780_000_000;

    fn ix() -> Index {
        let r = raw(
            "/h",
            &[
                ("cargo", true, 1, 0),
                ("cargo/registry.txt", false, 1, 0),
                ("Projects", true, 1, 0),
                ("Projects/trove", true, 1, 0),
                ("Projects/trove/Cargo.toml", false, NOW - 10, 900),
                ("Projects/trove/cargo", false, NOW - 100, 10),
                ("Projects/old/Cargo.toml", false, NOW - 5000, 900),
                ("Cargo.toml", false, NOW - 9000, 900),
                ("my_cargo_notes.md", false, NOW, 1),
                ("supercargo.png", false, NOW, 5_000_000),
                ("c-a-r-g-o.txt", false, NOW, 1),
                (".config", true, 1, 0),
                (".config/cargo", true, 1, 0),
                (".config/hypr", true, 1, 0),
                (".config/hypr/hyprland.conf", false, NOW, 1),
                ("Pictures", true, 1, 0),
                ("Pictures/beach.jpg", false, NOW, 2_000_000),
                ("Pictures/Beach Day.PNG", false, NOW - 1, 3_000_000),
                ("Pictures/beach.txt", false, NOW, 1),
            ],
        );
        let mut r = r;
        r.retain(|e| e.path != Path::new("/h/Projects/old/Cargo.toml"));
        r.push(crate::index::RawEntry { path: PathBuf::from("/h/Projects/old"), is_dir: true, mtime: 1, size: 0 });
        r.push(crate::index::RawEntry {
            path: PathBuf::from("/h/Projects/old/Cargo.toml"),
            is_dir: false,
            mtime: NOW - 5000,
            size: 900,
        });
        Index::from_raw(PathBuf::from("/h"), r, u64::from(NOW))
    }

    fn search(ix: &Index, q: &str, scope: &str) -> Vec<String> {
        let m = Matcher::parse(q, false, i64::from(NOW));
        let cancel = AtomicBool::new(false);
        let s = Scan::new(ix, &m, ix.lookup(Path::new(scope)).unwrap(), &[], &cancel);
        let (mut hits, n) = s.run(false, 50);
        if (n as usize) < 50 && m.has_fuzzy() {
            hits.extend(s.run(true, 50).0);
        }
        hits.sort_unstable_by(|a, b| b.cmp(a));
        hits.iter().map(|c| ix.path_of(c.idx).strip_prefix("/h").unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn ranking_order() {
        let ix = ix();
        assert_eq!(
            search(&ix, "cargo", "/h"),
            [
                "cargo",
                "Projects/trove/cargo",
                "Cargo.toml",
                "Projects/trove/Cargo.toml",
                "Projects/old/Cargo.toml",
                "my_cargo_notes.md",
                "supercargo.png",
                "c-a-r-g-o.txt",
            ]
        );
    }

    #[test]
    fn scope_hidden_and_ancestors() {
        let ix = ix();
        assert_eq!(
            search(&ix, "toml", "/h/Projects"),
            ["trove/Cargo.toml", "old/Cargo.toml"].map(|p| format!("Projects/{p}"))
        );
        assert!(!search(&ix, "hypr", "/h").iter().any(|p| p.contains(".config")));
        assert_eq!(search(&ix, ".config hypr", "/h"), [".config/hypr", ".config/hypr/hyprland.conf"]);
        assert_eq!(search(&ix, "hypr", "/h/.config"), [".config/hypr", ".config/hypr/hyprland.conf"]);
        assert_eq!(search(&ix, "trove toml", "/h"), ["Projects/trove/Cargo.toml"]);
    }

    #[test]
    fn filters_through_scan() {
        let ix = ix();
        assert_eq!(search(&ix, "ext:png", "/h"), ["supercargo.png", "Pictures/Beach Day.PNG"]);
        assert_eq!(search(&ix, "kind:image beach", "/h"), ["Pictures/beach.jpg", "Pictures/Beach Day.PNG"]);
        assert_eq!(search(&ix, "size:>2.5mb", "/h"), ["supercargo.png", "Pictures/Beach Day.PNG"]);
        assert_eq!(search(&ix, "kind:folder cargo", "/h"), ["cargo"]);
        assert_eq!(search(&ix, "in:trove", "/h"), ["Projects/trove/Cargo.toml", "Projects/trove/cargo"]);
        assert_eq!(search(&ix, "*.toml in:projects", "/h").len(), 2);
        assert_eq!(search(&ix, "modified:<2000-01-01", "/h/Pictures"), Vec::<String>::new());
        assert_eq!(search(&ix, "\"Beach Day\"", "/h"), ["Pictures/Beach Day.PNG"]);
    }

    #[test]
    fn typo_finds_by_subsequence() {
        let ix = ix();
        let hits = search(&ix, "crgo", "/h");
        let mut first: Vec<&str> = hits[..2].iter().map(String::as_str).collect();
        first.sort_unstable();
        assert_eq!(first, ["Cargo.toml", "cargo"]);
        assert_eq!(hits[2], "Projects/trove/Cargo.toml");
        assert!(hits.contains(&"c-a-r-g-o.txt".to_string()));
    }

    #[test]
    fn tombstones_and_cancel() {
        let ix = ix();
        let m = Matcher::parse("cargo", false, i64::from(NOW));
        let cancel = AtomicBool::new(false);
        let c = ix.lookup(Path::new("/h/Projects")).unwrap();
        let tomb = [(c, ix.end[c as usize])];
        let (hits, _) = Scan::new(&ix, &m, 0, &tomb, &cancel).run(false, 50);
        assert!(hits.iter().all(|h| !ix.path_of(h.idx).starts_with("/h/Projects")));
        assert_eq!(hits.len(), 4);
        cancel.store(true, Relaxed);
        assert_eq!(Scan::new(&ix, &m, 0, &[], &cancel).run(false, 50), (Vec::new(), 0));
    }
}

#[cfg(test)]
mod brute {
    use super::*;
    use std::path::Path;

    #[test]
    fn scan_agrees_with_brute_force() {
        let root = Path::new("/syn");
        let ix = Index::from_raw(root.to_path_buf(), crate::synth::generate(root, 60_000), 1_780_000_000);
        let cancel = AtomicBool::new(false);
        for q in ["a", "cargo", "toml", "ext:png", "kind:image beach", "beach notes", "*.md"] {
            let m = Matcher::parse(q, false, 1_780_000_000);
            let (hits, n) = Scan::new(&ix, &m, 0, &[], &cancel).run(false, 1_000_000);
            let mut want = 0u64;
            for i in 1..ix.len() as u32 {
                let mut j = i;
                let mut hidden = false;
                while j != 0 {
                    hidden |= ix.flags[j as usize] & HIDDEN != 0;
                    j = ix.parent[j as usize];
                }
                if hidden || !m.cheap_ok(ix.lower_name(i), ix.is_dir(i), ix.kind(i)) {
                    continue;
                }
                let anc = |t: &Term| {
                    let mut j = ix.parent[i as usize];
                    while j != 0 {
                        if t.in_ancestor(ix.name(j), ix.lower_name(j)) {
                            return true;
                        }
                        j = ix.parent[j as usize];
                    }
                    false
                };
                if m.eval(ix.name(i), ix.lower_name(i), ix.is_dir(i), false, anc).is_some() {
                    want += 1;
                }
            }
            assert_eq!(n, want, "{q}");
            assert_eq!(hits.len() as u64, want, "{q}");
            assert!(want > 0, "{q}");
        }
    }
}
