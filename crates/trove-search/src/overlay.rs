use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::index::Index;
use crate::matcher::Matcher;
use crate::scan::{Ranked, rank_key};
use crate::text::{FOLDER, fold, is_hidden_name, kind_code};
use crate::walk::{excluded, mtime_of};

/// More entries than this in a folder the UI just made or renamed and the overlay stops
/// copying them, asking for a rebuild instead.
const MAX_EXTRAS: usize = 20_000;

pub(crate) enum Note {
    Created(PathBuf),
    Removed(PathBuf),
    Renamed(PathBuf, PathBuf),
}

#[derive(Clone)]
pub(crate) struct Extra {
    pub path: PathBuf,
    pub lower: Vec<u8>,
    pub is_dir: bool,
    pub kind: u8,
    pub mtime: u32,
    pub size: u64,
}

/// Changes the UI made since the index was built: entries it added and paths it removed. The
/// removals are kept as paths and resolved into record ranges of whichever index is current.
#[derive(Clone, Default)]
pub(crate) struct Overlay {
    pub generation: u64,
    pub added: Vec<Extra>,
    removed: Vec<PathBuf>,
    pub tomb: Vec<(u32, u32)>,
}

impl Overlay {
    pub fn is_tombstoned(&self, i: u32) -> bool {
        let n = self.tomb.partition_point(|&(s, _)| s <= i);
        n > 0 && i < self.tomb[n - 1].1
    }

    fn live_record(&self, ix: Option<&Index>, p: &Path) -> Option<u32> {
        ix?.lookup(p).filter(|&i| !self.is_tombstoned(i))
    }

    /// Moves the overlay onto a freshly built index: what that index already holds is dropped
    /// from the additions, and removals it no longer holds are done with.
    pub fn rebase(&mut self, ix: Option<&Index>) {
        if let Some(ix) = ix {
            self.added.retain(|e| ix.lookup(&e.path).is_none());
            self.removed.retain(|p| ix.lookup(p).is_some());
            self.generation = ix.generation;
        }
        self.retomb(ix);
    }

    /// Applies one note; true when it was too big to mirror and the index should be rebuilt.
    pub fn apply(&mut self, note: Note, ix: Option<&Index>) -> bool {
        let rebuild = match note {
            Note::Created(p) => self.created(&p, ix),
            Note::Removed(p) => {
                self.added.retain(|e| !e.path.starts_with(&p));
                if ix.is_none() || self.live_record(ix, &p).is_some() {
                    self.removed.push(p);
                }
                false
            }
            Note::Renamed(from, to) => self.renamed(&from, &to, ix),
        };
        self.retomb(ix);
        rebuild
    }

    fn created(&mut self, p: &Path, ix: Option<&Index>) -> bool {
        self.added.retain(|e| !e.path.starts_with(p));
        if self.live_record(ix, p).is_some() {
            return false;
        }
        let Some(x) = stat_extra(p) else { return false };
        let dir = x.is_dir;
        self.added.push(x);
        dir && !walk_extras(p, &mut self.added)
    }

    fn renamed(&mut self, from: &Path, to: &Path, ix: Option<&Index>) -> bool {
        let mut moved = false;
        for e in &mut self.added {
            let Ok(rel) = e.path.strip_prefix(from) else { continue };
            if rel.as_os_str().is_empty() {
                e.path = to.to_path_buf();
                e.lower = fold(to.file_name().map_or(&b""[..], |n| n.as_bytes()));
            } else {
                e.path = to.join(rel);
            }
            moved = true;
        }
        let mut rebuild = false;
        if let (Some(ix), Some(i)) = (ix, self.live_record(ix, from)) {
            let end = ix.end[i as usize];
            let mut copied = 0;
            for r in i..end {
                if self.is_tombstoned(r) {
                    continue;
                }
                if copied == MAX_EXTRAS {
                    rebuild = true;
                    break;
                }
                let old = ix.path_of(r);
                let path = match old.strip_prefix(from) {
                    Ok(rel) if !rel.as_os_str().is_empty() => to.join(rel),
                    _ => to.to_path_buf(),
                };
                let lower = if r == i {
                    fold(to.file_name().map_or(&b""[..], |n| n.as_bytes()))
                } else {
                    ix.lower_name(r).to_vec()
                };
                let ru = r as usize;
                self.added.push(Extra {
                    path,
                    lower,
                    is_dir: ix.is_dir(r),
                    kind: ix.kind(r),
                    mtime: ix.mtime[ru],
                    size: ix.size[ru],
                });
                copied += 1;
            }
            self.removed.push(from.to_path_buf());
            moved = true;
        }
        if !moved {
            return self.created(to, ix);
        }
        rebuild
    }

