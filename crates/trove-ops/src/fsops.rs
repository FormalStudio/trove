use std::fs::{self, File, FileTimes, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::Resolution;
use crate::job::{At, Ctx, R, Stop, fail, name_of};
use crate::names::{copy_name, exists, free_name, keep_both};

pub(crate) const CHUNK: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Copy,
    Move,
}

fn meta(path: &Path) -> R<Metadata> {
    fs::symlink_metadata(path).at(path)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

fn is_real_dir(m: &Metadata) -> bool {
    m.file_type().is_dir()
}

/// Counts items and bytes under `paths` (symlinks are not followed), reporting as it goes so a
/// huge tree does not look stuck.
pub(crate) fn count(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
    while let Some(path) = stack.pop() {
        ctx.check_cancel()?;
        let m = meta(&path)?;
        ctx.total_files += 1;
        if is_real_dir(&m) {
            ctx.set_current(&path);
            let entries = fs::read_dir(&path).at(&path)?;
            for entry in entries {
                stack.push(entry.at(&path)?.path());
            }
        } else if m.file_type().is_file() {
            ctx.total_bytes += m.len();
        }
        ctx.progress(false);
    }
    ctx.progress(true);
    Ok(())
}

fn ensure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Copy or move `items` (source, destination folder) into place.
pub(crate) fn transfer(ctx: &mut Ctx, mode: Mode, items: &[(PathBuf, PathBuf)]) -> R<()> {
    for (src, dest_dir) in items {
        let m = meta(src)?;
        if !fs::metadata(dest_dir).at(dest_dir)?.is_dir() {
            return fail(format!("{} is not a folder", name_of(dest_dir)));
        }
        if is_real_dir(&m) {
            let real_dest = fs::canonicalize(dest_dir).at(dest_dir)?;
            let real_src = match (src.parent(), src.file_name()) {
                (Some(p), Some(n)) => fs::canonicalize(p).at(p)?.join(n),
                _ => src.clone(),
            };
            if real_dest.starts_with(&real_src) {
                let verb = if mode == Mode::Move { "move" } else { "copy" };
                return fail(format!("Cannot {verb} \"{}\" into itself", name_of(src)));
            }
        }
    }
    let cross_device = items.iter().any(|(src, dest)| {
        let a = fs::symlink_metadata(src).map(|m| m.dev());
        let b = fs::metadata(dest).map(|m| m.dev());
        a.ok() != b.ok()
    });
    if mode == Mode::Copy || cross_device {
        let sources: Vec<PathBuf> = items.iter().map(|(s, _)| s.clone()).collect();
        count(ctx, &sources)?;
    } else {
        ctx.total_files += items.len() as u64;
    }

    for (src, dest_dir) in items {
        ctx.check_cancel()?;
        ctx.set_current(src);
        let name = name_of(src);
        let src_is_dir = is_real_dir(&meta(src)?);
        let mut dest = dest_dir.join(&name);
        let mut replace = false;
        if exists(&dest) {
            if same_file(src, &dest) {
                if mode == Mode::Copy {
                    dest = dest_dir.join(copy_name(dest_dir, &name, src_is_dir));
                } else {
                    ctx.created.push(src.clone());
                    ctx.finish_item();
                    continue;
                }
            } else {
                match ctx.resolve_conflict(src, &dest)? {
                    Resolution::Skip => {
                        ctx.finish_item();
                        continue;
                    }
                    Resolution::KeepBoth => dest = dest_dir.join(keep_both(dest_dir, &name, src_is_dir)),
                    Resolution::Replace => {
                        replace = true;
                        ctx.tainted = true;
                    }
                    Resolution::Cancel => return Err(Stop::Cancelled),
                }
            }
        }
        match mode {
            Mode::Copy => copy_entry(ctx, src, &dest, replace)?,
            Mode::Move => {
                let skipped = ctx.skipped;
                move_entry(ctx, src, &dest, replace)?;
                if ctx.skipped == skipped {
                    ctx.pairs.push((src.clone(), dest.clone()));
                }
            }
        }
        ctx.created.push(dest);
    }
    ctx.progress(true);
    if mode == Mode::Move { left_in_place(ctx) } else { Ok(()) }
}

/// Fails a move that kept sources back because a copy across file systems skipped special files.
fn left_in_place(ctx: &Ctx) -> R<()> {
    match ctx.skipped {
        0 => Ok(()),
        1 => fail("Left 1 special file in place"),
        n => fail(format!("Left {n} special files in place")),
    }
}

/// Moves `src` to exactly `dest`, which must not exist.
pub(crate) fn move_exact(ctx: &mut Ctx, pairs: &[(PathBuf, PathBuf)]) -> R<()> {
    ctx.total_files += pairs.len() as u64;
    for (from, to) in pairs {
        ctx.check_cancel()?;
        ctx.set_current(from);
        meta(from)?;
        if exists(to) {
            return fail(format!("\"{}\" already exists", name_of(to)));
        }
        if let Some(parent) = to.parent() {
            ensure_dir(parent).at(parent)?;
        }
        let skipped = ctx.skipped;
        move_entry(ctx, from, to, false)?;
        if ctx.skipped == skipped {
            ctx.pairs.push((from.clone(), to.clone()));
        }
        ctx.created.push(to.clone());
    }
    ctx.progress(true);
    left_in_place(ctx)
}

/// `rename(2)` when it works; across file systems a full copy, then the source goes only once
/// every byte landed (and stays when the copy skipped a special file). With `replace`, an
/// existing folder is merged into, and an existing file overwritten; without it an existing
/// `dest` fails the move.
pub(crate) fn move_entry(ctx: &mut Ctx, src: &Path, dest: &Path, replace: bool) -> R<()> {
    ctx.check_cancel()?;
    let sm = meta(src)?;
    if replace && let Ok(dm) = fs::symlink_metadata(dest) {
        match (is_real_dir(&sm), is_real_dir(&dm)) {
            (true, true) => {
                for entry in fs::read_dir(src).at(src)? {
                    let child = entry.at(src)?.path();
                    let child_dest = dest.join(name_of(&child));
                    move_entry(ctx, &child, &child_dest, true)?;
                }
                let _ = fs::remove_dir(src);
                ctx.finish_item();
                return Ok(());
            }
            (false, true) => {
                return fail(format!("Cannot replace folder \"{}\" with a file", name_of(dest)));
            }
            (true, false) => fs::remove_file(dest).at(dest)?,
            (false, false) => {}
        }
    }
    #[cfg(test)]
    let forced = ctx.force_exdev;
    #[cfg(not(test))]
    let forced = false;
    let renamed = if forced {
        Err(io::Error::from_raw_os_error(libc::EXDEV))
    } else if replace {
        fs::rename(src, dest)
    } else {
        rename_noreplace(src, dest)
    };
    match renamed {
        Ok(()) => {
            ctx.finish_item();
            Ok(())
        }
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            let skipped = ctx.skipped;
            copy_entry(ctx, src, dest, replace)?;
            if ctx.skipped == skipped { remove_all(src).at(src) } else { Ok(()) }
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => fail(format!("\"{}\" already exists", name_of(dest))),
        Err(e) => Err(e).at(src),
    }
}

/// `rename(2)` that fails with `AlreadyExists` rather than replace `to`. A file system without
/// `RENAME_NOREPLACE` gets a check, then a plain rename.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let a = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let b = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both paths are NUL-terminated and live across the call.
        let rc =
            unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
        if rc == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(e);
        }
    }
    if exists(to) {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    fs::rename(from, to)
}

