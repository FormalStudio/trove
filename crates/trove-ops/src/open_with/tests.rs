use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::desktop::{Launch, expand_exec as expand_launches, in_terminal, set_default_text, tokenize_exec};

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self { root: tempfile::tempdir().unwrap() }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.p(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    fn desktop(&self, dir: &str, file: &str, name: &str, exec: &str, extra: &str) {
        self.write(
            &format!("{dir}/applications/{file}"),
            &format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n{extra}\n"),
        );
    }

    fn env(&self) -> Env {
        Env {
            data_home: self.p("home/share"),
            data_dirs: vec![self.p("sys/share"), self.p("usr/share")],
            config_home: self.p("home/config"),
            config_dirs: vec![self.p("etc/xdg")],
            path: vec![self.p("bin")],
            terminal: None,
        }
    }

    fn tool(&self, name: &str) {
        self.write(&format!("bin/{name}"), "");
    }
}

fn ids(apps: &[AppInfo]) -> Vec<&str> {
    apps.iter().map(|a| a.id.as_str()).collect()
}

fn app(exec: &str) -> AppInfo {
    AppInfo {
        id: "x.desktop".into(),
        name: "Ex App".into(),
        icon: Some("ex-icon".into()),
        exec: exec.into(),
        terminal: false,
        desktop_file: Some(PathBuf::from("/usr/share/applications/x.desktop")),
    }
}

