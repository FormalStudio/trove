//! Launching through the shell. Desktop entries mean nothing here, so no app is listed per
//! type; `open_with` runs the program in `desktop_file`, else the one named `name`.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::AppInfo;
use crate::job::{io_message, name_of};

pub fn apps_for(_mime: &str) -> Vec<AppInfo> {
    Vec::new()
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// Opens each file with its default app.
pub fn open(paths: &[PathBuf]) -> Result<(), String> {
    // Shell extensions behind ShellExecute expect COM on the calling thread.
    // SAFETY: a repeated call on the same thread only adds a reference.
    unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    let verb = wide(OsStr::new("open"));
    let mut errors = Vec::new();
    for path in paths {
        let file = wide(path.as_os_str());
        // SAFETY: both strings are NUL-terminated and live across the call.
        let rc = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Anything above 32 is success.
        if rc as usize <= 32 {
            errors.push(format!("Cannot open \"{}\"", name_of(path)));
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

pub fn open_with(app: &AppInfo, paths: &[PathBuf]) -> Result<(), String> {
    let program = app.desktop_file.clone().unwrap_or_else(|| PathBuf::from(&app.name));
    spawn(Command::new(&program).args(paths), &program.to_string_lossy())
}

/// Windows Terminal when installed, else a console window running cmd.
pub fn open_terminal(dir: &Path) -> Result<(), String> {
    if crate::archive::find_in_path("wt").is_some() {
        return spawn(Command::new("wt.exe").arg("-d").arg(dir).current_dir(dir), "Windows Terminal");
    }
    spawn(Command::new("cmd").args(["/c", "start", "", "cmd", "/K"]).current_dir(dir), "cmd")
}

pub fn set_default(_app: &AppInfo, _mime: &str) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Choose the default app in Windows Settings"))
}

/// Starts `cmd` with no stdio and outside Trove's console group, so closing Trove never takes
/// the app with it.
fn spawn(cmd: &mut Command, what: &str) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| format!("Cannot start {what}: {}", io_message(&e)))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
