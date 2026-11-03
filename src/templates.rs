//! The templates folder as the "New from template" menu lists it. The folder is read off the
//! main thread and the last answer kept, so a menu never waits on the disk; every menu that
//! opens asks for a fresh read, which the next one shows.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nd::prelude::*;
use trove_fs::natural_cmp;

/// How many folders deep templates are listed; a link back up the tree stops here.
const DEPTH: usize = 4;

/// One entry of the templates folder: a file, or a folder of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// The name without its extension; a folder keeps its whole name.
    pub label: String,
    pub path: PathBuf,
    /// The templates inside, for a folder.
    pub children: Option<Vec<Node>>,
}

thread_local! {
    static CACHE: RefCell<Option<Rc<Vec<Node>>>> = const { RefCell::new(None) };
}

/// The last read of the templates folder, `None` until the first one lands.
pub fn cached() -> Option<Rc<Vec<Node>>> {
    CACHE.with(|c| c.borrow().clone())
}

/// Reads the templates folder again off the main thread.
pub fn refresh(cx: &mut App) {
    let dir = crate::env::get().templates.clone();
    let host = crate::ops::clip(cx);
    crate::ops::background(
        host.downgrade(),
        move || dir.map(|d| read(&d)).unwrap_or_default(),
        |_: &mut crate::ops::Clip, found, _| CACHE.with(|c| *c.borrow_mut() = Some(Rc::new(found))),
    );
}

/// The templates under `dir`: folders first, then files, each by name. A folder with no
/// template in it is left out, and so are hidden files.
pub fn read(dir: &Path) -> Vec<Node> {
    read_depth(dir, DEPTH)
}

fn read_depth(dir: &Path, depth: usize) -> Vec<Node> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(bool, String, Node)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.is_dir() {
            let children = if depth > 1 { read_depth(&path, depth - 1) } else { Vec::new() };
            if !children.is_empty() {
                found.push((true, name.clone(), Node { label: name, path, children: Some(children) }));
            }
        } else if meta.is_file() {
            let label = path.file_stem().map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
            found.push((false, name, Node { label, path, children: None }));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| natural_cmp(&a.1.to_lowercase(), &b.1.to_lowercase())));
    found.into_iter().map(|(_, _, n)| n).collect()
}
