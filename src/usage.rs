//! The Disk Usage window: what fills a folder or a volume, as a list of its items largest first
//! beside a map where every file is a rectangle of its share of the space, coloured by kind.
//! A double click on a folder shows that folder alone; the path above the list leads back out.
//! The walk runs on `trove_fs::du_scan`'s threads and the window shows its count until it ends.
//! The developer's view lists what can be rebuilt or fetched again instead: linked worktrees,
//! build output, dependencies and caches, wherever they sit under the root.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nd::core::WindowId;
use nd::prelude::*;
use trove_fs::{DevTag, DuEvent, DuFound, DuKind, DuNode, DuProgress, FileKind};
use trove_ops::{JobId, Op, OpEvent};

use crate::actions::CopyPathOf;
use crate::format;
use crate::icons;
use crate::ops::{self, Origin};
use crate::pump::pump;
use crate::root::Start;
use crate::theme::{slot_color, tag_color};
use crate::treemap::{self, Cell, Colouring, Legend, Map, Paint, SLOTS, TITLE_H};

/// A file shown in its folder in a new window, or a folder opened in one.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct ShowInTrove(pub PathBuf);

/// One of the shown folder's items to the Trash, after asking.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = trove, no_json)]
pub struct TrashMeasured(pub PathBuf);

const ROW_H: f32 = 30.;
const LIST_W: f32 = 320.;
const BAR_W: f32 = 48.;
/// A file's rectangle carries its name from this size up.
const NAMED_FILE: (f32, f32) = (96., 40.);
const VIEWS: [&str; 2] = ["Kinds", "Developer"];

#[derive(Clone, Debug, PartialEq)]
pub enum Scan {
    Running(DuProgress),
    Done,
    /// The folder could not be read at all.
    Failed,
}

pub struct UsageView {
    root: PathBuf,
    window: WindowId,
    scan: Scan,
    events: flume::Receiver<DuEvent>,
    stop: Arc<AtomicBool>,
    tree: Option<DuNode>,
    legend: Rc<Legend>,
    /// Colours the map by what can be rebuilt and lists `finds` in place of the folder's items.
    dev: bool,
    finds: Rc<Vec<DuFound>>,
    /// The folder shown, as the child index at each level under the root.
    path: Vec<usize>,
    selected: Option<usize>,
    hovered: Option<usize>,
    list: ListState<u32>,
    /// Counts every change of the folder shown, so rows and the map know theirs are stale.
    generation: u64,
    area: (f32, f32),
    map: Rc<Map>,
    map_of: (u64, f32, f32),
    /// The item on its way to the Trash.
    pending: Option<(JobId, PathBuf)>,
    error: Option<String>,
    _jobs: Subscription,
}

/// Opens the window and starts measuring `root`.
pub fn open(cx: &mut App, root: PathBuf) -> WindowHandle<UsageView> {
    crate::init(cx);
    let options = WindowOptions::main("usage")
        .title(format!("Disk Usage of {}", title_of(&root)))
        .size(size(px(1040.), px(680.)))
        .min_size(size(px(680.), px(420.)));
    cx.open_window(options, move |window, cx| {
        let id = window.id();
        cx.new(|cx| UsageView::new(root, id, cx))
    })
}

/// The window on the demo fixture with its walk finished, for tests and the appearance audit.
pub fn open_demo(cx: &mut App) -> WindowId {
    let root = crate::demo_root();
    let window = open(cx, root);
    window.root().clone().update(cx, |v, cx| v.drain(Duration::from_secs(10), cx));
    window.id()
}

fn title_of(path: &std::path::Path) -> String {
    let env = crate::env::get();
    crate::location::Location::Dir(path.to_path_buf()).title(&env.home, &env.trash)
}