fn files(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn tokenizer_follows_the_quoting_rules() {
    assert_eq!(tokenize_exec("foo  bar\tbaz").unwrap(), ["foo", "bar", "baz"]);
    assert_eq!(tokenize_exec(r#"foo "a b" c"#).unwrap(), ["foo", "a b", "c"]);
    assert_eq!(tokenize_exec(r#"foo "say \"hi\" \$HOME \\ \`x\`""#).unwrap(), ["foo", r#"say "hi" $HOME \ `x`"#]);
    assert_eq!(tokenize_exec(r#"foo "" x"#).unwrap(), ["foo", "", "x"]);
    assert_eq!(tokenize_exec(r#"pre"fix suf"fix"#).unwrap(), ["prefix suffix"]);
    assert!(tokenize_exec(r#"foo "open"#).is_err());
}

#[test]
fn field_codes_expand() {
    let a = app("ex %F");
    assert_eq!(expand_exec(&a, &files(&["/a b", "/c"])).unwrap(), [["ex", "/a b", "/c"]]);
    assert_eq!(expand_exec(&a, &[]).unwrap(), [["ex"]]);

    let a = app("ex %f");
    assert_eq!(expand_exec(&a, &files(&["/a", "/b"])).unwrap(), [["ex", "/a"], ["ex", "/b"]]);
    assert_eq!(expand_exec(&a, &[]).unwrap(), [["ex"]]);

    let a = app("ex --open=%u");
    assert_eq!(expand_exec(&a, &files(&["/a b/é#"])).unwrap(), [["ex", "--open=file:///a%20b/%C3%A9%23"]]);

    let a = app("ex %U");
    assert_eq!(expand_exec(&a, &files(&["/x y"])).unwrap(), [["ex", "file:///x%20y"]]);

    let a = app(r#"ex %i --name "%c" --from %k 100%% %d %D %n %N %v %m %f"#);
    assert_eq!(
        expand_exec(&a, &files(&["/f"])).unwrap(),
        [["ex", "--icon", "ex-icon", "--name", "Ex App", "--from", "/usr/share/applications/x.desktop", "100%", "/f"]]
    );

    let mut a = app("ex %i");
    a.icon = None;
    assert_eq!(expand_exec(&a, &[]).unwrap(), [["ex"]]);

    // No file code: the files are not passed.
    assert_eq!(expand_exec(&app("ex --new"), &files(&["/a"])).unwrap(), [["ex", "--new"]]);
    assert!(expand_exec(&app("   "), &[]).is_err());
}

#[test]
fn exec_value_escapes_are_undone_before_quoting() {
    let f = Fixture::new();
    f.desktop("home/share", "q.desktop", "Q", r#"sh -c "echo \\\\ ok" %f"#, "");
    let reg = Registry::load(f.env());
    assert_eq!(reg.app("q.desktop").unwrap().exec, r#"sh -c "echo \\ ok" %f"#);
    let cmds = expand_exec(reg.app("q.desktop").unwrap(), &files(&["/z"])).unwrap();
    assert_eq!(cmds, [["sh", "-c", r"echo \ ok", "/z"]]);
}

#[test]
fn terminal_apps_are_wrapped() {
    let f = Fixture::new();
    let mut env = f.env();
    let cmd = || vec!["htop".to_owned()];
    assert!(in_terminal(&env, cmd()).is_err());
    f.tool("xterm");
    assert_eq!(in_terminal(&env, cmd()).unwrap(), ["xterm", "-e", "htop"]);
    f.tool("kitty");
    assert_eq!(in_terminal(&env, cmd()).unwrap(), ["kitty", "htop"]);
    f.tool("foot");
    assert_eq!(in_terminal(&env, cmd()).unwrap(), ["foot", "htop"]);
    env.terminal = Some("alacritty --class x".into());
    assert_eq!(in_terminal(&env, cmd()).unwrap(), ["alacritty", "--class", "x", "htop"]);
    env.terminal = Some("wezterm".into());
    assert_eq!(in_terminal(&env, cmd()).unwrap(), ["wezterm", "start", "--", "htop"]);
}

#[test]
fn mimeapps_precedence() {
    let f = Fixture::new();
    f.desktop("sys/share", "a.desktop", "A", "a %f", "MimeType=text/plain;image/png;");
    f.desktop("sys/share", "b.desktop", "B", "b %f", "MimeType=text/plain;");
    f.desktop("sys/share", "c.desktop", "C", "c %f", "MimeType=text/plain;");
    f.desktop("usr/share", "d.desktop", "D", "d %f", "MimeType=text/plain;");
    let env = f.env();

    // Without any mimeapps.list: scan order, the first is the default.
    let reg = Registry::load(env.clone());
    assert_eq!(ids(&reg.apps_for("text/plain")), ["a.desktop", "b.desktop", "c.desktop", "d.desktop"]);
    assert_eq!(reg.default_app("text/plain").unwrap().id, "a.desktop");

    // A system default beats the scan order.
    f.write("etc/xdg/mimeapps.list", "[Default Applications]\ntext/plain=c.desktop;\n");
    let reg = Registry::load(env.clone());
    assert_eq!(ids(&reg.apps_for("text/plain"))[0], "c.desktop");

    // The user's file beats the system's; an uninstalled entry in it is skipped.
    f.write("home/config/mimeapps.list", "[Default Applications]\ntext/plain=gone.desktop;b.desktop;\n");
    let reg = Registry::load(env.clone());
    assert_eq!(reg.default_app("text/plain").unwrap().id, "b.desktop");
    assert_eq!(ids(&reg.apps_for("text/plain"))[0], "b.desktop");

    // Added associations list an app that does not claim the type; removed ones drop one that does.
    f.write(
        "home/config/mimeapps.list",
        "[Added Associations]\nimage/png=d.desktop;\n[Removed Associations]\nimage/png=a.desktop;\ntext/plain=b.desktop;\n",
    );
    let reg = Registry::load(env.clone());
    assert_eq!(ids(&reg.apps_for("image/png")), ["d.desktop"]);
    assert_eq!(ids(&reg.apps_for("text/plain")), ["c.desktop", "a.desktop", "d.desktop"]);

    // The legacy data-dir file sits below the config ones.
    f.write("home/config/mimeapps.list", "");
    f.write("etc/xdg/mimeapps.list", "");
    f.write("home/share/applications/mimeapps.list", "[Default Applications]\ntext/plain=d.desktop;\n");
    f.write("sys/share/applications/mimeapps.list", "[Default Applications]\ntext/plain=b.desktop;\n");
    let reg = Registry::load(env);
    assert_eq!(reg.default_app("text/plain").unwrap().id, "d.desktop");
}

#[test]
fn listing_honours_nodisplay_hidden_and_tryexec_but_opening_does_not() {
    let f = Fixture::new();
    f.desktop("sys/share", "shown.desktop", "Shown", "shown %f", "MimeType=text/plain;");
    f.desktop("sys/share", "nodisplay.desktop", "Quiet", "quiet %f", "MimeType=text/plain;\nNoDisplay=true");
    f.desktop("sys/share", "hidden.desktop", "Hidden", "hidden %f", "MimeType=text/plain;\nHidden=true");
    f.desktop("sys/share", "needs.desktop", "Needs", "needs %f", "MimeType=text/plain;\nTryExec=needs-tool");
    f.desktop("sys/share", "link.desktop", "Link", "link", "MimeType=text/plain;\nType=Link");
    f.write("home/config/mimeapps.list", "[Default Applications]\ntext/plain=nodisplay.desktop;\n");
    let env = f.env();
    let reg = Registry::load(env.clone());
    assert_eq!(ids(&reg.apps_for("text/plain")), ["shown.desktop"]);
    assert_eq!(reg.default_app("text/plain").unwrap().id, "nodisplay.desktop");

    f.tool("needs-tool");
    let reg = Registry::load(env);
    assert_eq!(ids(&reg.apps_for("text/plain")), ["needs.desktop", "shown.desktop"]);
}

#[test]
fn earlier_directories_shadow_later_ones_and_subdirs_join_with_dashes() {
    let f = Fixture::new();
    f.desktop("home/share", "e.desktop", "User E", "user-e", "MimeType=audio/flac;");
    f.desktop("sys/share", "e.desktop", "System E", "sys-e", "MimeType=audio/flac;");
    f.desktop("sys/share", "kde/f.desktop", "F", "f", "MimeType=audio/flac;");
    f.desktop("var/lib/flatpak/exports/share", "g.desktop", "G", "g", "MimeType=audio/flac;");
    let mut env = f.env();
    env.data_dirs.push(f.p("var/lib/flatpak/exports/share"));
    let reg = Registry::load(env);
    let apps = reg.apps_for("audio/flac");
    assert_eq!(ids(&apps), ["e.desktop", "kde-f.desktop", "g.desktop"]);
    assert_eq!(apps[0].name, "User E");
}

#[test]
fn mimeinfo_cache_contributes_known_apps_only() {
    let f = Fixture::new();
    f.desktop("sys/share", "a.desktop", "A", "a", "");
    f.write("sys/share/applications/mimeinfo.cache", "[MIME Cache]\nvideo/mp4=a.desktop;missing.desktop;\n");
    let reg = Registry::load(f.env());
    assert_eq!(ids(&reg.apps_for("video/mp4")), ["a.desktop"]);
}

#[test]
fn text_subtypes_fall_back_to_text_plain() {
    let f = Fixture::new();
    f.desktop("sys/share", "ed.desktop", "Ed", "ed %f", "MimeType=text/plain;");
    f.desktop("sys/share", "md.desktop", "Md", "md %f", "MimeType=text/markdown;");
    let reg = Registry::load(f.env());
    assert_eq!(ids(&reg.apps_for("text/markdown")), ["md.desktop", "ed.desktop"]);
    assert_eq!(reg.default_app("text/x-rust").unwrap().id, "ed.desktop");
    assert_eq!(ids(&reg.apps_for("text/x-rust")), ["ed.desktop"]);
    assert!(reg.apps_for("image/png").is_empty());
}

#[test]
fn registry_notices_changes_on_disk() {
    let f = Fixture::new();
    f.desktop("home/share", "a.desktop", "A", "a", "MimeType=text/plain;");
    let reg = Registry::load(f.env());
    assert!(!reg.is_stale());
    std::thread::sleep(std::time::Duration::from_millis(20));
    f.desktop("home/share", "b.desktop", "B", "b", "MimeType=text/plain;");
    assert!(reg.is_stale());
    assert_eq!(Registry::load(f.env()).apps_for("text/plain").len(), 2);
}

#[test]
fn set_default_edits_only_its_line() {
    let original = "# mine\n[Added Associations]\ntext/plain=a.desktop;\n\n[Default Applications]\nimage/png=x.desktop;\ntext/plain=old.desktop;\n\n[Removed Associations]\nfoo/bar=z.desktop;\n";
    let out = set_default_text(original, "text/plain", "new.desktop");
    assert_eq!(out, original.replace("text/plain=old.desktop;", "text/plain=new.desktop;"));

    let out = set_default_text(original, "video/mp4", "v.desktop");
    assert_eq!(out, original.replace("text/plain=old.desktop;\n", "text/plain=old.desktop;\nvideo/mp4=v.desktop;\n"));

    assert_eq!(set_default_text("", "a/b", "c.desktop"), "[Default Applications]\na/b=c.desktop;\n");
    assert_eq!(
        set_default_text("[X]\nk=v\n", "a/b", "c.desktop"),
        "[X]\nk=v\n\n[Default Applications]\na/b=c.desktop;\n"
    );
}

#[test]
fn set_default_writes_the_user_file_and_wins() {
    let f = Fixture::new();
    f.desktop("sys/share", "a.desktop", "A", "a", "MimeType=text/plain;");
    f.desktop("sys/share", "b.desktop", "B", "b", "MimeType=text/plain;");
    let env = f.env();
    let b = Registry::load(env.clone()).app("b.desktop").unwrap().clone();
    set_default_in(&env, &b, "text/plain").unwrap();
    let text = fs::read_to_string(env.config_home.join("mimeapps.list")).unwrap();
    assert_eq!(text, "[Default Applications]\ntext/plain=b.desktop;\n");
    assert_eq!(Registry::load(env).default_app("text/plain").unwrap().id, "b.desktop");
}

fn expand_exec(app: &AppInfo, files: &[PathBuf]) -> Result<Vec<Vec<String>>, String> {
    Ok(expand_launches(app, files)?.into_iter().map(|l| l.argv).collect())
}

fn record(log: &RefCell<Vec<Vec<String>>>) -> impl Fn(&Launch) -> Result<(), String> + '_ {
    move |launch| {
        log.borrow_mut().push(launch.argv.clone());
        Ok(())
    }
}

#[test]
fn open_groups_files_by_app_and_falls_back_to_xdg_open() {
    let f = Fixture::new();
    f.desktop("sys/share", "ed.desktop", "Ed", "ed %F", "MimeType=text/plain;");
    f.desktop("sys/share", "img.desktop", "Img", "img %f", "MimeType=image/png;");
    let a = f.write("docs/a.txt", "a");
    let b = f.write("docs/b.txt", "b");
    let p = f.write("docs/p.png", "png");
    let q = f.write("docs/q.png", "png");
    let odd = f.write("docs/odd.zzzunknown", "\0\0\0");

    let reg = Registry::load(f.env());
    let log = RefCell::new(Vec::new());
    let err = open_in(&reg, &[a.clone(), p.clone(), b.clone(), q.clone(), odd.clone()], &record(&log)).unwrap_err();
    assert!(err.contains("neither xdg-open nor gio"), "{err}");
    let s = |p: &Path| p.to_string_lossy().into_owned();
    assert_eq!(
        *log.borrow(),
        [vec!["ed".to_owned(), s(&a), s(&b)], vec!["img".to_owned(), s(&p)], vec!["img".to_owned(), s(&q)]]
    );

    f.tool("gio");
    log.borrow_mut().clear();
    let reg = Registry::load(f.env());
    open_in(&reg, std::slice::from_ref(&odd), &record(&log)).unwrap();
    assert_eq!(*log.borrow(), [vec!["gio".to_owned(), "open".to_owned(), s(&odd)]]);

    f.tool("xdg-open");
    log.borrow_mut().clear();
    let reg = Registry::load(f.env());
    open_in(&reg, std::slice::from_ref(&odd), &record(&log)).unwrap();
    assert_eq!(*log.borrow(), [vec!["xdg-open".to_owned(), s(&odd)]]);
}

#[test]
fn open_with_runs_terminal_apps_in_a_terminal() {
    let f = Fixture::new();
    f.tool("foot");
    let mut a = app("vim %f");
    a.terminal = true;
    let log = RefCell::new(Vec::new());
    launch_app(&f.env(), &a, &files(&["/a.txt"]), &record(&log)).unwrap();
    assert_eq!(*log.borrow(), [["foot", "vim", "/a.txt"]]);
}

#[test]
fn mime_by_name_then_by_content() {
    let f = Fixture::new();
    assert_eq!(mime_of(f.root.path()), "inode/directory");
    assert_eq!(mime_of(&f.write("a.png", "not really a png")), "image/png");
    assert_eq!(mime_of(&f.write("A.JPG", "")), "image/jpeg");
    let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R'];
    let p = f.p("noext");
    fs::write(&p, png).unwrap();
    assert_eq!(mime_of(&p), "image/png");
    assert_eq!(mime_of(&f.write("README", "just words\nand lines\n")), "text/plain");
    assert_eq!(mime_of(&f.write("empty", "")), "text/plain");
    let bin = f.p("blob");
    fs::write(&bin, [1u8, 2, 0, 3, 255, 254]).unwrap();
    assert_eq!(mime_of(&bin), "application/octet-stream");
    assert_eq!(mime_of(&f.p("missing")), "application/octet-stream");
    let mut long = "é".repeat(SNIFF_BYTES);
    long.truncate(long.len());
    assert_eq!(mime_of(&f.write("long", &long)), "text/plain");
}
