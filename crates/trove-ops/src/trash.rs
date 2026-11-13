//! Moving to the trash, restoring and emptying. With an explicit `data_home` (tests, the demo)
//! Trove writes the freedesktop.org layout itself, on every OS; the configuration `from_env`
//! builds on macOS and Windows hands the work to the system trash instead.

mod freedesktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;

use std::path::PathBuf;

use crate::job::{Ctx, R};

pub const NO_VOLUME_TRASH: &str = "Cannot move to Trash on this volume";

#[derive(Clone, Debug)]
pub struct TrashConfig {
    /// `$XDG_DATA_HOME`; the home trash is `Trash` inside it.
    pub data_home: PathBuf,
    pub uid: u32,
    /// Mount points to look for volume trashes in. `None` reads `/proc/self/mountinfo`.
    pub mounts: Option<Vec<PathBuf>>,
    /// Use the system trash (the macOS Trash, the Windows Recycle Bin) rather than the
    /// freedesktop.org layout under `data_home`. Only `from_env` on macOS and Windows sets it;
    /// Linux has no other trash.
    pub native: bool,
}

impl TrashConfig {
    pub fn from_env() -> Self {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| crate::sys::home().map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let native = cfg!(any(target_os = "macos", windows));
        Self { data_home, uid: crate::sys::uid(), mounts: None, native }
    }

    pub fn home_trash(&self) -> PathBuf {
        #[cfg(any(target_os = "macos", windows))]
        if self.native {
            return native::home_trash();
        }
        self.data_home.join("Trash")
    }

    /// Every trash directory that exists: the home one and each volume's.
    pub fn all_trashes(&self) -> Vec<PathBuf> {
        #[cfg(any(target_os = "macos", windows))]
        if self.native {
            return native::all_trashes(self);
        }
        freedesktop::all_trashes(self)
    }
}

pub(crate) fn trash_paths(ctx: &mut Ctx, paths: &[PathBuf]) -> R<()> {
    #[cfg(any(target_os = "macos", windows))]
    if ctx.trash.native {
        return native::trash_paths(ctx, paths);
    }
    freedesktop::trash_paths(ctx, paths)
}

pub(crate) fn restore_paths(ctx: &mut Ctx, trashed: &[PathBuf]) -> R<()> {
    #[cfg(any(target_os = "macos", windows))]
    if ctx.trash.native {
        return native::restore_paths(ctx, trashed);
    }
    freedesktop::restore_paths(ctx, trashed)
}

pub(crate) fn empty_trash(ctx: &mut Ctx) -> R<()> {
    #[cfg(any(target_os = "macos", windows))]
    if ctx.trash.native {
        return native::empty_trash(ctx);
    }
    freedesktop::empty_trash(ctx)
}