impl UsageView {
    /// The view for the window `window`, its walk of `root` already started.
    pub fn new(root: PathBuf, window: WindowId, cx: &mut Context<Self>) -> Self {
        ops::init(cx);
        let jobs = cx.observe(&ops::jobs(cx), |this: &mut Self, jobs, cx| this.job_moved(&jobs, cx));
        let stop = Arc::new(AtomicBool::new(false));
        let events = trove_fs::du_scan(root.clone(), stop.clone());
        let view = Self {
            root,
            window,
            scan: Scan::Running(DuProgress::default()),
            events,
            stop,
            tree: None,
            legend: Rc::default(),
            dev: crate::settings::get(cx).usage_developer,
            finds: Rc::default(),
            path: Vec::new(),
            selected: None,
            hovered: None,
            list: ListState::new(cx).estimated_row_height(px(ROW_H)),
            generation: 0,
            area: (0., 0.),
            map: Rc::new(Map::default()),
            map_of: (0, 0., 0.),
            pending: None,
            error: None,
            _jobs: jobs,
        };
        view.listen(cx);
        view
    }

    fn listen(&self, cx: &mut Context<Self>) {
        pump(self.events.clone(), cx.weak_entity(), |this: &mut Self, events, cx| {
            this.apply(events, cx);
            matches!(this.scan, Scan::Running(_))
        });
    }

