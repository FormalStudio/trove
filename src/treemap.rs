//! The disk usage map's geometry: a squarified treemap (Bruls, Huizing and van Wijk), so every
//! item's rectangle has its share of the area and stays as close to a square as its row allows.
//! No UI types: the view turns the rectangles into paint.

use std::collections::HashMap;

use trove_fs::{DevTag, DuKind, DuNode};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn inset(self, by: f32) -> Self {
        Self::new(self.x + by, self.y + by, (self.w - 2. * by).max(0.), (self.h - 2. * by).max(0.))
    }
}

/// A rectangle in `area` for each of `sizes`, which come largest first. A size of zero gets an
/// empty rectangle.
pub fn squarify(sizes: &[u64], area: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::default(); sizes.len()];
    let total: f64 = sizes.iter().map(|s| *s as f64).sum();
    if total <= 0. || area.w <= 0. || area.h <= 0. {
        return out;
    }
    let scale = f64::from(area.w) * f64::from(area.h) / total;
    let (mut x, mut y, mut w, mut h) = (f64::from(area.x), f64::from(area.y), f64::from(area.w), f64::from(area.h));
    let mut i = 0;
    while i < sizes.len() {
        let side = w.min(h);
        let largest = sizes[i] as f64 * scale;
        if side <= 0. || largest <= 0. {
            break;
        }
        // The row grows while its worst aspect ratio keeps getting better.
        let (mut sum, mut best, mut j) = (0., f64::INFINITY, i);
        while j < sizes.len() {
            let a = sizes[j] as f64 * scale;
            if a <= 0. {
                break;
            }
            let s = sum + a;
            let worst = (side * side * largest / (s * s)).max(s * s / (side * side * a));
            if worst > best {
                break;
            }
            (best, sum, j) = (worst, s, j + 1);
        }
        let thick = sum / side;
        let mut along = 0.;
        for (k, rect) in out.iter_mut().enumerate().take(j).skip(i) {
            let len = sizes[k] as f64 * scale / thick;
            *rect = if w >= h {
                Rect::new(x as f32, (y + along) as f32, thick as f32, len as f32)
            } else {
                Rect::new((x + along) as f32, y as f32, len as f32, thick as f32)
            };
            along += len;
        }
        if w >= h {
            (x, w) = (x + thick, w - thick);
        } else {
            (y, h) = (y + thick, h - thick);
        }
        i = j;
    }
    out
}

/// How many extensions get a colour of their own; every other file shares one more.
pub const SLOTS: usize = 7;

/// The extensions that hold the most bytes in a measured tree, largest first: the map's colours
/// and its legend, as WinDirStat picks them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Legend {
    exts: Vec<(String, u64)>,
}

/// A name's extension, lower case; empty for a dot file or a name without one, which rank
/// together.
fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty() && !ext.is_empty() && ext.len() <= 12 && ext.chars().all(char::is_alphanumeric) =>
        {
            ext.to_lowercase()
        }
        _ => String::new(),
    }
}

impl Legend {
    pub fn of(tree: &DuNode) -> Self {
        let mut bytes: HashMap<String, u64> = HashMap::new();
        let mut todo = vec![tree];
        while let Some(node) = todo.pop() {
            todo.extend(&node.children);
            if let DuKind::File(_) = node.kind {
                *bytes.entry(extension(&node.name)).or_default() += node.size;
            }
        }
        let mut exts: Vec<(String, u64)> = bytes.into_iter().filter(|(_, size)| *size > 0).collect();
        exts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        exts.truncate(SLOTS);
        Self { exts }
    }

    /// The colour slot of a file: its extension's rank, or [`SLOTS`] for every other file.
    pub fn slot(&self, node: &DuNode) -> u8 {
        let ranked = match node.kind {
            DuKind::File(_) => {
                let ext = extension(&node.name);
                self.exts.iter().position(|(e, _)| *e == ext)
            }
            _ => None,
        };
        ranked.unwrap_or(SLOTS) as u8
    }

    /// Each ranked extension and the bytes its files hold; the empty one is the files that
    /// have none.
    pub fn entries(&self) -> &[(String, u64)] {
        &self.exts
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// A file, by its [`Legend::slot`].
    File(u8),
    /// A folder's own ground, lighter and darker by turns so nesting shows.
    Frame { odd: bool },
    /// A folder too small to show what is in it.
    Folder,
    /// In the developer's view: a folder that can be rebuilt or fetched again, as one block,
    /// or the ground of a worktree.
    Tag(DevTag),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub rect: Rect,
    pub paint: Paint,
}

/// One of the shown folder's own items: what a click or the pointer lands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub rect: Rect,
    /// Its index in the folder's children.
    pub child: usize,
    /// It is a folder large enough to carry its name in a strip along its top.
    pub titled: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    pub cells: Vec<Cell>,
    pub fills: Vec<Fill>,
}

