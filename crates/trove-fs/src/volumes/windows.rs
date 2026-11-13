use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf, Prefix};
use std::time::Duration;

use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::SystemServices::FILE_READ_ONLY_VOLUME;
use windows_sys::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};

use super::{Usage, Volume, VolumeKind, run_tool};

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
}

fn system_drive() -> Option<char> {
    std::env::var("SystemDrive").ok()?.chars().next().map(|c| c.to_ascii_uppercase())
}

/// Every lettered drive. A remote drive is never asked for its label: on a dead server that
/// call blocks, while WNetGetConnectionW reads the redirector's own table.
pub fn volumes() -> Vec<Volume> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let sys = system_drive().unwrap_or('C');
    let mut out = Vec::new();
    for (i, letter) in ('A'..='Z').enumerate() {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{letter}:\\");
        let root_w = wide(OsStr::new(&root));
        // SAFETY: `root_w` is NUL-terminated.
        let kind = match unsafe { GetDriveTypeW(root_w.as_ptr()) } {
            DRIVE_FIXED | DRIVE_RAMDISK if letter == sys => VolumeKind::Root,
            DRIVE_FIXED | DRIVE_RAMDISK => VolumeKind::Internal,
            DRIVE_REMOVABLE | DRIVE_CDROM => VolumeKind::Removable,
            DRIVE_REMOTE => VolumeKind::Network,
            _ => continue,
        };
        let (label, fs_type, read_only, device) = if kind == VolumeKind::Network {
            let remote = remote_name(letter).unwrap_or_default();
            let share = remote.trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_owned();
            (share, String::new(), false, remote)
        } else {
            let mut name = [0u16; 261];
            let mut fs = [0u16; 261];
            let mut flags = 0u32;
            // SAFETY: both buffers are writable for the lengths passed; the other out pointers
            // may be null.
            let ok = unsafe {
                GetVolumeInformationW(
                    root_w.as_ptr(),
                    name.as_mut_ptr(),
                    name.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut flags,
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            };
            // An empty card reader or optical drive has a letter but no volume.
            if ok == 0 {
                continue;
            }
            (from_wide(&name), from_wide(&fs), flags & FILE_READ_ONLY_VOLUME != 0, root.clone())
        };
        let shown = match (label.is_empty(), kind) {
            (true, VolumeKind::Removable) => format!("Removable Disk ({letter}:)"),
            (true, _) => format!("Local Disk ({letter}:)"),
            (false, _) => format!("{label} ({letter}:)"),
        };
        out.push(Volume { mount_point: PathBuf::from(&root), device, fs_type, label: shown, kind, read_only });
    }
    out.sort_by(|a, b| (a.kind.rank(), &a.mount_point).cmp(&(b.kind.rank(), &b.mount_point)));
    out
}

fn remote_name(letter: char) -> Option<String> {
    let local = wide(OsStr::new(&format!("{letter}:")));
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `local` is NUL-terminated and `buf` holds `len` u16s.
    let rc = unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) };
    (rc == 0).then(|| from_wide(&buf))
}

fn drive_letter(path: &Path) -> Option<char> {
    match path.components().next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(char::from(d).to_ascii_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

/// A UNC path or a mapped network drive. Drive letters are never children of a folder, so no
/// name in `dir` is a mount point.
pub(crate) fn network_here(dir: &Path) -> (bool, HashSet<String>) {
    let unc = matches!(
        dir.components().next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    );
    let remote = unc
        || drive_letter(dir).is_some_and(|l| {
            let root = wide(OsStr::new(&format!("{l}:\\")));
            // SAFETY: `root` is NUL-terminated.
            unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
        });
    (remote, HashSet::new())
}

/// A drive is one device to a walk, so there is nothing to tell apart.
pub(crate) fn disks() -> Vec<(PathBuf, Option<String>)> {
    Vec::new()
}

pub(super) fn statvfs(path: &Path) -> Option<Usage> {
    let p = wide(path.as_os_str());
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `p` is NUL-terminated; the out pointers are valid and the last may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    (ok != 0).then_some(Usage { total, free })
}

/// New drives are announced through WM_DEVICECHANGE, which needs a window; the drive mask is
/// cheap enough to compare every two seconds instead.
pub(super) fn watch_loop(tx: &flume::Sender<Vec<Volume>>) {
    // SAFETY: no arguments.
    let mut mask = unsafe { GetLogicalDrives() };
    let mut last = volumes();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        // SAFETY: no arguments.
        let now_mask = unsafe { GetLogicalDrives() };
        if now_mask == mask {
            continue;
        }
        mask = now_mask;
        let now = volumes();
        if now != last {
            last = now.clone();
            if tx.send(now).is_err() {
                return;
            }
        }
    }
}

pub(super) fn run_unmount(v: &Volume) -> Result<(), String> {
    let letter = drive_letter(&v.mount_point).ok_or("Not a drive")?;
    let drive = format!("{letter}:");
    match v.kind {
        VolumeKind::Network => run_tool("net", &["use".as_ref(), drive.as_ref(), "/delete".as_ref(), "/y".as_ref()]),
        VolumeKind::Removable => {
            let script = format!(
                "(New-Object -ComObject Shell.Application).Namespace(17).ParseName('{drive}').InvokeVerb('Eject')"
            );
            run_tool("powershell", &["-NoProfile".as_ref(), "-Command".as_ref(), script.as_ref()])
        }
        _ => Err("This drive cannot be ejected".into()),
    }
}