    fn apply(&mut self, events: Vec<DuEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                DuEvent::Progress(p) => self.scan = Scan::Running(p),
                DuEvent::Done(tree) => {
                    self.legend = Rc::new(Legend::of(&tree));
                    self.finds = Rc::new(trove_fs::dev_finds(&tree));
                    self.tree = Some(Arc::unwrap_or_clone(tree));
                    self.scan = Scan::Done;
                    self.path.clear();
                    self.refocus(cx);
                    self.relist(cx);
                }
                DuEvent::Failed => self.scan = Scan::Failed,
            }
        }
        cx.notify();
    }

    /// Takes the walk's events as they come until it ends; for tests, which have no pump.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        while matches!(self.scan, Scan::Running(_)) {
            let Ok(event) = self.events.recv_timeout(timeout) else { break };
            self.apply(vec![event], cx);
        }
    }

    /// Measures the root again from nothing.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        self.stop = Arc::new(AtomicBool::new(false));
        self.events = trove_fs::du_scan(self.root.clone(), self.stop.clone());
        self.scan = Scan::Running(DuProgress::default());
        self.tree = None;
        self.finds = Rc::default();
        self.error = None;
        self.path.clear();
        self.refocus(cx);
        self.relist(cx);
        self.listen(cx);
    }

    pub fn state(&self) -> &Scan {
        &self.scan
    }

    /// The folder shown.
    pub fn folder(&self) -> Option<&DuNode> {
        let mut node = self.tree.as_ref()?;
        for i in &self.path {
            node = node.children.get(*i)?;
        }
        Some(node)
    }

    /// The names from the root down to the folder shown.
    pub fn crumbs(&self) -> Vec<String> {
        let mut names = vec![title_of(&self.root)];
        let mut node = self.tree.as_ref();
        for i in &self.path {
            node = node.and_then(|n| n.children.get(*i));
            names.extend(node.map(|n| n.name.clone()));
        }
        names
    }

    fn folder_path(&self) -> PathBuf {
        let mut path = self.root.clone();
        let mut node = self.tree.as_ref();
        for i in &self.path {
            node = node.and_then(|n| n.children.get(*i));
            path.extend(node.map(|n| n.name.as_str()));
        }
        path
    }

    /// Where the shown folder's item `child` is on disk; `None` for the smaller files folded
    /// into one row, which are no one path.
    pub fn path_of(&self, child: usize) -> Option<PathBuf> {
        let node = self.folder()?.children.get(child)?;
        (node.kind != DuKind::Rest).then(|| self.folder_path().join(&node.name))
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The folder shown changed: the map and the selection are of the old one.
    fn refocus(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.selected = None;
        self.hovered = None;
        if !self.dev {
            self.relist(cx);
        }
        cx.notify();
    }

    /// Gives the list its rows: the shown folder's items, or every find in the developer's view.
    fn relist(&mut self, cx: &mut Context<Self>) {
        let rows = if self.dev { self.finds.len() } else { self.folder().map_or(0, |f| f.children.len()) } as u32;
        let generation = self.generation;
        self.list.set_selected(Vec::new(), cx);
        self.list.set_items_versioned((0..rows).map(|i| (i, generation)), cx);
    }

    pub fn developer(&self) -> bool {
        self.dev
    }

    /// What the developer's view lists, largest first.
    pub fn finds(&self) -> &[DuFound] {
        &self.finds
    }

    /// Switches between the map by kind and the developer's view.
    pub fn set_developer(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.dev != on {
            self.dev = on;
            self.generation += 1;
            self.relist(cx);
            cx.notify();
        }
    }

    /// The child index at each level down to `path`, which is under the root.
    fn steps_to(&self, path: &std::path::Path) -> Option<Vec<usize>> {
        let mut node = self.tree.as_ref()?;
        let mut steps = Vec::new();
        for name in path.iter() {
            let at = node.children.iter().position(|c| name == c.name.as_str())?;
            steps.push(at);
            node = &node.children[at];
        }
        Some(steps)
    }

    /// Shows the folder the find `i` sits in, with the find selected in the map.
    pub fn reveal(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(mut steps) = self.finds.get(i).and_then(|f| self.steps_to(&f.path)) else { return };
        let Some(child) = steps.pop() else { return };
        if self.path != steps {
            self.path = steps;
            self.refocus(cx);
        }
        self.selected = Some(child);
        cx.notify();
    }

    /// Shows what is inside the find `i`.
    pub fn enter_find(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(steps) = self.finds.get(i).and_then(|f| self.steps_to(&f.path)) else { return };
        if !steps.is_empty() {
            self.path = steps;
            self.refocus(cx);
        }
    }

    /// Shows the folder `child` of the one shown.
    pub fn enter(&mut self, child: usize, cx: &mut Context<Self>) {
        let opens = self.folder().and_then(|f| f.children.get(child)).is_some_and(|n| !n.children.is_empty());
        if opens {
            self.path.push(child);
            self.refocus(cx);
        }
    }

    /// Back out to the crumb at `depth`; 0 is the root.
    pub fn go_to(&mut self, depth: usize, cx: &mut Context<Self>) {
        if depth < self.path.len() {
            self.path.truncate(depth);
            self.refocus(cx);
        }
    }

    pub fn select(&mut self, child: usize, cx: &mut Context<Self>) {
        self.selected = Some(child);
        let row = if self.dev {
            let at = self.path_of(child).and_then(|p| Some(p.strip_prefix(&self.root).ok()?.to_path_buf()));
            at.and_then(|at| self.finds.iter().position(|f| f.path == at))
        } else {
            Some(child)
        };
        match row {
            Some(row) => {
                self.list.set_selected(vec![row as u32], cx);
                self.list.scroll_to(row as u32, Align::Center, px(0.), cx);
            }
            None => self.list.set_selected(Vec::new(), cx),
        }
        cx.notify();
    }

    fn ask_trash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.pending.is_some() || !path.starts_with(&self.root) || path == self.root {
            return;
        }
        let title = format!("Move \u{201c}{}\u{201d} to the Trash?", ops::name_of(&path));
        let me = cx.weak_entity();
        nd::chrome::alert(cx, self.window, Alert::confirm(title, "Move to Trash"), move |res, cx| {
            if res.0 != 0 {
                return;
            }
            let _ = me.update(cx, |this, cx| this.trash(path, cx));
        });
    }

    /// Sends the item at `path`, under the root, to the Trash; it leaves the map once it is there.
    pub fn trash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let origin = Origin { window: self.window, tab: None };
        let job = ops::submit(cx, origin, Op::Trash { paths: vec![path.clone()] });
        self.pending = Some((job, path));
        self.error = None;
        cx.notify();
    }

    fn job_moved(&mut self, jobs: &Entity<ops::Jobs>, cx: &mut Context<Self>) {
        let Some((job, path)) = self.pending.clone() else { return };
        let ended = jobs.read(cx).seen().iter().rev().find_map(|e| match e {
            OpEvent::Finished { job: j, .. } if *j == job => Some(None),
            OpEvent::Failed { job: j, error } if *j == job => Some(Some(error.clone())),
            OpEvent::Cancelled { job: j } if *j == job => Some(Some(String::new())),
            _ => None,
        });
        match ended {
            None => {}
            Some(None) => {
                self.pending = None;
                self.forget(&path, cx);
            }
            Some(Some(error)) => {
                self.pending = None;
                self.error = (!error.is_empty()).then_some(error);
                cx.notify();
            }
        }
    }

    /// Takes the trashed item out of the tree and its size off every folder above it.
    fn forget(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let Ok(under) = path.strip_prefix(&self.root) else { return };
        let names: Vec<String> = under.iter().map(|n| n.to_string_lossy().into_owned()).collect();
        let shown = self.folder_path();
        let Some(tree) = self.tree.as_mut() else { return };
        if take(tree, &names).is_none() {
            return;
        }
        self.finds = Rc::new(trove_fs::dev_finds(tree));
        // The indices moved; a shown folder that went with the item falls back to the nearest
        // one still there.
        let mut at = shown.strip_prefix(&self.root).unwrap_or(std::path::Path::new("")).to_path_buf();
        self.path = loop {
            match self.steps_to(&at) {
                Some(steps) => break steps,
                None if at.pop() => {}
                None => break Vec::new(),
            }
        };
        self.refocus(cx);
        self.relist(cx);
    }

    fn show(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let start = if path.is_dir() {
            Start::Path(path.to_path_buf())
        } else {
            Start::Places(crate::items_by_folder(vec![path.to_path_buf()]))
        };
        crate::open(cx, start);
    }

    /// The line under the map: the item under the pointer, else the selected one, else the
    /// folder shown.
    pub fn detail(&self) -> String {
        let Some(folder) = self.folder() else { return String::new() };
        let item = self.hovered.or(self.selected).and_then(|i| folder.children.get(i));
        let (name, node) = match item {
            Some(node) => (self.folder_path().join(&node.name).display().to_string(), node),
            None => (self.folder_path().display().to_string(), folder),
        };
        let mut line = format!("{name} \u{b7} {}", format::size(node.size));
        if node.items > 0 {
            line.push_str(&format!(" \u{b7} {}", format::items(node.items as usize)));
        }
        line
    }

    fn menu(&self, menu: ContextMenu, child: usize) -> ContextMenu {
        let Some(path) = self.path_of(child) else { return menu };
        let folder = self.folder().and_then(|f| f.children.get(child)).is_some_and(DuNode::is_dir);
        Self::menu_for(menu, path, folder)
    }

    fn menu_for(menu: ContextMenu, path: PathBuf, folder: bool) -> ContextMenu {
        menu.item(if folder { "Open in Trove" } else { "Show in Trove" }, ShowInTrove(path.clone()))
            .item("Copy path", CopyPathOf(path.clone()))
            .separator()
            .item_destructive("Move to Trash", TrashMeasured(path))
    }

    /// A row of the developer's view: what the folder is, where, and how much it holds.
    fn find_row(&self, i: u32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(find) = self.finds.get(i as usize) else { return div().into_any_element() };
        let name = find.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let inside = find.path.parent().map(|p| p.display().to_string()).filter(|p| !p.is_empty());
        let path = self.root.join(&find.path);
        h_flex()
            .id(SharedString::from(format!("usage-find-{i}")))
            .h(px(ROW_H))
            .px(theme.spacing.sm)
            .gap(theme.spacing.sm)
            .items_center()
            .child(div().size(px(10.)).flex_none().rounded(theme.radius.sm).bg(tag_color(find.tag)))
            .child(text(name).flex_none().truncate())
            .child(div().flex_1().min_w_0().children(
                inside.map(|p| text(p).text_size(theme.type_scale.footnote).text_color(theme.text.tertiary).truncate()),
            ))
            .child(
                text(find.tag.label())
                    .flex_none()
                    .text_size(theme.type_scale.footnote)
                    .text_color(theme.text.secondary),
            )
            .child(
                div().w(px(64.)).flex_none().child(
                    text(format::size(find.size))
                        .text_size(theme.type_scale.footnote)
                        .text_color(theme.text.secondary)
                        .tabular_nums()
                        .text_right(),
                ),
            )
            .context_menu(move |menu, _, _| Self::menu_for(menu, path.clone(), true))
            .into_any_element()
    }

    fn row(&mut self, i: u32, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.dev {
            return self.find_row(i, cx);
        }
        let theme = cx.theme();
        let Some(folder) = self.folder() else { return div().into_any_element() };
        let Some(node) = folder.children.get(i as usize) else { return div().into_any_element() };
        let share = if folder.size > 0 { node.size as f64 / folder.size as f64 } else { 0. };
        let mark = match node.kind {
            DuKind::Dir => icon(Icon::Folder, 14.).text_color(theme.accent).into_any_element(),
            DuKind::File(kind) => {
                icon(icons::for_kind(kind), 14.).text_color(slot_color(self.legend.slot(node))).into_any_element()
            }
            DuKind::Rest => {
                icon(icons::for_kind(FileKind::Unknown), 14.).text_color(theme.text.tertiary).into_any_element()
            }
        };
        let bar = div()
            .w(px(BAR_W))
            .h(px(4.))
            .rounded(theme.radius.sm)
            .bg(theme.border.subtle)
            .child(div().h_full().w(px((BAR_W * share as f32).max(1.))).rounded(theme.radius.sm).bg(theme.accent));
        let me = cx.weak_entity();
        let child = i as usize;
        h_flex()
            .id(SharedString::from(format!("usage-row-{i}")))
            .h(px(ROW_H))
            .px(theme.spacing.sm)
            .gap(theme.spacing.sm)
            .items_center()
            .child(mark)
            .child(div().flex_1().min_w_0().child(text(node.name.clone()).truncate()))
            .child(bar)
            .child(
                div().w(px(64.)).flex_none().child(
                    text(format::size(node.size))
                        .text_size(theme.type_scale.footnote)
                        .text_color(theme.text.secondary)
                        .tabular_nums()
                        .text_right(),
                ),
            )
            .context_menu(move |menu, _, cx| match me.upgrade() {
                Some(view) => view.read(cx).menu(menu, child),
                None => menu,
            })
            .into_any_element()
    }

    fn cell(&self, c: &Cell, node: &DuNode, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let child = c.child;
        let (tap, open, hover, menu) = (cx.weak_entity(), cx.weak_entity(), cx.weak_entity(), cx.weak_entity());
        let label = |d: Div| {
            d.child(
                div().flex_1().min_w_0().child(text(node.name.clone()).text_size(theme.type_scale.caption).truncate()),
            )
            .child(
                text(format::size(node.size))
                    .text_size(theme.type_scale.caption)
                    .text_color(theme.text.secondary)
                    .tabular_nums(),
            )
        };
        let mut cell = div()
            .id(SharedString::from(format!("usage-cell-{child}")))
            .absolute()
            .left(px(c.rect.x))
            .top(px(c.rect.y))
            .w(px(c.rect.w))
            .h(px(c.rect.h))
            .border_1()
            .border_color(theme.surface.base)
            .hover(|s| s.border_color(theme.text.primary))
            .when(self.selected == Some(child), |d| d.border_2().border_color(theme.accent))
            .on_hover(move |over: &bool, _, cx| {
                let over = *over;
                let _ = hover.update(cx, |this, cx| {
                    let now = if over { Some(child) } else { this.hovered.filter(|h| *h != child) };
                    if this.hovered != now {
                        this.hovered = now;
                        cx.notify();
                    }
                });
            })
            .on_tap(move |_: &TapEvent, _, cx| {
                let _ = tap.update(cx, |this, cx| this.select(child, cx));
            })
            .on_double_tap(move |_: &DoubleTapEvent, _, cx| {
                let _ = open.update(cx, |this, cx| this.enter(child, cx));
            })
            .context_menu(move |m, _, cx| match menu.upgrade() {
                Some(view) => view.read(cx).menu(m, child),
                None => m,
            });
        if c.titled {
            cell = cell.child(label(
                h_flex()
                    .w_full()
                    .h(px(TITLE_H))
                    .px(theme.spacing.sm)
                    .gap(theme.spacing.sm)
                    .items_center()
                    .bg(theme.surface.raised),
            ));
        } else if !node.is_dir() && c.rect.w >= NAMED_FILE.0 && c.rect.h >= NAMED_FILE.1 {
            cell = cell.p(theme.spacing.xs).child(label(
                h_flex()
                    .max_w_full()
                    .px(theme.spacing.xs)
                    .gap(theme.spacing.sm)
                    .items_center()
                    .rounded(theme.radius.sm)
                    .bg(theme.surface.raised),
            ));
        }
        cell
    }

    fn map(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (w, h) = self.area;
        if self.map_of != (self.generation, w, h) {
            let by = if self.dev { Colouring::Developer } else { Colouring::Extensions(&self.legend) };
            self.map = Rc::new(self.folder().map(|f| treemap::build(f, by, w, h)).unwrap_or_default());
            self.map_of = (self.generation, w, h);
        }
        let fills = self.map.clone();
        let (frame, frame_odd, closed) = (theme.surface.raised, theme.surface.base, theme.border.strong);
        let drawing = canvas(move |_, paint, _| {
            for f in &fills.fills {
                let color = match f.paint {
                    Paint::File(slot) => slot_color(slot),
                    Paint::Frame { odd: false } => frame,
                    Paint::Frame { odd: true } => frame_odd,
                    Paint::Folder => closed,
                    Paint::Tag(tag) => tag_color(tag),
                };
                let at = Bounds::new(point(px(f.rect.x), px(f.rect.y)), size(px(f.rect.w), px(f.rect.h)));
                paint.fill_rect(at, color);
            }
        })
        .id("usage-canvas")
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let map = self.map.clone();
        let cells: Vec<Div> = match self.folder() {
            Some(folder) => {
                map.cells.iter().filter_map(|c| Some(self.cell(c, folder.children.get(c.child)?, cx))).collect()
            }
            None => Vec::new(),
        };
        let resized = cx.weak_entity();
        div()
            .id("usage-map")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_resize(move |e, _, cx| {
                let area = (e.width, e.height);
                let _ = resized.update(cx, |this, cx| {
                    if this.area != area {
                        this.area = area;
                        cx.notify();
                    }
                });
            })
            .child(drawing)
            .children(cells)
    }

    fn crumb_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let names = self.crumbs();
        let last = names.len() - 1;
        let mut bar = h_flex()
            .id("usage-path")
            .w_full()
            .h(theme.control.regular)
            .px(theme.spacing.md)
            .gap(theme.spacing.xs)
            .items_center()
            .overflow_hidden();
        for (depth, name) in names.into_iter().enumerate() {
            if depth > 0 {
                bar = bar.child(icon(Icon::ChevronRight, 12.).text_color(theme.text.tertiary));
            }
            let here = depth == last;
            let me = cx.weak_entity();
            let crumb = div()
                .id(SharedString::from(format!("usage-crumb-{depth}")))
                .flex_none()
                .px(theme.spacing.xs)
                .rounded(theme.radius.sm)
                .child(
                    text(name)
                        .text_color(if here { theme.text.primary } else { theme.text.secondary })
                        .when(here, |t| t.font_weight(FontWeight::SEMIBOLD)),
                )
                .when(!here, |d| {
                    d.hover(|s| s.bg(theme.surface.raised_hover)).on_tap(move |_: &TapEvent, _, cx| {
                        let _ = me.update(cx, |this, cx| this.go_to(depth, cx));
                    })
                });
            bar = bar.child(crumb);
        }
        let total =
            self.folder().map(|f| format!("{} \u{b7} {}", format::size(f.size), format::items(f.items as usize)));
        bar.child(div().flex_1()).children(total.map(|t| {
            text(t)
                .id("usage-total")
                .text_size(theme.type_scale.footnote)
                .text_color(theme.text.secondary)
                .tabular_nums()
        }))
    }

    fn legend(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let key = |color: Hsla, label: String, bytes: Option<u64>| {
            h_flex()
                .gap(theme.spacing.xs)
                .items_center()
                .flex_none()
                .child(div().size(px(10.)).rounded(theme.radius.sm).bg(color))
                .child(text(label).text_size(theme.type_scale.caption).text_color(theme.text.primary))
                .children(bytes.map(|b| {
                    text(format::size(b))
                        .text_size(theme.type_scale.caption)
                        .text_color(theme.text.secondary)
                        .tabular_nums()
                }))
        };
        let mut keys: Vec<Div> = if self.dev {
            DevTag::ALL
                .into_iter()
                .filter_map(|tag| {
                    let bytes: u64 = self.finds.iter().filter(|f| f.tag == tag).map(|f| f.size).sum();
                    (bytes > 0).then(|| key(tag_color(tag), tag.label().to_owned(), Some(bytes)))
                })
                .collect()
        } else {
            self.legend
                .entries()
                .iter()
                .enumerate()
                .map(|(slot, (ext, bytes))| {
                    let label = if ext.is_empty() { "no extension".to_owned() } else { format!(".{ext}") };
                    key(slot_color(slot as u8), label, Some(*bytes))
                })
                .collect()
        };
        if self.tree.is_some() {
            let other = if self.dev { "Everything else" } else { "Other" };
            keys.push(key(slot_color(SLOTS as u8), other.to_owned(), None));
        }
        if self.dev && self.tree.is_some() {
            keys.push(
                div().flex_none().child(
                    text(format!("{} can be rebuilt", format::size(crate::du_cli::reclaimable(&self.finds))))
                        .id("usage-reclaimable")
                        .text_size(theme.type_scale.caption)
                        .text_color(theme.text.secondary)
                        .tabular_nums(),
                ),
            );
        }
        let line = match &self.error {
            Some(error) => text(error.clone()).text_color(theme.text.danger),
            None => text(self.detail()).text_color(theme.text.secondary),
        };
        h_flex()
            .id("usage-legend")
            .w_full()
            .h(theme.control.small)
            .px(theme.spacing.md)
            .gap(theme.spacing.md)
            .items_center()
            .children(keys)
            .child(div().flex_1().min_w_0().child(
                line.id("usage-detail").text_size(theme.type_scale.footnote).tabular_nums().truncate().text_right(),
            ))
    }

    fn body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = title_of(&self.root);
        match &self.scan {
            Scan::Running(p) => status_page(format!("Measuring {name}"))
                .id("usage-measuring")
                .icon(icons::for_volume(trove_fs::VolumeKind::Internal))
                .description(format!("{} \u{b7} {}", format::items(p.items as usize), format::size(p.bytes)))
                .into_any_element(),
            Scan::Failed => status_page(format!("{name} can\u{2019}t be read"))
                .id("usage-failed")
                .icon(Icon::Folder)
                .description("Trove has no permission to look inside it, or it is gone.")
                .into_any_element(),
            Scan::Done if self.folder().is_none_or(|f| f.children.is_empty()) => {
                status_page("Nothing in here").id("usage-empty").icon(Icon::Folder).compact().into_any_element()
            }
            Scan::Done if self.dev && self.finds.is_empty() => {
                let map = self.map(cx);
                let theme = cx.theme();
                let map = div().flex_1().min_w_0().h_full().p(theme.spacing.sm).child(map);
                let none = status_page("Nothing to rebuild")
                    .id("usage-no-finds")
                    .icon(Icon::Code)
                    .description("No worktrees, build output, dependencies or caches in here.")
                    .compact();
                h_flex()
                    .size_full()
                    .child(div().w(px(LIST_W)).h_full().flex_none().child(none))
                    .child(div().w(px(1.)).h_full().flex_none().bg(theme.separator))
                    .child(map)
                    .into_any_element()
            }
            Scan::Done => {
                let (select, enter) = (cx.weak_entity(), cx.weak_entity());
                let rows = list("usage-list", &self.list, cx.processor(Self::row))
                    .selection(Selection::single())
                    .estimated_row_height(px(ROW_H))
                    .on_select(move |e: &SelectEvent<u32>, _, cx| {
                        let _ = select.update(cx, |this, cx| {
                            if this.dev {
                                this.reveal(e.key as usize, cx);
                            } else {
                                this.selected = Some(e.key as usize);
                                cx.notify();
                            }
                        });
                    })
                    .on_activate(move |e: &ActivateEvent<u32>, _, cx| {
                        let _ = enter.update(cx, |this, cx| {
                            if this.dev {
                                this.enter_find(e.key as usize, cx);
                            } else {
                                this.enter(e.key as usize, cx);
                            }
                        });
                    });
                h_flex()
                    .size_full()
                    .child(div().w(px(LIST_W)).h_full().flex_none().child(rows))
                    .child(div().w(px(1.)).h_full().flex_none().bg(theme.separator))
                    .child(div().flex_1().min_w_0().h_full().p(theme.spacing.sm).child(self.map(cx)))
                    .into_any_element()
            }
        }
    }
}

