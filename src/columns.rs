//! Finder's columns: one list per folder from Home (or the root) down to the tab's folder,
//! then the folder under the cursor, or the preview of the file under it. The tab's folder is
//! the tab's own listing; the folders above it are listed once and kept while the tab moves
//! among them, the one under the cursor is listed as the cursor reaches it. A move between
//! columns changes the tab's folder without adding to its history. The columns scroll sideways
//! from the start folder on, the keyboard's column and what follows it kept in view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nd::prelude::*;
use trove_fs::{Change, Entry, EntryId, EntryKind, ListEvent, ListOptions, Listing, SortOrder};

use crate::actions::COLUMNS;
use crate::icons;
use crate::location::Location;
use crate::preview::{Item, Preview};
use crate::pump::pump;
use crate::tab::Tab;
use crate::zoom::{ROWS, RowSize};

const COLUMN_W: f32 = 220.;
const PREVIEW_MIN: f32 = 280.;
/// The preview column's content, centred in whatever room is left: a wide window does not
/// stretch its information rows apart.
const PREVIEW_MAX: f32 = 420.;
/// The tab's own column.
pub const CURRENT: &str = "files-columns";
/// The folder under the cursor.
const NEXT: &str = "column-next";
const PREVIEW_ID: &str = "column-preview";

pub struct Columns {
    /// The tab's own folder, the column the keyboard is in.
    pub current: ListState<EntryId>,
    levels: Vec<Level>,
    child: Option<Level>,
    preview: Option<Entity<Preview>>,
    /// What the last frame showed past the start: the tab's folder, the folder under the
    /// cursor and the file previewed. A change scrolls the end of the row into view.
    shown: Option<(PathBuf, Option<PathBuf>, Option<PathBuf>)>,
}

struct Level {
    dir: Arc<Path>,
    entries: Vec<Entry>,
    index: HashMap<EntryId, usize>,
    list: ListState<EntryId>,
    events: Option<flume::Receiver<ListEvent>>,
    _listing: Option<Listing>,
    /// The entry on the way down to the tab's folder, or under the cursor.
    on_path: Option<EntryId>,
    done: bool,
}

impl Columns {
    pub fn new<T: 'static>(cx: &mut Context<T>) -> Self {
        Self {
            current: ListState::new(cx).estimated_row_height(px(ROWS[0].height)),
            levels: Vec::new(),
            child: None,
            preview: None,
            shown: None,
        }
    }

    /// The folders shown before the tab's own, outermost first, for tests.
    pub fn ancestors(&self) -> Vec<PathBuf> {
        self.levels.iter().map(|l| l.dir.to_path_buf()).collect()
    }

    /// The folder listed after the tab's own, for tests.
    pub fn child(&self) -> Option<PathBuf> {
        self.child.as_ref().map(|l| l.dir.to_path_buf())
    }

    /// The names a column shows, for tests: the column of `dir`.
    pub fn names_in(&self, dir: &Path) -> Vec<String> {
        self.level(dir).map(|l| l.entries.iter().map(|e| e.name.to_string()).collect()).unwrap_or_default()
    }

    fn level(&self, dir: &Path) -> Option<&Level> {
        self.levels.iter().chain(self.child.as_ref()).find(|l| &*l.dir == dir)
    }

    /// The path of `id` in the column of `dir`, and whether it is a folder.
    fn path_in(&self, dir: &Path, id: EntryId) -> Option<(PathBuf, bool)> {
        let level = self.level(dir)?;
        let e = level.entries.get(*level.index.get(&id)?)?;
        let is_dir = e.is_dir() || matches!(e.kind, EntryKind::Symlink { .. }) && e.meta.is_none();
        Some((e.path_in(dir), is_dir))
    }

    fn level_mut(&mut self, dir: &Path) -> Option<&mut Level> {
        self.levels.iter_mut().chain(self.child.as_mut()).find(|l| &*l.dir == dir)
    }

    /// Redraws every row of every column: the rows changed size.
    pub fn invalidate_all(&mut self, cx: &mut App) {
        self.current.invalidate_all(cx);
        for level in self.levels.iter_mut().chain(self.child.as_mut()) {
            level.list.invalidate_all(cx);
        }
    }

    /// Redraws the rows of the columns above and below the tab's own whose thumbnail arrived;
    /// the tab's own column is the tab's listing, which redraws its own.
    pub fn thumbnails_arrived(&mut self, paths: &[PathBuf], cx: &mut App) {
        for path in paths {
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
            let id = id_of(name);
            if let Some(level) = self.level_mut(dir).filter(|l| l.index.contains_key(&id)) {
                level.list.invalidate(&id, cx);
            }
        }
    }

    /// Waits up to `timeout` for every column's listing: the host's pump, for tests.
    pub fn drain(&mut self, show_hidden: bool, timeout: Duration, cx: &mut App) {
        for level in self.levels.iter_mut().chain(self.child.as_mut()) {
            // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
            let deadline = std::time::Instant::now() + timeout;
            while let Some(rx) = level.events.clone() {
                if level.done {
                    break;
                }
                // nd-allow(clock-in-ui): as above
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let Ok(first) = rx.recv_timeout(left) else { break };
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                level.apply(batch, show_hidden, cx);
            }
        }
    }
}

