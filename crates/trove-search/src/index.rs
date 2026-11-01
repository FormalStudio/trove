use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::text::{FOLDER, fold, is_hidden_name, kind_code};

pub(crate) const NONE: u32 = u32::MAX;
pub(crate) const DIR: u8 = 1;
pub(crate) const HIDDEN: u8 = 2;
/// The entry or a folder above it, up to the root, is hidden.
pub(crate) const HIDDEN_PATH: u8 = 4;
const KIND_SHIFT: u8 = 4;

/// One walked entry before the index is laid out.
pub(crate) struct RawEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub mtime: u32,
    pub size: u64,
}

/// The home index: struct-of-arrays records in depth-first preorder, so a folder's subtree is
/// the record range `i..end[i]` and "is under the scope" is one comparison. Record 0 is the root.
/// Name `i` is `names[off[i]..off[i + 1] - 1]`; every name is followed by `/`, a byte no name can
/// hold, so a substring search over the whole blob never matches across two names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Index {
    pub root: PathBuf,
    pub built_at: u64,
    /// Set when the index is installed; tells an overlay which index its tombstones point into.
    pub generation: u64,
    pub names: Vec<u8>,
    /// `names` folded by `text::fold`, byte for byte the same offsets.
    pub lower: Vec<u8>,
    pub off: Vec<u32>,
    pub parent: Vec<u32>,
    pub end: Vec<u32>,
    pub mtime: Vec<u32>,
    pub size: Vec<u64>,
    pub depth: Vec<u8>,
    pub flags: Vec<u8>,
}

impl Index {
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    fn span(&self, i: u32) -> (usize, usize) {
        let i = i as usize;
        (self.off[i] as usize, self.off[i + 1] as usize - 1)
    }

    pub fn name(&self, i: u32) -> &[u8] {
        let (a, b) = self.span(i);
        &self.names[a..b]
    }

    pub fn lower_name(&self, i: u32) -> &[u8] {
        let (a, b) = self.span(i);
        &self.lower[a..b]
    }

    pub fn is_dir(&self, i: u32) -> bool {
        self.flags[i as usize] & DIR != 0
    }

    pub fn kind(&self, i: u32) -> u8 {
        self.flags[i as usize] >> KIND_SHIFT
    }

    pub fn age(&self) -> Duration {
        Duration::from_secs(crate::now_secs().saturating_sub(self.built_at))
    }

    pub fn from_raw(root: PathBuf, raw: Vec<RawEntry>, built_at: u64) -> Index {
        let n = raw.len() + 1;
        let name_of = |node: u32| -> &[u8] {
            if node == 0 { b"" } else { raw[node as usize - 1].path.file_name().map_or(&b""[..], OsStr::as_bytes) }
        };

        let mut dirs: HashMap<&Path, u32> = HashMap::with_capacity(raw.len() / 8 + 1);
        dirs.insert(root.as_path(), 0);
        for (i, e) in raw.iter().enumerate() {
            if e.is_dir {
                dirs.insert(e.path.as_path(), i as u32 + 1);
            }
        }
        let mut parent_old = vec![NONE; n];
        let mut counts = vec![0u32; n];
        for (i, e) in raw.iter().enumerate() {
            if let Some(&p) = e.path.parent().and_then(|p| dirs.get(p)) {
                parent_old[i + 1] = p;
                counts[p as usize] += 1;
            }
        }
        drop(dirs);

        let mut start = vec![0u32; n + 1];
        for i in 0..n {
            start[i + 1] = start[i] + counts[i];
        }
        let mut fill = start.clone();
        let mut kids = vec![0u32; start[n] as usize];
        for (i, &p) in parent_old.iter().enumerate().skip(1) {
            if p != NONE {
                kids[fill[p as usize] as usize] = i as u32;
                fill[p as usize] += 1;
            }
        }
        for w in start.windows(2) {
            kids[w[0] as usize..w[1] as usize].sort_unstable_by(|a, b| name_of(*a).cmp(name_of(*b)));
        }

        let mut order: Vec<u32> = Vec::with_capacity(n);
        let mut new_of = vec![NONE; n];
        let mut end_by_new = vec![0u32; n];
        let mut stack: Vec<(u32, u32)> = vec![(0, start[0])];
        new_of[0] = 0;
        order.push(0);
        while let Some(&(old, cur)) = stack.last() {
            if cur < start[old as usize + 1] {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                let c = kids[cur as usize];
                new_of[c as usize] = order.len() as u32;
                order.push(c);
                stack.push((c, start[c as usize]));
            } else {
                end_by_new[new_of[old as usize] as usize] = order.len() as u32;
                stack.pop();
            }
        }

        let m = order.len();
        let mut ix = Index {
            root,
            built_at,
            generation: 0,
            names: Vec::new(),
            lower: Vec::new(),
            off: Vec::with_capacity(m + 1),
            parent: Vec::with_capacity(m),
            end: end_by_new,
            mtime: Vec::with_capacity(m),
            size: Vec::with_capacity(m),
            depth: Vec::with_capacity(m),
            flags: Vec::with_capacity(m),
        };
        ix.end.truncate(m);
        let total: usize = order.iter().map(|&o| name_of(o).len() + 1).sum();
        ix.names.reserve_exact(total);
        for (new, &old) in order.iter().enumerate() {
            let name = name_of(old);
            ix.off.push(ix.names.len() as u32);
            ix.names.extend_from_slice(name);
            ix.names.push(b'/');
            let (is_dir, mtime, size) = if old == 0 {
                (true, 0, 0)
            } else {
                let e = &raw[old as usize - 1];
                (e.is_dir, e.mtime, e.size)
            };
            let hidden = new != 0 && is_hidden_name(name);
            let (parent, depth, hidden_path) = if new == 0 {
                (NONE, 0, false)
            } else {
                let p = new_of[parent_old[old as usize] as usize];
                (p, ix.depth[p as usize].saturating_add(1), hidden || ix.flags[p as usize] & HIDDEN_PATH != 0)
            };
            let kind = if is_dir { FOLDER } else { kind_code(name) };
            ix.parent.push(parent);
            ix.depth.push(depth);
            ix.mtime.push(mtime);
            ix.size.push(if is_dir { 0 } else { size });
            ix.flags.push(
                (u8::from(is_dir) * DIR)
                    | (u8::from(hidden) * HIDDEN)
                    | (u8::from(hidden_path) * HIDDEN_PATH)
                    | (kind << KIND_SHIFT),
            );
        }
        ix.off.push(ix.names.len() as u32);
        ix.lower = fold(&ix.names);
        ix
    }

