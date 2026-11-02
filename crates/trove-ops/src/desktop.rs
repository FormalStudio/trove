//! Desktop entries, `mimeapps.list`, and Exec field codes, per the freedesktop.org Desktop Entry,
//! Association and Menu specifications.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

const PATH_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'/').remove(b'-').remove(b'_').remove(b'.').remove(b'~');

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppInfo {
    /// Desktop file id, e.g. "org.gnome.TextEditor.desktop".
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// The Exec line, unexpanded.
    pub exec: String,
    pub terminal: bool,
    pub desktop_file: Option<PathBuf>,
}

/// Where everything is looked up. Built from the environment in production; tests pass their own.
#[derive(Clone, Debug)]
pub struct Env {
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    pub path: Vec<PathBuf>,
    /// `$TERMINAL`.
    pub terminal: Option<String>,
}

impl Env {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let home = var("HOME").map(PathBuf::from).unwrap_or_default();
        let abs = |v: Option<std::ffi::OsString>, fallback: PathBuf| {
            v.map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or(fallback)
        };
        let split = |v: Option<std::ffi::OsString>, fallback: &[&str]| -> Vec<PathBuf> {
            match v {
                Some(v) => std::env::split_paths(&v).filter(|p| p.is_absolute()).collect(),
                None => fallback.iter().map(PathBuf::from).collect(),
            }
        };
        let data_home = abs(var("XDG_DATA_HOME"), home.join(".local/share"));
        let mut data_dirs = split(var("XDG_DATA_DIRS"), &["/usr/local/share", "/usr/share"]);
        // Flatpak exports its launchers here whether or not XDG_DATA_DIRS lists them.
        for exports in [data_home.join("flatpak/exports/share"), PathBuf::from("/var/lib/flatpak/exports/share")] {
            if !data_dirs.contains(&exports) {
                data_dirs.push(exports);
            }
        }
        Self {
            data_home,
            data_dirs,
            config_home: abs(var("XDG_CONFIG_HOME"), home.join(".config")),
            config_dirs: split(var("XDG_CONFIG_DIRS"), &["/etc/xdg"]),
            path: var("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default(),
            terminal: var("TERMINAL").map(|t| t.to_string_lossy().into_owned()),
        }
    }

    /// Application directories, highest priority first.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.data_home.clone()];
        roots.extend(self.data_dirs.iter().cloned());
        roots.into_iter().map(|r| r.join("applications")).collect()
    }

    /// `mimeapps.list` files, highest priority first: user config, system config, then the
    /// legacy locations under the data directories.
    pub fn mimeapps_files(&self) -> Vec<PathBuf> {
        let mut out = vec![self.config_home.join("mimeapps.list")];
        out.extend(self.config_dirs.iter().map(|d| d.join("mimeapps.list")));
        out.push(self.data_home.join("applications/mimeapps.list"));
        out.extend(self.data_dirs.iter().map(|d| d.join("applications/mimeapps.list")));
        out
    }

    pub fn has_tool(&self, tool: &str) -> bool {
        self.find_tool(tool).is_some()
    }

    pub fn find_tool(&self, tool: &str) -> Option<PathBuf> {
        if tool.contains('/') {
            return Path::new(tool).is_file().then(|| PathBuf::from(tool));
        }
        self.path.iter().map(|d| d.join(tool)).find(|p| p.is_file())
    }
}

#[derive(Debug)]
struct Entry {
    info: AppInfo,
    mime_types: Vec<String>,
    no_display: bool,
    hidden: bool,
    try_exec: Option<String>,
}

#[derive(Debug, Default)]
struct MimeApps {
    default: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
}