impl Level {
    fn new(dir: &Path, show_hidden: bool, me: WeakEntity<Tab>, cx: &mut Context<Tab>) -> Self {
        let dir: Arc<Path> = Arc::from(dir);
        let listing = trove_fs::list(dir.to_path_buf(), ListOptions { show_hidden });
        let events = listing.events.clone();
        let at = dir.clone();
        pump(events.clone(), me, move |tab: &mut Tab, batch, cx| {
            let show_hidden = tab.show_hidden();
            match tab.extras.columns.level_mut(&at) {
                Some(level) => {
                    level.apply(batch, show_hidden, cx);
                    cx.notify();
                    true
                }
                None => false,
            }
        });
        Self {
            dir,
            entries: Vec::new(),
            index: HashMap::new(),
            list: ListState::new(cx).estimated_row_height(px(ROWS[0].height)),
            events: Some(events),
            _listing: Some(listing),
            on_path: None,
            done: false,
        }
    }

    fn apply(&mut self, batch: Vec<ListEvent>, show_hidden: bool, cx: &mut App) {
        for event in batch {
            match event {
                ListEvent::Entries(chunk) => {
                    for e in chunk {
                        if !self.index.contains_key(&e.id) {
                            self.index.insert(e.id, self.entries.len());
                            self.entries.push(e);
                        }
                    }
                }
                ListEvent::Meta(metas) => {
                    for (id, meta) in metas {
                        if let Some(&i) = self.index.get(&id) {
                            self.entries[i].meta = Some(meta);
                        }
                    }
                }
                ListEvent::Changed(changes) => {
                    for change in changes {
                        match change {
                            Change::Added(e) => {
                                if !self.index.contains_key(&e.id) {
                                    self.index.insert(e.id, self.entries.len());
                                    self.entries.push(e);
                                }
                            }
                            Change::Removed(id) => self.entries.retain(|e| e.id != id),
                            Change::Updated(id, meta) => {
                                if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
                                    e.meta = Some(meta);
                                }
                            }
                            Change::Renamed { from, to } => {
                                self.entries.retain(|e| e.id != from);
                                self.entries.push(to);
                            }
                        }
                    }
                }
                ListEvent::Complete | ListEvent::Error(_) => self.done = true,
                ListEvent::Listed { .. } | ListEvent::Slow => {}
            }
        }
        trove_fs::sort_entries(&mut self.entries, SortOrder::default());
        self.index = self.entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
        let keys: Vec<EntryId> = self.entries.iter().filter(|e| show_hidden || !e.hidden).map(|e| e.id).collect();
        self.list.set_items(keys, cx);
        self.mark(cx);
    }

    fn mark(&mut self, cx: &mut App) {
        let keys: Vec<EntryId> = self.on_path.into_iter().filter(|id| self.index.contains_key(id)).collect();
        if self.list.selected() != keys {
            self.list.set_selected(keys.clone(), cx);
            if let Some(id) = keys.first() {
                self.list.scroll_to(*id, Align::Nearest, px(0.), cx);
            }
        }
    }

    fn set_on_path(&mut self, id: Option<EntryId>, cx: &mut App) {
        if self.on_path != id {
            self.on_path = id;
            self.mark(cx);
        }
    }
}

