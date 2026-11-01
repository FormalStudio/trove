use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const MOUNTINFO: &str = "/proc/self/mountinfo";
const BY_LABEL: &str = "/dev/disk/by-label";

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

/// One line of mountinfo, escapes decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountInfo {
    pub root: PathBuf,
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub source: String,
    pub read_only: bool,
}

/// Filtered mounts a user cares about. Reads procfs and `/dev/disk/by-label` link names only.
pub fn volumes() -> Vec<Volume> {
    let text = std::fs::read_to_string(MOUNTINFO).unwrap_or_default();
    let mounts = parse_mountinfo(&text);
    let mut labels = HashMap::new();
    if mounts.iter().any(|m| m.source.starts_with("/dev/")) {
        labels = read_labels(Path::new(BY_LABEL));
        // by-label links point at dm-N, while a mounted LUKS volume is named /dev/mapper/<name>.
        for m in mounts.iter().filter(|m| m.source.starts_with("/dev/mapper/")) {
            let target = std::fs::read_link(&m.source)
                .ok()
                .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()));
            if let (Some(label), Some(name)) =
                (target.and_then(|t| labels.get(&t).cloned()), Path::new(&m.source).file_name())
            {
                labels.insert(name.to_string_lossy().into_owned(), label);
            }
        }
    }
    // SAFETY: getuid has no preconditions.
    build_volumes(&mounts, &labels, unsafe { libc::getuid() })
}

pub(crate) fn path_is_network(path: &Path) -> bool {
    let text = std::fs::read_to_string(MOUNTINFO).unwrap_or_default();
    // SAFETY: getuid has no preconditions.
    let vols = build_volumes(&parse_mountinfo(&text), &HashMap::new(), unsafe { libc::getuid() });
    is_network_path(path, &vols)
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
        let _ = done_tx.send(statvfs(&path));
    });
    std::thread::spawn(move || {
        let _ = tx.send(done_rx.recv_timeout(timeout).ok().flatten());
    });
    rx
}

#[allow(clippy::unnecessary_cast)]
fn statvfs(path: &Path) -> Option<Usage> {
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

pub fn watch_volumes() -> flume::Receiver<Vec<Volume>> {
    let (tx, rx) = flume::unbounded();
    let _ = std::thread::Builder::new().name("trove-volumes".into()).spawn(move || watch_loop(&tx));
    rx
}

fn watch_loop(tx: &flume::Sender<Vec<Volume>>) {
    let Ok(mut file) = File::open(MOUNTINFO) else { return };
    let mut last = volumes();
    let mut buf = String::new();
    // The kernel arms POLLPRI against the state seen by the last read from offset 0.
    let _ = file.read_to_string(&mut buf);
    loop {
        let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLPRI | libc::POLLERR, revents: 0 };
        // SAFETY: one valid pollfd, count 1.
        let r = unsafe { libc::poll(&mut pfd, 1, -1) };
        if r < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        // A mount or unmount is several events in a burst; let it settle.
        std::thread::sleep(Duration::from_millis(50));
        buf.clear();
        if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut buf).is_err() {
            return;
        }
        let now = volumes();
        if now != last {
            last = now.clone();
            if tx.send(now).is_err() {
                return;
            }
        }
    }
}

pub fn unmount(v: &Volume) -> flume::Receiver<Result<(), String>> {
    let (tx, rx) = flume::bounded(1);
    let v = v.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run_unmount(&v));
    });
    rx
}

const MISSING: &str = "Not installed: ";

fn run_unmount(v: &Volume) -> Result<(), String> {
    if v.kind == VolumeKind::Root {
        return Err("The system volume cannot be unmounted".into());
    }
    let mp = v.mount_point.as_os_str();
    if v.device.starts_with("/dev/") {
        return run_tool("udisksctl", &["unmount".as_ref(), "-b".as_ref(), v.device.as_ref()]);
    }
    if matches!(v.kind, VolumeKind::Gvfs | VolumeKind::Phone) {
        return run_tool("gio", &["mount".as_ref(), "-u".as_ref(), mp]);
    }
    if v.fs_type.starts_with("fuse") {
        return match run_tool("fusermount3", &["-u".as_ref(), mp]) {
            Err(e) if e.starts_with(MISSING) => run_tool("fusermount", &["-u".as_ref(), mp]),
            r => r,
        };
    }
    run_tool("umount", &[mp])
}

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