pub(crate) fn remove_all(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Copies one entry, recursing into folders. With `replace`, an existing folder is merged into
/// and an existing file overwritten (through a temporary name, so the old file survives a
/// failure).
pub(crate) fn copy_entry(ctx: &mut Ctx, src: &Path, dest: &Path, replace: bool) -> R<()> {
    ctx.check_cancel()?;
    ctx.set_current(src);
    let m = meta(src)?;
    let ft = m.file_type();
    let dest_meta = fs::symlink_metadata(dest).ok();
    if replace && let Some(dm) = &dest_meta {
        match (ft.is_dir(), is_real_dir(dm)) {
            (false, true) => return fail(format!("Cannot replace folder \"{}\" with a file", name_of(dest))),
            (true, false) => fs::remove_file(dest).at(dest)?,
            _ => {}
        }
    }
    if ft.is_dir() {
        let existing = replace && fs::symlink_metadata(dest).is_ok_and(|d| is_real_dir(&d));
        if !existing {
            fs::DirBuilder::new().mode((m.mode() & 0o7777) | 0o700).create(dest).at(dest)?;
        }
        for entry in fs::read_dir(src).at(src)? {
            let child = entry.at(src)?.path();
            copy_entry(ctx, &child, &dest.join(name_of(&child)), replace)?;
        }
        if !existing {
            fs::set_permissions(dest, fs::Permissions::from_mode(m.mode() & 0o7777)).at(dest)?;
            apply_times(dest, &m);
        }
        ctx.finish_item();
    } else if ft.is_symlink() {
        let target = fs::read_link(src).at(src)?;
        if replace && fs::symlink_metadata(dest).is_ok() {
            fs::remove_file(dest).at(dest)?;
        }
        symlink(&target, dest).at(dest)?;
        ctx.finish_item();
    } else if ft.is_file() {
        copy_file(ctx, src, dest, replace, &m)?;
        ctx.finish_item();
    } else {
        ctx.skipped += 1;
        ctx.finish_item();
    }
    Ok(())
}

fn apply_times(path: &Path, m: &Metadata) {
    let mut times = FileTimes::new();
    if let Ok(t) = m.accessed() {
        times = times.set_accessed(t);
    }
    if let Ok(t) = m.modified() {
        times = times.set_modified(t);
    }
    if let Ok(f) = File::open(path) {
        let _ = f.set_times(times);
    }
}

/// Deletes the file it guards unless disarmed, so a failed or cancelled copy leaves no partial.
struct Partial {
    path: PathBuf,
    armed: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let name = name_of(dest);
    dest.with_file_name(format!(".{name}.trove-part"))
}

fn copy_file(ctx: &mut Ctx, src: &Path, dest: &Path, replace: bool, m: &Metadata) -> R<()> {
    let tmp = (replace && exists(dest)).then(|| part_path(dest));
    let target = tmp.as_deref().unwrap_or(dest);
    let mut input = File::open(src).at(src)?;
    let mut output = OpenOptions::new().write(true).create_new(true).mode(0o600).open(target).at(target)?;
    let mut guard = Partial { path: target.to_path_buf(), armed: true };
    copy_data(ctx, &mut input, &mut output, m.len()).map_err(|e| match e {
        Stop::Failed(msg) => Stop::Failed(format!("{}: {msg}", name_of(src))),
        other => other,
    })?;
    output.set_permissions(fs::Permissions::from_mode(m.mode() & 0o7777)).at(target)?;
    let mut times = FileTimes::new();
    if let Ok(t) = m.accessed() {
        times = times.set_accessed(t);
    }
    if let Ok(t) = m.modified() {
        times = times.set_modified(t);
    }
    let _ = output.set_times(times);
    drop(output);
    if let Some(tmp) = &tmp {
        fs::rename(tmp, dest).at(dest)?;
    }
    guard.armed = false;
    Ok(())
}

fn copy_data(ctx: &mut Ctx, input: &mut File, output: &mut File, len: u64) -> R<()> {
    #[cfg(target_os = "linux")]
    if ctx.reflink && len > 0 {
        // SAFETY: both descriptors are open for the duration of the call.
        let rc = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
        if rc == 0 {
            ctx.add_bytes(len);
            return Ok(());
        }
    }
    let mut use_cfr = cfg!(target_os = "linux");
    let mut buf: Vec<u8> = Vec::new();
    let mut copied: u64 = 0;
    loop {
        ctx.check_cancel()?;
        let n = if use_cfr {
            match copy_range(input, output) {
                Ok(0) if copied == 0 && len > 0 => {
                    use_cfr = false;
                    continue;
                }
                Ok(n) => n,
                Err(e) if copied == 0 && cfr_unsupported(&e) => {
                    use_cfr = false;
                    continue;
                }
                Err(e) => return Err(Stop::Failed(crate::job::io_message(&e))),
            }
        } else {
            if buf.is_empty() {
                buf.resize(CHUNK, 0);
            }
            let n = loop {
                match input.read(&mut buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    r => break r.map_err(|e| Stop::Failed(crate::job::io_message(&e)))?,
                }
            };
            output.write_all(&buf[..n]).map_err(|e| Stop::Failed(crate::job::io_message(&e)))?;
            n
        };
        if n == 0 {
            return Ok(());
        }
        copied += n as u64;
        ctx.add_bytes(n as u64);
    }
}

fn cfr_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM | libc::EBADF)
    )
}