#[derive(Debug)]
pub struct Registry {
    pub env: Env,
    entries: Vec<Entry>,
    by_id: HashMap<String, usize>,
    base: HashMap<String, Vec<String>>,
    /// Highest priority first.
    mimeapps: Vec<MimeApps>,
    stamp: Vec<(PathBuf, Option<SystemTime>)>,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn stamp_of(env: &Env) -> Vec<(PathBuf, Option<SystemTime>)> {
    env.application_dirs().into_iter().chain(env.mimeapps_files()).map(|p| (p.clone(), mtime(&p))).collect()
}

impl Registry {
    pub fn load(env: Env) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        let dirs = env.application_dirs();
        for dir in &dirs {
            let mut files = Vec::new();
            collect_desktop_files(dir, dir, &mut files);
            files.sort();
            for (id, path) in files {
                if by_id.contains_key(&id) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else { continue };
                if let Some(entry) = parse_desktop(&id, &path, &text) {
                    by_id.insert(id, entries.len());
                    entries.push(entry);
                }
            }
        }
        let mut base: HashMap<String, Vec<String>> = HashMap::new();
        for e in &entries {
            for mime in &e.mime_types {
                push_unique(base.entry(mime.clone()).or_default(), &e.info.id);
            }
        }
        for dir in &dirs {
            let Ok(text) = fs::read_to_string(dir.join("mimeinfo.cache")) else { continue };
            for (mime, ids) in parse_group(&text, "MIME Cache") {
                for id in ids {
                    if by_id.contains_key(&id) {
                        push_unique(base.entry(mime.clone()).or_default(), &id);
                    }
                }
            }
        }
        let mimeapps = env
            .mimeapps_files()
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .map(|text| MimeApps {
                default: parse_group(&text, "Default Applications").into_iter().collect(),
                added: parse_group(&text, "Added Associations").into_iter().collect(),
                removed: parse_group(&text, "Removed Associations").into_iter().collect(),
            })
            .collect();
        let stamp = stamp_of(&env);
        Self { env, entries, by_id, base, mimeapps, stamp }
    }

    pub fn is_stale(&self) -> bool {
        stamp_of(&self.env) != self.stamp
    }

    pub fn app(&self, id: &str) -> Option<&AppInfo> {
        self.by_id.get(id).map(|&i| &self.entries[i].info)
    }

    fn ids_for(&self, mime: &str) -> Vec<String> {
        let mut list = self.base.get(mime).cloned().unwrap_or_default();
        for f in self.mimeapps.iter().rev() {
            if let Some(removed) = f.removed.get(mime) {
                list.retain(|id| !removed.contains(id));
            }
            if let Some(added) = f.added.get(mime) {
                for id in added.iter().rev() {
                    list.retain(|x| x != id);
                    list.insert(0, id.clone());
                }
            }
        }
        list
    }

    fn configured_default(&self, mime: &str) -> Option<String> {
        self.mimeapps
            .iter()
            .filter_map(|f| f.default.get(mime))
            .flatten()
            .find(|id| self.by_id.contains_key(id.as_str()))
            .cloned()
    }

    /// The app that opens `mime`, whether or not it is listed (a NoDisplay one still opens).
    pub fn default_app(&self, mime: &str) -> Option<&AppInfo> {
        let id = self
            .configured_default(mime)
            .or_else(|| self.ids_for(mime).into_iter().find(|id| self.by_id.contains_key(id.as_str())))
            .or_else(|| {
                (mime.starts_with("text/") && mime != "text/plain")
                    .then(|| self.default_app("text/plain").map(|a| a.id.clone()))
                    .flatten()
            })?;
        self.app(&id)
    }

    /// Apps that can open `mime`, default first. NoDisplay, Hidden and entries whose TryExec is
    /// missing are left out.
    pub fn apps_for(&self, mime: &str) -> Vec<AppInfo> {
        let mut ids = self.ids_for(mime);
        if mime.starts_with("text/") && mime != "text/plain" {
            for id in self.ids_for("text/plain") {
                push_unique(&mut ids, &id);
            }
        }
        if let Some(default) = self.default_app(mime).map(|a| a.id.clone()) {
            ids.retain(|id| *id != default);
            ids.insert(0, default);
        }
        ids.iter()
            .filter_map(|id| self.by_id.get(id.as_str()).map(|&i| &self.entries[i]))
            .filter(|e| !e.no_display && !e.hidden && e.try_exec.as_deref().is_none_or(|t| self.env.has_tool(t)))
            .map(|e| e.info.clone())
            .collect()
    }
}

fn push_unique(v: &mut Vec<String>, id: &str) {
    if !v.iter().any(|x| x == id) {
        v.push(id.to_owned());
    }
}

