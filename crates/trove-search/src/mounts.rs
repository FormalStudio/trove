use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    pub point: PathBuf,
    pub fstype: String,
    pub source: String,
}

pub(crate) fn read_mounts() -> Vec<Mount> {
    std::fs::read("/proc/self/mountinfo").map(|b| parse_mountinfo(&b)).unwrap_or_default()
}

/// `36 35 98:0 /mnt1 /mnt/parent rw,noatime master:1 - ext3 /dev/root rw`: the mount point is
/// field 5, and the file system type and source follow the lone `-` that ends the optional
/// fields.
pub(crate) fn parse_mountinfo(b: &[u8]) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in b.split(|c| *c == b'\n') {
        let fields: Vec<&[u8]> = line.split(|c| *c == b' ').collect();
        if fields.len() < 7 {
            continue;
        }
        let Some(dash) = fields.iter().skip(6).position(|f| *f == b"-").map(|p| p + 6) else {
            continue;
        };
        let (Some(fstype), Some(source)) = (fields.get(dash + 1), fields.get(dash + 2)) else {
            continue;
        };
        out.push(Mount {
            point: PathBuf::from(OsStr::from_bytes(&unescape(fields[4]))),
            fstype: String::from_utf8_lossy(fstype).into_owned(),
            source: String::from_utf8_lossy(&unescape(source)).into_owned(),
        });
    }
    out
}

/// The kernel writes space, tab, newline and backslash in mount fields as `\ooo` octal.
fn unescape(f: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        if f[i] == b'\\' && i + 4 <= f.len() {
            let oct = &f[i + 1..i + 4];
            if oct.iter().all(|c| (b'0'..=b'7').contains(c)) {
                let v = oct.iter().fold(0u32, |a, c| a * 8 + u32::from(c - b'0'));
                if let Ok(v) = u8::try_from(v) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(f[i]);
        i += 1;
    }
    out
}

/// The mount a path lives on: the longest mount point that is a prefix of it, the later of two
/// equal ones, since a later mount covers an earlier one at the same place.
pub(crate) fn mount_of<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    let mut best: Option<&Mount> = None;
    for m in mounts {
        if path.starts_with(&m.point) && best.is_none_or(|b| m.point.as_os_str().len() >= b.point.as_os_str().len()) {
            best = Some(m);
        }
    }
    best
}

/// Network and FUSE file systems: every stat can wait on a server or a user-space daemon.
pub(crate) fn is_remote(fstype: &str) -> bool {
    fstype.starts_with("fuse.")
        || matches!(
            fstype,
            "nfs"
                | "nfs4"
                | "cifs"
                | "smb3"
                | "smbfs"
                | "ncpfs"
                | "afs"
                | "9p"
                | "ceph"
                | "glusterfs"
                | "lustre"
                | "davfs"
                | "fuse"
                | "sshfs"
                | "gvfs"
        )
}

pub(crate) fn is_pseudo(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "cgroup"
            | "cgroup2"
            | "debugfs"
            | "tracefs"
            | "securityfs"
            | "pstore"
            | "bpf"
            | "configfs"
            | "fusectl"
            | "mqueue"
            | "hugetlbfs"
            | "autofs"
            | "binfmt_misc"
            | "efivarfs"
            | "nsfs"
            | "rpc_pipefs"
            | "selinuxfs"
    )
}

pub(crate) fn is_slow(m: &Mount) -> bool {
    is_remote(&m.fstype) || is_pseudo(&m.fstype)
}

/// Mount points under `root` a walk must not enter: anything on another device (a USB stick
/// mounted in the home directory) and any network or pseudo file system. A btrfs subvolume
/// mounted from the root's own device is the same file system and stays.
pub(crate) fn skip_points(root: &Path, mounts: &[Mount]) -> HashSet<PathBuf> {
    let source = mount_of(root, mounts).map(|m| m.source.as_str());
    mounts
        .iter()
        .filter(|m| m.point != root && m.point.starts_with(root))
        .filter(|m| is_slow(m) || Some(m.source.as_str()) != source)
        .map(|m| m.point.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &[u8] = b"\
22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,subvol=/@
23 22 0:21 /@/home /home rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw,subvol=/@/home
24 22 0:5 / /proc rw,nosuid shared:3 - proc proc rw
25 23 8:17 / /home/u/USB\\040Stick rw shared:4 - vfat /dev/sdb1 rw
26 23 0:50 / /home/u/nas rw shared:5 - nfs4 nas:/export rw
27 23 0:51 / /home/u/remote rw shared:6 - fuse.sshfs u@h:/ rw,user_id=1000
28 23 0:21 /@/data /home/u/data rw shared:7 master:9 - btrfs /dev/nvme0n1p2 rw,subvol=/@/data
";

    #[test]
    fn parses_and_classifies() {
        let m = parse_mountinfo(INFO);
        assert_eq!(m.len(), 7);
        assert_eq!(m[3].point, Path::new("/home/u/USB Stick"));
        assert_eq!(m[6].fstype, "btrfs");
        assert_eq!(mount_of(Path::new("/home/u/x"), &m).map(|m| m.point.as_path()), Some(Path::new("/home")));
        assert_eq!(mount_of(Path::new("/proc/1"), &m).map(|m| m.fstype.as_str()), Some("proc"));
        let skip = skip_points(Path::new("/home/u"), &m);
        assert!(skip.contains(Path::new("/home/u/USB Stick")));
        assert!(skip.contains(Path::new("/home/u/nas")));
        assert!(skip.contains(Path::new("/home/u/remote")));
        assert!(!skip.contains(Path::new("/home/u/data")));
        assert_eq!(skip.len(), 3);
        assert!(is_remote("fuse.sshfs") && is_remote("nfs4") && !is_remote("ext4"));
        assert!(is_pseudo("proc") && !is_pseudo("tmpfs"));
    }
}
