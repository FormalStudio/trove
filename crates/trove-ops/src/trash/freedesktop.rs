//! The freedesktop.org Trash specification: the home trash for anything on its file system,
//! `$topdir/.Trash/$uid` or `$topdir/.Trash-$uid` for other volumes.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use super::{NO_VOLUME_TRASH, TrashConfig};
use crate::job::{At, Ctx, R, fail, name_of};
use crate::names::{exists, free_name};
use crate::sys::{self, ModeExt};

const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'/').remove(b'-').remove(b'_').remove(b'.').remove(b'~');

fn mount_points(cfg: &TrashConfig) -> Vec<PathBuf> {
    match &cfg.mounts {
        Some(m) => m.clone(),
        None => read_mountinfo(),
    }
}

/// Every trash directory that exists: the home one and each volume's.
pub(super) fn all_trashes(cfg: &TrashConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = cfg.data_home.join("Trash");
    if home.is_dir() {
        out.push(home);
    }
    let uid = cfg.uid.to_string();
    for mp in mount_points(cfg) {
        let shared = mp.join(".Trash");
        if usable_shared(&shared) {
            let t = shared.join(&uid);
            if t.is_dir() {
                out.push(t);
            }
        }
        let own = mp.join(format!(".Trash-{}", cfg.uid));
        if own.is_dir() && !out.contains(&own) {
            out.push(own);
        }
    }
    out
}

/// Mount points come from `/proc`, which only Linux has; elsewhere the config lists them.
#[cfg(not(target_os = "linux"))]
fn read_mountinfo() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn read_mountinfo() -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(|raw| {
            let mut out = Vec::with_capacity(raw.len());
            let b = raw.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'\\'
                    && i + 3 < b.len()
                    && let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 4], 8)
                {
                    out.push(v);
                    i += 4;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            PathBuf::from(trove_fs::os::os_string_from_bytes(out))
        })
        .collect()
}

/// `$topdir/.Trash` is only used when it is a real folder (not a symlink) with the sticky bit.
fn usable_shared(dir: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir() && sys::mode(&m) & 0o1000 != 0)
}

fn make_private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).unix_mode(0o700).create(path)
}

pub(super) fn trash_paths(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    ctx.total_files += paths.len() as u64;
    for path in paths {
        ctx.check_cancel()?;
        ctx.set_current(path);
        let trashed = trash_one(&ctx.trash, path)?;
        ctx.trashed.push((path.clone(), trashed));
        ctx.finish_item();
    }
    ctx.progress(true);
    Ok(())
}