fn collect_desktop_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.filter_map(Result::ok) {
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir())
            || (path.is_dir() && entry.file_type().is_ok_and(|t| t.is_symlink()));
        if is_dir {
            collect_desktop_files(root, &path, out);
        } else if path.extension() == Some(OsStr::new("desktop"))
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push((rel.to_string_lossy().replace('/', "-"), path));
        }
    }
}

/// Values of one group of an INI-like file, each split on unescaped `;`.
fn parse_group(text: &str, group: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            inside = rest.strip_suffix(']') == Some(group);
        } else if inside
            && !line.starts_with('#')
            && let Some((k, v)) = line.split_once('=')
        {
            let ids = v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
            out.push((k.trim().to_owned(), ids));
        }
    }
    out
}

fn unescape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_desktop(id: &str, path: &Path, text: &str) -> Option<Entry> {
    let mut inside = false;
    let (mut name, mut exec, mut icon, mut mime, mut try_exec) = (None, None, None, String::new(), None);
    let (mut no_display, mut hidden, mut terminal, mut kind_ok) = (false, false, false, true);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if inside {
                break;
            }
            inside = line == "[Desktop Entry]";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "Type" => kind_ok = v == "Application",
            "Name" => name = Some(unescape_value(v)),
            "Exec" => exec = Some(unescape_value(v)),
            "Icon" => icon = Some(v.to_owned()).filter(|i| !i.is_empty()),
            "MimeType" => mime = v.to_owned(),
            "TryExec" => try_exec = Some(v.to_owned()).filter(|t| !t.is_empty()),
            "NoDisplay" => no_display = v == "true",
            "Hidden" => hidden = v == "true",
            "Terminal" => terminal = v == "true",
            _ => {}
        }
    }
    if !kind_ok {
        return None;
    }
    let exec = exec?;
    let name = name.unwrap_or_else(|| id.trim_end_matches(".desktop").to_owned());
    Some(Entry {
        info: AppInfo { id: id.to_owned(), name, icon, exec, terminal, desktop_file: Some(path.to_path_buf()) },
        mime_types: mime.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
        no_display,
        hidden,
        try_exec,
    })
}

/// Splits an Exec value into arguments: spaces separate, double quotes group, and inside quotes
/// a backslash escapes `"`, `` ` ``, `$` and `\`.
pub(crate) fn tokenize_exec(exec: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.next() {
                    Some(n @ ('"' | '`' | '$' | '\\')) => cur.push(n),
                    Some(n) => {
                        cur.push('\\');
                        cur.push(n);
                    }
                    None => cur.push('\\'),
                },
                _ => cur.push(c),
            }
        } else {
            match c {
                '"' => {
                    quoted = true;
                    in_arg = true;
                }
                ' ' | '\t' => {
                    if in_arg {
                        args.push(std::mem::take(&mut cur));
                        in_arg = false;
                    }
                }
                _ => {
                    cur.push(c);
                    in_arg = true;
                }
            }
        }
    }
    if quoted {
        return Err("Unterminated quote in the Exec line".into());
    }
    if in_arg {
        args.push(cur);
    }
    Ok(args)
}

fn file_uri(p: &Path) -> String {
    format!("file://{}", utf8_percent_encode(&String::from_utf8_lossy(p.as_os_str().as_bytes()), PATH_SET))
}

/// A command to spawn. `env` carries the file-derived values that the Exec line's shell script
/// reads through `$TROVE_ARG_n` references.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

const SHELLS: [&str; 8] = ["sh", "bash", "dash", "zsh", "ksh", "mksh", "ash", "fish"];

fn basename(t: &str) -> &str {
    t.rsplit('/').next().unwrap_or(t)
}