/// Removes the item at `names` under `node`, taking its size and count off every folder on the
/// way; what it held, itself counted.
fn take(node: &mut DuNode, names: &[String]) -> Option<(u64, u64)> {
    let (first, rest) = names.split_first()?;
    let at = node.children.iter().position(|c| c.name == *first)?;
    let gone = if rest.is_empty() {
        let gone = node.children.remove(at);
        (gone.size, gone.items + 1)
    } else {
        take(node.children.get_mut(at)?, rest)?
    };
    node.size = node.size.saturating_sub(gone.0);
    node.items = node.items.saturating_sub(gone.1);
    Some(gone)
}

impl Drop for UsageView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Render for UsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let up = self.path.len();
        let toolbar = toolbar()
            .title(format!("Disk Usage of {}", title_of(&self.root)))
            .navigation(
                icon_button("usage-up", Icon::ArrowUp)
                    .tooltip("Enclosing folder")
                    .disabled(up == 0)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_to(up.saturating_sub(1), cx))),
            )
            .item(segmented("usage-view", VIEWS).selected(usize::from(self.dev)).on_change(cx.listener(
                |this, e: &SelectChanged, _, cx| {
                    let on = e.index == 1;
                    crate::settings::set(cx, "usage_developer", &on);
                    this.set_developer(on, cx);
                },
            )))
            .item(
                icon_button("usage-rescan", Icon::RotateCw)
                    .tooltip("Measure again")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.rescan(cx))),
            );
        let content = v_flex()
            .size_full()
            .child(self.crumb_bar(cx))
            .child(separator())
            .child(div().flex_1().min_h_0().child(self.body(cx)))
            .child(separator())
            .child(self.legend(cx));
        scaffold()
            .key_context(crate::actions::WINDOW)
            .toolbar(toolbar)
            .content(content)
            .on_action(cx.listener(|this, ShowInTrove(path): &ShowInTrove, _, cx| this.show(&path.clone(), cx)))
            .on_action(cx.listener(|_, CopyPathOf(path): &CopyPathOf, _, cx| {
                nd::clipboard::write_text(cx, path.display().to_string());
            }))
            .on_action(cx.listener(|this, TrashMeasured(path): &TrashMeasured, _, cx| this.ask_trash(path.clone(), cx)))
            .on_action(|_: &nd::actions::Cancel, window, cx| cx.close_window(window.id()))
    }
}
