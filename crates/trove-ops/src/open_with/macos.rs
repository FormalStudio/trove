//! Launching through LaunchServices with `open`. Desktop entries mean nothing here, so no app
//! is listed per type; `open_with` takes the app's bundle path from `desktop_file`, else its
//! name.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::AppInfo;
use crate::job::io_message;

pub fn apps_for(_mime: &str) -> Vec<AppInfo> {
    Vec::new()
}

/// Opens each file with its default app.
pub fn open(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    run(Command::new("open").arg("--").args(paths))
}

pub fn open_with(app: &AppInfo, paths: &[PathBuf]) -> Result<(), String> {
    let target = app.desktop_file.as_deref().map_or_else(|| app.name.clone().into(), |p| p.as_os_str().to_owned());
    run(Command::new("open").arg("-a").arg(target).arg("--").args(paths))
}

pub fn open_terminal(dir: &Path) -> Result<(), String> {
    run(Command::new("open").args(["-a", "Terminal", "--"]).arg(dir))
}

pub fn set_default(_app: &AppInfo, _mime: &str) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Choose the default app in Finder's Get Info"))
}

/// `open` hands the launch to LaunchServices and exits, so the app outlives Trove.
fn run(cmd: &mut Command) -> Result<(), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start open: {}", io_message(&e)))?;
    let stderr = child.stderr.take();
    let status = child.wait().map_err(|e| io_message(&e))?;
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut s) = stderr {
        let _ = io::Read::read_to_string(&mut s, &mut err);
    }
    Err(err.lines().next().unwrap_or("open failed").to_owned())
}