fn id_of(name: &std::ffi::OsStr) -> EntryId {
    EntryId::of(name)
}

/// Where the columns start for `dir`: Home when it is inside Home, else the root.
fn origin(dir: &Path, home: &Path) -> PathBuf {
    if dir.starts_with(home) { home.to_path_buf() } else { PathBuf::from("/") }
}

/// Lines the columns up with the tab's folder and cursor: reuses the columns above it, lists
/// the ones it has not listed, and the folder under the cursor.
fn sync(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Location::Dir(dir) = tab.location().clone() else {
        tab.extras.columns.levels.clear();
        tab.extras.columns.child = None;
        return;
    };
    let home = crate::env::get().home.clone();
    let start = origin(&dir, &home);
    let mut chain: Vec<PathBuf> =
        dir.ancestors().skip(1).take_while(|a| a.starts_with(&start)).map(Path::to_path_buf).collect();
    chain.reverse();
    let show_hidden = tab.show_hidden();
    let me = cx.weak_entity();
    let mut old = std::mem::take(&mut tab.extras.columns.levels);
    let mut levels = Vec::with_capacity(chain.len());
    for (i, at) in chain.iter().enumerate() {
        let next = chain.get(i + 1).map_or(dir.as_path(), PathBuf::as_path);
        let mut level = match old.iter().position(|l| &*l.dir == at.as_path()) {
            Some(p) => old.swap_remove(p),
            None => match tab.extras.columns.child.take() {
                Some(c) if &*c.dir == at.as_path() => c,
                other => {
                    tab.extras.columns.child = other;
                    Level::new(at, show_hidden, me.clone(), cx)
                }
            },
        };
        level.set_on_path(next.file_name().map(id_of), cx);
        levels.push(level);
    }
    tab.extras.columns.levels = levels;

    let under = tab.cursor().filter(|_| tab.selected().len() == 1).filter(|id| tab.is_folder(id));
    let child_dir = under.and_then(|id| tab.path_of(&id));
    match child_dir {
        Some(path) => {
            if tab.extras.columns.child.as_ref().is_none_or(|c| *c.dir != *path) {
                let reuse = old.iter().position(|l| *l.dir == *path).map(|p| old.swap_remove(p));
                let mut level = reuse.unwrap_or_else(|| Level::new(&path, show_hidden, me.clone(), cx));
                level.set_on_path(None, cx);
                tab.extras.columns.child = Some(level);
            }
        }
        None => tab.extras.columns.child = None,
    }
}

/// Left and Right: out to the folder above, with this one selected, or into the folder under
/// the cursor.
pub fn step(tab: &mut Tab, by: isize, cx: &mut Context<Tab>) {
    if tab.extras.results.shown() {
        return;
    }
    let Location::Dir(dir) = tab.location().clone() else { return };
    if by < 0 {
        let home = crate::env::get().home.clone();
        let Some(parent) = dir.parent().filter(|p| p.starts_with(origin(&dir, &home))) else { return };
        let select = dir.file_name().map(id_of);
        tab.retarget(Location::Dir(parent.to_path_buf()), select, cx);
    } else if let Some(id) = tab.cursor().filter(|id| tab.is_folder(id))
        && let Some(path) = tab.path_of(&id)
    {
        tab.retarget(Location::Dir(path), None, cx);
    }
}

