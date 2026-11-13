use std::collections::HashSet;
use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{MISSING, Usage, Volume, VolumeKind, is_network_path, run_tool};

const NETWORK_FS: &[&str] = &["smbfs", "nfs", "afpfs", "webdav", "cifs", "ftp", "macfuse", "osxfuse", "osxfusefs"];

struct Mount {
    point: PathBuf,
    fs_type: String,
    source: String,
    flags: u32,
}

/// The mount table as `getmntinfo` caches it; MNT_NOWAIT never asks a file system, so a dead
/// server cannot stall it.
fn mounts() -> Vec<Mount> {
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: `buf` is a valid out pointer; the kernel points it at a buffer libc owns.
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return Vec::new();
    }
    // SAFETY: getmntinfo returned `n` initialised entries at `buf`, valid until the next call on
    // this thread.
    let all = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    all.iter()
        .map(|s| {
            // SAFETY: the name fields are NUL-terminated C strings inside the struct.
            let text = |f: &[libc::c_char]| unsafe { CStr::from_ptr(f.as_ptr()) };
            Mount {
                point: PathBuf::from(OsStr::from_bytes(text(&s.f_mntonname).to_bytes())),
                fs_type: text(&s.f_fstypename).to_string_lossy().into_owned(),
                source: text(&s.f_mntfromname).to_string_lossy().into_owned(),
                flags: s.f_flags,
            }
        })
        .collect()
}

/// `/dev/disk3s1s1` -> `disk3`: volumes of one APFS container share the whole-disk name.
fn whole_disk(source: &str) -> Option<&str> {
    let name = source.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("disk")?;
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    (end > 0).then(|| &name[..4 + end])
}

fn build_volumes(mounts: &[Mount]) -> Vec<Volume> {
    let boot = mounts.iter().find(|m| m.point == Path::new("/")).and_then(|m| whole_disk(&m.source));
    let mut out: Vec<Volume> = mounts
        .iter()
        .filter_map(|m| {
            let kind = if m.point == Path::new("/") {
                VolumeKind::Root
            } else if m.flags & libc::MNT_DONTBROWSE as u32 != 0 || m.fs_type == "autofs" || m.fs_type == "devfs" {
                return None;
            } else if NETWORK_FS.contains(&m.fs_type.as_str()) {
                VolumeKind::Network
            } else if boot.is_some() && whole_disk(&m.source) == boot {
                VolumeKind::Internal
            } else {
                VolumeKind::Removable
            };
            let label = match kind {
                VolumeKind::Root => "Computer".to_owned(),
                _ => m.point.file_name().map_or_else(|| m.source.clone(), |n| n.to_string_lossy().into_owned()),
            };
            // The sealed system volume is read-only, but everything a user writes lands on the
            // Data volume behind it through firmlinks.
            let read_only = kind != VolumeKind::Root && m.flags & libc::MNT_RDONLY as u32 != 0;
            Some(Volume {
                mount_point: m.point.clone(),
                device: m.source.clone(),
                fs_type: m.fs_type.clone(),
                label,
                kind,
                read_only,
            })
        })
        .collect();
    out.sort_by(|a, b| (a.kind.rank(), &a.mount_point).cmp(&(b.kind.rank(), &b.mount_point)));
    out
}

pub fn volumes() -> Vec<Volume> {
    build_volumes(&mounts())
}

pub(crate) fn network_here(dir: &Path) -> (bool, HashSet<String>) {
    let vols = volumes();
    let mounts = vols
        .iter()
        .filter(|v| v.kind == VolumeKind::Network && v.mount_point.parent() == Some(dir))
        .filter_map(|v| Some(v.mount_point.file_name()?.to_string_lossy().into_owned()))
        .collect();
    (is_network_path(dir, &vols), mounts)
}

#[allow(clippy::unnecessary_cast)]
pub(super) fn statvfs(path: &Path) -> Option<Usage> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is NUL-terminated and `st` is a valid out pointer for the call.
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so the struct is initialised.
    let st = unsafe { st.assume_init() };
    let unit = if st.f_frsize > 0 { st.f_frsize } else { st.f_bsize } as u64;
    Some(Usage { total: st.f_blocks as u64 * unit, free: st.f_bavail as u64 * unit })
}

