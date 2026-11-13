//! `trove du`: the Disk Usage walk without a window, for a shell or an agent. It prints the
//! largest items as a tree, or with `--dev` the worktrees, build output, dependencies and caches
//! under the folder, and with `--json` either of them for a program to read.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trove_fs::{DuFound, DuKind, DuNode};

use crate::format;

pub const HELP: &str = "\
trove du [--dev] [--json] [--depth N] [--top N] [FOLDER]

Measures FOLDER (default: the current one) and prints what fills it, largest first.
Sizes are bytes on disk; other disks mounted inside are left out.

  --dev      list worktrees, build output, dependencies and caches instead of the tree
  --json     print JSON: {path, bytes, items, children[], dev[], reclaimable_bytes}
  --depth N  levels of the tree to print (default 2)
  --top N    items to print in each folder (default 15)
";

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub root: PathBuf,
    pub dev: bool,
    pub json: bool,
    pub depth: usize,
    pub top: usize,
}

/// The options `args` ask for; `Err` is what to tell the user instead of running.
pub fn parse(args: Vec<OsString>) -> Result<Options, String> {
    let mut o = Options { root: PathBuf::from("."), dev: false, json: false, depth: 2, top: 15 };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut number = |flag: &str| {
            let value = args.next().and_then(|v| v.to_str().and_then(|v| v.parse::<usize>().ok()));
            value.ok_or_else(|| format!("{flag} takes a number"))
        };
        match arg.to_str() {
            Some("--dev") => o.dev = true,
            Some("--json") => o.json = true,
            Some("--depth") => o.depth = number("--depth")?,
            Some("--top") => o.top = number("--top")?,
            Some("--help" | "-h") => return Err(HELP.to_owned()),
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}\n\n{HELP}")),
            _ => o.root = PathBuf::from(arg),
        }
    }
    o.root = std::path::absolute(&o.root).unwrap_or(o.root);
    Ok(o)
}

/// The finds no other find holds: what deleting them all gives back, counted once.
fn outermost(finds: &[DuFound]) -> impl Iterator<Item = &DuFound> {
    finds.iter().filter(|f| !finds.iter().any(|o| o.path != f.path && f.path.starts_with(&o.path)))
}

pub fn reclaimable(finds: &[DuFound]) -> u64 {
    outermost(finds).map(|f| f.size).sum()
}

fn kind(node: &DuNode) -> &'static str {
    match node.kind {
        DuKind::Dir => "dir",
        DuKind::File(_) => "file",
        DuKind::Rest => "rest",
    }
}

fn node_json(node: &DuNode, depth: usize, top: usize) -> Value {
    let mut v = json!({ "name": node.name, "bytes": node.size, "items": node.items, "kind": kind(node) });
    if let Some(tag) = node.tag {
        v["tag"] = json!(tag.id());
    }
    if depth > 0 && !node.children.is_empty() {
        v["children"] = node.children.iter().take(top).map(|c| node_json(c, depth - 1, top)).collect();
    }
    v
}

pub fn to_json(tree: &DuNode, o: &Options) -> Value {
    let finds = trove_fs::dev_finds(tree);
    let dev: Vec<Value> = finds
        .iter()
        .map(|f| json!({ "path": o.root.join(&f.path), "kind": f.tag.id(), "bytes": f.size, "items": f.items }))
        .collect();
    let mut v = node_json(tree, o.depth, o.top);
    v["path"] = json!(o.root);
    v["dev"] = Value::Array(dev);
    v["reclaimable_bytes"] = json!(reclaimable(&finds));
    v
}

fn tree_lines(node: &DuNode, depth: usize, o: &Options, out: &mut String) {
    for child in node.children.iter().take(o.top) {
        let slash = if child.is_dir() { "/" } else { "" };
        let tag = child.tag.map(|t| format!("  [{}]", t.id())).unwrap_or_default();
        let indent = "  ".repeat(depth + 1);
        out.push_str(&format!("{indent}{:>9}  {}{slash}{tag}\n", format::size(child.size), child.name));
        if depth + 1 < o.depth {
            tree_lines(child, depth + 1, o, out);
        }
    }
    let more = node.children.len().saturating_sub(o.top);
    if more > 0 {
        out.push_str(&format!("{}{:>9}  and {more} more\n", "  ".repeat(depth + 1), ""));
    }
}

