use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::index::{DIR, Index, NONE};
use crate::text::fold;

const MAGIC: &[u8; 8] = b"TROVEIDX";
/// Bump on any layout or meaning change; an older file is then rebuilt rather than read.
pub(crate) const VERSION: u32 = 1;

pub(crate) fn file_name() -> String {
    format!("index-v{VERSION}.bin")
}

/// Little-endian header (magic, version, built_at, record count, names length, root), then each
/// record array whole, then the names blob, then a checksum of everything before it.
pub(crate) fn save(ix: &Index, path: &Path) -> io::Result<()> {
    let n = ix.len();
    let root = ix.root.as_os_str().as_bytes();
    let mut b = Vec::with_capacity(64 + root.len() + n * 26 + ix.names.len());
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&VERSION.to_le_bytes());
    b.extend_from_slice(&ix.built_at.to_le_bytes());
    b.extend_from_slice(&(n as u32).to_le_bytes());
    b.extend_from_slice(&(ix.names.len() as u32).to_le_bytes());
    b.extend_from_slice(&(root.len() as u32).to_le_bytes());
    b.extend_from_slice(root);
    for arr in [&ix.off, &ix.parent, &ix.end, &ix.mtime] {
        for v in arr.iter() {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    for v in &ix.size {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&ix.depth);
    b.extend_from_slice(&ix.flags);
    b.extend_from_slice(&ix.names);
    let h = checksum(&b);
    b.extend_from_slice(&h.to_le_bytes());

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let written = fs::write(&tmp, &b).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

pub(crate) fn load(path: &Path, root: &Path) -> io::Result<Index> {
    let data = fs::read(path)?;
    if data.len() < MAGIC.len() + 8 {
        return Err(bad("short file"));
    }
    let (body, tail) = data.split_at(data.len() - 8);
    if checksum(body).to_le_bytes() != tail {
        return Err(bad("checksum"));
    }
    let mut r = Reader { b: body, pos: 0 };
    if r.take(8)? != MAGIC {
        return Err(bad("magic"));
    }
    if r.u32()? != VERSION {
        return Err(bad("version"));
    }
    let built_at = r.u64()?;
    let n = r.u32()? as usize;
    let names_len = r.u32()? as usize;
    let root_len = r.u32()? as usize;
    if r.take(root_len)? != root.as_os_str().as_bytes() {
        return Err(bad("root"));
    }
    let off = r.u32s(n.checked_add(1).ok_or_else(|| bad("count"))?)?;
    let parent = r.u32s(n)?;
    let end = r.u32s(n)?;
    let mtime = r.u32s(n)?;
    let size = r.u64s(n)?;
    let depth = r.take(n)?.to_vec();
    let flags = r.take(n)?.to_vec();
    let names = r.take(names_len)?.to_vec();
    if r.pos != body.len() {
        return Err(bad("trailing bytes"));
    }
    let mut ix = Index {
        root: root.to_path_buf(),
        built_at,
        generation: 0,
        names,
        lower: Vec::new(),
        off,
        parent,
        end,
        mtime,
        size,
        depth,
        flags,
    };
    validate(&ix)?;
    ix.lower = fold(&ix.names);
    Ok(ix)
}

/// Every invariant the queries index by, so a file that passes its checksum but was written by
/// a bug can still never make a lookup loop or slice out of bounds.
fn validate(ix: &Index) -> io::Result<()> {
    let n = ix.len();
    if n == 0 || ix.off.len() != n + 1 || ix.off[0] != 0 || ix.off[n] as usize != ix.names.len() {
        return Err(bad("offsets"));
    }
    for w in ix.off.windows(2) {
        if w[0] >= w[1] || ix.names[w[1] as usize - 1] != b'/' {
            return Err(bad("names"));
        }
    }
    if ix.parent[0] != NONE || ix.end[0] as usize != n || ix.flags[0] & DIR == 0 {
        return Err(bad("root record"));
    }
    for i in 1..n {
        let p = ix.parent[i] as usize;
        let e = ix.end[i] as usize;
        if p >= i || ix.flags[p] & DIR == 0 || e <= i || e > ix.end[p] as usize {
            return Err(bad("tree"));
        }
    }
    Ok(())
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("search index: {what}"))
}

fn checksum(b: &[u8]) -> u64 {
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = 0x243f_6a88_85a3_08d3 ^ b.len() as u64;
    let mut words = b.chunks_exact(8);
    for w in &mut words {
        let w = u64::from_le_bytes(w.try_into().unwrap_or([0; 8]));
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    }
    for &x in words.remainder() {
        h = (h.rotate_left(5) ^ u64::from(x)).wrapping_mul(K);
    }
    h ^ (h >> 29)
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| bad("truncated"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or([0; 4])))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap_or([0; 8])))
    }

    fn u32s(&mut self, n: usize) -> io::Result<Vec<u32>> {
        let s = self.take(n.checked_mul(4).ok_or_else(|| bad("count"))?)?;
        Ok(s.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    fn u64s(&mut self, n: usize) -> io::Result<Vec<u64>> {
        let s = self.take(n.checked_mul(8).ok_or_else(|| bad("count"))?)?;
        Ok(s.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::raw;
    use std::path::PathBuf;

    fn sample() -> Index {
        let r = raw("/h", &[("Docs", true, 9, 0), ("Docs/Ünï.txt", false, 10, 1234), ("a.png", false, 11, u64::MAX)]);
        Index::from_raw(PathBuf::from("/h"), r, 1_700_000_000)
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(file_name());
        let ix = sample();
        save(&ix, &p).unwrap();
        let back = load(&p, Path::new("/h")).unwrap();
        assert_eq!(back, ix);
        assert!(load(&p, Path::new("/other")).is_err());
    }

    #[test]
    fn rejects_damage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(file_name());
        save(&sample(), &p).unwrap();
        let good = fs::read(&p).unwrap();
        for i in [0, 9, 20, good.len() / 2, good.len() - 1] {
            let mut b = good.clone();
            b[i] ^= 0x41;
            fs::write(&p, &b).unwrap();
            assert!(load(&p, Path::new("/h")).is_err(), "flip at {i}");
        }
        fs::write(&p, &good[..good.len() / 3]).unwrap();
        assert!(load(&p, Path::new("/h")).is_err());
        fs::write(&p, b"").unwrap();
        assert!(load(&p, Path::new("/h")).is_err());
    }

    #[test]
    fn rejects_old_version_and_broken_tree() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(file_name());
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&(VERSION - 1).to_le_bytes());
        b.extend_from_slice(&[0; 32]);
        let h = checksum(&b);
        b.extend_from_slice(&h.to_le_bytes());
        fs::write(&p, &b).unwrap();
        assert!(load(&p, Path::new("/h")).unwrap_err().to_string().contains("version"));

        let mut ix = sample();
        ix.parent[2] = 3;
        save(&ix, &p).unwrap();
        assert!(load(&p, Path::new("/h")).unwrap_err().to_string().contains("tree"));
    }
}