    pub fn child(&self, p: u32, name: &[u8]) -> Option<u32> {
        if !self.is_dir(p) {
            return None;
        }
        let stop = self.end[p as usize];
        let mut c = p + 1;
        while c < stop {
            if self.name(c) == name {
                return Some(c);
            }
            c = self.end[c as usize];
        }
        None
    }

    pub fn lookup(&self, path: &Path) -> Option<u32> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut cur = 0;
        for comp in rel.components() {
            match comp {
                Component::Normal(name) => cur = self.child(cur, name.as_bytes())?,
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(cur)
    }

    pub fn path_of(&self, i: u32) -> PathBuf {
        let mut chain = Vec::new();
        let mut j = i;
        while j != 0 && j != NONE {
            chain.push(j);
            j = self.parent[j as usize];
        }
        let mut p = self.root.clone();
        for &k in chain.iter().rev() {
            p.push(OsStr::from_bytes(self.name(k)));
        }
        p
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn raw(root: &str, items: &[(&str, bool, u32, u64)]) -> Vec<RawEntry> {
        items
            .iter()
            .map(|&(p, is_dir, mtime, size)| RawEntry { path: Path::new(root).join(p), is_dir, mtime, size })
            .collect()
    }

    #[test]
    fn preorder_layout() {
        let r = raw(
            "/h",
            &[
                ("b", true, 1, 0),
                ("b/z.txt", false, 2, 10),
                ("a", true, 3, 0),
                ("a/.hid", true, 4, 0),
                ("a/.hid/x", false, 5, 1),
                ("a/y", false, 6, 2),
                ("orphan/q", false, 7, 3),
            ],
        );
        let ix = Index::from_raw(PathBuf::from("/h"), r, 0);
        assert_eq!(ix.len(), 7);
        let names: Vec<&[u8]> = (0..ix.len() as u32).map(|i| ix.name(i)).collect();
        assert_eq!(names, [&b""[..], b"a", b".hid", b"x", b"y", b"b", b"z.txt"]);
        assert_eq!(ix.end, [7, 5, 4, 4, 5, 7, 7]);
        assert_eq!(ix.parent, [NONE, 0, 1, 2, 1, 0, 5]);
        assert_eq!(ix.depth, [0, 1, 2, 3, 2, 1, 2]);
        assert!(ix.flags[3] & HIDDEN_PATH != 0 && ix.flags[3] & HIDDEN == 0);
        assert!(ix.flags[4] & HIDDEN_PATH == 0);
        assert_eq!(ix.lookup(Path::new("/h/a/.hid/x")), Some(3));
        assert_eq!(ix.lookup(Path::new("/h")), Some(0));
        assert_eq!(ix.lookup(Path::new("/h/a/nope")), None);
        assert_eq!(ix.lookup(Path::new("/elsewhere")), None);
        assert_eq!(ix.path_of(6), Path::new("/h/b/z.txt"));
        assert_eq!(ix.kind(6), crate::text::code_of(trove_fs::FileKind::Text));
    }
}
