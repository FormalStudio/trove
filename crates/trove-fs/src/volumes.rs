//! Mounted volumes: what to show in the sidebar, what each has free, and unmounting.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(windows)]
use windows as sys;

pub(crate) use sys::network_here;
pub use sys::volumes;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VolumeKind {
    Root,
    Internal,
    Removable,
    Network,
    Phone,
    Gvfs,
}

impl VolumeKind {
    fn rank(self) -> u8 {
        match self {
            Self::Root => 0,
            Self::Internal => 1,
            Self::Removable => 2,
            Self::Network => 3,
            Self::Phone => 4,
            Self::Gvfs => 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub mount_point: PathBuf,
    pub device: String,
    pub fs_type: String,
    pub label: String,
    pub kind: VolumeKind,
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub total: u64,
    pub free: u64,
}

pub fn is_network_path(path: &Path, vols: &[Volume]) -> bool {
    if is_gvfs_path(path) {
        return true;
    }
    vols.iter()
        .filter(|v| path.starts_with(&v.mount_point))
        .max_by_key(|v| v.mount_point.components().count())
        .is_some_and(|v| matches!(v.kind, VolumeKind::Network | VolumeKind::Phone | VolumeKind::Gvfs))
}

/// mountinfo shows only the gvfsd-fuse mount itself, never the locations under it.
fn is_gvfs_path(path: &Path) -> bool {
    let mut c = path.components().map(|c| c.as_os_str());
    matches!(
        (c.next(), c.next(), c.next(), c.next(), c.next(), c.next()),
        (Some(root), Some(run), Some(user), Some(_), Some(gvfs), Some(_))
            if root == "/" && run == "run" && user == "user" && gvfs == "gvfs"
    )
}

pub fn usage(path: PathBuf, timeout: Duration) -> flume::Receiver<Option<Usage>> {
    let (tx, rx) = flume::bounded(1);
    let (done_tx, done_rx) = flume::bounded(1);
    // statvfs on a dead server never returns, so it gets a thread that is simply left behind.
    std::thread::spawn(move || {
        let _ = done_tx.send(sys::statvfs(&path));
    });
    std::thread::spawn(move || {
        let _ = tx.send(done_rx.recv_timeout(timeout).ok().flatten());
    });
    rx
}

pub fn watch_volumes() -> flume::Receiver<Vec<Volume>> {
    let (tx, rx) = flume::unbounded();
    let _ = std::thread::Builder::new().name("trove-volumes".into()).spawn(move || sys::watch_loop(&tx));
    rx
}

pub fn unmount(v: &Volume) -> flume::Receiver<Result<(), String>> {
    let (tx, rx) = flume::bounded(1);
    let v = v.clone();
    std::thread::spawn(move || {
        let _ = tx.send(sys::run_unmount(&v));
    });
    rx
}

const MISSING: &str = "Not installed: ";

fn run_tool(tool: &str, args: &[&OsStr]) -> Result<(), String> {
    match Command::new(tool).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() { format!("{tool} exited with {}", out.status) } else { err })
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Err(format!("{MISSING}{tool}")),
        Err(e) => Err(format!("{tool}: {e}")),
    }
}