/// The height of a titled folder's name strip.
pub const TITLE_H: f32 = 20.;
/// Smaller than this on a side, an item of the shown folder is not worth a rectangle of its own.
const MIN_CELL: f32 = 4.;
const MIN_NESTED: f32 = 6.;
const MAX_DEPTH: usize = 8;
/// Past this many rectangles the smallest folders stay closed.
const MAX_FILLS: usize = 20_000;

/// What the map colours by.
#[derive(Clone, Copy)]
pub enum Colouring<'a> {
    /// Files by the extensions that hold the most space.
    Extensions(&'a Legend),
    /// Worktrees, build output, dependencies and caches; every other file alike.
    Developer,
}

impl Colouring<'_> {
    fn file(self, node: &DuNode) -> Paint {
        match self {
            Colouring::Extensions(legend) => Paint::File(legend.slot(node)),
            Colouring::Developer => Paint::File(SLOTS as u8),
        }
    }

    /// The tag `folder` is painted with, and whether that is the whole of it.
    fn folder(self, folder: &DuNode) -> Option<(DevTag, bool)> {
        match (self, folder.tag) {
            (Colouring::Developer, Some(tag)) => Some((tag, tag != DevTag::Worktree)),
            _ => None,
        }
    }
}

/// The map of `folder` in a `w` by `h` area.
pub fn build(folder: &DuNode, by: Colouring, w: f32, h: f32) -> Map {
    let mut map = Map::default();
    let sizes: Vec<u64> = folder.children.iter().map(|c| c.size).collect();
    for (child, rect) in squarify(&sizes, Rect::new(0., 0., w, h)).into_iter().enumerate() {
        if rect.w < MIN_CELL || rect.h < MIN_CELL {
            break;
        }
        let node = &folder.children[child];
        let titled = node.is_dir() && rect.w >= 72. && rect.h >= 2. * TITLE_H;
        map.cells.push(Cell { rect, child, titled });
        let body = rect.inset(0.5);
        if !node.is_dir() {
            map.fills.push(Fill { rect: body, paint: by.file(node) });
            continue;
        }
        let tagged = by.folder(node);
        let ground = tagged.map_or(Paint::Frame { odd: false }, |(tag, _)| Paint::Tag(tag));
        map.fills.push(Fill { rect: body, paint: ground });
        if tagged.is_some_and(|(_, whole)| whole) {
            continue;
        }
        let inner = if titled {
            Rect::new(body.x, body.y + TITLE_H, body.w, (body.h - TITLE_H).max(0.)).inset(1.)
        } else {
            body.inset(1.)
        };
        nest(node, by, inner, 1, &mut map.fills);
    }
    map
}

