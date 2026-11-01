use std::fs;
use std::path::Path;

pub(crate) fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Splits "name.ext" into ("name", ".ext"). A dotfile has no extension, a folder never does, and
/// "x.tar.gz" splits at ".tar.gz".
pub(crate) fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        None | Some(0) => (name, ""),
        Some(i) => {
            let (stem, ext) = name.split_at(i);
            match stem.strip_suffix(".tar") {
                Some(base) if !base.is_empty() => (base, &name[base.len()..]),
                _ => (stem, ext),
            }
        }
    }
}

/// "name 2.ext", "name 3.ext", ... the first one not taken in `dir`.
pub(crate) fn keep_both(dir: &Path, name: &str, is_dir: bool) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    (2u32..).map(|n| format!("{stem} {n}{ext}")).find(|c| !exists(&dir.join(c))).unwrap_or_else(|| name.to_owned())
}

/// `name` itself when free, else the first keep-both name.
pub(crate) fn free_name(dir: &Path, name: &str, is_dir: bool) -> String {
    if exists(&dir.join(name)) { keep_both(dir, name, is_dir) } else { name.to_owned() }
}

/// "name copy.ext", "name copy 2.ext", ... Copying "name copy.ext" gives "name copy 2.ext", not
/// "name copy copy.ext".
pub(crate) fn copy_name(dir: &Path, name: &str, is_dir: bool) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    let base = strip_copy_suffix(stem);
    let first = format!("{base} copy{ext}");
    if !exists(&dir.join(&first)) {
        return first;
    }
    (2u32..).map(|n| format!("{base} copy {n}{ext}")).find(|c| !exists(&dir.join(c))).unwrap_or(first)
}

fn strip_copy_suffix(stem: &str) -> &str {
    if let Some(base) = stem.strip_suffix(" copy") {
        return base;
    }
    if let Some((head, tail)) = stem.rsplit_once(' ')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
        && let Some(base) = head.strip_suffix(" copy")
    {
        return base;
    }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(split_ext("a.txt", false), ("a", ".txt"));
        assert_eq!(split_ext(".bashrc", false), (".bashrc", ""));
        assert_eq!(split_ext("a.tar.gz", false), ("a", ".tar.gz"));
        assert_eq!(split_ext("v1.2", true), ("v1.2", ""));
        assert_eq!(split_ext("noext", false), ("noext", ""));
    }

    #[test]
    fn copy_names_count_up() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert_eq!(copy_name(p, "a.txt", false), "a copy.txt");
        fs::write(p.join("a copy.txt"), "").unwrap();
        assert_eq!(copy_name(p, "a.txt", false), "a copy 2.txt");
        assert_eq!(copy_name(p, "a copy.txt", false), "a copy 2.txt");
        fs::write(p.join("a copy 2.txt"), "").unwrap();
        assert_eq!(copy_name(p, "a copy 2.txt", false), "a copy 3.txt");
        assert_eq!(copy_name(p, ".env", false), ".env copy");
    }

    #[test]
    fn keep_both_counts_up() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::write(p.join("a.txt"), "").unwrap();
        assert_eq!(keep_both(p, "a.txt", false), "a 2.txt");
        fs::write(p.join("a 2.txt"), "").unwrap();
        assert_eq!(keep_both(p, "a.txt", false), "a 3.txt");
        assert_eq!(free_name(p, "b.txt", false), "b.txt");
    }
}
