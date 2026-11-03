//! Opening files: MIME types, the apps that handle them, the default one, and launching.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub use crate::desktop::{AppInfo, Env, Registry};
use crate::desktop::{Launch, expand_exec, in_terminal, set_default_text, terminal_in};
use crate::job::io_message;

const SNIFF_BYTES: usize = 8192;

static REGISTRY: Mutex<Option<Arc<Registry>>> = Mutex::new(None);

/// The parsed desktop entries, loaded on first use and again whenever an applications
/// directory or a `mimeapps.list` changed on disk.
pub fn registry() -> Arc<Registry> {
    let mut slot = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(reg) = slot.as_ref()
        && !reg.is_stale()
    {
        return Arc::clone(reg);
    }
    let reg = Arc::new(Registry::load(Env::from_env()));
    *slot = Some(Arc::clone(&reg));
    reg
}

/// Drops the cache so the next call reads the disk again.
pub fn refresh() {
    *REGISTRY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// By name first; only a file whose name says nothing has its first 8 KiB sniffed.
pub fn mime_of(path: &Path) -> String {
    if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return "inode/directory".into();
    }
    if let Some(m) = mime_guess::from_path(path).first_raw() {
        return m.to_owned();
    }
    sniff(path).unwrap_or_else(|| "application/octet-stream".into())
}

fn sniff(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; SNIFF_BYTES];
    let mut file = File::open(path).ok()?;
    let mut len = 0;
    while len < buf.len() {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    buf.truncate(len);
    if let Some(kind) = infer::get(&buf) {
        return Some(kind.mime_type().to_owned());
    }
    let text_like = !buf.contains(&0)
        && match std::str::from_utf8(&buf) {
            Ok(_) => true,
            // A multi-byte character cut off by the 8 KiB limit is still text.
            Err(e) => e.error_len().is_none(),
        };
    text_like.then(|| "text/plain".to_owned())
}

/// Apps that can open `mime`, the default first.
pub fn apps_for(mime: &str) -> Vec<AppInfo> {
    registry().apps_for(mime)
}

/// Opens each file with its default app, one launch per app. Files with no default go to
/// `xdg-open`, then `gio open`.
pub fn open(paths: &[PathBuf]) -> Result<(), String> {
    open_in(&registry(), paths, &spawn_detached)
}

/// Opens `paths` with `app`, expanding the Exec line's field codes.
pub fn open_with(app: &AppInfo, paths: &[PathBuf]) -> Result<(), String> {
    launch_app(&registry().env, app, paths, &spawn_detached)
}

/// Opens the user's terminal in `dir`.
pub fn open_terminal(dir: &Path) -> Result<(), String> {
    spawn_detached(&terminal_in(&Env::from_env(), dir)?)
}

/// Makes `app` the default for `mime` in `~/.config/mimeapps.list`.
pub fn set_default(app: &AppInfo, mime: &str) -> io::Result<()> {
    let result = set_default_in(&Env::from_env(), app, mime);
    refresh();
    result
}

pub(crate) fn set_default_in(env: &Env, app: &AppInfo, mime: &str) -> io::Result<()> {
    let path = env.config_home.join("mimeapps.list");
    let old = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let new = set_default_text(&old, mime, &app.id);
    fs::create_dir_all(&env.config_home)?;
    let tmp = path.with_extension("list.trove-tmp");
    fs::write(&tmp, new)?;
    fs::rename(&tmp, &path)
}

type Launcher<'a> = &'a dyn Fn(&Launch) -> Result<(), String>;

fn launch_app(env: &Env, app: &AppInfo, paths: &[PathBuf], launch: Launcher<'_>) -> Result<(), String> {
    for mut cmd in expand_exec(app, paths)? {
        if app.terminal {
            cmd.argv = in_terminal(env, cmd.argv)?;
        }
        launch(&cmd)?;
    }
    Ok(())
}

pub(crate) fn open_in(reg: &Registry, paths: &[PathBuf], launch: Launcher<'_>) -> Result<(), String> {
    let mut groups: Vec<(AppInfo, Vec<PathBuf>)> = Vec::new();
    let mut loose: Vec<PathBuf> = Vec::new();
    for path in paths {
        match reg.default_app(&mime_of(path)) {
            Some(app) => match groups.iter_mut().find(|(a, _)| a.id == app.id) {
                Some((_, files)) => files.push(path.clone()),
                None => groups.push((app.clone(), vec![path.clone()])),
            },
            None => loose.push(path.clone()),
        }
    }
    let mut errors: Vec<String> = Vec::new();
    for (app, files) in &groups {
        if let Err(e) = launch_app(&reg.env, app, files, launch) {
            // The app's own launch failed, so hand the files to the generic opener instead.
            match fallback_open(&reg.env, files, launch) {
                Ok(()) => {}
                Err(_) => errors.push(e),
            }
        }
    }
    if !loose.is_empty()
        && let Err(e) = fallback_open(&reg.env, &loose, launch)
    {
        errors.push(e);
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

fn fallback_open(env: &Env, files: &[PathBuf], launch: Launcher<'_>) -> Result<(), String> {
    let base: Vec<&str> = if env.has_tool("xdg-open") {
        vec!["xdg-open"]
    } else if env.has_tool("gio") {
        vec!["gio", "open"]
    } else {
        let name =
            files.first().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(format!("No app found to open \"{name}\" and neither xdg-open nor gio is installed"));
    };
    for file in files {
        let mut argv: Vec<String> = base.iter().map(|s| (*s).to_owned()).collect();
        argv.push(file.to_string_lossy().into_owned());
        launch(&Launch { argv, ..Launch::default() })?;
    }
    Ok(())
}

/// Starts `argv` in a session of its own with no stdio, so closing Trove never takes the app
/// with it.
fn spawn_detached(launch: &Launch) -> Result<(), String> {
    let Some((program, args)) = launch.argv.split_first() else {
        return Err("Empty command".into());
    };
    let mut cmd = Command::new(program);
    cmd.args(args).envs(launch.env.iter().cloned()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(dir) = &launch.cwd {
        cmd.current_dir(dir);
    }
    // SAFETY: setsid is async-signal-safe and the closure touches nothing else.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("Cannot start {program}: {}", io_message(&e)))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
