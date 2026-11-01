use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::index::RawEntry;

const WORDS: &[&str] = &[
    "report",
    "notes",
    "beach",
    "cargo",
    "main",
    "index",
    "config",
    "photo",
    "invoice",
    "draft",
    "backup",
    "music",
    "track",
    "video",
    "project",
    "lib",
    "test",
    "build",
    "readme",
    "summary",
    "budget",
    "holiday",
    "tenerife",
    "kernel",
    "shader",
    "render",
    "window",
    "server",
    "client",
    "data",
    "export",
    "import",
    "theme",
    "icon",
    "font",
    "player",
    "queue",
    "search",
    "trove",
    "shell",
    "screenshot",
    "camera",
    "family",
    "letter",
    "resume",
    "slides",
    "chart",
    "model",
    "train",
    "scene",
    "level",
    "sprite",
    "map",
    "audio",
];
const EXTS: &[&str] = &[
    "txt", "md", "rs", "toml", "json", "png", "jpg", "jpeg", "pdf", "mp3", "flac", "mp4", "js", "ts", "py", "c", "h",
    "html", "css", "zip", "tar.gz", "docx", "xlsx", "svg", "lock", "log", "conf", "",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.below(v.len())]
    }
}

/// A plausible home tree of `n` unique paths, deterministic for a given `n`: about one folder in
/// eight, folders nested up to twelve deep, names from a word list with separators, camel
/// humps, numbers and extensions, one in twenty hidden.
pub(crate) fn generate(root: &Path, n: usize) -> Vec<RawEntry> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ n as u64);
    let mut dirs: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    let mut out = Vec::with_capacity(n);
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(n);
    let now = crate::now_secs() as u32;
    for i in 0..n {
        let p = if rng.below(2) == 0 { rng.below(dirs.len()) } else { dirs.len() - 1 - rng.below(dirs.len().min(32)) };
        let (parent, depth) = if dirs[p].1 >= 12 { (root.to_path_buf(), 0) } else { dirs[p].clone() };
        let is_dir = rng.below(8) == 0;
        let mut name = String::new();
        if rng.below(20) == 0 {
            name.push('.');
        }
        name.push_str(rng.pick(WORDS));
        match rng.below(5) {
            0 => {
                name.push(['_', '-', ' ', '.'][rng.below(4)]);
                name.push_str(rng.pick(WORDS));
            }
            1 => {
                let w = rng.pick(WORDS);
                let mut cs = w.chars();
                name.extend(cs.next().map(|c| c.to_ascii_uppercase()));
                name.push_str(cs.as_str());
            }
            _ => {}
        }
        if rng.below(3) == 0 {
            name.push_str(&format!("{}", i % 1000));
        }
        if !is_dir {
            let ext = rng.pick(EXTS);
            if !ext.is_empty() {
                name.push('.');
                name.push_str(ext);
            }
        }
        let mut path = parent.join(&name);
        if !seen.insert(path.clone()) {
            name.insert_str(0, &format!("{i}-"));
            path = parent.join(&name);
            seen.insert(path.clone());
        }
        if is_dir {
            dirs.push((path.clone(), depth + 1));
        }
        out.push(RawEntry {
            path,
            is_dir,
            mtime: now - (rng.below(3 * 365 * 86_400)) as u32,
            size: if is_dir { 0 } else { rng.next() % 50_000_000 },
        });
    }
    out
}