fn is_env_assignment(t: &str) -> bool {
    t.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The script argument of `[env [-flags] [NAME=value]...] <shell> [-flags] -c <script>`: its index
/// and whether the shell is fish.
fn script_arg(tokens: &[String]) -> Option<(usize, bool)> {
    let mut i = 0;
    if tokens.first().map(|t| basename(t)) == Some("env") {
        i = 1;
        while let Some(t) = tokens.get(i) {
            if matches!(t.as_str(), "-u" | "-C" | "--unset" | "--chdir") {
                i += 2;
            } else if (t.starts_with('-') && t != "-") || is_env_assignment(t) {
                i += 1;
            } else {
                break;
            }
        }
    }
    let shell = basename(tokens.get(i)?);
    if !SHELLS.contains(&shell) {
        return None;
    }
    let mut j = i + 1;
    while let Some(t) = tokens.get(j) {
        let flags = t.strip_prefix('-').filter(|f| !f.is_empty())?;
        j += 1;
        if flags == "-" {
            return None;
        }
        if flags.starts_with('-') {
            continue;
        }
        if flags.contains('c') {
            return (j < tokens.len()).then_some((j, shell == "fish"));
        }
        // `-o name` and `-O name` take the next token.
        if flags.ends_with(['o', 'O']) {
            j += 1;
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
enum Quoting {
    Bare,
    Single,
    Double,
}

// File names must never become script text: any quoting of them is a bet on which shell parses it
// and how, and fish, nested double quotes and `$(...)` have each lost that bet. The child's
// environment carries the value, and the script only ever holds a reference to it.
struct Script<'a> {
    env: &'a mut Vec<(String, String)>,
    state: Quoting,
    escaped: bool,
    fish: bool,
}

impl<'a> Script<'a> {
    fn new(env: &'a mut Vec<(String, String)>, fish: bool) -> Self {
        Self { env, state: Quoting::Bare, escaped: false, fish }
    }

    /// Tracks the quote the script text so far is in, so a reference can close and reopen it.
    fn literal(&mut self, out: &mut String, c: char) {
        out.push(c);
        if self.escaped {
            self.escaped = false;
            return;
        }
        match (self.state, c) {
            (Quoting::Bare | Quoting::Double, '\\') => self.escaped = true,
            (Quoting::Single, '\\') if self.fish => self.escaped = true,
            (Quoting::Bare, '\'') => self.state = Quoting::Single,
            (Quoting::Bare, '"') => self.state = Quoting::Double,
            (Quoting::Double, '"') | (Quoting::Single, '\'') => self.state = Quoting::Bare,
            _ => {}
        }
    }

    fn reference(&mut self, out: &mut String, value: String) {
        let name = format!("TROVE_ARG_{}", self.env.len() + 1);
        let quote = match self.state {
            Quoting::Bare => "",
            Quoting::Single => "'",
            Quoting::Double => "\"",
        };
        out.push_str(&format!("{quote}\"${name}\"{quote}"));
        self.env.push((name, value));
    }

    fn values(&mut self, out: &mut String, values: Vec<String>) {
        for (n, v) in values.into_iter().filter(|v| !v.is_empty()).enumerate() {
            if n > 0 {
                self.literal(out, ' ');
            }
            self.reference(out, v);
        }
    }
}

fn expand_inline(arg: &str, app: &AppInfo, batch: &[PathBuf], mut script: Option<Script<'_>>) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    // In a script %F and %U take every file; elsewhere an inline code takes the first.
    let files = |all: bool| if all { batch } else { &batch[..batch.len().min(1)] };
    while let Some(c) = chars.next() {
        if c != '%' {
            match &mut script {
                Some(s) => s.literal(&mut out, c),
                None => out.push(c),
            }
            continue;
        }
        let values: Vec<String> = match chars.next() {
            Some(k @ ('f' | 'F')) => {
                files(k == 'F' && script.is_some()).iter().map(|f| f.to_string_lossy().into_owned()).collect()
            }
            Some(k @ ('u' | 'U')) => files(k == 'U' && script.is_some()).iter().map(|f| file_uri(f)).collect(),
            Some('i') => vec![app.icon.clone().unwrap_or_default()],
            Some('c') => vec![app.name.clone()],
            Some('k') => {
                vec![app.desktop_file.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()]
            }
            Some('%') => {
                match &mut script {
                    Some(s) => s.literal(&mut out, '%'),
                    None => out.push('%'),
                }
                continue;
            }
            // %d %D %n %N %v %m are deprecated and expand to nothing.
            Some(_) | None => continue,
        };
        match &mut script {
            Some(s) => s.values(&mut out, values),
            None => out.extend(values),
        }
    }
    out
}

/// Expands field codes into the commands to run: `%F`/`%U` take every file in one command,
/// `%f`/`%u` run one command per file, and an Exec line without file codes runs once.
pub(crate) fn expand_exec(app: &AppInfo, files: &[PathBuf]) -> Result<Vec<Launch>, String> {
    let tokens = tokenize_exec(&app.exec)?;
    if tokens.is_empty() {
        return Err(format!("{} has an empty Exec line", app.name));
    }
    let multi = tokens.iter().any(|t| t == "%F" || t == "%U");
    let single = tokens.iter().any(|t| t.contains("%f") || t.contains("%u"));
    let script = script_arg(&tokens);
    let build = |batch: &[PathBuf]| -> Launch {
        let mut launch = Launch::default();
        for (at, t) in tokens.iter().enumerate() {
            if let Some((_, fish)) = script.filter(|(i, _)| *i == at) {
                let s = expand_inline(t, app, batch, Some(Script::new(&mut launch.env, fish)));
                launch.argv.push(s);
                continue;
            }
            match t.as_str() {
                "%F" => launch.argv.extend(batch.iter().map(|f| f.to_string_lossy().into_owned())),
                "%U" => launch.argv.extend(batch.iter().map(|f| file_uri(f))),
                "%i" => {
                    if let Some(icon) = &app.icon {
                        launch.argv.push("--icon".into());
                        launch.argv.push(icon.clone());
                    }
                }
                _ => {
                    let s = expand_inline(t, app, batch, None);
                    if !s.is_empty() || t.is_empty() {
                        launch.argv.push(s);
                    }
                }
            }
        }
        launch
    };
    Ok(if multi {
        vec![build(files)]
    } else if single && !files.is_empty() {
        files.iter().map(|f| build(std::slice::from_ref(f))).collect()
    } else {
        vec![build(&[])]
    })
}

const TERMINALS: [&str; 7] = ["foot", "kitty", "alacritty", "wezterm", "gnome-terminal", "konsole", "xterm"];

/// Wraps `argv` so it runs inside a terminal emulator.
pub(crate) fn in_terminal(env: &Env, argv: Vec<String>) -> Result<Vec<String>, String> {
    let (term, mut lead): (String, Vec<String>) = match env.terminal.as_deref().map(str::trim).filter(|t| !t.is_empty())
    {
        Some(t) => {
            let mut parts = t.split_whitespace().map(str::to_owned);
            let program = parts.next().unwrap_or_default();
            (program, parts.collect())
        }
        None => match TERMINALS.iter().find(|t| env.has_tool(t)) {
            Some(t) => ((*t).to_owned(), Vec::new()),
            None => return Err("No terminal emulator found to run this app in".into()),
        },
    };
    if lead.is_empty() {
        let base = Path::new(&term).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        lead = match base.as_str() {
            "foot" | "kitty" => vec![],
            "wezterm" => vec!["start".into(), "--".into()],
            "gnome-terminal" => vec!["--".into()],
            _ => vec!["-e".into()],
        };
    }
    let mut out = vec![term];
    out.extend(lead);
    out.extend(argv);
    Ok(out)
}

/// `[Default Applications]` with `mime=id;`, everything else in the file left as it was.
pub(crate) fn set_default_text(text: &str, mime: &str, id: &str) -> String {
    let new_line = format!("{mime}={id};");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = lines.iter().position(|l| l.trim() == "[Default Applications]");
    match header {
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[Default Applications]".into());
            lines.push(new_line);
        }
        Some(h) => {
            let end = lines[h + 1..].iter().position(|l| l.trim().starts_with('[')).map_or(lines.len(), |i| h + 1 + i);
            let existing = (h + 1..end).find(|&i| lines[i].split_once('=').is_some_and(|(k, _)| k.trim() == mime));
            match existing {
                Some(i) => lines[i] = new_line,
                None => {
                    let last = (h + 1..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap_or(h);
                    lines.insert(last + 1, new_line);
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    use std::process::Command;

    fn app(exec: &str) -> AppInfo {
        AppInfo {
            id: "x.desktop".into(),
            name: "X".into(),
            icon: None,
            exec: exec.into(),
            terminal: false,
            desktop_file: None,
        }
    }

    const NAMES: [&str; 3] = ["/tmp/a'$(touch X).txt", "/tmp/b\"; touch X; \".txt", "/tmp/c\\'x"];

    #[test]
    fn script_text_holds_only_the_reference() {
        let execs = [
            r#"sh -c "cat %f" --name=%f"#,
            r#"sh -ec "cat %f" --name=%f"#,
            r#"env FOO=1 bash -c "cat %f" --name=%f"#,
            r#"/usr/bin/fish -c "cat %f" --name=%f"#,
            r#"env -i -u HOME sh -xc "cat %f" --name=%f"#,
        ];
        for exec in execs {
            for name in NAMES {
                let launches = expand_exec(&app(exec), &[PathBuf::from(name)]).unwrap();
                let l = &launches[0];
                let at = l.argv.iter().position(|a| a.starts_with("cat ")).unwrap();
                assert_eq!(l.argv[at], r#"cat "$TROVE_ARG_1""#, "{exec}");
                assert_eq!(l.env, [("TROVE_ARG_1".to_owned(), name.to_owned())], "{exec}");
                assert_eq!(l.argv.last().unwrap(), &format!("--name={name}"), "plain args stay raw");
            }
        }
    }

    #[test]
    fn references_follow_the_quote_the_code_sat_in() {
        let name = [PathBuf::from("/q")];
        let script = |exec: &str| expand_exec(&app(exec), &name).unwrap().remove(0).argv.pop().unwrap();
        assert_eq!(script(r#"sh -c "cat '%f'""#), r#"cat ''"$TROVE_ARG_1"''"#);
        assert_eq!(script(r#"sh -c "cat \"%f\"x""#), r#"cat """$TROVE_ARG_1"""x"#);
        // A backslash escapes the quote inside fish's single quotes and not inside sh's.
        assert_eq!(script(r#"fish -c "echo '\\' %f""#), r#"echo '\' '"$TROVE_ARG_1"'"#);
        assert_eq!(script(r#"sh -c "echo '\\' %f""#), r#"echo '\' "$TROVE_ARG_1""#);
    }

    #[test]
    fn not_a_script_means_raw_argv() {
        for exec in ["sh script.sh %f", "env FOO=%f cat", "sh -- -c %f", "bash -o pipefail %f", "python3 -c %f"] {
            let l = expand_exec(&app(exec), &[PathBuf::from("/tmp/a'b")]).unwrap().remove(0);
            assert!(l.env.is_empty(), "{exec}");
            assert!(l.argv.iter().any(|a| a.contains("a'b")), "{exec}");
        }
        let l = expand_exec(&app(r#"bash -o pipefail -c "cat %f""#), &[PathBuf::from("/x")]).unwrap().remove(0);
        assert_eq!(l.argv.last().unwrap(), r#"cat "$TROVE_ARG_1""#);
    }

    #[test]
    fn a_bare_code_in_the_script_position_is_still_a_reference() {
        let l = expand_exec(&app("sh -c %F"), &[PathBuf::from("/a b"), PathBuf::from("/c")]).unwrap().remove(0);
        assert_eq!(l.argv, ["sh", "-c", r#""$TROVE_ARG_1" "$TROVE_ARG_2""#]);
        assert_eq!(l.env.len(), 2);
    }

    #[test]
    fn real_shell_writes_the_literal_name_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for shell in ["sh", "bash", "dash"] {
            if Command::new(shell).args(["-c", "exit 0"]).status().is_err() {
                continue;
            }
            for (n, name) in ["a'$(touch X).txt", "b\"; touch X; \".txt", "c\\'x", "d e*"].into_iter().enumerate() {
                let out = dir.path().join(format!("out-{shell}-{n}"));
                let exec = format!(
                    r#"env FOO=1 {shell} -ec "printf %%s \"%f\" > {o}; printf %%s '%f' >> {o}" _"#,
                    o = out.display()
                );
                let launch = expand_exec(&app(&exec), &[PathBuf::from(name)]).unwrap().remove(0);
                let status = Command::new(&launch.argv[0])
                    .args(&launch.argv[1..])
                    .envs(launch.env.iter().cloned())
                    .current_dir(dir.path())
                    .status()
                    .unwrap();
                assert!(status.success(), "{shell} {name}");
                assert_eq!(fs::read_to_string(&out).unwrap(), format!("{name}{name}"), "{shell} {name}");
                assert!(!dir.path().join("X").exists(), "{shell} {name}");
            }
        }
    }
}