pub fn to_text(tree: &DuNode, o: &Options) -> String {
    let mut out =
        format!("{:>9}  {}  ({})\n", format::size(tree.size), o.root.display(), format::items(tree.items as usize));
    if !o.dev {
        tree_lines(tree, 0, o, &mut out);
        return out;
    }
    let finds = trove_fs::dev_finds(tree);
    for f in &finds {
        out.push_str(&format!("{:>9}  {:<8}  {}\n", format::size(f.size), f.tag.id(), o.root.join(&f.path).display()));
    }
    if finds.is_empty() {
        out.push_str("no worktrees, build output, dependencies or caches\n");
    } else {
        out.push_str(&format!("{:>9}  reclaimable\n", format::size(reclaimable(&finds))));
    }
    out
}

fn print(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// Runs `trove du` and returns the process's exit code.
pub fn run(args: Vec<OsString>) -> i32 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(message) => {
            let asked = message.starts_with("trove du");
            if asked {
                let _ = print(&message);
            } else {
                eprintln!("trove du: {message}");
            }
            return if asked { 0 } else { 2 };
        }
    };
    let Some(tree) = trove_fs::du_walk(Path::new(&o.root)) else {
        eprintln!("trove du: {} can't be read", o.root.display());
        return 1;
    };
    let text = if o.json { format!("{}\n", to_json(&tree, &o)) } else { to_text(&tree, &o) };
    match print(&text) {
        Ok(()) => 0,
        // A reader that closed the pipe early, as `head` does, has what it wanted.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            eprintln!("trove du: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trove_fs::{DevTag, FileKind};

    fn file(name: &str, size: u64) -> DuNode {
        DuNode { name: name.into(), size, items: 0, kind: DuKind::File(FileKind::Unknown), tag: None, children: vec![] }
    }

    fn dir(name: &str, tag: Option<DevTag>, children: Vec<DuNode>) -> DuNode {
        let size = children.iter().map(|c| c.size).sum();
        let items = children.iter().map(|c| c.items + 1).sum();
        DuNode { name: name.into(), size, items, kind: DuKind::Dir, tag, children }
    }

    fn tree() -> DuNode {
        dir(
            "code",
            None,
            vec![
                dir(
                    "app-wt",
                    Some(DevTag::Worktree),
                    vec![dir("target", Some(DevTag::Build), vec![file("bin", 4_000_000)]), file("main.rs", 2_000)],
                ),
                dir("app", None, vec![dir("node_modules", Some(DevTag::Deps), vec![file("a.js", 1_000_000)])]),
                file("notes.md", 500),
            ],
        )
    }

    fn options(args: &[&str]) -> Options {
        let mut o = parse(args.iter().map(OsString::from).collect()).expect("options");
        o.root = PathBuf::from("/code");
        o
    }

    #[test]
    fn options_and_their_errors() {
        let o = parse(["--dev", "--depth", "4", "--top", "3", "/tmp"].map(OsString::from).to_vec()).expect("options");
        assert_eq!(o, Options { root: "/tmp".into(), dev: true, json: false, depth: 4, top: 3 });
        assert!(parse(vec!["--depth".into()]).is_err_and(|e| e == "--depth takes a number"));
        assert!(parse(vec!["--nope".into()]).is_err_and(|e| e.starts_with("unknown option --nope")));
        assert!(parse(vec!["--help".into()]).is_err_and(|e| e == HELP));
    }

    #[test]
    fn the_tree_prints_largest_first_to_the_depth_asked() {
        let text = to_text(&tree(), &options(&["--top", "2"]));
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(
            lines,
            [
                "5.0 MB  /code  (8 items)",
                "4.0 MB  app-wt/  [worktree]",
                "4.0 MB  target/  [build]",
                "2.0 kB  main.rs",
                "1.0 MB  app/",
                "1.0 MB  node_modules/  [deps]",
                "and 1 more",
            ]
        );
    }

    #[test]
    fn dev_lists_what_can_go_and_counts_nested_finds_once() {
        let text = to_text(&tree(), &options(&["--dev"]));
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert_eq!(
            lines,
            [
                "5.0 MB  /code  (8 items)",
                "4.0 MB  worktree  /code/app-wt",
                "4.0 MB  build     /code/app-wt/target",
                "1.0 MB  deps      /code/app/node_modules",
                "5.0 MB  reclaimable",
            ]
        );
        let v = to_json(&tree(), &options(&["--json", "--depth", "1"]));
        assert_eq!(v["reclaimable_bytes"], 5_002_000);
        assert_eq!(v["dev"][1]["path"], "/code/app-wt/target");
        assert_eq!(v["dev"][1]["kind"], "build");
        assert_eq!(v["children"][0]["tag"], "worktree");
        assert!(v["children"][0].get("children").is_none());
        assert_eq!(v["path"], "/code");
    }
}
