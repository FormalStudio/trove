//! Launching through desktop entries and `mimeapps.list`.

use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{AppInfo, Env, Registry, mime_of, refresh, registry};
use crate::desktop::{Launch, expand_exec, in_terminal, set_default_text, terminal_in};
use crate::job::io_message;

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

pub(super) fn launch_app(env: &Env, app: &AppInfo, paths: &[PathBuf], launch: Launcher<'_>) -> Result<(), String> {
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