pub(crate) fn parse_mountinfo(text: &str) -> Vec<MountInfo> {
    text.lines().filter_map(parse_mountinfo_line).collect()
}

fn parse_mountinfo_line(line: &str) -> Option<MountInfo> {
    let mut f = line.split(' ');
    let (_id, _parent, _dev) = (f.next()?, f.next()?, f.next()?);
    let root = unescape(f.next()?);
    let mount_point = unescape(f.next()?);
    let opts = f.next()?;
    // Optional fields (shared:N, master:N) run up to a lone "-".
    f.by_ref().find(|s| *s == "-")?;
    let fs_type = f.next()?.to_string();
    let source = unescape(f.next()?).to_string_lossy().into_owned();
    let super_opts = f.next().unwrap_or("");
    let read_only = opts.split(',').any(|o| o == "ro") || super_opts.split(',').any(|o| o == "ro");
    Some(MountInfo { root: root.into(), mount_point: mount_point.into(), fs_type, source, read_only })
}

/// mountinfo writes space, tab, newline and backslash as `\040`, `\011`, `\012`, `\134`.
fn unescape(s: &str) -> OsString {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    OsString::from_vec(out)
}

const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "sshfs",
    "fuse.sshfs",
    "fuse.rclone",
    "davfs",
    "fuse.davfs",
    "fuse.s3fs",
    "9p",
    "ceph",
    "glusterfs",
    "fuse.glusterfs",
];

const NOISE_FS: &[&str] = &[
    "tmpfs",
    "ramfs",
    "overlay",
    "squashfs",
    "nsfs",
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "autofs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
    "fusectl",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "efivarfs",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "rpc_pipefs",
    "selinuxfs",
    "fuse.lxcfs",
];

const BLOCK_FS: &[&str] = &[
    "ext2",
    "ext3",
    "ext4",
    "btrfs",
    "xfs",
    "f2fs",
    "jfs",
    "reiserfs",
    "ntfs",
    "ntfs3",
    "fuseblk",
    "fuse.ntfs-3g",
    "vfat",
    "exfat",
    "udf",
    "iso9660",
    "hfsplus",
    "bcachefs",
    "zfs",
];

const NOISE_ROOTS: &[&str] = &[
    "/boot",
    "/efi",
    "/var",
    "/snap",
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/tmp",
    "/usr",
    "/etc",
    "/nix",
    "/swap",
    "/.snapshots",
];

fn under(path: &Path, root: &str) -> bool {
    path.starts_with(root)
}

fn child_name<'a>(path: &'a Path, root: &Path) -> Option<&'a OsStr> {
    path.strip_prefix(root).ok()?.components().next().map(|c| c.as_os_str())
}

fn gvfs_kind(mp: &Path, uid: u32) -> Option<VolumeKind> {
    let name = child_name(mp, &PathBuf::from(format!("/run/user/{uid}/gvfs")))?.to_string_lossy();
    let phone = ["mtp:", "gphoto2:", "afc:"].iter().any(|p| name.starts_with(p));
    Some(if phone { VolumeKind::Phone } else { VolumeKind::Gvfs })
}

fn classify(m: &MountInfo, uid: u32) -> Option<VolumeKind> {
    let (mp, fs) = (m.mount_point.as_path(), m.fs_type.as_str());
    if NETWORK_FS.contains(&fs) || fs.starts_with("nfs") {
        return Some(VolumeKind::Network);
    }
    if let Some(kind) = gvfs_kind(mp, uid) {
        return Some(kind);
    }
    if NOISE_FS.contains(&fs) {
        return None;
    }
    if child_name(mp, Path::new("/run/media")).is_some_and(|_| mp.components().count() >= 5) {
        return Some(VolumeKind::Removable);
    }
    if NOISE_ROOTS.iter().any(|r| under(mp, r)) {
        return None;
    }
    if child_name(mp, Path::new("/media")).is_some() {
        return Some(VolumeKind::Removable);
    }
    if child_name(mp, Path::new("/mnt")).is_some() {
        return Some(VolumeKind::Internal);
    }
    let real = BLOCK_FS.contains(&fs) && (m.source.starts_with("/dev/") || mp == Path::new("/home"));
    real.then_some(VolumeKind::Internal)
}

