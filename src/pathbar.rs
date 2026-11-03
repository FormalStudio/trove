//! The path bar: breadcrumbs of the active tab's location, Home named rather than spelled out,
//! the middle folded into a menu when the path runs long. Ctrl+L or a click past the last
//! segment turns it into a field holding the path, with Tab completing the last component from
//! a readdir off the main thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nd::prelude::*;
use trove_fs::Volume;

use crate::actions::{DRAG_FILES, DropOnto, FocusFiles, GoTo, OpenPlaceInNewTab, PATH_EDIT};
use crate::fileops::Transfer;
use crate::icons;
use crate::location::Location;
use crate::pump::pump;

type OnGo = Rc<dyn Fn(Location, &mut Window, &mut App)>;

/// More segments than this fold the middle ones into the menu.
const MAX_SEGMENTS: usize = 5;
/// Rough widths for fitting crumbs before they are laid out; generous, so a crumb that does not
/// fit folds rather than clips. A character, a button's padding, an icon, a chevron, the "..."
/// button, and the room kept for the click target that opens the field.
const CHAR_W: f32 = 8.;
const CRUMB_PAD: f32 = 24.;
const ICON_W: f32 = 22.;
const CHEVRON_W: f32 = 20.;
const MORE_W: f32 = 36.;
const END_W: f32 = 24.;
/// The most folders a segment's menu lists beside it.
const MAX_SIBLINGS: usize = 100;
pub const FIELD: &str = "path-edit";

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub label: String,
    pub location: Location,
    pub icon: Option<Icon>,
}

pub struct PathBar {
    location: Location,
    editing: bool,
    typed: SharedString,
    completion: u64,
    volumes: Vec<Volume>,
    on_go: OnGo,
    /// The subfolders of each folder on the path, for the segments' menus, and the location
    /// they were read for.
    siblings: HashMap<PathBuf, Vec<String>>,
    siblings_of: Option<Location>,
    /// The bar's laid-out width, once it has one.
    width: Option<f32>,
}

impl PathBar {
    pub fn new(location: Location, on_go: impl Fn(Location, &mut Window, &mut App) + 'static) -> Self {
        Self {
            location,
            editing: false,
            typed: SharedString::default(),
            completion: 0,
            volumes: Vec::new(),
            on_go: Rc::new(on_go),
            siblings: HashMap::new(),
            siblings_of: None,
            width: None,
        }
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if width > 1. && self.width.is_none_or(|w| (w - width).abs() > 0.5) {
            self.width = Some(width);
            cx.notify();
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// The mounts a path can start in, so a path on a volume begins with the volume's name.
    pub fn set_volumes(&mut self, volumes: &[Volume], cx: &mut Context<Self>) {
        if self.volumes != volumes {
            self.volumes = volumes.to_vec();
            cx.notify();
        }
    }

    pub fn set_location(&mut self, location: &Location, cx: &mut Context<Self>) {
        if self.location != *location {
            self.location = location.clone();
            self.editing = false;
            cx.notify();
        }
    }

    pub fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        self.typed = match &self.location {
            Location::Dir(p) => with_slash(p).into(),
            Location::Recent => SharedString::default(),
        };
        window.focus(FIELD);
        cx.notify();
    }

    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            self.editing = false;
            cx.dispatch_action(window.id(), &FocusFiles);
            cx.notify();
        }
    }

    pub fn set_typed(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.typed = text;
        self.completion += 1;
        cx.notify();
    }

    fn submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = resolve(text, self.location.dir()) else { return };
        self.editing = false;
        cx.notify();
        let on_go = self.on_go.clone();
        on_go(Location::Dir(path), window, cx);
    }

    /// Tab: completes the last component to the longest prefix the folder's subfolders share.
    pub fn complete(&mut self, cx: &mut Context<Self>) {
        let Some(path) = resolve(&self.typed, self.location.dir()) else { return };
        let (dir, prefix) = if self.typed.ends_with('/') {
            (path, String::new())
        } else {
            let prefix = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")), prefix)
        };
        self.completion += 1;
        let ticket = self.completion;
        let (tx, rx) = flume::bounded(1);
        let _ = std::thread::Builder::new().name("trove-complete".into()).spawn(move || {
            let _ = tx.send(complete_in(&dir, &prefix));
        });
        pump(rx, cx.weak_entity(), move |this: &mut PathBar, done, cx| {
            if let Some(Some(completed)) = done.into_iter().last()
                && this.completion == ticket
                && this.editing
            {
                this.typed = completed.into();
                cx.notify();
            }
            false
        });
    }

    pub fn segments(&self) -> Vec<Segment> {
        segments(&self.location, &self.volumes)
    }

    /// The folders beside `dir`, its own name among them; `None` until they have been read.
    pub fn siblings(&self, dir: &Path) -> Option<&[String]> {
        self.siblings.get(dir.parent()?).map(Vec::as_slice)
    }

    /// Reads the subfolders of every folder above the location, off the main thread, once per
    /// location.
    fn read_siblings(&mut self, cx: &mut Context<Self>) {
        if self.siblings_of.as_ref() == Some(&self.location) {
            return;
        }
        self.siblings_of = Some(self.location.clone());
        let Some(dir) = self.location.dir() else { return };
        let parents: Vec<PathBuf> = dir.ancestors().skip(1).map(Path::to_path_buf).collect();
        let current = dir.to_path_buf();
        let asked = self.location.clone();
        crate::ops::background(
            cx.weak_entity(),
            move || parents.into_iter().map(|p| (subfolders(&p, &current), p)).collect::<Vec<_>>(),
            move |this: &mut PathBar, found, cx| {
                if this.siblings_of.as_ref() == Some(&asked) {
                    this.siblings = found.into_iter().map(|(names, p)| (p, names)).collect();
                    cx.notify();
                }
            },
        );
    }
}