/// Blocks on a kqueue `EVFILT_FS` filter, which fires on every mount, unmount and volume change.
pub(super) fn watch_loop(tx: &flume::Sender<Vec<Volume>>) {
    // SAFETY: kqueue takes no arguments; a negative result is an error.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return;
    }
    let change = libc::kevent {
        ident: 0,
        filter: libc::EVFILT_FS,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let mut last = volumes();
    // SAFETY: one valid change, no event buffer, no timeout.
    if unsafe { libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
        // SAFETY: `kq` is a descriptor this function opened.
        unsafe { libc::close(kq) };
        return;
    }
    loop {
        // SAFETY: a zeroed kevent is a valid out value.
        let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: no changes, one event slot, no timeout.
        let r = unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut ev, 1, std::ptr::null()) };
        if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break;
        }
        // A mount is several events in a burst; let it settle.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = volumes();
        if now != last {
            last = now.clone();
            if tx.send(now).is_err() {
                break;
            }
        }
    }
    // SAFETY: `kq` is a descriptor this function opened.
    unsafe { libc::close(kq) };
}

pub(super) fn run_unmount(v: &Volume) -> Result<(), String> {
    if v.kind == VolumeKind::Root {
        return Err("The system volume cannot be unmounted".into());
    }
    let mp = v.mount_point.as_os_str();
    match run_tool("diskutil", &["unmount".as_ref(), mp]) {
        Err(e) if e.starts_with(MISSING) => run_tool("umount", &[mp]),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(point: &str, fs_type: &str, source: &str, flags: u32) -> Mount {
        Mount { point: point.into(), fs_type: fs_type.into(), source: source.into(), flags }
    }

    #[test]
    fn root_volumes_shares_and_noise() {
        let nobrowse = libc::MNT_DONTBROWSE as u32;
        let ro = libc::MNT_RDONLY as u32;
        let m = [
            mount("/", "apfs", "/dev/disk3s1s1", ro),
            mount("/dev", "devfs", "devfs", nobrowse),
            mount("/System/Volumes/Data", "apfs", "/dev/disk3s5", nobrowse),
            mount("/System/Volumes/Data/home", "autofs", "map auto_home", nobrowse),
            mount("/Volumes/Kyan Drive", "apfs", "/dev/disk5s1", 0),
            mount("/Volumes/Spare", "apfs", "/dev/disk3s7", 0),
            mount("/Volumes/share", "smbfs", "//kyan@nas/share", 0),
            mount("/Volumes/Installer", "hfs", "/dev/disk9s1", ro),
        ];
        let v = build_volumes(&m);
        let got: Vec<_> = v.iter().map(|v| (v.mount_point.to_str().unwrap_or_default(), v.kind)).collect();
        assert_eq!(
            got,
            [
                ("/", VolumeKind::Root),
                ("/Volumes/Spare", VolumeKind::Internal),
                ("/Volumes/Installer", VolumeKind::Removable),
                ("/Volumes/Kyan Drive", VolumeKind::Removable),
                ("/Volumes/share", VolumeKind::Network),
            ]
        );
        assert_eq!(v[0].label, "Computer");
        assert!(!v[0].read_only && v[2].read_only);
        assert_eq!(v[3].label, "Kyan Drive");
        assert!(is_network_path(Path::new("/Volumes/share/docs"), &v));
        assert!(!is_network_path(Path::new("/Users/kyan"), &v));
    }

    #[test]
    fn whole_disk_names() {
        assert_eq!(whole_disk("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(whole_disk("/dev/disk12"), Some("disk12"));
        assert_eq!(whole_disk("map auto_home"), None);
    }

    #[test]
    fn live_volumes_have_root() {
        assert_eq!(volumes().first().map(|v| v.kind), Some(VolumeKind::Root));
    }
}
