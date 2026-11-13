//! The macOS Trash through NSFileManager, which picks the right trash for each volume. macOS
//! keeps no record Trove can read of where an item came from, so its original path goes in an
//! extended attribute before the move and restore reads it back.

use std::ffi::{CStr, CString, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use objc2_foundation::{NSCocoaErrorDomain, NSFeatureUnsupportedError, NSFileManager, NSURL};

use super::{NO_VOLUME_TRASH, TrashConfig};
use crate::job::{At, Ctx, R, fail, name_of};
use crate::names::free_name;

const ORIGIN: &CStr = c"com.formalstudio.trove.trash-origin";

pub(super) fn home_trash() -> PathBuf {
    crate::sys::home().map(PathBuf::from).unwrap_or_default().join(".Trash")
}

/// The home Trash and each mounted volume's `.Trashes/$uid`.
pub(super) fn all_trashes(cfg: &TrashConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = home_trash();
    if home.is_dir() {
        out.push(home);
    }
    let volumes = match &cfg.mounts {
        Some(m) => m.clone(),
        None => {
            fs::read_dir("/Volumes").map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect()).unwrap_or_default()
        }
    };
    for v in volumes {
        let t = v.join(".Trashes").join(cfg.uid.to_string());
        if t.is_dir() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn c_path(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_encoded_bytes())?)
}

fn set_origin(path: &Path) -> io::Result<()> {
    let c = c_path(path)?;
    let v = path.as_os_str().as_encoded_bytes();
    // SAFETY: both strings are NUL-terminated and the value is `v.len()` readable bytes.
    let rc =
        unsafe { libc::setxattr(c.as_ptr(), ORIGIN.as_ptr(), v.as_ptr().cast(), v.len(), 0, libc::XATTR_NOFOLLOW) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

fn remove_origin(path: &Path) {
    if let Ok(c) = c_path(path) {
        // SAFETY: both strings are NUL-terminated.
        unsafe { libc::removexattr(c.as_ptr(), ORIGIN.as_ptr(), libc::XATTR_NOFOLLOW) };
    }
}

fn origin_of(path: &Path) -> Option<PathBuf> {
    let c = c_path(path).ok()?;
    // SAFETY: a null buffer asks for the size only.
    let len = unsafe { libc::getxattr(c.as_ptr(), ORIGIN.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) };
    let mut buf = vec![0u8; usize::try_from(len).ok().filter(|&n| n > 0)?];
    // SAFETY: the buffer holds `buf.len()` writable bytes.
    let got = unsafe {
        libc::getxattr(c.as_ptr(), ORIGIN.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, libc::XATTR_NOFOLLOW)
    };
    buf.truncate(usize::try_from(got).ok()?);
    let p = PathBuf::from(OsString::from_vec(buf));
    p.is_absolute().then_some(p)
}

pub(super) fn trash_paths(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    ctx.total_files += paths.len() as u64;
    for path in paths {
        ctx.check_cancel()?;
        ctx.set_current(path);
        let trashed = trash_one(path)?;
        ctx.trashed.push((path.clone(), trashed));
        ctx.finish_item();
    }
    ctx.progress(true);
    Ok(())
}

/// Moves `path` into the Trash and returns where it now lives.
fn trash_one(path: &Path) -> R<PathBuf> {
    let Some(name) = path.file_name() else {
        return fail("Cannot move the root folder to Trash");
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    let full = fs::canonicalize(parent).at(parent)?.join(name);
    fs::symlink_metadata(&full).at(&full)?;
    let Some(url) = NSURL::from_file_path(&full) else {
        return fail(format!("Cannot move \"{}\" to Trash", name_of(&full)));
    };
    // Without the record the item still goes to the Trash; only restoring it fails later.
    let _ = set_origin(&full);
    let mut out = None;
    let moved = NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, Some(&mut out));
    match (moved, out.and_then(|u| u.to_file_path())) {
        (Ok(()), Some(dest)) => Ok(dest),
        (Ok(()), None) => fail(format!("Moved \"{}\" to Trash, but cannot tell where", name_of(&full))),
        (Err(e), _) => {
            remove_origin(&full);
            // SAFETY: NSCocoaErrorDomain is an immutable string constant.
            if e.code() == NSFeatureUnsupportedError && &*e.domain() == unsafe { NSCocoaErrorDomain } {
                return fail(format!("{NO_VOLUME_TRASH}: {}", name_of(&full)));
            }
            fail(format!("{}: {}", name_of(&full), e.localizedDescription()))
        }
    }
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
    let name = name_of(item);
    fs::symlink_metadata(item).at(item)?;
    let Some(original) = origin_of(item) else {
        return fail(format!("\"{name}\" has no original location recorded"));
    };
    let Some(dir) = original.parent() else {
        return fail(format!("\"{name}\" has no original location recorded"));
    };
    fs::create_dir_all(dir).at(dir)?;
    let is_dir = fs::symlink_metadata(item).at(item)?.file_type().is_dir();
    let dest = dir.join(free_name(dir, &name_of(&original), is_dir));
    crate::fsops::move_entry(ctx, item, &dest, false)?;
    remove_origin(&dest);
    Ok(dest)
}

/// Deletes everything in every trash but Finder's own `.DS_Store`.
pub(super) fn empty_trash(ctx: &mut Ctx) -> R<()> {
    let mut items = Vec::new();
    for trash in all_trashes(&ctx.trash) {
        let rd = fs::read_dir(&trash).at(&trash)?;
        items.extend(rd.filter_map(Result::ok).filter(|e| e.file_name() != ".DS_Store").map(|e| e.path()));
    }
    crate::fsops::count(ctx, &items)?;
    for item in &items {
        crate::fsops::delete_tree(ctx, item)?;
    }
    ctx.progress(true);
    Ok(())
}