/// `dir`'s subfolders in natural order, the hidden ones left out unless `on` passes through one.
fn subfolders(dir: &Path, on: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) || e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') || on.starts_with(dir.join(n)))
        .collect();
    names.sort_by(|a, b| trove_fs::natural_cmp(a, b));
    names.truncate(MAX_SIBLINGS);
    names
}

/// A segment's right-click menu, read from the path bar when it opens.
fn crumb_menu(bar: WeakEntity<PathBar>, dir: PathBuf) -> impl Fn(ContextMenu, &mut Window, &mut App) -> ContextMenu {
    move |menu, _, cx| {
        let siblings = bar.upgrade().and_then(|b| b.read(cx).siblings(&dir).map(<[String]>::to_vec));
        crate::menus::crumb(menu, &dir, siblings.as_deref())
    }
}

/// `typed` as an absolute path: `~` is home, a relative path starts at `cwd`.
fn resolve(typed: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    let home = &crate::env::get().home;
    let path = if typed == "~" {
        home.clone()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home.join(rest)
    } else if typed.starts_with('/') {
        PathBuf::from(typed)
    } else {
        cwd?.join(typed)
    };
    Some(path)
}

fn with_slash(p: &Path) -> String {
    let s = p.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

/// The completed path for `prefix` among `dir`'s subfolders: exact case first, then any case.
fn complete_in(dir: &Path, prefix: &str) -> Option<String> {
    let names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) || e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') || prefix.starts_with('.'))
        .collect();
    let exact: Vec<&String> = names.iter().filter(|n| n.starts_with(prefix)).collect();
    let lower = prefix.to_lowercase();
    let matches =
        if exact.is_empty() { names.iter().filter(|n| n.to_lowercase().starts_with(&lower)).collect() } else { exact };
    let first = matches.first()?;
    let common = matches.iter().skip(1).fold(first.as_str(), |acc, n| common_prefix(acc, n));
    let base = with_slash(dir);
    Some(if matches.len() == 1 { format!("{base}{common}/") } else { format!("{base}{common}") })
}

fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a
        .char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((i, _), _)| i);
    &a[..end]
}

/// The breadcrumbs for `location`: the place it sits in (Home, Trash, a volume, the
/// computer), then every folder down to it.
pub fn segments(location: &Location, volumes: &[Volume]) -> Vec<Segment> {
    let Location::Dir(path) = location else {
        return vec![Segment { label: "Recent".into(), location: Location::Recent, icon: Some(icons::recent()) }];
    };
    let env = crate::env::get();
    let mount = volumes
        .iter()
        .filter(|v| v.mount_point != Path::new("/") && path.starts_with(&v.mount_point))
        .max_by_key(|v| v.mount_point.components().count());
    let (base, label, icon) = if path.starts_with(&env.trash) {
        (env.trash.clone(), "Trash".to_owned(), Icon::Trash)
    } else if path.starts_with(&env.home) {
        (env.home.clone(), "Home".to_owned(), Icon::House)
    } else if let Some(v) = mount {
        let label = if v.label.is_empty() {
            v.mount_point.file_name().map_or_else(|| "Volume".to_owned(), |n| n.to_string_lossy().into_owned())
        } else {
            v.label.clone()
        };
        (v.mount_point.clone(), label, icons::for_volume(v.kind))
    } else {
        (PathBuf::from("/"), "Computer".to_owned(), icons::for_volume(trove_fs::VolumeKind::Root))
    };
    let mut out = vec![Segment { label, location: Location::Dir(base.clone()), icon: Some(icon) }];
    let mut at = base.clone();
    if let Ok(rest) = path.strip_prefix(&base) {
        for c in rest.components() {
            at.push(c);
            out.push(Segment {
                label: c.as_os_str().to_string_lossy().into_owned(),
                location: Location::Dir(at.clone()),
                icon: None,
            });
        }
    }
    out
}

