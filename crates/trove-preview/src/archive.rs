use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LISTED: usize = 200;
/// How long counting past the listed names may take.
const COUNT_BUDGET: Duration = Duration::from_millis(300);
const EXTERNAL_BUDGET: Duration = Duration::from_secs(3);

pub(crate) struct Listing {
    pub entries: Vec<String>,
    /// The full count, or what was counted before the budget ran out.
    pub total: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Zip,
    Tar,
    TarGz,
    /// Compressions or containers only libarchive reads.
    Libarchive {
        tar_family: bool,
    },
}

fn format_of(name: &str) -> Option<Format> {
    let n = name.to_ascii_lowercase();
    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| n.ends_with(s));
    if ends(&[".zip", ".jar", ".whl", ".apk"]) {
        Some(Format::Zip)
    } else if ends(&[".tar"]) {
        Some(Format::Tar)
    } else if ends(&[".tar.gz", ".tgz", ".crate"]) {
        Some(Format::TarGz)
    } else if ends(&[".tar.xz", ".txz", ".tar.zst", ".tzst", ".tar.bz2", ".tbz2", ".tbz", ".tar.lz", ".tar.lzma"]) {
        Some(Format::Libarchive { tar_family: true })
    } else if ends(&[".7z", ".rar", ".deb", ".rpm", ".cab"]) {
        Some(Format::Libarchive { tar_family: false })
    } else {
        None
    }
}

pub(crate) fn list(path: &Path) -> Option<Listing> {
    match format_of(path.file_name()?.to_str()?)? {
        Format::Zip => list_zip(path),
        Format::Tar => list_tar(File::open(path).ok()?),
        Format::TarGz => list_tar(flate2::read::GzDecoder::new(BufReader::new(File::open(path).ok()?))),
        Format::Libarchive { tar_family } => list_external(path, tar_family),
    }
}

fn list_zip(path: &Path) -> Option<Listing> {
    let zip = zip::ZipArchive::new(BufReader::new(File::open(path).ok()?)).ok()?;
    let entries =
        (0..zip.len().min(LISTED)).filter_map(|i| zip.name_for_index(i)?.ok().map(|n| n.into_owned())).collect();
    Some(Listing { entries, total: zip.len() })
}

fn list_tar<R: std::io::Read>(reader: R) -> Option<Listing> {
    let started = Instant::now();
    let mut archive = tar::Archive::new(Budgeted { inner: reader, started, read: 0 });
    let mut entries = Vec::new();
    let mut total = 0;
    for entry in archive.entries().ok()? {
        let Ok(entry) = entry else { break };
        total += 1;
        if entries.len() < LISTED {
            entries.push(entry.path().ok()?.to_string_lossy().into_owned());
        } else if started.elapsed() > COUNT_BUDGET {
            break;
        }
    }
    (total > 0).then_some(Listing { entries, total })
}

// Stepping to the next tar header decompresses the whole previous entry, so one huge (or
// bomb-sized) member would hold the worker for as long as it takes. Reads fail past the budget,
// which ends the listing with what was found so far.
const READ_BUDGET: u64 = 512 << 20;
const TIME_BUDGET: Duration = Duration::from_secs(2);

struct Budgeted<R> {
    inner: R,
    started: Instant,
    read: u64,
}

impl<R: std::io::Read> std::io::Read for Budgeted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.read > READ_BUDGET || self.started.elapsed() > TIME_BUDGET {
            return Err(std::io::Error::other("archive listing budget spent"));
        }
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

fn list_external(path: &Path, tar_family: bool) -> Option<Listing> {
    let mut tools = vec!["bsdtar"];
    // GNU tar reads xz, zstd and bzip2 tarballs too, but not 7z or rar.
    if tar_family {
        tools.push("tar");
    }
    tools.into_iter().find_map(|tool| run_lister(which(tool)?, path))
}

fn run_lister(tool: PathBuf, path: &Path) -> Option<Listing> {
    let mut child = Command::new(tool)
        .arg("-tf")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = flume::bounded::<String>(4096);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + EXTERNAL_BUDGET;
    let mut entries = Vec::new();
    let mut total = 0;
    loop {
        match rx.recv_deadline(deadline) {
            Ok(line) => {
                total += 1;
                if entries.len() < LISTED {
                    entries.push(line);
                }
            }
            Err(flume::RecvTimeoutError::Disconnected) => break,
            Err(flume::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                break;
            }
        }
    }
    drop(rx);
    let _ = child.wait();
    (total > 0).then_some(Listing { entries, total })
}

pub(crate) fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(tool)).find(|p| is_executable(p))
}

pub(crate) fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn formats_by_name() {
        assert_eq!(format_of("a.ZIP"), Some(Format::Zip));
        assert_eq!(format_of("a.tar.gz"), Some(Format::TarGz));
        assert_eq!(format_of("a.tar.zst"), Some(Format::Libarchive { tar_family: true }));
        assert_eq!(format_of("a.7z"), Some(Format::Libarchive { tar_family: false }));
        assert_eq!(format_of("a.gz"), None);
    }

    #[test]
    fn zip_lists_names_and_caps_at_200() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.zip");
        let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..250 {
            w.start_file(format!("dir/f{i}.txt"), opts).unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
        let l = list(&p).unwrap();
        assert_eq!((l.entries.len(), l.total), (200, 250));
        assert_eq!(l.entries[0], "dir/f0.txt");
    }

    fn tar_bytes(names: &[&str]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for n in names {
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, n, &b"hi"[..]).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn tar_and_tar_gz_list() {
        let dir = tempfile::tempdir().unwrap();
        let raw = tar_bytes(&["a/one", "a/two", "three"]);
        let plain = dir.path().join("x.tar");
        std::fs::write(&plain, &raw).unwrap();
        let l = list(&plain).unwrap();
        assert_eq!((l.entries.as_slice(), l.total), (["a/one", "a/two", "three"].map(String::from).as_slice(), 3));
        let gz = dir.path().join("x.tgz");
        let mut enc = flate2::write::GzEncoder::new(File::create(&gz).unwrap(), flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
        let l = list(&gz).unwrap();
        assert_eq!(l.total, 3);
        assert_eq!(l.entries[2], "three");
    }

    #[test]
    fn bsdtar_lists_zst_when_installed() {
        let Some(tool) = which("bsdtar") else { return };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("f"), "x").unwrap();
        let out = dir.path().join("x.tar.xz");
        let ok = Command::new(tool).arg("-cJf").arg(&out).arg("-C").arg(&src).arg("f").status().unwrap().success();
        assert!(ok);
        let l = list(&out).unwrap();
        assert_eq!((l.entries, l.total), (vec!["f".to_owned()], 1));
    }

    #[test]
    fn not_an_archive_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.zip");
        std::fs::write(&p, "not a zip").unwrap();
        assert!(list(&p).is_none());
    }
}