/// A row of any column: picture, name, and a chevron for folders.
fn row(e: &Entry, thumb: Option<PathBuf>, size: RowSize, theme: &Theme) -> Div {
    let symlinked = matches!(e.kind, EntryKind::Symlink { .. }) && e.is_dir();
    let glyph = if symlinked { icons::symlinked_folder() } else { icons::for_kind(e.file_kind) };
    h_flex()
        .h(px(size.height))
        .px(theme.spacing.sm)
        .gap(theme.spacing.sm)
        .items_center()
        .min_w_0()
        .child(crate::thumbs::row_art(thumb, glyph, icons::tint(e.file_kind, theme), size, theme))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(text(e.name.to_string()).truncate().when(e.hidden, |t| t.text_color(theme.text.secondary))),
        )
        .when(e.is_dir(), |r| r.child(icon(Icon::ChevronRight, 12.).text_color(theme.text.tertiary)))
}

impl Tab {
    fn column_row(
        &mut self,
        (dir, id): (Arc<Path>, EntryId),
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let size = self.row_size();
        let columns = &self.extras.columns;
        let Some(e) = columns.level(&dir).and_then(|l| l.index.get(&id).and_then(|i| l.entries.get(*i))).cloned()
        else {
            return div().into_any_element();
        };
        let path = e.path_in(&dir);
        let thumb = crate::thumbs::for_entry(Some(path.clone()), &e, size.art, cx);
        let r = row(&e, thumb, size, cx.theme());
        if e.is_dir() {
            return r.into_any_element();
        }
        let me = cx.weak_entity();
        r.on_tap(move |tap: &TapEvent, window, cx| {
            let opens = me.update(cx, |t, cx| t.single_click(tap, window, cx)).unwrap_or(false);
            if opens {
                crate::keys::open_file(&path, window, cx);
            }
        })
        .into_any_element()
    }

    fn current_row(&mut self, id: EntryId, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(r) = self.renaming().filter(|r| r.id == id).cloned() {
            return self.rename_field(&r, false, cx);
        }
        let size = self.row_size();
        let path = self.path_of(&id);
        let Some(e) = self.entry(&id) else { return div().into_any_element() };
        let thumb = crate::thumbs::for_entry(path, e, size.art, cx);
        let r =
            row(e, thumb, size, cx.theme()).id(crate::tab::row_id(&e.name)).when(self.is_cut(&id), |d| d.opacity(0.5));
        self.file_events(r, id, cx).into_any_element()
    }
}

