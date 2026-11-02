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

/// A thumbnailer command: `env` carries the file-derived values that the Exec line's shell
/// script reads through `$TROVE_ARG_n` references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
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
    pub fn command_line(&self, input: &Path, uri: &str, output: &Path, size: u32) -> Invocation {
        let tokens = split_exec(&self.exec);
        let script = script_arg(&tokens);
        let mut env = Vec::new();
        let argv = tokens
            .iter()
            .enumerate()
            .map(|(at, arg)| {
                let mut script = script.filter(|(i, _)| *i == at).map(|(_, fish)| Script::new(&mut env, fish));
                let mut out = String::new();
                let mut chars = arg.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        match &mut script {
                            Some(s) => s.literal(&mut out, c),
                            None => out.push(c),
                        }
                        continue;
                    }
                    let value = match chars.next() {
                        Some('i') => input.to_string_lossy().into_owned(),
                        Some('u') => uri.to_owned(),
                        Some('o') => output.to_string_lossy().into_owned(),
                        // A number and a percent sign are not file-derived, so they stay in the text.
                        Some(k @ ('s' | '%')) => {
                            let text = if k == 's' { size.to_string() } else { "%".to_owned() };
                            for c in text.chars() {
                                match &mut script {
                                    Some(s) => s.literal(&mut out, c),
                                    None => out.push(c),
                                }
                            }
                            continue;
                        }
                        _ => continue,
                    };
                    match &mut script {
                        Some(s) => s.reference(&mut out, value),
                        None => out.push_str(&value),
                    }
                }
                out
            })
            .collect();
        Invocation { argv, env }
    }
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
    let cmd = t.command_line(input, uri, output, size);
    let Some((program, args)) = cmd.argv.split_first() else { return false };
    let spawned = Command::new(program)
        .args(args)
        .envs(cmd.env.iter().cloned())
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
        let cmd = t.command_line(Path::new("/in dir/a.mp4"), "file:///in%20dir/a.mp4", Path::new("/out/o.png"), 256);
        assert_eq!(
            cmd.argv,
            ["sh", "-c", r#"echo "$TROVE_ARG_1" > "$TROVE_ARG_2""#, "--size=256", "file:///in%20dir/a.mp4", "100%"]
        );
        assert_eq!(
            cmd.env,
            [
                ("TROVE_ARG_1".to_owned(), "/in dir/a.mp4".to_owned()),
                ("TROVE_ARG_2".to_owned(), "/out/o.png".to_owned())
            ]
        );
    }

    const NAMES: [&str; 3] = ["/tmp/a'$(touch X).txt", "/tmp/b\"; touch X; \".txt", "/tmp/c\\'x"];

    #[test]
    fn script_text_holds_only_the_reference() {
        let shells = ["sh -c", "sh -ec", "env FOO=1 bash -c", "/usr/bin/fish -c", "env -i sh -xc"];
        for shell in shells {
            for name in NAMES {
                let t = Thumbnailer {
                    try_exec: None,
                    exec: format!(r#"{shell} "convert %i %s" --uri=%u"#),
                    mime_types: Vec::new(),
                };
                let cmd = t.command_line(Path::new(name), name, Path::new("/out/o.png"), 64);
                let script = cmd.argv.iter().find(|a| a.starts_with("convert")).unwrap();
                assert_eq!(script, r#"convert "$TROVE_ARG_1" 64"#, "{shell}");
                assert_eq!(cmd.env[0], ("TROVE_ARG_1".to_owned(), name.to_owned()));
                assert_eq!(cmd.argv.last().unwrap(), &format!("--uri={name}"), "plain args stay raw");
            }
        }
    }

    #[test]
    fn quoted_codes_keep_their_quote_and_non_scripts_stay_raw() {
        let cmd = |exec: &str| {
            let t = Thumbnailer { try_exec: None, exec: exec.into(), mime_types: Vec::new() };
            t.command_line(Path::new("/i'x"), "u", Path::new("/o"), 8)
        };
        assert_eq!(cmd(r#"sh -c "cat '%i' \"%o\"""#).argv[2], r##"cat ''"$TROVE_ARG_1"'' """$TROVE_ARG_2""""##);
        for exec in ["sh run.sh %i", "python3 -c %i", "sh -- -c %i"] {
            let c = cmd(exec);
            assert!(c.env.is_empty(), "{exec}");
            assert!(c.argv.contains(&"/i'x".to_owned()), "{exec}");
        }
    }

    #[test]
    fn real_shell_writes_the_literal_name_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for (n, name) in ["a'$(touch X).txt", "b\"; touch X; \".txt", "c\\'x"].into_iter().enumerate() {
            let out = dir.path().join(format!("out-{n}"));
            let t = parse("[Thumbnailer Entry]\nExec=sh -ec \"printf %%s \\\"%i\\\" > %o\"\nMimeType=a/b;\n").unwrap();
            let cmd = t.command_line(Path::new(name), "file:///x", &out, 64);
            let status = Command::new(&cmd.argv[0])
                .args(&cmd.argv[1..])
                .envs(cmd.env.iter().cloned())
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success(), "{name}");
            assert_eq!(std::fs::read_to_string(&out).unwrap(), name);
            assert!(!dir.path().join("X").exists(), "{name}");
        }
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
