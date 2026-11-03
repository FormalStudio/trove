//! Batch rename: the names a rule gives a set of items, and what stops any of them. The plan is
//! what the sheet previews and what `Op::RenameMany` then runs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::job::name_of;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Every match of `find` in the name replaced by `with`; with `regex`, `find` is a regular
    /// expression and `with` can name its groups (`$1`).
    Replace { find: String, with: String, regex: bool },
    /// "base 01", "base 02", ... in the items' order, each file keeping its extension.
    Number { base: String, start: u64, digits: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    Empty,
    Slash,
    Dots,
    /// Something else in the folder already has the name.
    Taken,
    /// Two items would get the same name.
    Twice,
}

impl Problem {
    pub fn message(self) -> &'static str {
        match self {
            Problem::Empty => "The name is empty",
            Problem::Slash => "Names can\u{2019}t contain \u{201c}/\u{201d}",
            Problem::Dots => "Not a valid name",
            Problem::Taken => "Already exists",
            Problem::Twice => "Two items get this name",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub path: PathBuf,
    pub name: String,
    pub problem: Option<Problem>,
}

impl Planned {
    pub fn changed(&self) -> bool {
        name_of(&self.path) != self.name
    }
}

/// The names already in each item's folder, read from disk.
pub fn siblings(items: &[Item]) -> HashMap<PathBuf, HashSet<String>> {
    let mut out: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for dir in items.iter().filter_map(|i| i.path.parent()) {
        if out.contains_key(dir) {
            continue;
        }
        let names = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        out.insert(dir.to_path_buf(), names);
    }
    out
}

/// What `rule` makes of each item's name, in order. `Err` when the rule's pattern does not
/// compile.
pub fn plan(items: &[Item], rule: &Rule, siblings: &HashMap<PathBuf, HashSet<String>>) -> Result<Vec<Planned>, String> {
    let pattern = match rule {
        Rule::Replace { find, regex: true, .. } if !find.is_empty() => {
            Some(Regex::new(find).map_err(|e| first_line(&e.to_string()))?)
        }
        _ => None,
    };
    let mut planned: Vec<Planned> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let old = name_of(&item.path);
            let made = match (rule, &pattern) {
                (Rule::Replace { find, .. }, _) if find.is_empty() => old.clone(),
                (Rule::Replace { with, .. }, Some(re)) => re.replace_all(&old, with.as_str()).into_owned(),
                (Rule::Replace { find, with, .. }, None) => old.replace(find.as_str(), with),
                (Rule::Number { base, start, digits }, _) => {
                    let n = start.saturating_add(i as u64);
                    let number = format!("{n:0digits$}");
                    let ext = if item.is_dir { "" } else { extension(&old) };
                    let base = base.trim();
                    if base.is_empty() { format!("{number}{ext}") } else { format!("{base} {number}{ext}") }
                }
            };
            // The rename trims, so the preview shows what it will make.
            let name = if made == old { made } else { made.trim().to_owned() };
            Planned { path: item.path.clone(), name, problem: None }
        })
        .collect();

    let mut counts: HashMap<(PathBuf, String), usize> = HashMap::new();
    for p in planned.iter().filter(|p| p.changed()) {
        let dir = p.path.parent().map(Path::to_path_buf).unwrap_or_default();
        *counts.entry((dir, p.name.clone())).or_default() += 1;
    }
    let empty = HashSet::new();
    for p in planned.iter_mut().filter(|p| p.changed()) {
        let dir = p.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let taken = siblings.get(&dir).unwrap_or(&empty);
        p.problem = if p.name.is_empty() {
            Some(Problem::Empty)
        } else if p.name.contains('/') || p.name.contains('\0') {
            Some(Problem::Slash)
        } else if p.name == "." || p.name == ".." {
            Some(Problem::Dots)
        } else if counts.get(&(dir, p.name.clone())).is_some_and(|n| *n > 1) {
            Some(Problem::Twice)
        } else if taken.contains(&p.name) && !same_but_case(&p.path, &p.name) {
            Some(Problem::Taken)
        } else {
            None
        };
    }
    Ok(planned)
}

/// The renames a plan asks for, when none of them has a problem and at least one changes.
pub fn renames(plan: &[Planned]) -> Option<Vec<(PathBuf, String)>> {
    if plan.iter().any(|p| p.problem.is_some()) {
        return None;
    }
    let out: Vec<(PathBuf, String)> =
        plan.iter().filter(|p| p.changed()).map(|p| (p.path.clone(), p.name.clone())).collect();
    (!out.is_empty()).then_some(out)
}

