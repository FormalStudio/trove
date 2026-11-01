use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::archive::{is_executable, which};

/// One `*.thumbnailer` file: https://specifications.freedesktop.org/thumbnailer-spec/latest/
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Thumbnailer {
    pub try_exec: Option<String>,
    pub exec: String,
    pub mime_types: Vec<String>,
}

pub(crate) fn parse(text: &str) -> Option<Thumbnailer> {
    let mut in_entry = false;
    let (mut try_exec, mut exec, mut mimes) = (None, None, Vec::new());
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Thumbnailer Entry]";
        } else if in_entry && let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            match k.trim() {
                "TryExec" => try_exec = Some(v.to_owned()),
                "Exec" => exec = Some(v.to_owned()),
                "MimeType" => mimes = v.split(';').filter(|m| !m.is_empty()).map(str::to_owned).collect(),
                _ => {}
            }
        }
    }
    Some(Thumbnailer { try_exec, exec: exec?, mime_types: mimes })
}

impl Thumbnailer {
    fn available(&self) -> bool {
        match &self.try_exec {
            None => true,
            Some(t) if t.contains('/') => is_executable(Path::new(t)),
            Some(t) => which(t).is_some(),
        }
    }

    /// `Exec` split into arguments with the field codes filled in: `%i` input path, `%u` input
    /// URI, `%o` output path, `%s` size in pixels, `%%` a percent sign.
    pub fn command_line(&self, input: &Path, uri: &str, output: &Path, size: u32) -> Vec<String> {
        split_exec(&self.exec)
            .into_iter()
            .map(|arg| {
                let mut out = String::new();
                let mut chars = arg.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('i') => out.push_str(&input.to_string_lossy()),
                        Some('u') => out.push_str(uri),
                        Some('o') => out.push_str(&output.to_string_lossy()),
                        Some('s') => out.push_str(&size.to_string()),
                        Some('%') => out.push('%'),
                        _ => {}
                    }
                }
                out
            })
            .collect()
    }
}

/// Desktop Entry quoting: whitespace separates, double quotes group, backslash escapes inside them.
fn split_exec(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut live) = (false, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                live = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if live {
                    args.push(std::mem::take(&mut cur));
                    live = false;
                }
            }
            c => {
                cur.push(c);
                live = true;
            }
        }
    }
    if live {
        args.push(cur);
    }
    args
}

/// Every installed thumbnailer by mime type.
#[derive(Default)]
pub(crate) struct Registry {
    by_mime: HashMap<String, Vec<Arc<Thumbnailer>>>,
}

impl Registry {
    /// Directories come highest priority first; a file name in an earlier one hides the same name later.
    pub fn load(dirs: &[PathBuf]) -> Self {
        let mut seen = HashSet::new();
        let mut reg = Self::default();
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(dir) else { continue };
            let mut files: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
            files.sort();
            for path in files {
                let Some(name) = path.file_name().and_then(|n| n.to_str()).filter(|n| n.ends_with(".thumbnailer"))
                else {
                    continue;
                };
                if !seen.insert(name.to_owned()) {
                    continue;
                }
                let Some(t) =
                    std::fs::read_to_string(&path).ok().and_then(|s| parse(&s)).filter(Thumbnailer::available)
                else {
                    continue;
                };
                let t = Arc::new(t);
                for m in &t.mime_types {
                    reg.by_mime.entry(m.clone()).or_default().push(t.clone());
                }
            }
        }
        reg
    }

    pub fn handles(&self, mime: &str) -> bool {
        self.by_mime.contains_key(mime)
    }

    pub fn for_mime(&self, mime: &str) -> &[Arc<Thumbnailer>] {
        self.by_mime.get(mime).map_or(&[], Vec::as_slice)
    }
}

/// Run one thumbnailer to completion or kill it (and its children) at `timeout`.
pub(crate) fn run(t: &Thumbnailer, input: &Path, uri: &str, output: &Path, size: u32, timeout: Duration) -> bool {
    let argv = t.command_line(input, uri, output, size);
    let Some((program, args)) = argv.split_first() else { return false };
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = spawned else { return false };
    wait_or_kill(&mut child, timeout)
}

fn wait_or_kill(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut nap = Duration::from_millis(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(40));
            }
            _ => {
                // SAFETY: signalling the process group the child leads; the pid is ours.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "[Thumbnailer Entry]\nTryExec=sh\nExec=sh -c \"echo %i > %o\" --size=%s %u 100%%\nMimeType=video/mp4;video/x-matroska;\n";

    #[test]
    fn parses_entry_and_substitutes_codes() {
        let t = parse(FILE).unwrap();
        assert_eq!(t.try_exec.as_deref(), Some("sh"));
        assert_eq!(t.mime_types, ["video/mp4", "video/x-matroska"]);
        assert!(t.available());
        let argv = t.command_line(Path::new("/in dir/a.mp4"), "file:///in%20dir/a.mp4", Path::new("/out/o.png"), 256);
        assert_eq!(
            argv,
            ["sh", "-c", "echo /in dir/a.mp4 > /out/o.png", "--size=256", "file:///in%20dir/a.mp4", "100%"]
        );
    }

    #[test]
    fn missing_exec_or_try_exec_is_unusable() {
        assert!(parse("[Thumbnailer Entry]\nMimeType=a/b;\n").is_none());
        let t = parse("[Thumbnailer Entry]\nTryExec=/nonexistent/tool\nExec=tool %i\nMimeType=a/b;\n").unwrap();
        assert!(!t.available());
    }

    #[test]
    fn earlier_directory_hides_the_same_file_name() {
        let hi = tempfile::tempdir().unwrap();
        let lo = tempfile::tempdir().unwrap();
        std::fs::write(hi.path().join("x.thumbnailer"), "[Thumbnailer Entry]\nExec=high %i\nMimeType=a/b;\n").unwrap();
        std::fs::write(lo.path().join("x.thumbnailer"), "[Thumbnailer Entry]\nExec=low %i\nMimeType=a/b;c/d;\n")
            .unwrap();
        std::fs::write(lo.path().join("ignored.txt"), "").unwrap();
        let reg = Registry::load(&[hi.path().to_path_buf(), lo.path().to_path_buf()]);
        assert_eq!(reg.for_mime("a/b").len(), 1);
        assert_eq!(reg.for_mime("a/b")[0].exec, "high %i");
        assert!(!reg.handles("c/d"));
    }

    #[test]
    fn runs_and_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let ok = parse("[Thumbnailer Entry]\nExec=sh -c \"cp %i %o\"\nMimeType=a/b;\n").unwrap();
        let src = dir.path().join("in");
        std::fs::write(&src, "x").unwrap();
        let out = dir.path().join("out");
        assert!(run(&ok, &src, "file:///in", &out, 128, Duration::from_secs(5)));
        assert!(out.exists());
        let slow = parse("[Thumbnailer Entry]\nExec=sh -c \"sleep 30\"\nMimeType=a/b;\n").unwrap();
        let started = Instant::now();
        assert!(!run(&slow, &src, "file:///in", &out, 128, Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