    fn retomb(&mut self, ix: Option<&Index>) {
        self.tomb.clear();
        let Some(ix) = ix else { return };
        let mut r: Vec<(u32, u32)> =
            self.removed.iter().filter_map(|p| ix.lookup(p)).map(|i| (i, ix.end[i as usize])).collect();
        r.sort_unstable();
        for (s, e) in r {
            match self.tomb.last() {
                Some(&(_, le)) if s < le => {}
                _ => self.tomb.push((s, e)),
            }
        }
    }

    /// The additions matching under `scope`, as the index scan would rank them.
    pub fn matches(&self, m: &Matcher, scope: &Path, within: &[Vec<u8>], fuzzy: bool) -> (Vec<Ranked>, u64) {
        let mut out = Vec::new();
        let mut matched = 0;
        for e in &self.added {
            let Ok(rel) = e.path.strip_prefix(scope) else { continue };
            let comps: Vec<&[u8]> = rel
                .components()
                .filter_map(|c| match c {
                    Component::Normal(n) => Some(n.as_bytes()),
                    _ => None,
                })
                .collect();
            let Some((&name, above)) = comps.split_last() else { continue };
            if !m.allow_hidden && comps.iter().any(|c| is_hidden_name(c)) {
                continue;
            }
            if !m.cheap_ok(&e.lower, e.is_dir, e.kind) || !m.meta_ok(e.size, u64::from(e.mtime)) {
                continue;
            }
            let folded: Vec<Vec<u8>> = above.iter().map(|a| fold(a)).collect();
            if !within.iter().all(|w| folded.iter().any(|f| f == w)) {
                continue;
            }
            let in_ancestor = |t: &crate::matcher::Term| above.iter().zip(&folded).any(|(a, f)| t.in_ancestor(a, f));
            let Some(q) = m.eval(name, &e.lower, e.is_dir, fuzzy, in_ancestor) else { continue };
            matched += 1;
            out.push(Ranked { key: rank_key(q, comps.len(), e.mtime), path: e.path.clone(), is_dir: e.is_dir, fuzzy });
        }
        (out, matched)
    }
}

fn stat_extra(p: &Path) -> Option<Extra> {
    let md = std::fs::symlink_metadata(p).ok()?;
    let name = p.file_name()?.as_bytes();
    let is_dir = md.is_dir();
    Some(Extra {
        path: p.to_path_buf(),
        lower: fold(name),
        is_dir,
        kind: if is_dir { FOLDER } else { kind_code(name) },
        mtime: mtime_of(&md),
        size: if is_dir { 0 } else { md.len() },
    })
}

/// Adds what is inside a folder the UI just made (a copy, an extracted archive). False when it
/// held more than the overlay copies.
fn walk_extras(dir: &Path, out: &mut Vec<Extra>) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if n == MAX_EXTRAS {
                return false;
            }
            let p = e.path();
            let Some(x) = stat_extra(&p) else { continue };
            if x.is_dir && !excluded(&p, p.file_name().map_or(&b""[..], |n| n.as_bytes())) {
                stack.push(p);
            }
            out.push(x);
            n += 1;
        }
    }
    true
}