/// A rename that only changes the case of the name finds its own old name in the folder.
fn same_but_case(path: &Path, name: &str) -> bool {
    name_of(path).to_lowercase() == name.to_lowercase()
}

/// ".jpg" of "photo.jpg"; nothing for ".bashrc" or "notes".
fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

fn first_line(s: &str) -> String {
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(s).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item { path: PathBuf::from("/d").join(n), is_dir: !n.contains('.') }).collect()
    }

    fn names(plan: &[Planned]) -> Vec<&str> {
        plan.iter().map(|p| p.name.as_str()).collect()
    }

    fn folder(names: &[&str]) -> HashMap<PathBuf, HashSet<String>> {
        HashMap::from([(PathBuf::from("/d"), names.iter().map(|n| (*n).to_owned()).collect())])
    }

    #[test]
    fn plain_and_regex_replace() {
        let it = items(&["IMG_001.jpg", "IMG_002.jpg", "notes.txt"]);
        let all = folder(&["IMG_001.jpg", "IMG_002.jpg", "notes.txt"]);
        let rule = Rule::Replace { find: "IMG_".into(), with: "Holiday ".into(), regex: false };
        let p = plan(&it, &rule, &all).unwrap();
        assert_eq!(names(&p), ["Holiday 001.jpg", "Holiday 002.jpg", "notes.txt"]);
        assert!(!p[2].changed());
        assert_eq!(renames(&p).unwrap().len(), 2);

        let rule = Rule::Replace { find: r"IMG_0*(\d+)".into(), with: "Photo $1".into(), regex: true };
        assert_eq!(names(&plan(&it, &rule, &all).unwrap()), ["Photo 1.jpg", "Photo 2.jpg", "notes.txt"]);

        let bad = Rule::Replace { find: "(".into(), with: String::new(), regex: true };
        assert!(plan(&it, &bad, &all).is_err());
    }

    #[test]
    fn numbering_keeps_extensions_and_pads() {
        let it = items(&["b.png", "a.jpg", "Album", ".bashrc"]);
        let rule = Rule::Number { base: "Trip".into(), start: 9, digits: 2 };
        let p = plan(&it, &rule, &folder(&[])).unwrap();
        assert_eq!(names(&p), ["Trip 09.png", "Trip 10.jpg", "Trip 11", "Trip 12"]);
        let rule = Rule::Number { base: " ".into(), start: 1, digits: 3 };
        assert_eq!(plan(&it, &rule, &folder(&[])).unwrap()[0].name, "001.png");
    }

    #[test]
    fn conflicts_are_flagged() {
        let it = items(&["a.txt", "b.txt", "keep.txt"]);
        let all = folder(&["a.txt", "b.txt", "keep.txt", "c.txt"]);
        let to_c = Rule::Replace { find: r"^[ab]".into(), with: "c".into(), regex: true };
        let p = plan(&it, &to_c, &all).unwrap();
        assert_eq!(p[0].problem, Some(Problem::Twice));
        assert_eq!(p[1].problem, Some(Problem::Twice));
        assert_eq!(p[2].problem, None);
        assert!(renames(&p).is_none());

        let onto_keep = Rule::Replace { find: "a".into(), with: "keep".into(), regex: false };
        assert_eq!(plan(&it, &onto_keep, &all).unwrap()[0].problem, Some(Problem::Taken));

        let case = Rule::Replace { find: "a".into(), with: "A".into(), regex: false };
        assert_eq!(plan(&it, &case, &folder(&["a.txt", "A.txt"])).unwrap()[0].problem, None);

        let gone = Rule::Replace { find: "a.txt".into(), with: String::new(), regex: false };
        assert_eq!(plan(&it, &gone, &all).unwrap()[0].problem, Some(Problem::Empty));
        let slash = Rule::Replace { find: "a".into(), with: "x/y".into(), regex: false };
        assert_eq!(plan(&it, &slash, &all).unwrap()[0].problem, Some(Problem::Slash));
    }

    #[test]
    fn nothing_changed_is_no_rename() {
        let it = items(&["a.txt"]);
        let rule = Rule::Replace { find: "zzz".into(), with: "y".into(), regex: false };
        assert!(renames(&plan(&it, &rule, &folder(&["a.txt"])).unwrap()).is_none());
    }
}