/// Moves `path` into the right trash and returns where it now lives.
fn trash_one(cfg: &TrashConfig, path: &Path) -> R<PathBuf> {
    let Some(name) = path.file_name() else {
        return fail("Cannot move the root folder to Trash");
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    let full = fs::canonicalize(parent).at(parent)?.join(name);
    let m = fs::symlink_metadata(&full).at(&full)?;

    let home = cfg.data_home.join("Trash");
    let dev = sys::dev(&m, &full);
    if make_private_dir(&cfg.data_home).is_ok()
        && fs::metadata(&cfg.data_home).is_ok_and(|d| sys::dev(&d, &cfg.data_home) == dev)
        && make_private_dir(&home).is_ok()
    {
        return trash_into(&home, &full, &full);
    }
    let top = topdir(cfg, &full, dev);
    let uid = cfg.uid.to_string();
    let shared = top.join(".Trash");
    let candidates = [usable_shared(&shared).then(|| shared.join(&uid)), Some(top.join(format!(".Trash-{}", cfg.uid)))];
    for dir in candidates.into_iter().flatten() {
        if make_private_dir(&dir).is_ok() {
            let rel = full.strip_prefix(&top).unwrap_or(&full).to_path_buf();
            return trash_into(&dir, &full, &rel);
        }
    }
    fail(format!("{NO_VOLUME_TRASH}: {}", name_of(&full)))
}

/// The root of the file system `path` is on: the deepest configured mount point above it, else
/// the highest ancestor with the same device.
fn topdir(cfg: &TrashConfig, path: &Path, dev: Option<u64>) -> PathBuf {
    if let Some(mounts) = &cfg.mounts
        && let Some(best) = mounts.iter().filter(|m| path.starts_with(m)).max_by_key(|m| m.as_os_str().len())
    {
        return best.clone();
    }
    let mut top = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    while let Some(up) = top.parent() {
        if fs::metadata(up).is_ok_and(|m| sys::dev(&m, up) == dev) {
            top = up.to_path_buf();
        } else {
            break;
        }
    }
    top
}

fn trash_into(trash: &Path, src: &Path, recorded: &Path) -> R<PathBuf> {
    let files = trash.join("files");
    let info = trash.join("info");
    make_private_dir(&files).at(&files)?;
    make_private_dir(&info).at(&info)?;
    let is_dir = fs::symlink_metadata(src).at(src)?.file_type().is_dir();
    let name = name_of(src);
    let mut candidate = free_name(&files, &name, is_dir);
    let (info_path, mut file) = loop {
        let info_path = info.join(format!("{candidate}.trashinfo"));
        match OpenOptions::new().write(true).create_new(true).unix_mode(0o600).open(&info_path) {
            Ok(f) if !exists(&files.join(&candidate)) => break (info_path, f),
            Ok(_) => {
                let _ = fs::remove_file(&info_path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).at(&info_path),
        }
        candidate = crate::names::keep_both(&files, &candidate, is_dir);
    };
    let encoded =
        utf8_percent_encode(&String::from_utf8_lossy(recorded.as_os_str().as_encoded_bytes()), PATH_SET).to_string();
    let text = format!("[Trash Info]\nPath={encoded}\nDeletionDate={}\n", iso_local(SystemTime::now()));
    if let Err(e) = file.write_all(text.as_bytes()) {
        let _ = fs::remove_file(&info_path);
        return Err(e).at(&info_path);
    }
    let dest = files.join(&candidate);
    if let Err(e) = fs::rename(src, &dest) {
        let _ = fs::remove_file(&info_path);
        return Err(e).at(src);
    }
    Ok(dest)
}

pub(super) fn restore_paths(ctx: &mut Ctx, trashed: &[PathBuf]) -> R<()> {
    ctx.total_files += trashed.len() as u64;
    for item in trashed {
        ctx.check_cancel()?;
        ctx.set_current(item);
        let dest = restore_one(ctx, item)?;
        ctx.restored.push((item.clone(), dest.clone()));
        ctx.created.push(dest);
        ctx.finish_item();
    }
    ctx.progress(true);
    Ok(())
}

fn restore_one(ctx: &mut Ctx, item: &Path) -> R<PathBuf> {
    let files = item.parent().filter(|p| p.file_name().is_some_and(|n| n == "files"));
    let Some(trash) = files.and_then(Path::parent) else {
        return fail(format!("\"{}\" is not in a Trash folder", name_of(item)));
    };
    let name = name_of(item);
    let info_path = trash.join("info").join(format!("{name}.trashinfo"));
    let Ok(text) = fs::read_to_string(&info_path) else {
        return fail(format!("\"{name}\" has no restore information"));
    };
    let Some(recorded) = parse_trashinfo_path(&text) else {
        return fail(format!("\"{name}\" has no original location recorded"));
    };
    let original = if recorded.is_absolute() {
        recorded
    } else {
        let base = trash.parent().unwrap_or(Path::new("/"));
        let top = if trash.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b".Trash-")) {
            base
        } else {
            base.parent().unwrap_or(base)
        };
        top.join(recorded)
    };
    let Some(dir) = original.parent() else {
        return fail(format!("\"{name}\" has no original location recorded"));
    };
    fs::create_dir_all(dir).at(dir)?;
    let is_dir = fs::symlink_metadata(item).at(item)?.file_type().is_dir();
    let dest = dir.join(free_name(dir, &name_of(&original), is_dir));
    crate::fsops::move_entry(ctx, item, &dest, false)?;
    let _ = fs::remove_file(&info_path);
    Ok(dest)
}

fn parse_trashinfo_path(text: &str) -> Option<PathBuf> {
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Trash Info]";
        } else if in_group && let Some(v) = line.strip_prefix("Path=") {
            let bytes: Vec<u8> = percent_decode_str(v).collect();
            return Some(PathBuf::from(trove_fs::os::os_string_from_bytes(bytes)));
        }
    }
    None
}

pub(super) fn empty_trash(ctx: &mut Ctx) -> R<()> {
    let trashes = all_trashes(&ctx.trash);
    let mut items = Vec::new();
    for trash in &trashes {
        for sub in ["files", "info"] {
            let dir = trash.join(sub);
            if let Ok(rd) = fs::read_dir(&dir) {
                items.extend(rd.filter_map(Result::ok).map(|e| e.path()));
            }
        }
    }
    crate::fsops::count(ctx, &items)?;
    for item in &items {
        crate::fsops::delete_tree(ctx, item)?;
    }
    for trash in &trashes {
        let _ = fs::remove_file(trash.join("directorysizes"));
    }
    ctx.progress(true);
    Ok(())
}

fn iso_local(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = sys::local_time(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}