pub(crate) fn build_volumes(mounts: &[MountInfo], labels: &HashMap<String, String>, uid: u32) -> Vec<Volume> {
    let root = mounts.iter().rposition(|m| m.mount_point == Path::new("/"));
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for (i, m) in mounts.iter().enumerate() {
        let kind = if Some(i) == root {
            VolumeKind::Root
        } else if m.mount_point == Path::new("/") {
            continue;
        } else {
            let Some(kind) = classify(m, uid) else { continue };
            kind
        };
        // A btrfs subvolume or bind mount of a device already shown is the same storage.
        if m.source.starts_with('/') && !seen.insert(&m.source) && kind != VolumeKind::Root {
            continue;
        }
        out.push(Volume {
            label: label_of(m, kind, labels),
            mount_point: m.mount_point.clone(),
            device: m.source.clone(),
            fs_type: m.fs_type.clone(),
            kind,
            read_only: m.read_only,
        });
    }
    out.sort_by(|a, b| (a.kind.rank(), &a.mount_point).cmp(&(b.kind.rank(), &b.mount_point)));
    out
}

fn label_of(m: &MountInfo, kind: VolumeKind, labels: &HashMap<String, String>) -> String {
    if kind == VolumeKind::Root {
        return "Computer".into();
    }
    let dev = Path::new(&m.source).file_name().map(|n| n.to_string_lossy().into_owned());
    if let Some(label) = dev.and_then(|d| labels.get(&d)) {
        return label.clone();
    }
    match m.mount_point.file_name() {
        Some(n) => n.to_string_lossy().trim_start_matches('.').to_string(),
        None => m.mount_point.to_string_lossy().into_owned(),
    }
}

/// by-label entries are links named by label (udev escapes as `\xNN`) pointing at the device.
fn read_labels(dir: &Path) -> HashMap<String, String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return HashMap::new() };
    rd.flatten()
        .filter_map(|e| {
            let target = std::fs::read_link(e.path()).ok()?;
            let dev = target.file_name()?.to_string_lossy().into_owned();
            Some((dev, unescape_udev(&e.file_name().to_string_lossy())))
        })
        .collect()
}