fn crumb_width(seg: &Segment) -> f32 {
    CHEVRON_W + CRUMB_PAD + CHAR_W * seg.label.chars().count() as f32 + if seg.icon.is_some() { ICON_W } else { 0. }
}

/// Which segments show and which fold into the menu. The current folder always shows. Crumbs
/// fold from the middle outwards, the ones next to the current folder last, then the first one
/// goes too. Without a width yet, the first and the last few show.
fn fold(segments: &[Segment], width: Option<f32>) -> (Vec<(usize, &Segment)>, Vec<&Segment>) {
    let n = segments.len();
    let tail_for = |with_first: bool, budget: f32| -> usize {
        let cap = if with_first { MAX_SEGMENTS - 1 } else { MAX_SEGMENTS }.min(n);
        let first = if with_first { crumb_width(&segments[0]) } else { 0. };
        let mut t = 1;
        let mut used = first + crumb_width(&segments[n - 1]);
        while t < cap {
            let next = used + crumb_width(&segments[n - 1 - t]);
            let hidden = n - t - 1 > usize::from(with_first);
            if next + if hidden { MORE_W } else { 0. } > budget {
                break;
            }
            used = next;
            t += 1;
        }
        t
    };
    if n <= 1 {
        return (segments.iter().enumerate().collect(), Vec::new());
    }
    let Some(budget) = width.map(|w| w - END_W) else {
        if n <= MAX_SEGMENTS {
            return (segments.iter().enumerate().collect(), Vec::new());
        }
        let cut = n - (MAX_SEGMENTS - 2);
        let shown = std::iter::once((0, &segments[0])).chain(segments.iter().enumerate().skip(cut)).collect();
        return (shown, segments[1..cut].iter().collect());
    };
    let all: f32 = segments.iter().map(crumb_width).sum();
    if n <= MAX_SEGMENTS && all <= budget {
        return (segments.iter().enumerate().collect(), Vec::new());
    }
    let with_first = crumb_width(&segments[0]) + MORE_W + crumb_width(&segments[n - 1]) <= budget;
    let t = tail_for(with_first, budget).min(n - usize::from(with_first));
    let cut = n - t;
    let mut shown: Vec<(usize, &Segment)> = Vec::new();
    if with_first {
        shown.push((0, &segments[0]));
    }
    shown.extend(segments.iter().enumerate().skip(cut));
    let folded = segments[usize::from(with_first)..cut].iter().collect();
    (shown, folded)
}