/// The columns view: every column from the start folder on in a row that scrolls sideways,
/// then the preview of the file under the cursor.
pub fn render(tab: &mut Tab, cx: &mut Context<Tab>) -> AnyElement {
    sync(tab, cx);
    let me = cx.weak_entity();

    let file_path = tab
        .cursor()
        .filter(|_| tab.selected().len() == 1)
        .filter(|id| !tab.is_folder(id))
        .and_then(|id| tab.path_of(&id));
    let file = file_path.clone().map(|p| Item::of(p, false, None));
    if file.is_some() && tab.extras.columns.preview.is_none() {
        tab.extras.columns.preview = Some(cx.new(Preview::new));
    }
    if let Some(preview) = tab.extras.columns.preview.clone() {
        preview.update(cx, |p, cx| p.show(file.clone(), cx));
    }

    let shown = (
        tab.location().dir().map(Path::to_path_buf).unwrap_or_default(),
        tab.extras.columns.child.as_ref().map(|c| c.dir.to_path_buf()),
        file_path,
    );
    if tab.extras.columns.shown.as_ref() != Some(&shown) {
        let end = match &shown {
            (_, _, Some(_)) => PREVIEW_ID.to_owned(),
            (_, Some(_), _) => format!("{NEXT}-box"),
            _ => format!("{CURRENT}-box"),
        };
        reveal(tab, end);
        tab.extras.columns.shown = Some(shown);
    }

    let separator = cx.theme().separator;
    let column = |id: &str, content: AnyElement| {
        div()
            .id(format!("{id}-box"))
            .flex_shrink_0()
            .w(px(COLUMN_W))
            .h_full()
            .border_r_1()
            .border_color(separator)
            .child(content)
            .into_any_element()
    };
    let level_list = |level: &Level, id: &str, me: WeakEntity<Tab>, cx: &Context<Tab>| {
        let dir = level.dir.clone();
        let render = cx.processor(Tab::column_row);
        let (sel, act) = (me.clone(), me);
        let at = dir.clone();
        let dir_of_act = dir.clone();
        list(id.to_owned(), &level.list, move |key, w, cx| render((dir.clone(), key), w, cx))
            .selection(Selection::single())
            .on_select(move |e: &SelectEvent<EntryId>, window, cx| {
                let to = at.to_path_buf();
                let key = e.key;
                let _ = sel.update(cx, |t, cx| t.retarget(Location::Dir(to), Some(key), cx));
                window.focus(CURRENT);
            })
            .on_activate(move |e: &ActivateEvent<EntryId>, window, cx| {
                let key = e.key;
                let _ = act.update(cx, |t, cx| {
                    if t.second_press(window) {
                        return;
                    }
                    let Some((path, is_dir)) = t.extras.columns.path_in(&dir_of_act, key) else { return };
                    if is_dir {
                        t.navigate(Location::Dir(path), window, cx);
                    } else {
                        crate::keys::open_file(&path, window, cx);
                    }
                });
            })
            .into_any_element()
    };

    let mut row_items: Vec<AnyElement> = Vec::new();
    for (i, level) in tab.extras.columns.levels.iter().enumerate() {
        let id = format!("column-{i}");
        row_items.push(column(&id, level_list(level, &id, me.clone(), cx)));
    }
    let current = list(CURRENT, &tab.extras.columns.current, cx.processor(Tab::current_row));
    let current = tab.file_list(current, &tab.extras.columns.current, cx).into_any_element();
    row_items.push(column(CURRENT, current));
    if let Some(child) = &tab.extras.columns.child {
        row_items.push(column(NEXT, level_list(child, NEXT, me.clone(), cx)));
    }
    if file.is_some()
        && let Some(preview) = &tab.extras.columns.preview
    {
        let resized = preview.downgrade();
        row_items.push(
            div()
                .id(PREVIEW_ID)
                .flex_1()
                .flex_shrink_0()
                .min_w(px(PREVIEW_MIN))
                .h_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("column-preview-body")
                        .w_full()
                        .max_w(px(PREVIEW_MAX))
                        .h_full()
                        .on_resize(move |r, _, cx| {
                            let _ = resized.update(cx, |p, cx| p.set_width(r.width, cx));
                        })
                        .child(preview.clone()),
                )
                .into_any_element(),
        );
    }
    h_flex()
        .id("columns")
        .when(tab.renaming().is_none(), |d| d.key_context(COLUMNS))
        .size_full()
        // nd-allow(scroll-list): one column per folder from the start folder down, a handful
        .overflow_x_scroll()
        .children(row_items)
        .into_any_element()
}

/// Scrolls the column row to `id` once the frame being built has laid it out.
fn reveal(tab: &Tab, id: String) {
    let Some(window) = tab.window_id() else { return };
    if !crate::pump::live() {
        return;
    }
    nd::system::spawn(async {}, move |_, cx| {
        cx.scroll_into_view(window, &id);
    });
}

impl Tab {
    /// Waits for every column's listing, for tests.
    pub fn drain_columns(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        let show_hidden = self.show_hidden();
        self.extras.columns.drain(show_hidden, timeout, cx);
        cx.notify();
    }

    pub fn column_view(&self) -> &Columns {
        &self.extras.columns
    }
}