fn nest(folder: &DuNode, by: Colouring, area: Rect, depth: usize, fills: &mut Vec<Fill>) {
    let sizes: Vec<u64> = folder.children.iter().map(|c| c.size).collect();
    for (node, rect) in folder.children.iter().zip(squarify(&sizes, area)) {
        if rect.w < 1. && rect.h < 1. {
            break;
        }
        let body = rect.inset(0.5);
        let tagged = by.folder(node);
        if !node.is_dir() {
            fills.push(Fill { rect: body, paint: by.file(node) });
        } else if let Some((tag, true)) = tagged {
            fills.push(Fill { rect: body, paint: Paint::Tag(tag) });
        } else if depth < MAX_DEPTH && rect.w >= MIN_NESTED && rect.h >= MIN_NESTED && fills.len() < MAX_FILLS {
            let ground = tagged.map_or(Paint::Frame { odd: depth % 2 == 1 }, |(tag, _)| Paint::Tag(tag));
            fills.push(Fill { rect: body, paint: ground });
            nest(node, by, body.inset(1.), depth + 1, fills);
        } else {
            fills.push(Fill { rect: body, paint: tagged.map_or(Paint::Folder, |(tag, _)| Paint::Tag(tag)) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trove_fs::FileKind;

    fn file(name: &str, size: u64) -> DuNode {
        DuNode {
            name: name.into(),
            size,
            items: 0,
            kind: DuKind::File(FileKind::from_name(name)),
            tag: None,
            children: vec![],
        }
    }

    fn dir(name: &str, children: Vec<DuNode>) -> DuNode {
        let size = children.iter().map(|c| c.size).sum();
        DuNode { name: name.into(), size, items: children.len() as u64, kind: DuKind::Dir, tag: None, children }
    }

    #[test]
    fn areas_follow_sizes_and_fill_the_area() {
        let sizes = [600, 300, 200, 100, 50, 30, 20, 0];
        let area = Rect::new(10., 20., 400., 300.);
        let rects = squarify(&sizes, area);
        let total: u64 = sizes.iter().sum();
        let mut covered = 0.;
        for (size, r) in sizes.iter().zip(&rects).take(7) {
            let want = *size as f32 / total as f32 * area.w * area.h;
            assert!((r.w * r.h - want).abs() < 1., "{size}: {r:?}");
            assert!(r.x >= area.x - 0.01 && r.y >= area.y - 0.01, "{r:?}");
            assert!(r.x + r.w <= area.x + area.w + 0.01 && r.y + r.h <= area.y + area.h + 0.01, "{r:?}");
            covered += r.w * r.h;
        }
        assert!((covered - area.w * area.h).abs() < 1.);
        assert_eq!(rects[7], Rect::default());
    }

    #[test]
    fn rectangles_stay_near_square_and_apart() {
        let sizes = [500, 400, 300, 200, 100, 100, 100, 50, 50];
        let rects = squarify(&sizes, Rect::new(0., 0., 600., 400.));
        for r in &rects {
            let ratio = (r.w / r.h).max(r.h / r.w);
            assert!(ratio < 3., "{r:?}");
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let apart = a.x + a.w <= b.x + 0.01
                    || b.x + b.w <= a.x + 0.01
                    || a.y + a.h <= b.y + 0.01
                    || b.y + b.h <= a.y + 0.01;
                assert!(apart, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn nothing_to_lay_out() {
        assert!(squarify(&[], Rect::new(0., 0., 10., 10.)).is_empty());
        assert_eq!(squarify(&[0, 0], Rect::new(0., 0., 10., 10.)), [Rect::default(); 2]);
        assert_eq!(squarify(&[5], Rect::new(0., 0., 0., 10.)), [Rect::default()]);
    }

    #[test]
    fn the_map_names_big_folders_and_colours_files_by_extension() {
        let tree = dir(
            "home",
            vec![
                dir("Videos", vec![file("a.mkv", 4000), file("b.MKV", 3000), dir("old", vec![file("c.avi", 900)])]),
                file("disk.iso", 1500),
                dir("tiny", vec![file("n.txt", 1), file(".hidden", 1), file("Makefile", 1)]),
            ],
        );
        let legend = Legend::of(&tree);
        let ranked: Vec<(&str, u64)> = legend.entries().iter().map(|(e, b)| (e.as_str(), *b)).collect();
        assert_eq!(ranked, [("mkv", 7000), ("iso", 1500), ("avi", 900), ("", 2), ("txt", 1)]);
        assert_eq!(legend.slot(&file("x.iso", 1)), 1);
        assert_eq!(legend.slot(&file("x.bin", 1)), SLOTS as u8);
        assert_eq!(legend.slot(&file(".iso", 1)), 3);
        assert_eq!(legend.slot(&dir("x.mkv", vec![])), SLOTS as u8);
        let map = build(&tree, Colouring::Extensions(&legend), 800., 500.);
        assert_eq!(map.cells.len(), 2);
        assert!(map.cells[0].titled);
        assert!(!map.cells[1].titled);
        let painted = |p: Paint| map.fills.iter().filter(|f| f.paint == p).count();
        assert_eq!(painted(Paint::File(0)), 2);
        assert_eq!(painted(Paint::File(1)), 1);
        assert_eq!(painted(Paint::File(2)), 1);
        assert_eq!(painted(Paint::Frame { odd: true }), 1);
        let first = map.cells[0].rect;
        let inside = map.fills.iter().filter(|f| f.paint == Paint::File(0)).all(|f| {
            f.rect.y >= first.y + TITLE_H && f.rect.x >= first.x && f.rect.x + f.rect.w <= first.x + first.w + 0.01
        });
        assert!(inside);
        assert!(build(&tree, Colouring::Extensions(&legend), 0., 0.).cells.is_empty());
    }

    #[test]
    fn the_developer_map_paints_tagged_folders_whole_and_worktrees_as_ground() {
        let tagged = |name: &str, tag, children| DuNode { tag: Some(tag), ..dir(name, children) };
        let tree = dir(
            "code",
            vec![
                tagged(
                    "fix",
                    DevTag::Worktree,
                    vec![tagged("target", DevTag::Build, vec![file("app", 5000)]), file("a.rs", 900)],
                ),
                tagged("node_modules", DevTag::Deps, vec![file("x.js", 3000), file("y.js", 2000)]),
            ],
        );
        let map = build(&tree, Colouring::Developer, 800., 500.);
        let painted = |p: Paint| map.fills.iter().filter(|f| f.paint == p).count();
        assert_eq!(painted(Paint::Tag(DevTag::Worktree)), 1);
        assert_eq!(painted(Paint::Tag(DevTag::Build)), 1);
        assert_eq!(painted(Paint::Tag(DevTag::Deps)), 1);
        assert_eq!(painted(Paint::File(SLOTS as u8)), 1);
        assert_eq!(map.fills.len(), 4);
    }
}