fn unescape_udev(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'\\' && b.get(i + 1) == Some(&b'x') && i + 4 <= b.len())
            .then(|| std::str::from_utf8(&b[i + 2..i + 4]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
            .flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "\
47 1 0:40 /@tumbleweed / rw,noatime shared:1 - btrfs /dev/nvme0n1p5 rw,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=263,subvol=/@tumbleweed
42 47 0:9 / /dev rw,nosuid shared:2 - devtmpfs devtmpfs rw,size=15843176k,nr_inodes=3960794,mode=755,inode64
56 47 0:27 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
57 47 0:31 / /run rw,nosuid,nodev shared:13 - tmpfs tmpfs rw,size=6441080k,nr_inodes=819200,mode=755,inode64
31 47 0:41 / /mnt/steamlib rw,relatime shared:16 - autofs systemd-1 rw,fd=60,pgrp=1,timeout=0,minproto=5,maxproto=5,direct,pipe_ino=12087
60 47 0:48 /@swap /swap rw,noatime shared:49 - btrfs /dev/nvme0n1p5 rw,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=259,subvol=/@swap
62 47 0:49 /@home /home rw,noatime shared:47 - btrfs /dev/nvme0n1p5 rw,compress=zstd:1,ssd,discard=async,space_cache=v2,subvolid=258,subvol=/@home
66 47 0:53 / /tmp rw,nosuid,nodev shared:93 - tmpfs tmpfs rw,nr_inodes=1048576,inode64,usrquota
72 47 259:1 / /boot/efi rw,relatime shared:99 - vfat /dev/nvme0n1p1 rw,fmask=0077,dmask=0077,codepage=437,iocharset=iso8859-1,shortname=winnt,errors=remount-ro
69 47 259:6 / /mnt/games rw,noatime shared:96 - ntfs3 /dev/nvme0n1p6 rw,uid=1000,gid=100,dmask=0022,fmask=0022,windows_names,acl,iocharset=utf8,prealloc,ads
99 47 259:3 / /mnt/windows ro,noatime shared:104 - ntfs3 /dev/nvme0n1p3 rw,uid=1000,gid=100
612 57 0:83 / /run/user/1000 rw,nosuid,nodev,relatime shared:603 - tmpfs tmpfs rw,size=3220540k,nr_inodes=805135,mode=700,uid=1000,gid=100,inode64
1023 612 0:109 / /run/user/1000/doc rw,nosuid,nodev,relatime shared:1238 - fuse.portal portal rw,user_id=1000,group_id=100
1049 612 0:121 / /run/user/1000/gvfs rw,nosuid,nodev,relatime shared:1291 - fuse.gvfsd-fuse gvfsd-fuse rw,user_id=1000,group_id=100
32 62 0:58 / /home/kyandesutter/.macbook rw,nosuid,nodev,relatime shared:140 - fuse.sshfs macbook: rw,user_id=1000,group_id=100
211 66 0:59 / /tmp/.mount_t3codeMAnAhn ro,nosuid,nodev,relatime shared:146 - fuse.t3code-desktop t3code-desktop ro,user_id=1000,group_id=100
35 31 8:2 / /mnt/steamlib rw,noatime shared:704 - ntfs3 /dev/sda2 rw,uid=1000,gid=100,dmask=0022,fmask=0022,windows_names,acl,iocharset=utf8,prealloc,ads
";

    fn labels() -> HashMap<String, String> {
        [("nvme0n1p6", "Games"), ("sda2", "SteamLib")].map(|(k, v)| (k.to_string(), v.to_string())).into()
    }

    fn mount_points(v: &[Volume]) -> Vec<&str> {
        v.iter().map(|v| v.mount_point.to_str().unwrap_or_default()).collect()
    }

    #[test]
    fn parses_fields_and_escapes() {
        let m = parse_mountinfo(
            "98 47 8:17 /sub\\040dir /run/media/kyan/My\\040Stick rw,nosuid,relatime shared:5 master:2 - vfat /dev/sdb1 ro,fmask=0022\n",
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].mount_point, Path::new("/run/media/kyan/My Stick"));
        assert_eq!(m[0].root, Path::new("/sub dir"));
        assert_eq!(m[0].fs_type, "vfat");
        assert_eq!(m[0].source, "/dev/sdb1");
        assert!(m[0].read_only);
        assert_eq!(unescape("a\\134b\\011c"), OsString::from("a\\b\tc"));
        assert!(parse_mountinfo("garbage\n\n").is_empty());
    }

    #[test]
    fn filters_system_noise() {
        let v = build_volumes(&parse_mountinfo(REAL), &labels(), 1000);
        assert_eq!(
            mount_points(&v),
            ["/", "/mnt/games", "/mnt/steamlib", "/mnt/windows", "/home/kyandesutter/.macbook"]
        );
        assert_eq!(v[0].kind, VolumeKind::Root);
        assert_eq!(v[0].label, "Computer");
        assert_eq!(v[1].label, "Games");
        assert_eq!(v[1].kind, VolumeKind::Internal);
        assert_eq!(v[2].device, "/dev/sda2");
        assert_eq!(v[2].label, "SteamLib");
        assert_eq!(v[3].label, "windows");
        assert!(v[3].read_only);
        assert_eq!(v[4].kind, VolumeKind::Network);
        assert_eq!(v[4].label, "macbook");
    }

    #[test]
    fn removable_gvfs_and_separate_home() {
        let text = "\
1 0 8:1 / / rw - ext4 /dev/sda1 rw
2 1 8:5 / /home rw - ext4 /dev/sda5 rw
3 1 8:17 / /run/media/kyan/USB rw - vfat /dev/sdb1 rw
4 1 0:90 / /run/media/kyan rw - tmpfs tmpfs rw
5 1 0:91 / /var/lib/docker/overlay2/x rw - overlay overlay rw
6 1 0:92 / /srv/share rw - nfs4 server:/export rw
7 1 0:93 / /run/user/1000/gvfs/mtp:host=Pixel rw - fuse.gvfsd-fuse gvfsd-fuse rw
8 1 0:94 / /run/user/1000/gvfs/smb-share:server=nas rw - fuse.gvfsd-fuse gvfsd-fuse rw
9 1 0:95 / /run/user/1001/gvfs/smb-share:server=nas rw - fuse.gvfsd-fuse gvfsd-fuse rw
10 1 8:18 / /media/data rw - exfat /dev/sdc1 rw
11 1 8:19 / /snap/core/1 ro - squashfs /dev/loop1 ro
";
        let labels: HashMap<String, String> = [("sdb1".to_string(), "Holiday Photos".to_string())].into();
        let v = build_volumes(&parse_mountinfo(text), &labels, 1000);
        let kinds: Vec<_> = v.iter().map(|v| (v.mount_point.to_str().unwrap_or_default(), v.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("/", VolumeKind::Root),
                ("/home", VolumeKind::Internal),
                ("/media/data", VolumeKind::Removable),
                ("/run/media/kyan/USB", VolumeKind::Removable),
                ("/srv/share", VolumeKind::Network),
                ("/run/user/1000/gvfs/mtp:host=Pixel", VolumeKind::Phone),
                ("/run/user/1000/gvfs/smb-share:server=nas", VolumeKind::Gvfs),
            ]
        );
        let usb = v.iter().find(|v| v.device == "/dev/sdb1");
        assert_eq!(usb.map(|v| v.label.as_str()), Some("Holiday Photos"));
    }

    #[test]
    fn network_paths() {
        let v = build_volumes(&parse_mountinfo(REAL), &labels(), 1000);
        assert!(is_network_path(Path::new("/home/kyandesutter/.macbook/docs"), &v));
        assert!(!is_network_path(Path::new("/home/kyandesutter/Documents"), &v));
        assert!(!is_network_path(Path::new("/mnt/games/x"), &v));
        assert!(is_network_path(Path::new("/run/user/1000/gvfs/sftp:host=a/b"), &v));
        assert!(!is_network_path(Path::new("/run/user/1000/gvfs"), &v));
    }

    #[test]
    fn udev_label_escapes() {
        assert_eq!(unescape_udev("My\\x20Disk\\x2fB"), "My Disk/B");
        assert_eq!(unescape_udev("plain"), "plain");
    }

    #[test]
    fn reads_labels_from_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink("../../sdz9", dir.path().join("Data\\x20Disk")).expect("symlink");
        let m = read_labels(dir.path());
        assert_eq!(m.get("sdz9").map(String::as_str), Some("Data Disk"));
    }

    #[test]
    fn live_volumes_have_root() {
        let v = volumes();
        assert_eq!(v.first().map(|v| v.kind), Some(VolumeKind::Root));
    }

    #[test]
    fn usage_of_tempdir_and_missing_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let u = usage(dir.path().to_path_buf(), Duration::from_secs(5)).recv().expect("reply");
        let u = u.expect("usage");
        assert!(u.total > 0 && u.free <= u.total);
        let none = usage(dir.path().join("missing"), Duration::from_secs(5)).recv().expect("reply");
        assert!(none.is_none());
    }

    #[test]
    fn unmount_refuses_root_and_reports_missing_tools() {
        let v = build_volumes(&parse_mountinfo(REAL), &labels(), 1000);
        let err = unmount(&v[0]).recv().expect("reply").expect_err("root");
        assert!(err.contains("cannot be unmounted"));
        let ghost = Volume {
            mount_point: "/nonexistent-mount".into(),
            device: String::new(),
            fs_type: "ext4".into(),
            label: String::new(),
            kind: VolumeKind::Internal,
            read_only: false,
        };
        assert!(unmount(&ghost).recv().expect("reply").is_err());
    }

    #[test]
    fn watch_volumes_stays_quiet_without_changes() {
        let rx = watch_volumes();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
