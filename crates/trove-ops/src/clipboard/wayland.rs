//! `wl-copy` and `wl-paste`, when `WAYLAND_DISPLAY` is set. `wl-copy` serves one type per
//! process, so only `x-special/gnome-copied-files` is offered; GNOME Files, Nautilus, Dolphin
//! and Thunar read it.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::Backend;

const TOOL_TIMEOUT: Duration = Duration::from_millis(1500);

pub(super) struct System;

impl System {
    fn available(tool: &str) -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
            && crate::archive::find_in_path(tool).is_some()
    }
}

/// Runs `cmd` and returns stdout when it exits 0, killing it after `TOOL_TIMEOUT`.
fn capture(cmd: &mut Command) -> Option<Vec<u8>> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait().ok()? {
            Some(s) => break s,
            None if start.elapsed() > TOOL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let buf = reader.join().ok()?;
    status.success().then_some(buf)
}

impl Backend for System {
    fn offer(&self, mime: &str, data: &[u8]) -> bool {
        if !Self::available("wl-copy") {
            return false;
        }
        let Ok(mut child) = Command::new("wl-copy")
            .args(["--type", mime])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(data).is_ok());
        // wl-copy forks the process that keeps serving the selection; this one exits at once.
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return wrote && status.success(),
                Ok(None) if start.elapsed() < TOOL_TIMEOUT => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    fn types(&self) -> Option<Vec<String>> {
        if !Self::available("wl-paste") {
            return None;
        }
        // An empty clipboard makes wl-paste exit non-zero; that reads as no types.
        let out = capture(Command::new("wl-paste").arg("--list-types")).unwrap_or_default();
        Some(String::from_utf8_lossy(&out).lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect())
    }

    fn read(&self, mime: &str) -> Option<Vec<u8>> {
        capture(Command::new("wl-paste").args(["--no-newline", "--type", mime]))
    }
}
