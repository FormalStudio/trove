use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

use crate::fsops::{CHUNK, count};
use crate::job::{At, Ctx, R, Stop, fail, io_message, name_of};
use crate::names::{exists, free_name, split_ext};
use crate::trash::local_time;

fn zip_err(e: zip::result::ZipError) -> Stop {
    Stop::Failed(e.to_string())
}

struct Part(PathBuf, bool);

impl Drop for Part {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}

pub(crate) fn compress(ctx: &mut Ctx, paths: &[PathBuf], dest: &Path) -> R<()> {
    let Some(first) = paths.first() else {
        return fail("Nothing to compress");
    };
    let target = if dest.is_dir() {
        let name = if paths.len() == 1 { format!("{}.zip", name_of(first)) } else { "Archive.zip".to_owned() };
        dest.join(name)
    } else {
        dest.to_path_buf()
    };
    let Some(dir) = target.parent() else {
        return fail("Invalid archive location");
    };
    let final_path = dir.join(free_name(dir, &name_of(&target), false));
    count(ctx, paths)?;

    let tmp = dir.join(format!(".{}.trove-part", name_of(&final_path)));
    let file = OpenOptions::new().write(true).create_new(true).mode(0o666).open(&tmp).at(&tmp)?;
    let mut guard = Part(tmp.clone(), true);
    let mut zip = ZipWriter::new(BufWriter::new(file));
    for path in paths {
        add_entry(ctx, &mut zip, path, &name_of(path))?;
    }
    zip.finish().map_err(zip_err)?.flush().at(&tmp)?;
    fs::rename(&tmp, &final_path).at(&final_path)?;
    guard.1 = false;
    ctx.created.push(final_path);
    ctx.progress(true);
    Ok(())
}

fn zip_time(m: &fs::Metadata) -> Option<DateTime> {
    let (y, mo, d, h, mi, s) = local_time(m.modified().ok()?);
    DateTime::from_date_and_time(u16::try_from(y).ok()?, mo as u8, d as u8, h as u8, mi as u8, s as u8).ok()
}

fn add_entry<W: Write + io::Seek>(ctx: &mut Ctx, zip: &mut ZipWriter<W>, path: &Path, name: &str) -> R<()> {
    ctx.check_cancel()?;
    ctx.set_current(path);
    let m = fs::symlink_metadata(path).at(path)?;
    let mut opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(m.mode() & 0o7777)
        .large_file(m.len() >= u64::from(u32::MAX));
    if let Some(t) = zip_time(&m) {
        opts = opts.last_modified_time(t);
    }
    let ft = m.file_type();
    if ft.is_dir() {
        zip.add_directory(format!("{name}/"), opts).map_err(zip_err)?;
        let mut children: Vec<PathBuf> =
            fs::read_dir(path).at(path)?.filter_map(Result::ok).map(|e| e.path()).collect();
        children.sort();
        for child in children {
            add_entry(ctx, zip, &child, &format!("{name}/{}", name_of(&child)))?;
        }
    } else if ft.is_symlink() {
        let target = fs::read_link(path).at(path)?;
        zip.add_symlink(name, target.to_string_lossy(), opts).map_err(zip_err)?;
    } else if ft.is_file() {
        zip.start_file(name, opts).map_err(zip_err)?;
        let mut input = File::open(path).at(path)?;
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            ctx.check_cancel()?;
            let n = input.read(&mut buf).at(path)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n]).at(path)?;
            ctx.add_bytes(n as u64);
        }
    } else {
        return Ok(());
    }
    ctx.finish_item();
    Ok(())
}

const TAR_SUFFIXES: [&str; 8] = [".tar", ".tgz", ".tbz2", ".txz", ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"];

fn is_zip(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".zip")
}

fn is_tar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    TAR_SUFFIXES.iter().any(|s| lower.ends_with(s))
}

/// "photos.zip" -> "photos", "src.tar.gz" -> "src".
fn archive_stem(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for suffix in TAR_SUFFIXES {
        if lower.ends_with(suffix) {
            return &name[..name.len() - suffix.len()];
        }
    }
    split_ext(name, false).0
}

pub(crate) fn find_in_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(tool)).find(|p| p.is_file())
}

pub(crate) fn extract(ctx: &mut Ctx, archive: &Path, dest_dir: &Path) -> R<()> {
    let name = name_of(archive);
    fs::metadata(archive).at(archive)?;
    if !fs::metadata(dest_dir).at(dest_dir)?.is_dir() {
        return fail(format!("{} is not a folder", name_of(dest_dir)));
    }
    let staging = (0u32..)
        .map(|n| dest_dir.join(format!(".{name}.trove-extract-{}-{n}", std::process::id())))
        .find(|p| !exists(p))
        .unwrap_or_else(|| dest_dir.join(".trove-extract"));
    fs::create_dir(&staging).at(&staging)?;
    let result = extract_into(ctx, archive, &name, &staging);
    let placed = result.and_then(|()| place_extracted(ctx, &staging, dest_dir, archive_stem(&name)));
    let _ = fs::remove_dir_all(&staging);
    placed?;
    ctx.progress(true);
    Ok(())
}