#[cfg(target_os = "linux")]
fn copy_range(input: &File, output: &File) -> io::Result<usize> {
    loop {
        // SAFETY: null offsets use and advance the descriptors' own positions.
        let n = unsafe {
            libc::copy_file_range(
                input.as_raw_fd(),
                std::ptr::null_mut(),
                output.as_raw_fd(),
                std::ptr::null_mut(),
                CHUNK,
                0,
            )
        };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_range(_: &File, _: &File) -> io::Result<usize> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
}

/// Permanent delete, checking for cancel between items.
pub(crate) fn delete_paths(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    count(ctx, paths)?;
    for path in paths {
        delete_tree(ctx, path)?;
    }
    ctx.progress(true);
    Ok(())
}

pub(crate) fn delete_tree(ctx: &mut Ctx, path: &Path) -> R<()> {
    ctx.check_cancel()?;
    ctx.set_current(path);
    let m = meta(path)?;
    if is_real_dir(&m) {
        for entry in fs::read_dir(path).at(path)? {
            delete_tree(ctx, &entry.at(path)?.path())?;
        }
        fs::remove_dir(path).at(path)?;
    } else {
        fs::remove_file(path).at(path)?;
        ctx.done_bytes += m.len();
    }
    ctx.finish_item();
    Ok(())
}

pub(crate) fn rename(ctx: &mut Ctx, path: &Path, new_name: &str) -> R<()> {
    let name = new_name.trim();
    if name.is_empty() {
        return fail("The name cannot be empty");
    }
    if name.contains('/') || name.contains('\0') {
        return fail("The name cannot contain \"/\"");
    }
    if name == "." || name == ".." {
        return fail(format!("\"{name}\" is not a valid name"));
    }
    let m = meta(path)?;
    let Some(parent) = path.parent() else {
        return fail("Cannot rename the root folder");
    };
    let target = parent.join(name);
    if name_of(path) == name {
        ctx.created.push(path.to_path_buf());
        return Ok(());
    }
    let renamed = match fs::symlink_metadata(&target) {
        Ok(tm) if tm.dev() == m.dev() && tm.ino() == m.ino() => fs::rename(path, &target),
        Ok(_) => return fail(format!("\"{name}\" already exists")),
        Err(_) => rename_noreplace(path, &target),
    };
    match renamed {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return fail(format!("\"{name}\" already exists")),
        r => r.at(path)?,
    }
    ctx.pairs.push((path.to_path_buf(), target.clone()));
    ctx.created.push(target);
    Ok(())
}

pub(crate) fn create(ctx: &mut Ctx, parent: &Path, name: &str, folder: bool) -> R<()> {
    if name.is_empty() || name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return fail(format!("\"{name}\" is not a valid name"));
    }
    if !fs::metadata(parent).at(parent)?.is_dir() {
        return fail(format!("{} is not a folder", name_of(parent)));
    }
    let mut candidate = free_name(parent, name, folder);
    loop {
        let path = parent.join(&candidate);
        let made = if folder {
            fs::DirBuilder::new().mode(0o777).create(&path)
        } else {
            OpenOptions::new().write(true).create_new(true).mode(0o666).open(&path).map(drop)
        };
        match made {
            Ok(()) => {
                ctx.created.push(path);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => candidate = keep_both(parent, &candidate, folder),
            Err(e) => return Err(e).at(&path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Stop;
    use crate::trash::TrashConfig;
    use crate::{JobId, OpEvent};
    use std::os::unix::fs::FileTypeExt;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn ctx() -> (Ctx, flume::Receiver<OpEvent>) {
        let (etx, erx) = flume::unbounded();
        let (_rtx, rrx) = flume::unbounded();
        let cfg = TrashConfig { data_home: PathBuf::new(), uid: 0, mounts: Some(Vec::new()) };
        (Ctx::new(JobId(1), etx, rrx, Arc::new(AtomicBool::new(false)), Arc::new(cfg)), erx)
    }

    #[test]
    fn cancel_mid_copy_leaves_no_partial() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("big.bin");
        let data = vec![7u8; 64 << 20];
        fs::write(&src, &data).unwrap();
        let dest = d.path().join("out.bin");
        let (mut c, _rx) = ctx();
        c.reflink = false;
        c.cancel_after_bytes = Some(8 << 20);
        let r = copy_entry(&mut c, &src, &dest, false);
        assert!(matches!(r, Err(Stop::Cancelled)));
        assert!(!dest.exists());
        let names: Vec<_> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "only the source is left: {names:?}");
        assert!(c.done_bytes >= 8 << 20 && c.done_bytes < 64 << 20);
    }

    #[test]
    fn cancel_during_replace_keeps_the_old_file() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("new.bin");
        fs::write(&src, vec![1u8; 16 << 20]).unwrap();
        let dest = d.path().join("old.bin");
        fs::write(&dest, "old").unwrap();
        let (mut c, _rx) = ctx();
        c.reflink = false;
        c.cancel_after_bytes = Some(2 << 20);
        assert!(matches!(copy_entry(&mut c, &src, &dest, true), Err(Stop::Cancelled)));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);
    }

    #[test]
    fn copy_then_delete_is_the_cross_device_move() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("tree");
        fs::create_dir_all(src.join("sub/deep")).unwrap();
        fs::write(src.join("a.txt"), "alpha").unwrap();
        fs::write(src.join("sub/b.sh"), "#!/bin/sh").unwrap();
        fs::set_permissions(src.join("sub/b.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        symlink("a.txt", src.join("link")).unwrap();
        symlink("missing", src.join("dangling")).unwrap();
        fs::set_permissions(src.join("sub/deep"), fs::Permissions::from_mode(0o555)).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        File::open(src.join("a.txt")).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();

        let dest = d.path().join("copy");
        let (mut c, _rx) = ctx();
        copy_entry(&mut c, &src, &dest, false).unwrap();
        remove_all(&src).unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "alpha");
        assert_eq!(fs::metadata(dest.join("a.txt")).unwrap().modified().unwrap(), old);
        assert_eq!(fs::metadata(dest.join("sub/b.sh")).unwrap().mode() & 0o777, 0o750);
        assert_eq!(fs::metadata(dest.join("sub/deep")).unwrap().mode() & 0o777, 0o555);
        assert_eq!(fs::read_link(dest.join("link")).unwrap(), PathBuf::from("a.txt"));
        assert_eq!(fs::read_link(dest.join("dangling")).unwrap(), PathBuf::from("missing"));
        fs::set_permissions(dest.join("sub/deep"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn cross_device_replace_merges_and_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let (src, dest) = (d.path().join("src"), d.path().join("dest"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(dest.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "new").unwrap();
        fs::write(src.join("sub/b.txt"), "new b").unwrap();
        fs::write(dest.join("a.txt"), "old").unwrap();
        fs::write(dest.join("sub/b.txt"), "old b").unwrap();
        fs::write(dest.join("keep.txt"), "kept").unwrap();
        let (mut c, _rx) = ctx();
        c.force_exdev = true;
        move_entry(&mut c, &src, &dest, true).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.join("sub/b.txt")).unwrap(), "new b");
        assert_eq!(fs::read_to_string(dest.join("keep.txt")).unwrap(), "kept");
    }

    #[test]
    fn cross_device_move_keeps_a_source_holding_special_files() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("tree");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), "alpha").unwrap();
        let fifo = std::ffi::CString::new(src.join("pipe").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let (mut c, _rx) = ctx();
        c.force_exdev = true;
        match transfer(&mut c, Mode::Move, &[(src.clone(), out.clone())]) {
            Err(Stop::Failed(msg)) => assert_eq!(msg, "Left 1 special file in place"),
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(fs::symlink_metadata(src.join("pipe")).unwrap().file_type().is_fifo());
        assert_eq!(fs::read_to_string(out.join("tree/a.txt")).unwrap(), "alpha");
        assert!(c.pairs.is_empty(), "nothing to move back");
    }

    #[test]
    fn a_move_without_replace_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let (mut c, _rx) = ctx();
        assert!(matches!(move_entry(&mut c, &a, &b, false), Err(Stop::Failed(m)) if m.contains("already exists")));
        assert_eq!(fs::read_to_string(&b).unwrap(), "b");
        assert!(a.exists());
    }

    #[test]
    fn large_copy_is_byte_exact_both_paths() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("f");
        let data: Vec<u8> = (0..(3 << 20) + 17).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &data).unwrap();
        for (i, reflink) in [true, false].into_iter().enumerate() {
            let dest = d.path().join(format!("out{i}"));
            let (mut c, _rx) = ctx();
            c.reflink = reflink;
            copy_entry(&mut c, &src, &dest, false).unwrap();
            assert_eq!(fs::read(&dest).unwrap(), data);
            assert_eq!(c.done_bytes, data.len() as u64);
        }
    }
}
