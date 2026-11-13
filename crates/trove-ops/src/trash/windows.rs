//! The Windows Recycle Bin through the shell, by way of the `trash` crate. An item's place in
//! the bin (its `$R` file) is what undo and restore take.

use std::fs;
use std::path::{Path, PathBuf};

use trash::TrashItem;
use trash::os_limited;
use windows_sys::Win32::UI::Shell::{COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, COPYENGINE_E_RECYCLE_FORCE_NUKE};

use super::{NO_VOLUME_TRASH, TrashConfig};
use crate::job::{At, Ctx, R, Stop, fail, name_of};
use crate::names::free_name;

pub(super) fn home_trash() -> PathBuf {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    PathBuf::from(format!(r"{drive}\$Recycle.Bin"))
}

/// The Recycle Bin is no folder of items to walk; it is only reached through the shell.
pub(super) fn all_trashes(_: &TrashConfig) -> Vec<PathBuf> {
    Vec::new()
}

fn failed(what: &Path, e: trash::Error) -> Stop {
    let name = name_of(what);
    let text = match e {
        trash::Error::Os { code, .. }
            if code == COPYENGINE_E_RECYCLE_FORCE_NUKE || code == COPYENGINE_E_RECYCLE_BIN_NOT_FOUND =>
        {
            return Stop::Failed(format!("{NO_VOLUME_TRASH}: {name}"));
        }
        trash::Error::Os { description, .. } | trash::Error::Unknown { description } => {
            description.strip_prefix("windows error: ").unwrap_or(&description).to_owned()
        }
        trash::Error::CouldNotAccess { .. } => "cannot be reached".to_owned(),
        trash::Error::RestoreCollision { .. } => "already exists in its original folder".to_owned(),
        other => format!("{other:?}"),
    };
    Stop::Failed(format!("{name}: {text}"))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy();
        s.strip_prefix(r"\\?\").unwrap_or(&s).trim_end_matches('\\').to_lowercase()
    };
    norm(a) == norm(b)
}

pub(super) fn trash_paths(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    ctx.total_files += paths.len() as u64;
    let mut done = Vec::new();
    let result = delete_each(ctx, paths, &mut done);
    // One listing of the bin finds every item this job put there, also when it stopped early.
    if !done.is_empty()
        && let Ok(items) = os_limited::list()
    {
        for path in done {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { continue };
            let newest = items
                .iter()
                .filter(|i| i.name == name && same_dir(&i.original_parent, parent))
                .max_by_key(|i| i.time_deleted);
            if let Some(item) = newest {
                ctx.trashed.push((path, PathBuf::from(&item.id)));
            }
        }
    }
    result?;
    ctx.progress(true);
    Ok(())
}

fn delete_each(ctx: &mut Ctx, paths: &[PathBuf], done: &mut Vec<PathBuf>) -> R<()> {
    for path in paths {
        ctx.check_cancel()?;
        ctx.set_current(path);
        if path.parent().is_none() {
            return fail("Cannot move the root folder to Trash");
        }
        trash::delete(path).map_err(|e| failed(path, e))?;
        done.push(path.clone());
        ctx.finish_item();
    }
    Ok(())
}

pub(super) fn restore_paths(ctx: &mut Ctx, trashed: &[PathBuf]) -> R<()> {
    ctx.total_files += trashed.len() as u64;
    let items = os_limited::list().map_err(|e| failed(Path::new("Recycle Bin"), e))?;
    for path in trashed {
        ctx.check_cancel()?;
        ctx.set_current(path);
        let Some(item) = items.iter().find(|i| Path::new(&i.id) == path) else {
            return fail(format!("\"{}\" is not in the Recycle Bin", name_of(path)));
        };
        let dir = &item.original_parent;
        fs::create_dir_all(dir).at(dir)?;
        let is_dir = fs::symlink_metadata(path).at(path)?.is_dir();
        let name = free_name(dir, &item.name.to_string_lossy(), is_dir);
        let dest = dir.join(&name);
        let renamed = TrashItem { name: name.into(), ..item.clone() };
        os_limited::restore_all([renamed]).map_err(|e| failed(&item.original_path(), e))?;
        ctx.restored.push((path.clone(), dest.clone()));
        ctx.created.push(dest);
        ctx.finish_item();
    }
    ctx.progress(true);
    Ok(())
}

pub(super) fn empty_trash(ctx: &mut Ctx) -> R<()> {
    let items = os_limited::list().map_err(|e| failed(Path::new("Recycle Bin"), e))?;
    ctx.total_files = items.len() as u64;
    ctx.check_cancel()?;
    os_limited::purge_all(&items).map_err(|e| failed(Path::new("Recycle Bin"), e))?;
    ctx.done_files = ctx.total_files;
    ctx.progress(true);
    Ok(())
}