impl Render for PathBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.read_siblings(cx);
        let theme = cx.theme();
        if self.editing {
            let (change, submit) = (cx.weak_entity(), cx.weak_entity());
            return h_flex()
                .min_w(px(200.))
                .flex_1()
                .child(
                    text_field(FIELD)
                        .text(self.typed.clone())
                        .placeholder("Go to a folder")
                        .key_context(PATH_EDIT)
                        .flex_1()
                        .on_change(move |e: &TextChanged, _, cx| {
                            let _ = change.update(cx, |this, cx| this.set_typed(e.text.clone(), cx));
                        })
                        .on_submit(move |e: &TextSubmitted, window, cx| {
                            let _ = submit.update(cx, |this, cx| this.submit(&e.text, window, cx));
                        }),
                )
                .into_any_element();
        }
        let segments = self.segments();
        let (shown, folded) = fold(&segments, self.width);
        let last = segments.len().saturating_sub(1);
        let resized = cx.weak_entity();
        let mut row = h_flex()
            .id("pathbar")
            .min_w(px(120.))
            .flex_1()
            .overflow_hidden()
            .items_center()
            .gap(theme.spacing.xs)
            .on_resize(move |e, _, cx| {
                let _ = resized.update(cx, |this, cx| this.set_width(e.width, cx));
            });
        let mut previous: Option<usize> = None;
        for (n, (i, seg)) in shown.iter().enumerate() {
            if n > 0 {
                row = row.child(icon(Icon::ChevronRight, 12.).text_color(theme.text.tertiary));
            }
            let gap = if n == 0 { *i > 0 } else { previous.is_some_and(|p| p + 1 < *i) };
            previous = Some(*i);
            if gap && !folded.is_empty() {
                let items: Vec<(String, PathBuf)> =
                    folded.iter().filter_map(|s| Some((s.label.clone(), s.location.dir()?.to_path_buf()))).collect();
                row = row
                    .child(
                        menu_button("crumb-more", Icon::Ellipsis, move |mut menu, _, _| {
                            for (label, path) in &items {
                                menu = menu.item(label.clone(), GoTo(path.clone()));
                            }
                            menu
                        })
                        .tooltip("Enclosing folders"),
                    )
                    .child(icon(Icon::ChevronRight, 12.).text_color(theme.text.tertiary));
            }
            if *i == last {
                let menu = seg.location.dir().map(|d| crumb_menu(cx.weak_entity(), d.to_path_buf()));
                row = row.child(
                    h_flex()
                        .id("crumb-current")
                        .when_some(menu, |d, menu| d.context_menu(menu))
                        .gap(theme.spacing.xs)
                        .px(theme.spacing.xs)
                        .when_some(seg.icon, |d, glyph| d.child(icon(glyph, 16.).text_color(theme.text.primary)))
                        .child(
                            text(seg.label.clone())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text.primary)
                                .truncate(),
                        ),
                );
            } else {
                let on_go = self.on_go.clone();
                let to = seg.location.clone();
                let mut b = button(format!("crumb-{i}"), seg.label.clone()).borderless();
                if let Some(glyph) = seg.icon {
                    b = b.icon(glyph);
                }
                let b = b.on_click(move |_: &ClickEvent, window, cx| on_go(to.clone(), window, cx));
                let crumb = div().id(format!("crumb-drop-{i}")).flex_shrink_0().child(b);
                row = row.child(match seg.location.dir().map(Path::to_path_buf) {
                    Some(dest) => {
                        let tab_dest = dest.clone();
                        crumb
                            .context_menu(crumb_menu(cx.weak_entity(), dest.clone()))
                            .on_middle_click(move |_: &MiddleClickEvent, window, cx| {
                                cx.dispatch_action(window.id(), &OpenPlaceInNewTab(Location::Dir(tab_dest.clone())));
                            })
                            .accepts_drops([DRAG_FILES, DragItem::FILES])
                            .on_drop(move |e: &Dropped, window, cx| {
                                let paths = e.item.ids.iter().map(|id| PathBuf::from(id.as_ref())).collect();
                                let mode = Transfer::from_modifiers(e.modifiers);
                                cx.dispatch_action(window.id(), &DropOnto { paths, dest: dest.clone(), mode });
                            })
                    }
                    None => crumb,
                });
            }
        }
        let edit = cx.weak_entity();
        row.child(div().id("pathbar-end").flex_1().h(theme.control.regular).on_tap(move |_: &TapEvent, window, cx| {
            let _ = edit.update(cx, |this, cx| this.edit(window, cx));
        }))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefixes() {
        assert_eq!(common_prefix("Documents", "Downloads"), "Do");
        assert_eq!(common_prefix("Music", "Music"), "Music");
        assert_eq!(common_prefix("a", ""), "");
    }

    fn seg(l: &str) -> Segment {
        Segment { label: l.into(), location: Location::Recent, icon: None }
    }

    fn labels(v: &[(usize, &Segment)]) -> Vec<String> {
        v.iter().map(|(_, s)| s.label.clone()).collect()
    }

    #[test]
    fn folding_keeps_the_first_and_the_last_three() {
        let all: Vec<Segment> = ["Home", "a", "b", "c", "d", "e", "f"].map(seg).to_vec();
        let (shown, folded) = fold(&all, None);
        let folded: Vec<&str> = folded.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels(&shown), ["Home", "d", "e", "f"]);
        assert_eq!(folded, ["a", "b", "c"]);
    }

    #[test]
    fn the_current_folder_shows_at_every_width() {
        let all: Vec<Segment> = ["Home", "Projects", "trove", "picker-build", "files"].map(seg).to_vec();
        let mut last_shown = 0;
        for width in (120..1400).step_by(8) {
            let (shown, folded) = fold(&all, Some(width as f32));
            assert_eq!(shown.last().map(|(_, s)| s.label.as_str()), Some("files"), "width {width}");
            assert_eq!(shown.len() + folded.len(), all.len(), "width {width}");
            assert!(shown.len() >= last_shown, "width {width} shows fewer than a narrower bar");
            last_shown = shown.len();
        }
        let (shown, _) = fold(&all, Some(180.));
        assert_eq!(labels(&shown), ["files"]);
        let (shown, folded) = fold(&all, Some(400.));
        assert_eq!(labels(&shown), ["Home", "picker-build", "files"]);
        assert_eq!(folded.len(), 2);
    }
}