fn extract_into(ctx: &mut Ctx, archive: &Path, name: &str, staging: &Path) -> R<()> {
    ctx.set_current(archive);
    if is_zip(name) {
        return extract_zip(ctx, archive, staging);
    }
    // Both refuse absolute and ".." members by default and never extract through a symlink the
    // archive made (GNU tar holds such links back as placeholders), as long as -P is never passed.
    if let Some(bsdtar) = find_in_path("bsdtar") {
        return run_tool(
            ctx,
            Command::new(bsdtar).arg("--no-same-owner").arg("-xf").arg(archive).arg("-C").arg(staging),
        );
    }
    if is_tar(name) {
        if let Some(tar) = find_in_path("tar") {
            return run_tool(
                ctx,
                Command::new(tar).arg("--no-same-owner").arg("-xf").arg(archive).arg("-C").arg(staging),
            );
        }
        return fail(format!("Cannot extract \"{name}\": tar is not installed"));
    }
    fail(format!("Cannot extract \"{name}\": bsdtar is not installed"))
}

fn run_tool(ctx: &mut Ctx, cmd: &mut Command) -> R<()> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Stop::Failed(io_message(&e)))?;
    loop {
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Stop::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                let line = err.lines().next().unwrap_or("the archive could not be read");
                return fail(format!("Extract failed: {line}"));
            }
            Ok(None) => {
                ctx.progress(false);
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(Stop::Failed(io_message(&e))),
        }
    }
}

fn extract_zip(ctx: &mut Ctx, archive: &Path, staging: &Path) -> R<()> {
    let file = File::open(archive).at(archive)?;
    let mut zip = ZipArchive::new(file).map_err(zip_err)?;
    ctx.total_files = zip.len() as u64;
    for i in 0..zip.len() {
        ctx.check_cancel()?;
        let mut entry = zip.by_index(i).map_err(zip_err)?;
        let unsafe_path = || fail(format!("Unsafe path in archive: {}", entry.name().unwrap_or_default()));
        let Some(rel) = entry.enclosed_name() else {
            return unsafe_path();
        };
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) || through_symlink(staging, &rel) {
            return unsafe_path();
        }
        let out = staging.join(rel);
        ctx.set_current(&out);
        if entry.is_dir() {
            fs::create_dir_all(&out).at(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).at(parent)?;
            }
            if entry.is_symlink() {
                let mut target = String::new();
                entry.read_to_string(&mut target).at(&out)?;
                std::os::unix::fs::symlink(target, &out).at(&out)?;
            } else {
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&out)
                    .at(&out)?;
                let mut buf = vec![0u8; CHUNK.min(256 * 1024)];
                loop {
                    ctx.check_cancel()?;
                    let n = entry.read(&mut buf).at(&out)?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n]).at(&out)?;
                    ctx.add_bytes(n as u64);
                }
                if let Some(mode) = entry.unix_mode() {
                    f.set_permissions(fs::Permissions::from_mode(mode & 0o7777)).at(&out)?;
                }
            }
        }
        ctx.finish_item();
    }
    Ok(())
}

/// Whether any part of `rel` under `staging` is a symlink an earlier entry made, which writing
/// through would land outside staging.
fn through_symlink(staging: &Path, rel: &Path) -> bool {
    let mut at = staging.to_path_buf();
    rel.components().any(|c| {
        at.push(c);
        fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink())
    })
}

/// A single top-level entry goes straight into `dest_dir`; anything else lands in a new folder
/// named after the archive.
fn place_extracted(ctx: &mut Ctx, staging: &Path, dest_dir: &Path, stem: &str) -> R<()> {
    let mut entries: Vec<PathBuf> =
        fs::read_dir(staging).at(staging)?.filter_map(Result::ok).map(|e| e.path()).collect();
    if entries.is_empty() {
        return fail("The archive is empty");
    }
    let (from, name) = if entries.len() == 1 {
        let only = entries.remove(0);
        let name = name_of(&only);
        (only, name)
    } else {
        (staging.to_path_buf(), if stem.is_empty() { "Extracted".to_owned() } else { stem.to_owned() })
    };
    let is_dir = fs::symlink_metadata(&from).at(&from)?.is_dir();
    let dest = dest_dir.join(free_name(dest_dir, &name, is_dir));
    fs::rename(&from, &dest).at(&dest)?;
    ctx.created.push(dest);
    Ok(())
}
