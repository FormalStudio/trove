//! One tab: its location and history, the streaming listing, sort, filter, selection, scroll
//! and view mode, and the content it draws. A notify here re-renders this tab's content and
//! whatever observes it (the status bar, the preview), never the sidebar or the other tabs.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use nd::core::WindowId;
use nd::prelude::*;
use nd::virtual_list::{List, RowDropped, SelectionChanged};
use trove_fs::{
    Change, Entry, EntryId, EntryKind, IdMap, IdSet, ListError, ListEvent, ListOptions, Listing, Meta, SortKey,
    SortOrder, Usage,
};

use crate::actions::{DRAG_FILES, DropOnto, FILES, OpenPlaceInNewTab, RENAMING};
use crate::fileops::Transfer;
use crate::format::{self, Today};
use crate::icons;
use crate::location::Location;
use crate::menus;
use crate::ops::{self, Clip, Origin};
use crate::pump::pump;
use crate::trash::{self, TrashInfo};
use crate::zoom::{RowSize, TileSize, Zoom};

const TYPE_SELECT_WINDOW: f64 = 0.8;
const SIZE_W: f32 = 96.;
const KIND_W: f32 = 132.;
const DATE_W: f32 = 168.;
const NAME_MIN: f32 = 200.;
const NAME_MAX: f32 = 560.;
/// The share of a wide window the names grow to before the other columns take the rest.
const NAME_SHARE: f32 = 0.5;
/// What a still-unmeasured content area is taken to be on the first frame.
const DEFAULT_WIDTH: f32 = 760.;
const ORIGIN_W: f32 = 220.;
/// How long a job's results wait for the listing to show them before selection gives up.
const PENDING_FOR: f64 = 4.;
pub const RENAME_FIELD: &str = "rename";
/// The listing's box, whatever it shows: it takes the keys itself only when no list shows.
pub const VIEW_ID: &str = "files";
pub const TABLE_ID: &str = "files-table";
pub const GRID_ID: &str = "files-grid";

/// A name being edited in place.
#[derive(Clone, Debug, PartialEq)]
pub struct Rename {
    pub id: EntryId,
    pub original: String,
    pub text: SharedString,
    pub error: Option<String>,
    /// The rename job running for it; the field stays until it ends.
    pub job: Option<trove_ops::JobId>,
}

/// What a finished job made, selected as soon as the listing has it.
struct Pending {
    ids: Vec<EntryId>,
    rename: bool,
    until: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    List,
    Grid,
    Columns,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    /// No first chunk after a second: a slow or dead mount.
    Slow,
    Listed,
    Complete,
    Failed(ListError),
}

pub struct Tab {
    location: Location,
    back: Vec<Location>,
    forward: Vec<Location>,
    entries: Vec<Entry>,
    index: IdMap<usize>,
    versions: IdMap<u64>,
    /// Full paths of Recent's entries, which share no folder.
    paths: HashMap<EntryId, PathBuf>,
    visible: Vec<EntryId>,
    table: ListState<EntryId>,
    grid: ListState<EntryId>,
    view: View,
    sort: SortOrder,
    recent_sort: SortOrder,
    show_hidden: bool,
    search: Entity<TextFieldState>,
    query: SharedString,
    load: Load,
    listing: Option<Listing>,
    events: Option<flume::Receiver<ListEvent>>,
    recent: Option<flume::Receiver<Vec<(PathBuf, Entry)>>>,
    usage: Option<flume::Receiver<Option<Usage>>>,
    generation: u64,
    free: Option<Usage>,
    waiting_for: SharedString,
    today: Today,
    typed: String,
    typed_at: f64,
    /// The row the last click or key landed on, which Enter opens.
    cursor: Option<EntryId>,
    /// Selected and scrolled to as soon as the listing has it: the folder just left on the way
    /// up, or the file named on the command line.
    reveal: Option<EntryId>,
    width: f32,
    /// The shown folders, which take drops on their rows.
    folders: Rc<IdSet>,
    /// The tiles last drawn selected. The grid redraws a tile only when its version moves, and
    /// a tile draws its own selection, so a change of selection bumps the tiles it touches.
    lit: IdSet,
    /// The location came off the command line and may name a file: on `NotADirectory` the
    /// tab opens its folder with it selected instead of failing.
    maybe_file: bool,
    home: PathBuf,
    trash: PathBuf,
    window: Option<WindowId>,
    renaming: Option<Rename>,
    cut: HashSet<EntryId>,
    pending: Option<Pending>,
    /// The listing is a trash's `files` folder.
    in_trash: bool,
    trash_info: HashMap<EntryId, TrashInfo>,
    trash_asked: HashSet<EntryId>,
    /// An element to focus on the next frame, for state changes that arrive without a window.
    focus_next: Option<&'static str>,
    /// The element [`Tab::focus_target`] named on the last frame, none before the first.
    focus_shown: &'static str,
    /// The generation whose first names `TROVE_TRACE` reported.
    names_traced: u64,
    /// The rows the view's list was last given.
    synced: (View, Vec<(EntryId, u64)>),
    /// Picking for another app: which files list, and activating a file accepts the pick.
    pick: Option<Rc<crate::picker::Shown>>,
    zoom: Zoom,
    /// Ctrl+scroll not yet a whole zoom step: a trackpad sends fractions of a notch.
    zoom_scroll: f32,
    /// When a single click last opened something, on the frame clock.
    tapped_at: f64,
    _clip: Subscription,
    pub(crate) extras: crate::extras::TabExtras,
}

impl Tab {
    pub fn new(location: Location, show_hidden: bool, view: View, cx: &mut Context<Self>) -> Self {
        let zoom = Zoom::of(&crate::settings::get(cx));
        let table = ListState::new(cx).estimated_row_height(px(zoom.row().height));
        let grid = ListState::new(cx).estimated_row_height(px(zoom.tile().tile.1));
        let search = cx.new(|_| TextFieldState::default().placeholder("Search"));
        cx.observe(&search, |this: &mut Tab, search, cx| {
            if search.read(cx).last_event() == Some(TextEvent::Edited) {
                this.query = search.read(cx).text().clone();
                crate::trace::timed("filter", || {
                    crate::results::before_refresh(this, cx);
                    this.refresh(cx);
                    crate::results::after_refresh(this, cx);
                });
            }
        })
        .detach();
        let clip = ops::clip(cx);
        let watch_clip = cx.observe(&clip, |this: &mut Tab, clip, cx| this.sync_cut(&clip, cx));
        let weak = cx.weak_entity();
        ops::jobs(cx).update(cx, |j, _| j.register(weak));
        let mut tab = Self {
            location: location.clone(),
            back: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            index: IdMap::default(),
            versions: IdMap::default(),
            lit: IdSet::default(),
            paths: HashMap::new(),
            visible: Vec::new(),
            table,
            grid,
            view,
            sort: SortOrder::default(),
            recent_sort: SortOrder { key: SortKey::Modified, ascending: false, folders_first: false },
            show_hidden,
            search,
            query: SharedString::default(),
            load: Load::Loading,
            listing: None,
            events: None,
            recent: None,
            usage: None,
            generation: 0,
            free: None,
            waiting_for: SharedString::default(),
            today: Today::now(),
            typed: String::new(),
            typed_at: f64::NEG_INFINITY,
            cursor: None,
            reveal: None,
            width: DEFAULT_WIDTH,
            folders: Rc::default(),
            maybe_file: false,
            home: crate::env::get().home.clone(),
            trash: crate::env::get().trash.clone(),
            window: None,
            renaming: None,
            cut: HashSet::new(),
            pending: None,
            in_trash: false,
            trash_info: HashMap::new(),
            trash_asked: HashSet::new(),
            focus_next: None,
            focus_shown: "",
            names_traced: 0,
            synced: (view, Vec::new()),
            pick: None,
            zoom,
            tapped_at: f64::NEG_INFINITY,
            zoom_scroll: 0.,
            _clip: watch_clip,
            extras: crate::extras::TabExtras::new(cx),
        };
        tab.load(location, cx);
        tab
    }

    /// See `maybe_file`.
    pub fn open_parent_if_file(&mut self) {
        self.maybe_file = true;
    }

    // Reading.

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn title(&self) -> String {
        self.location.title(&self.home, &self.trash)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn sort(&self) -> SortOrder {
        if self.location == Location::Recent { self.recent_sort } else { self.sort }
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn load_state(&self) -> &Load {
        &self.load
    }

    pub fn search(&self) -> &Entity<TextFieldState> {
        &self.search
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn free(&self) -> Option<Usage> {
        self.free
    }

    /// What the folder shows with no search: hidden entries only while they are shown.
    pub fn total(&self) -> usize {
        if self.show_hidden { self.entries.len() } else { self.entries.iter().filter(|e| !e.hidden).count() }
    }

    /// What the view shows, in order.
    pub fn visible(&self) -> &[EntryId] {
        &self.visible
    }

    pub fn entry(&self, id: &EntryId) -> Option<&Entry> {
        self.index.get(id).and_then(|i| self.entries.get(*i))
    }

    pub fn visible_names(&self) -> Vec<String> {
        self.visible.iter().filter_map(|id| self.entry(id)).map(|e| e.name.to_string()).collect()
    }

    pub fn list_state(&self) -> &ListState<EntryId> {
        match self.view {
            View::List => &self.table,
            View::Grid => &self.grid,
            View::Columns => &self.extras.columns.current,
        }
    }

    fn list_state_mut(&mut self) -> &mut ListState<EntryId> {
        match self.view {
            View::List => &mut self.table,
            View::Grid => &mut self.grid,
            View::Columns => &mut self.extras.columns.current,
        }
    }

    pub fn selected(&self) -> Vec<EntryId> {
        if self.extras.results.shown() {
            return self.extras.results.selected();
        }
        self.list_state().selected()
    }

    pub fn selected_names(&self) -> Vec<String> {
        self.selected().iter().filter_map(|id| self.entry(id)).map(|e| e.name.to_string()).collect()
    }

    pub fn path_of(&self, id: &EntryId) -> Option<PathBuf> {
        if let Some(p) = self.extras.results.path_of(id) {
            return Some(p);
        }
        if let Some(p) = self.paths.get(id) {
            return Some(p.clone());
        }
        Some(self.entry(id)?.path_in(self.location.dir()?))
    }

    pub fn today(&self) -> &Today {
        &self.today
    }

    // Navigation. Nothing here waits on the old listing: dropping it cancels its threads.

    pub fn navigate(&mut self, to: Location, window: &mut Window, cx: &mut Context<Self>) {
        if to == self.location {
            return;
        }
        let from = std::mem::replace(&mut self.location, to.clone());
        self.back.push(from.clone());
        self.forward.clear();
        self.clear_search(window, cx);
        self.reveal = child_of(&from, &to);
        self.load(to, cx);
    }

    /// Opens `dir` with `file` selected in it: a file path given on the command line.
    pub fn reveal_in(&mut self, file: &Path, cx: &mut Context<Self>) {
        self.reveal = file.file_name().map(EntryId::of);
        self.apply_reveal(cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(to) = self.back.pop() else { return };
        let from = std::mem::replace(&mut self.location, to.clone());
        self.forward.push(from.clone());
        self.clear_search(window, cx);
        self.reveal = child_of(&from, &to);
        self.load(to, cx);
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(to) = self.forward.pop() else { return };
        let from = std::mem::replace(&mut self.location, to.clone());
        self.back.push(from.clone());
        self.clear_search(window, cx);
        self.reveal = child_of(&from, &to);
        self.load(to, cx);
    }

    pub fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.location.parent() {
            self.navigate(parent, window, cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let keep = self.selected();
        let at = self.location.clone();
        self.load(at, cx);
        self.reveal = keep.first().copied();
    }

    fn load(&mut self, location: Location, cx: &mut Context<Self>) {
        self.listing = None;
        self.events = None;
        self.recent = None;
        self.usage = None;
        self.generation += 1;
        self.entries.clear();
        self.index.clear();
        self.versions.clear();
        self.paths.clear();
        self.visible.clear();
        self.cursor = None;
        self.typed.clear();
        self.renaming = None;
        self.pending = None;
        self.trash_info.clear();
        self.trash_asked.clear();
        self.in_trash = location.dir().is_some_and(|d| trash::is_trash_files(d, &self.trash));
        self.load = Load::Loading;
        self.free = None;
        self.today = Today::now();
        crate::thumbs::cancel(cx);
        self.synced.1.clear();
        for state in [&mut self.table, &mut self.grid] {
            state.set_items(Vec::new(), cx);
            state.scroll_to_top(cx);
        }
        let generation = self.generation;
        crate::trace::mark("load");
        let me = cx.weak_entity();
        match &location {
            Location::Dir(dir) => {
                let listing = trove_fs::list(dir.clone(), ListOptions { show_hidden: self.show_hidden });
                self.events = Some(listing.events.clone());
                pump(listing.events.clone(), me.clone(), move |tab: &mut Tab, batch, cx| {
                    tab.generation == generation && crate::trace::timed("apply", || tab.apply(batch, cx))
                });
                self.listing = Some(listing);
                let usage = crate::env::usage(dir.clone(), Duration::from_secs(2));
                self.usage = Some(usage.clone());
                pump(usage, me, move |tab: &mut Tab, answers, cx| {
                    if tab.generation == generation {
                        tab.free = answers.into_iter().flatten().last();
                        cx.notify();
                    }
                    false
                });
            }
            Location::Recent => {
                let rx = recent_entries();
                self.recent = Some(rx.clone());
                pump(rx, me, move |tab: &mut Tab, batches, cx| {
                    if tab.generation == generation {
                        tab.apply_recent(batches.into_iter().flatten().collect(), cx);
                    }
                    false
                });
            }
        }
        cx.notify();
    }

    /// Waits up to `timeout` for the listing to finish and applies everything it sent: what a
    /// host's pump does, for tier 1 tests and the appearance audit, which run no host.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
        let deadline = std::time::Instant::now() + timeout;
        if let Some(rx) = self.recent.take()
            && let Ok(batch) = rx.recv_timeout(timeout)
        {
            self.apply_recent(batch, cx);
        }
        while let Some(rx) = self.events.clone() {
            if matches!(self.load, Load::Complete | Load::Failed(_)) {
                break;
            }
            // nd-allow(clock-in-ui): as above
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(first) = rx.recv_timeout(left) else { break };
            let mut batch = vec![first];
            batch.extend(rx.try_iter());
            self.apply(batch, cx);
        }
        // Live changes after the listing finished: what a job made lands this way.
        if let Some(rx) = self.events.clone() {
            loop {
                let mut batch: Vec<ListEvent> = rx.try_iter().collect();
                if batch.is_empty() {
                    if self.pending.is_none() {
                        break;
                    }
                    match rx.recv_timeout(Duration::from_millis(1500)) {
                        Ok(e) => batch.push(e),
                        Err(_) => break,
                    }
                }
                self.apply(batch, cx);
            }
        }
        if let Some(rx) = self.usage.take() {
            self.free = rx.recv_timeout(Duration::from_millis(500)).ok().flatten();
        }
        cx.notify();
    }

    // The listing's events.

    fn apply(&mut self, batch: Vec<ListEvent>, cx: &mut Context<Self>) -> bool {
        let mut resort = false;
        for event in batch {
            match event {
                ListEvent::Entries(chunk) => {
                    self.maybe_file = false;
                    for e in chunk {
                        resort |= self.insert(e);
                    }
                    if self.load == Load::Slow {
                        self.load = Load::Loading;
                    }
                }
                ListEvent::Meta(metas) => {
                    for (id, meta) in metas {
                        resort |= self.set_meta(id, meta);
                    }
                }
                ListEvent::Listed { .. } => {
                    if !matches!(self.load, Load::Failed(_) | Load::Complete) {
                        self.load = Load::Listed;
                    }
                }
                ListEvent::Complete => {
                    crate::trace::mark("complete");
                    if !matches!(self.load, Load::Failed(_)) {
                        self.load = Load::Complete;
                    }
                }
                ListEvent::Slow => {
                    if self.entries.is_empty() {
                        self.load = Load::Slow;
                        self.waiting_for = self.volume_name().into();
                    }
                }
                ListEvent::Error(ListError::NotADirectory) if self.maybe_file => {
                    self.maybe_file = false;
                    if let Location::Dir(file) = self.location.clone()
                        && let Some(dir) = file.parent()
                    {
                        self.location = Location::Dir(dir.to_path_buf());
                        self.load(self.location.clone(), cx);
                        self.reveal_in(&file, cx);
                        return false;
                    }
                }
                ListEvent::Error(e) => self.load = Load::Failed(e),
                ListEvent::Changed(changes) => {
                    for change in changes {
                        resort |= self.change(change);
                    }
                }
            }
        }
        if resort {
            self.resort();
        }
        self.refresh(cx);
        if self.in_trash {
            self.ask_trash_info(cx);
        }
        true
    }

    fn apply_recent(&mut self, batch: Vec<(PathBuf, Entry)>, cx: &mut Context<Self>) {
        for (path, entry) in batch {
            self.paths.insert(entry.id, path);
            self.insert(entry);
        }
        self.load = Load::Complete;
        self.resort();
        self.refresh(cx);
    }

    /// Adds an entry the view has not seen; true when the order needs work.
    fn insert(&mut self, e: Entry) -> bool {
        if self.index.contains_key(&e.id) {
            return false;
        }
        self.index.insert(e.id, self.entries.len());
        self.entries.push(e);
        true
    }

    fn set_meta(&mut self, id: EntryId, meta: Meta) -> bool {
        let sort = self.sort();
        let Some(&i) = self.index.get(&id) else { return false };
        let entry = &mut self.entries[i];
        let was_dir = entry.is_dir();
        entry.meta = Some(meta);
        *self.versions.entry(id).or_insert(0) += 1;
        was_dir != entry.is_dir() || !matches!(sort.key, SortKey::Name | SortKey::Kind)
    }

    fn change(&mut self, change: Change) -> bool {
        match change {
            Change::Added(e) => {
                let id = e.id;
                if self.index.contains_key(&id) {
                    if let Some(meta) = e.meta {
                        self.set_meta(id, meta);
                    }
                    false
                } else {
                    self.insert(e)
                }
            }
            Change::Removed(id) => self.remove(id),
            Change::Updated(id, meta) => self.set_meta(id, meta),
            Change::Renamed { from, to } => {
                let was_selected = self.selected().contains(&from);
                let id = to.id;
                self.remove(from);
                self.insert(to);
                if was_selected {
                    self.reveal = Some(id);
                }
                true
            }
        }
    }

    fn remove(&mut self, id: EntryId) -> bool {
        let Some(i) = self.index.remove(&id) else { return false };
        self.entries.remove(i);
        self.versions.remove(&id);
        true
    }

    fn resort(&mut self) {
        let sort = self.sort();
        trove_fs::sort_entries(&mut self.entries, sort);
        self.index = self.entries.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
    }

    /// Recomputes what shows from the sorted entries, the hidden toggle and the filter, and
    /// hands the keys to the view in use. Only rows whose version moved render again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let filter = Filter::new(self.query.trim());
        let (show_hidden, in_trash) = (self.show_hidden, self.in_trash);
        let pick = self.pick.clone();
        let mut folders = IdSet::default();
        self.visible.clear();
        for e in &self.entries {
            if (show_hidden || !e.hidden)
                && filter.matches(&e.name)
                && pick.as_ref().is_none_or(|p| p.shows(e, listed_folder(e)))
            {
                self.visible.push(e.id);
                if !in_trash && listed_folder(e) {
                    folders.insert(e.id);
                }
            }
        }
        self.folders = Rc::new(folders);
        // Search results cover the listing; its list catches up when they go, which spares a
        // keystroke handing a 100k-row list over that nobody sees.
        if !self.extras.results.shown() {
            self.sync_view(cx);
        }
        self.apply_reveal(cx);
        self.apply_pending(cx);
        cx.notify();
    }

    fn sync_view(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<(EntryId, u64)> =
            self.visible.iter().map(|id| (*id, self.versions.get(id).copied().unwrap_or(0))).collect();
        // The list hashes every row it is handed, which on 100k rows is longer than a frame,
        // so the same rows at the same versions are not handed over again.
        if self.view != View::Columns && self.synced.0 == self.view && self.synced.1 == rows {
            return;
        }
        self.list_state_mut().set_items_versioned(rows.iter().copied(), cx);
        self.synced = (self.view, rows);
    }

    fn apply_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.reveal else { return };
        if !self.visible.contains(&id) {
            return;
        }
        self.reveal = None;
        self.select_only(id, cx);
    }

    fn volume_name(&self) -> String {
        let Some(dir) = self.location.dir() else { return "the folder".to_owned() };
        let vols = crate::env::volumes();
        vols.iter()
            .filter(|v| dir.starts_with(&v.mount_point))
            .max_by_key(|v| v.mount_point.components().count())
            .map(|v| {
                if !v.label.is_empty() {
                    v.label.clone()
                } else {
                    v.mount_point
                        .file_name()
                        .map_or_else(|| "the disk".to_owned(), |n| n.to_string_lossy().into_owned())
                }
            })
            .unwrap_or_else(|| "the folder".to_owned())
    }

    // View settings.

    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.view == view {
            return;
        }
        let selected = self.selected();
        self.view = view;
        self.sync_view(cx);
        self.list_state_mut().set_selected(selected.clone(), cx);
        if let Some(first) = self.cursor.or_else(|| selected.first().copied()) {
            self.list_state_mut().jump_to(first, Align::Center, px(0.), cx);
        }
        cx.notify();
    }

    /// Lists for a pick: the files `shown` passes, one row selectable unless it allows many.
    pub fn set_pick(&mut self, shown: Rc<crate::picker::Shown>, cx: &mut Context<Self>) {
        self.pick = Some(shown);
        self.refresh(cx);
    }

    pub fn picking(&self) -> bool {
        self.pick.is_some()
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    pub(crate) fn row_size(&self) -> RowSize {
        self.zoom.row()
    }

    pub(crate) fn tile_size(&self) -> TileSize {
        self.zoom.tile()
    }

    /// Redraws every row and tile at the sizes `zoom` gives.
    pub fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if self.zoom == zoom {
            return;
        }
        self.zoom = zoom;
        self.table.invalidate_all(cx);
        self.grid.invalidate_all(cx);
        self.extras.results.list.invalidate_all(cx);
        self.extras.columns.invalidate_all(cx);
        cx.notify();
    }

    pub fn set_show_hidden(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.refresh(cx);
        }
    }

    pub fn set_sort(&mut self, sort: SortOrder, cx: &mut Context<Self>) {
        if self.location == Location::Recent {
            self.recent_sort = sort;
        } else {
            self.sort = sort;
        }
        crate::trace::timed("sort", || {
            self.resort();
            self.refresh(cx);
        });
    }

    fn on_sort(&mut self, e: &SortChanged, cx: &mut Context<Self>) {
        let key = match e.column.as_ref() {
            "size" => SortKey::Size,
            "kind" => SortKey::Kind,
            "modified" => SortKey::Modified,
            _ => SortKey::Name,
        };
        let ascending = e.direction == SortDirection::Ascending;
        let folders_first = self.location != Location::Recent;
        self.set_sort(SortOrder { key, ascending, folders_first }, cx);
    }

    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.query.is_empty() && self.search.read(cx).text().is_empty() {
            return false;
        }
        self.search.update(cx, |s, cx| s.set_text("", window, cx));
        self.query = SharedString::default();
        crate::results::before_refresh(self, cx);
        self.refresh(cx);
        crate::results::after_refresh(self, cx);
        true
    }

    // Selection. The lists select as the platform does (clicks, Shift and Ctrl, the arrow
    // keys, Ctrl+A, the marquee) and report each change here.

    fn select_only(&mut self, id: EntryId, cx: &mut Context<Self>) {
        self.cursor = Some(id);
        let state = self.list_state_mut();
        state.set_selected(vec![id], cx);
        state.scroll_to(id, Align::Nearest, px(0.), cx);
        cx.notify();
    }

    pub(crate) fn on_selection_change(&mut self, e: &SelectionChanged<EntryId>, cx: &mut Context<Self>) {
        self.pending = None;
        self.cursor = e.cursor.or_else(|| e.keys.last().copied());
        cx.notify();
    }

    /// Selects the row `by` rows from the cursor, wrapping round: Quick Look's arrows.
    pub fn select_step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.extras.results.shown() {
            self.extras.results.select_step(by, cx);
            return cx.notify();
        }
        if let Some(id) = step_in(&self.visible, self.cursor, by) {
            self.select_only(id, cx);
        }
    }

    /// Escape: clears the search first, then the selection.
    pub fn deselect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clear_search(window, cx) {
            return;
        }
        self.list_state_mut().set_selected(Vec::new(), cx);
        self.cursor = None;
        cx.notify();
    }

    /// Type-select: characters typed within a short pause build a prefix, and the first name
    /// in natural order starting with it is selected.
    pub fn type_select(&mut self, c: char, now: f64, cx: &mut Context<Self>) {
        if self.extras.results.shown() {
            self.extras.results.type_select(c, now, cx);
            return cx.notify();
        }
        if now - self.typed_at > TYPE_SELECT_WINDOW {
            self.typed.clear();
        }
        self.typed_at = now;
        self.typed.push(c);
        let prefix = self.typed.to_lowercase();
        let found = self
            .visible
            .iter()
            .filter_map(|id| self.entry(id))
            .filter(|e| e.name.to_lowercase().starts_with(&prefix))
            .min_by(|a, b| trove_fs::natural_cmp(&a.name, &b.name))
            .map(|e| e.id);
        if let Some(id) = found {
            self.select_only(id, cx);
        }
    }

    /// The entry to open with Enter or a double click: the cursor's row, else the only one
    /// selected.
    pub fn target(&self) -> Option<EntryId> {
        if self.extras.results.shown() {
            return self.extras.results.target();
        }
        let selected = self.selected();
        self.cursor.filter(|c| selected.contains(c)).or_else(|| selected.first().copied())
    }

    /// Whether `id` names a folder this tab can go into.
    pub fn is_folder(&self, id: &EntryId) -> bool {
        if self.extras.results.shown() {
            return self.extras.results.is_dir(id);
        }
        self.listed_folder(id)
    }

    /// Whether `id` names a folder of the listing, results or not.
    fn listed_folder(&self, id: &EntryId) -> bool {
        self.entry(id).is_some_and(listed_folder)
    }

    /// Tiles to a row: what Up and Down step by.
    pub fn lanes(&self) -> usize {
        match self.view {
            View::Grid if !self.extras.results.shown() => self.grid.handle().0.borrow().lanes().max(1),
            _ => 1,
        }
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if (self.width - width).abs() < 0.5 {
            return;
        }
        self.width = width;
        self.table.invalidate_all(cx);
        self.extras.results.list.invalidate_all(cx);
        cx.notify();
    }

    // Drawing.

    /// The table's columns by id, in order: a trash shows where each item came from and when
    /// it was deleted instead of its kind and date.
    /// A narrow pane drops Kind, then Size, before the names get too short to read.
    fn column_ids(&self) -> &'static [&'static str] {
        if self.in_trash {
            return &["name", "origin", "deleted", "size"];
        }
        if self.width - SIZE_W - KIND_W - DATE_W >= NAME_MIN {
            &["name", "size", "kind", "modified"]
        } else if self.width - SIZE_W - DATE_W >= NAME_MIN {
            &["name", "size", "modified"]
        } else {
            &["name", "modified"]
        }
    }

    pub fn columns(&self) -> Vec<Column> {
        let sort = self.sort();
        let dir = if sort.ascending { SortDirection::Ascending } else { SortDirection::Descending };
        let col = |id: &'static str, title: &'static str, key: SortKey, width: f32| {
            let c = column(id, title).width(width).min_width(60.);
            if sort.key == key { c.sorted(dir) } else { c.sortable() }
        };
        if self.in_trash {
            let name = (self.width - ORIGIN_W - DATE_W - SIZE_W).max(NAME_MIN);
            return vec![
                col("name", "Name", SortKey::Name, name),
                column("origin", "Original location").width(ORIGIN_W).min_width(60.),
                column("deleted", "Date deleted").width(DATE_W).min_width(60.),
                col("size", "Size", SortKey::Size, SIZE_W),
            ];
        }
        let ids = self.column_ids();
        let rest: f32 = ids
            .iter()
            .map(|id| match *id {
                "size" => SIZE_W,
                "kind" => KIND_W,
                "modified" => DATE_W,
                _ => 0.,
            })
            .sum();
        // The names grow with the window to half of it; past that the details share the rest,
        // so a wide window keeps them beside the names with no empty band at the end.
        let name = (self.width - rest).clamp(NAME_MIN, NAME_MAX.max(self.width * NAME_SHARE));
        let more = (self.width - rest - name).max(0.) / (ids.len() - 1).max(1) as f32;
        ids.iter()
            .map(|id| match *id {
                "size" => col("size", "Size", SortKey::Size, SIZE_W + more),
                "kind" => col("kind", "Kind", SortKey::Kind, KIND_W + more),
                "modified" => col("modified", "Modified", SortKey::Modified, DATE_W + more),
                _ => col("name", "Name", SortKey::Name, name),
            })
            .collect()
    }

    fn cell(&mut self, id: EntryId, col: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(e) = self.entry(&id) else { return div().into_any_element() };
        let col_id = self.column_ids().get(col).copied().unwrap_or("name");
        if col_id == "name"
            && let Some(r) = self.renaming.as_ref().filter(|r| r.id == id)
        {
            return self.rename_field(r, false, cx);
        }
        let size = self.row_size();
        let thumb = if col_id == "name" { crate::thumbs::for_entry(self.path_of(&id), e, size.art, cx) } else { None };
        let theme = cx.theme();
        let info = self.trash_info.get(&id);
        let quiet = |s: String| text(s).text_color(theme.text.secondary).tabular_nums().truncate().into_any_element();
        let content = match col_id {
            "name" => {
                let symlinked = matches!(e.kind, EntryKind::Symlink { .. }) && e.is_dir();
                let glyph = if symlinked { icons::symlinked_folder() } else { icons::for_kind(e.file_kind) };
                h_flex()
                    .gap(theme.spacing.sm)
                    .min_w_0()
                    .child(crate::thumbs::row_art(thumb, glyph, icons::tint(e.file_kind, theme), size, theme))
                    .child(div().flex_1().min_w_0().child(
                        text(e.name.to_string()).truncate().when(e.hidden, |t| t.text_color(theme.text.secondary)),
                    ))
                    .into_any_element()
            }
            "size" if e.is_dir() && e.meta.as_ref().and_then(|m| m.child_count).is_none() => quiet("--".to_owned()),
            "size" => quiet(size_text(e)),
            "kind" => quiet(e.file_kind.label().to_owned()),
            "origin" => quiet(info.and_then(|i| i.origin.as_ref()).map(|p| self.place_label(p)).unwrap_or_default()),
            "deleted" => quiet(
                info.and_then(|i| i.deleted.as_deref())
                    .map(|d| trash::deleted_label(d, &self.today))
                    .unwrap_or_default(),
            ),
            _ => quiet(e.meta.as_ref().and_then(|m| m.modified).map(|t| self.today.modified(t)).unwrap_or_default()),
        };
        let row = h_flex()
            .when(col_id == "name", |d| d.id(row_id(&e.name)))
            .h(px(size.height))
            .px(theme.spacing.sm)
            .items_center()
            .when(self.cut.contains(&id), |d| d.opacity(0.5))
            .child(content);
        self.file_events(row, id, cx).into_any_element()
    }

    /// A row's or tile's menu, a click opening it under the single click setting, and a middle
    /// click on a folder opening it in a new tab.
    pub(crate) fn file_events(&self, d: Div, id: EntryId, cx: &mut Context<Self>) -> Div {
        let Some(path) = self.path_of(&id) else { return d };
        let (me, tap) = (cx.weak_entity(), cx.weak_entity());
        let d = d
            .context_menu(move |menu, _, cx| {
                let Some(target) = me.update(cx, |t, cx| t.menu_target(id, cx)) else { return menu };
                menus::files(menu, &target, cx)
            })
            .on_tap(move |e: &TapEvent, window, cx| {
                let _ = tap.update(cx, |t, cx| t.tapped(id, e, window, cx));
            });
        if !self.is_folder(&id) {
            return d;
        }
        d.on_middle_click(move |_: &MiddleClickEvent, window, cx| {
            cx.dispatch_action(window.id(), &OpenPlaceInNewTab(Location::Dir(path.clone())));
        })
    }

    /// Where a drop on a row or on empty space goes: into the folder dropped on, else the folder
    /// shown. Nothing lands in a trash, in Recent or on the results' empty space.
    pub(crate) fn drop_on_row(&self, e: &RowDropped<EntryId>) -> Option<DropOnto> {
        let kind = e.item.kind.as_ref();
        if self.in_trash || kind != DRAG_FILES && kind != DragItem::FILES {
            return None;
        }
        let dest = match e.key {
            Some(id) if self.is_folder(&id) => self.path_of(&id),
            _ if self.extras.results.shown() => None,
            _ => self.location.dir().map(Path::to_path_buf),
        }?;
        let paths = e.item.ids.iter().map(|id| PathBuf::from(id.as_ref())).collect();
        Some(DropOnto { paths, dest, mode: Transfer::from_modifiers(e.modifiers) })
    }

    /// What a dragged row carries: its path.
    pub(crate) fn drag_id(&self, id: &EntryId) -> SharedString {
        self.path_of(id).map(|p| p.display().to_string()).unwrap_or_default().into()
    }

    /// The name field of a rename in progress, in a row or (`tile`) under a tile's picture.
    pub(crate) fn rename_field(&self, r: &Rename, tile: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (change, end, cancel) = (cx.weak_entity(), cx.weak_entity(), cx.weak_entity());
        let folder = self.listed_folder(&r.id);
        let field = text_field(RENAME_FIELD)
            .text(r.text.clone())
            .label("Name")
            .disabled(r.job.is_some())
            .select_on_focus(0..stem_chars(&r.original, folder))
            .flex_1()
            .min_w_0()
            .on_change(move |e: &TextChanged, _, cx| {
                let _ = change.update(cx, |t, cx| t.set_rename_text(e.text.clone(), cx));
            })
            .on_end_editing(move |e: &TextEditingEnded, _, cx| {
                let _ = end.update(cx, |t, cx| {
                    t.set_rename_text(e.text.clone(), cx);
                    t.commit_rename(cx);
                });
            })
            .on_cancel(move |_: &TextCancelled, _, cx| {
                let _ = cancel.update(cx, |t, cx| t.cancel_rename(cx));
            });
        let error = r.error.clone().map(|why| {
            text(why).id("rename-error").text_size(theme.type_scale.footnote).text_color(theme.text.danger).truncate()
        });
        if tile {
            return v_flex().w_full().gap(theme.spacing.xs).child(field).children(error).into_any_element();
        }
        let size = self.row_size();
        let glyph = self.entry(&r.id).map(|e| {
            div()
                .size(px(size.art))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icons::for_kind(e.file_kind), size.glyph).text_color(icons::tint(e.file_kind, theme)))
        });
        h_flex()
            .h(px(size.height))
            .px(theme.spacing.sm)
            .gap(theme.spacing.sm)
            .items_center()
            .children(glyph)
            .child(field)
            .children(error.map(|e| div().max_w(px(260.)).child(e)))
            .into_any_element()
    }

    /// "Documents" for a folder inside the home, else the full path.
    fn place_label(&self, p: &Path) -> String {
        if p == self.home {
            return "Home".to_owned();
        }
        match p.strip_prefix(&self.home) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => p.display().to_string(),
        }
    }

    fn light_tiles(&mut self, cx: &mut Context<Self>) {
        let now: IdSet = self.grid.selected().into_iter().collect();
        if now == self.lit {
            return;
        }
        let moved: Vec<EntryId> = now.symmetric_difference(&self.lit).copied().collect();
        for id in moved {
            self.bump(id);
        }
        self.lit = now;
        self.sync_view(cx);
    }

    fn tile(&mut self, id: EntryId, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let field = self.renaming.as_ref().filter(|r| r.id == id).map(|r| self.rename_field(r, true, cx));
        let path = self.path_of(&id);
        let Some(e) = self.entry(&id) else { return div().into_any_element() };
        let lit = self.lit.contains(&id);
        let size = self.tile_size();
        let art = crate::thumbs::tile_art(path, e, lit, size, cx);
        let theme = cx.theme();
        let label = match field {
            Some(field) => field,
            None => div()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w_full()
                        .px(theme.spacing.xs)
                        .rounded(theme.radius.sm)
                        .when(lit, |d| d.bg(theme.accent_fill))
                        .child(
                            text(e.name.to_string())
                                .text_size(theme.type_scale.footnote)
                                .text_center()
                                .line_clamp(2)
                                .when(lit, |t| t.text_color(theme.text.on_accent))
                                .when(e.hidden && !lit, |t| t.text_color(theme.text.secondary)),
                        ),
                )
                .into_any_element(),
        };
        // The grid fills a selected cell whole; a lit tile covers that with the listing's own
        // background and draws Finder's selection instead: a plate behind the picture and the
        // name on an accent pill.
        let tile = div().id(row_id(&e.name)).w_full().h(px(size.tile.1)).when(lit, |d| d.bg(theme.surface.base)).child(
            v_flex()
                .size_full()
                .items_center()
                .gap(theme.spacing.xs)
                .p(theme.spacing.sm)
                .when(self.cut.contains(&id), |d| d.opacity(0.5))
                .child(art)
                .child(label),
        );
        self.file_events(tile, id, cx).into_any_element()
    }

    fn empty_state(&self) -> AnyElement {
        if !self.query.is_empty() {
            return status_page("No matches")
                .icon(icons::no_results())
                .description(format!("Nothing here is named like \u{201c}{}\u{201d}.", self.query))
                .compact()
                .into_any_element();
        }
        match self.load {
            Load::Listed | Load::Complete if self.location == Location::Recent => {
                status_page("Nothing recent").icon(icons::recent()).compact().into_any_element()
            }
            Load::Listed | Load::Complete if self.in_trash && self.entries.is_empty() => {
                status_page("Trash is empty").id("trash-empty").icon(Icon::Trash).compact().into_any_element()
            }
            Load::Listed | Load::Complete if !self.entries.is_empty() => status_page("Only hidden files")
                .icon(Icon::EyeOff)
                .description("Press Ctrl+H to show them.")
                .compact()
                .into_any_element(),
            Load::Listed | Load::Complete => {
                status_page("Folder is empty").icon(Icon::Folder).compact().into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn failure(&self, e: &ListError) -> AnyElement {
        let name = self.title();
        let (title, icon, description) = match e {
            ListError::NotFound => {
                ("Folder not found", Icon::TriangleAlert, format!("\u{201c}{name}\u{201d} was moved or deleted."))
            }
            ListError::PermissionDenied => (
                "Permission denied",
                Icon::Lock,
                format!("You don\u{2019}t have permission to open \u{201c}{name}\u{201d}."),
            ),
            ListError::NotADirectory => {
                ("Not a folder", Icon::TriangleAlert, format!("\u{201c}{name}\u{201d} is a file."))
            }
            ListError::Io(why) => ("Could not read the folder", Icon::TriangleAlert, why.to_string()),
        };
        status_page(title).id("listing-error").icon(icon).description(description).into_any_element()
    }
}

/// The test id of the row or tile showing the entry `name`.
pub fn row_id(name: &str) -> String {
    format!("file-{name}")
}

/// The key `by` rows from `cursor` in `rows`, wrapping round; the first or last with none.
pub(crate) fn step_in(rows: &[EntryId], cursor: Option<EntryId>, by: isize) -> Option<EntryId> {
    let n = rows.len() as isize;
    if n == 0 {
        return None;
    }
    let at = cursor.and_then(|c| rows.iter().position(|id| *id == c));
    let next = match at {
        Some(i) => (i as isize + by).rem_euclid(n),
        None if by < 0 => n - 1,
        None => 0,
    };
    rows.get(next as usize).copied()
}

/// How much of `name` a rename preselects: all of a folder's, a file's up to its extension
/// (".tar.gz" and the like counting as one), and all of a name that only starts with a dot.
pub fn stem_chars(name: &str, folder: bool) -> usize {
    let all = name.chars().count();
    if folder {
        return all;
    }
    let lower = name.to_lowercase();
    let double = [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst"].iter().find(|ext| lower.ends_with(*ext));
    let cut = match double {
        Some(ext) if name.len() > ext.len() && name.is_char_boundary(name.len() - ext.len()) => {
            Some(name.len() - ext.len())
        }
        _ => name.rfind('.').filter(|at| *at > 0),
    };
    cut.map_or(all, |at| name[..at].chars().count())
}

/// The size column: bytes for files, the count for a folder once something counted it.
/// A folder, or a link not yet stat'd that may turn out to be one.
fn listed_folder(e: &Entry) -> bool {
    e.is_dir() || matches!(e.kind, EntryKind::Symlink { .. }) && e.meta.is_none()
}

/// The search field's filter over names: case-insensitive unless the text has a capital.
struct Filter {
    needle: String,
    exact: bool,
}

impl Filter {
    fn new(query: &str) -> Self {
        let exact = query.chars().any(char::is_uppercase);
        Self { needle: if exact { query.to_owned() } else { query.to_lowercase() }, exact }
    }

    fn matches(&self, name: &str) -> bool {
        let n = self.needle.as_bytes();
        if n.is_empty() {
            return true;
        }
        if self.exact {
            return name.contains(&self.needle);
        }
        // ASCII names compare in place; lowercasing each one allocates, which a keystroke over
        // 100k names feels.
        if name.is_ascii() {
            return n.len() <= name.len() && name.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
        }
        name.to_lowercase().contains(&self.needle)
    }
}

fn size_text(e: &Entry) -> String {
    match &e.meta {
        Some(m) if m.kind.is_dir() => m.child_count.map_or_else(|| "--".to_owned(), |n| format::items(n as usize)),
        Some(m) => format::size(m.size),
        None if e.is_dir() => "--".to_owned(),
        None => String::new(),
    }
}

/// The entry of `child` when it sits directly inside `parent`: selected after going up.
fn child_of(child: &Location, parent: &Location) -> Option<EntryId> {
    let (c, p) = (child.dir()?, parent.dir()?);
    (c.parent()? == p).then(|| c.file_name().map(EntryId::of)).flatten()
}

/// Recent's entries, stat'd on a thread of their own: the list never touches the disk on the
/// main thread, and files that have gone since are left out.
fn recent_entries() -> flume::Receiver<Vec<(PathBuf, Entry)>> {
    use std::os::unix::fs::MetadataExt;
    let (tx, rx) = flume::bounded(1);
    let _ = std::thread::Builder::new().name("trove-recent".into()).spawn(move || {
        let found = crate::env::recent(200)
            .into_iter()
            .filter_map(|path| {
                let md = std::fs::metadata(&path).ok()?;
                let kind = if md.is_dir() { EntryKind::Dir } else { EntryKind::File };
                let mut entry = Entry::new(path.file_name()?, kind);
                entry.id = EntryId::of(&path);
                entry.meta = Some(Meta {
                    size: md.len(),
                    modified: md.modified().ok(),
                    created: md.created().ok(),
                    accessed: md.accessed().ok(),
                    mode: md.mode(),
                    uid: md.uid(),
                    gid: md.gid(),
                    kind,
                    child_count: None,
                });
                Some((path, entry))
            })
            .collect();
        let _ = tx.send(found);
    });
    rx
}

impl Render for Tab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.focus_target();
        let was = std::mem::replace(&mut self.focus_shown, target);
        if let Some(id) = self.focus_next.take() {
            window.focus(id);
        } else if was != target && was != crate::results::TABLE && target != crate::results::TABLE {
            // Another view, or a folder that failed, emptied or answered, put another list in
            // place of the one with the keys. The results come and go as the search field is
            // typed in, which keeps them; the window moves them there and back itself.
            window.focus(target);
        }
        if self.names_traced != self.generation && !self.visible.is_empty() {
            self.names_traced = self.generation;
            crate::trace::mark("names");
        }
        if self.view == View::Grid {
            self.light_tiles(cx);
        }
        let width = self.width;
        let theme = cx.theme();
        let me = cx.weak_entity();
        let body: AnyElement = match &self.load {
            _ if self.extras.results.shown() => crate::results::render(self, width, cx),
            Load::Failed(e) => self.failure(e),
            Load::Slow => h_flex()
                .w_full()
                .justify_center()
                .pt(theme.spacing.xl)
                .child(
                    text(format!("Waiting for {}\u{2026}", self.waiting_for))
                        .id("waiting")
                        .text_color(theme.text.secondary),
                )
                .into_any_element(),
            _ => match self.view {
                View::List => {
                    let sort = me.clone();
                    let cell = cx.processor(Self::table_cell);
                    let list =
                        table(TABLE_ID, &self.table, self.columns(), move |id, col, w, cx| cell((id, col), w, cx))
                            .estimated_row_height(px(self.row_size().height))
                            .on_sort(move |e: &SortChanged, _, cx| {
                                let _ = sort.update(cx, |t, cx| t.on_sort(e, cx));
                            })
                            .empty(self.empty_state());
                    self.file_list(list, &self.table, cx).into_any_element()
                }
                View::Grid => {
                    let (w, h) = self.tile_size().tile;
                    let list = grid(GRID_ID, &self.grid, (px(w), px(h)), cx.processor(Self::tile))
                        .gap(theme.spacing.xs)
                        .empty(self.empty_state());
                    self.file_list(list, &self.grid, cx).into_any_element()
                }
                View::Columns => crate::columns::render(self, cx),
            },
        };
        let (resize, menu, zoom) = (me.clone(), me.clone(), me);
        let banner = (self.in_trash && !self.entries.is_empty()).then(|| {
            banner("trash-banner", "Items stay in the Trash until you empty it.").action("Empty Trash").on_action(
                |_: &BannerActionClicked, window, cx| {
                    cx.dispatch_action(window.id(), &crate::actions::EmptyTrash);
                },
            )
        });
        div()
            .id(VIEW_ID)
            .key_context(if self.renaming.is_some() { RENAMING } else { FILES })
            .focusable()
            .size_full()
            .flex()
            .flex_col()
            .on_resize(move |r, _, cx| {
                let _ = resize.update(cx, |t, cx| t.set_width(r.width, cx));
            })
            .on_scroll_zoom(move |e: &ScrollZoomEvent, window, cx| {
                let Some(steps) = zoom.update(cx, |t, _| t.scroll_zoom(e.steps)) else { return };
                for _ in 0..steps.unsigned_abs() {
                    if steps > 0 {
                        cx.dispatch_action(window.id(), &crate::keys::ZoomIn);
                    } else {
                        cx.dispatch_action(window.id(), &crate::keys::ZoomOut);
                    }
                }
            })
            .context_menu(move |m, window, cx| {
                let Some(here) = menu.update(cx, |t, cx| t.here(window, cx)) else { return m };
                menus::folder(m, &here, crate::templates::cached().as_deref().map(Vec::as_slice))
            })
            .children(banner)
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Tab {
    /// A listing's list: multiple selection reported back, rows dragging their paths, and drops
    /// on folders and on empty space.
    pub(crate) fn file_list(
        &self,
        list: List<EntryId>,
        state: &ListState<EntryId>,
        cx: &mut Context<Self>,
    ) -> List<EntryId> {
        let me = cx.weak_entity();
        let (sel, act, drop) = (me.clone(), me.clone(), me);
        let single = self.pick.as_ref().is_some_and(|p| !p.multiple);
        let list = list
            .selection(if single { Selection::single() } else { Selection::multiple() })
            .on_selection_change(move |e: &SelectionChanged<EntryId>, _, cx| {
                let _ = sel.update(cx, |t, cx| t.on_selection_change(e, cx));
            })
            .on_activate(move |e: &ActivateEvent<EntryId>, window, cx| {
                let _ = act.update(cx, |t, cx| t.activate(e.key, window, cx));
            });
        if self.in_trash {
            return list;
        }
        let ids = self.drag_ids(state);
        let folders = self.folders.clone();
        carry_files_out(list, ids.clone())
            .drag_rows(DRAG_FILES, move |id| ids.get(id).cloned().unwrap_or_default())
            .accepts_drops([DRAG_FILES, DragItem::FILES])
            .drop_on(move |id| folders.contains(id))
            .on_drop(move |e: &RowDropped<EntryId>, window, cx| {
                let Some(tab) = drop.upgrade() else { return };
                if let Some(action) = tab.read(cx).drop_on_row(e) {
                    cx.dispatch_action(window.id(), &action);
                }
            })
    }

    /// The whole zoom steps `steps` more of Ctrl+scroll makes, keeping the remainder.
    fn scroll_zoom(&mut self, steps: f32) -> i32 {
        self.zoom_scroll += steps;
        let whole = self.zoom_scroll.trunc();
        self.zoom_scroll -= whole;
        whole as i32
    }

    /// The paths the rows a drag can start on carry: the ones drawn and the ones selected.
    pub(crate) fn drag_ids(&self, state: &ListState<EntryId>) -> Rc<HashMap<EntryId, SharedString>> {
        let rows = state.rendered().into_iter().chain(state.selected());
        Rc::new(rows.map(|id| (id, self.drag_id(&id))).collect())
    }

    fn table_cell(&mut self, (id, col): (EntryId, usize), window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.cell(id, col, window, cx)
    }
}

// File operations: what the menus and keys act on, inline rename, and selecting what a job
// made.
impl Tab {
    pub fn set_window(&mut self, window: WindowId) {
        self.window = Some(window);
    }

    pub fn window_id(&self) -> Option<WindowId> {
        self.window
    }

    fn origin(&self, cx: &mut Context<Self>) -> Option<Origin> {
        Some(Origin { window: self.window?, tab: Some(cx.weak_entity()) })
    }

    pub fn is_trash(&self) -> bool {
        self.in_trash
    }

    pub fn renaming(&self) -> Option<&Rename> {
        self.renaming.as_ref()
    }

    pub fn trash_info(&self, name: &str) -> Option<&TrashInfo> {
        self.trash_info.get(&EntryId::of(name))
    }

    pub(crate) fn is_cut(&self, id: &EntryId) -> bool {
        self.cut.contains(id)
    }

    pub fn cut_names(&self) -> Vec<String> {
        self.entries.iter().filter(|e| self.cut.contains(&e.id)).map(|e| e.name.to_string()).collect()
    }

    /// The selected paths, in the order the view shows them.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        if self.extras.results.shown() {
            return self.selected().iter().filter_map(|id| self.path_of(id)).collect();
        }
        let selected: HashSet<EntryId> = self.selected().into_iter().collect();
        self.visible.iter().filter(|id| selected.contains(id)).filter_map(|id| self.path_of(id)).collect()
    }

    /// The one selected folder, for Paste into folder.
    pub fn selected_folder(&self) -> Option<PathBuf> {
        let selected = self.selected();
        match selected.as_slice() {
            [one] if self.is_folder(one) => self.path_of(one),
            _ => None,
        }
    }

    /// A right click on `id`: an unselected row takes the selection, as in Finder and GNOME
    /// Files, and the menu acts on the selection.
    pub fn menu_target(&mut self, id: EntryId, cx: &mut Context<Self>) -> menus::Target {
        if !self.selected().contains(&id) {
            self.commit_rename(cx);
            self.select_only(id, cx);
        }
        let selected = self.selected();
        let folders = selected.iter().filter(|id| self.is_folder(id)).count();
        let archive = matches!(selected.as_slice(), [one] if self.entry(one).is_some_and(|e| e.file_kind == trove_fs::FileKind::Archive));
        let paths = self.selected_paths();
        if folders == 0 {
            ops::look_up_apps(cx, paths.iter().take(1).cloned().collect());
        }
        menus::Target { paths, folders, archive, in_trash: self.in_trash, recent: self.location == Location::Recent }
    }

    /// A right click on empty space clears the selection and shows the folder's menu.
    fn here(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> menus::Here {
        self.commit_rename(cx);
        crate::templates::refresh(cx);
        if !self.selected().is_empty() {
            self.list_state_mut().set_selected(Vec::new(), cx);
            self.cursor = None;
            cx.notify();
        }
        menus::Here {
            show_hidden: self.show_hidden,
            sort: self.sort(),
            view: self.view,
            in_trash: self.in_trash,
            recent: self.location == Location::Recent,
        }
    }

    fn bump(&mut self, id: EntryId) {
        *self.versions.entry(id).or_insert(0) += 1;
    }

    /// Re-reads which entries here are cut and redraws the ones that changed.
    fn sync_cut(&mut self, clip: &Entity<Clip>, cx: &mut Context<Self>) {
        let Some(dir) = self.location.dir().map(Path::to_path_buf) else { return };
        let now: HashSet<EntryId> = clip
            .read(cx)
            .cut()
            .iter()
            .filter(|p| p.parent() == Some(dir.as_path()))
            .filter_map(|p| p.file_name())
            .map(EntryId::of)
            .filter(|id| self.index.contains_key(id))
            .collect();
        if now == self.cut {
            return;
        }
        let changed: Vec<EntryId> = now.symmetric_difference(&self.cut).copied().collect();
        self.cut = now;
        for id in changed {
            self.bump(id);
        }
        self.sync_view(cx);
        cx.notify();
    }

    // Inline rename.

    /// F2: edits the cursor's (or the only selected) name in place.
    pub fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.location == Location::Recent || self.in_trash {
            return;
        }
        let Some(id) = self.target() else { return };
        self.begin_rename(id, cx);
        window.focus(RENAME_FIELD);
    }

    fn begin_rename(&mut self, id: EntryId, cx: &mut Context<Self>) {
        let Some(e) = self.entry(&id) else { return };
        let name = e.name.to_string();
        if let Some(old) = self.renaming.take() {
            self.bump(old.id);
        }
        self.renaming = Some(Rename { id, original: name.clone(), text: name.into(), error: None, job: None });
        self.bump(id);
        self.select_only(id, cx);
        self.sync_view(cx);
        cx.notify();
    }

    pub fn set_rename_text(&mut self, text: SharedString, cx: &mut Context<Self>) {
        if let Some(r) = self.renaming.as_mut() {
            r.text = text;
            if r.error.take().is_some() {
                let id = r.id;
                self.bump(id);
                self.sync_view(cx);
            }
        }
    }

    /// Enter, a click elsewhere: an unchanged name ends the edit with nothing done; a bad one
    /// keeps the field with the reason beside it.
    pub fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.renaming.clone() else { return };
        if r.job.is_some() {
            return;
        }
        let name = r.text.to_string();
        if name == r.original {
            self.end_rename(cx);
            return;
        }
        let why = if name.trim().is_empty() {
            Some("A name can\u{2019}t be empty")
        } else if name.contains('/') {
            Some("A name can\u{2019}t contain \u{201c}/\u{201d}")
        } else if name == "." || name == ".." {
            Some("That name is reserved")
        } else {
            None
        };
        if let Some(why) = why {
            self.set_rename_error(why.to_owned(), cx);
            return;
        }
        let (Some(path), Some(origin)) = (self.path_of(&r.id), self.origin(cx)) else { return };
        let job = ops::submit(cx, origin, trove_ops::Op::Rename { path, new_name: name });
        if let Some(r) = self.renaming.as_mut() {
            r.job = Some(job);
        }
        self.bump(r.id);
        self.sync_view(cx);
        cx.notify();
    }

    pub fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.as_ref().is_some_and(|r| r.job.is_none()) {
            self.end_rename(cx);
        }
    }

    fn end_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = self.renaming.take() {
            self.bump(r.id);
            self.sync_view(cx);
            self.focus_next = Some(self.focus_target());
            cx.notify();
        }
    }

    fn set_rename_error(&mut self, why: String, cx: &mut Context<Self>) {
        let Some(r) = self.renaming.as_mut() else { return };
        r.error = Some(why);
        r.job = None;
        let id = r.id;
        self.bump(id);
        self.sync_view(cx);
        self.focus_next = Some(RENAME_FIELD);
        cx.notify();
    }

    /// The rename job ended; the listing's change selects the new name.
    pub fn rename_ended(&mut self, job: trove_ops::JobId, cx: &mut Context<Self>) {
        if self.renaming.as_ref().is_some_and(|r| r.job == Some(job)) {
            self.end_rename(cx);
        }
    }

    pub fn rename_failed(&mut self, job: trove_ops::JobId, error: String, cx: &mut Context<Self>) {
        if self.renaming.as_ref().is_some_and(|r| r.job == Some(job)) {
            // "name: reason" from the job; the field already names the item.
            let why = error.split_once(": ").map_or(error.clone(), |(_, w)| w.to_owned());
            self.set_rename_error(why, cx);
        }
    }

    // Selecting what a job made.

    /// Selects `paths` (all in this tab's folder) once the listing shows them, and starts a
    /// rename on a lone new item when `rename` is set.
    pub fn select_created(&mut self, paths: Vec<PathBuf>, rename: bool, cx: &mut Context<Self>) {
        let ids: Vec<EntryId> = paths.iter().filter_map(|p| p.file_name()).map(EntryId::of).collect();
        if ids.is_empty() {
            return;
        }
        self.pending = Some(Pending { ids, rename, until: cx.now() + PENDING_FOR });
        self.apply_pending(cx);
    }

    /// Selects the named entries that are shown, for tests and tooling.
    pub fn select_names(&mut self, names: &[&str], cx: &mut Context<Self>) {
        let shown: HashSet<EntryId> = self.visible.iter().copied().collect();
        let ids: Vec<EntryId> = names.iter().map(EntryId::of).filter(|id| shown.contains(id)).collect();
        let Some(&first) = ids.first() else { return };
        self.cursor = Some(first);
        self.list_state_mut().set_selected(ids, cx);
        cx.notify();
    }

    fn apply_pending(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &self.pending else { return };
        if cx.now() > p.until {
            self.pending = None;
            return;
        }
        let shown: HashSet<EntryId> = self.visible.iter().copied().collect();
        let landed: Vec<EntryId> = p.ids.iter().copied().filter(|id| shown.contains(id)).collect();
        if landed.is_empty() {
            return;
        }
        let all = landed.len() == p.ids.len();
        let rename = p.rename && all && landed.len() == 1;
        let first = landed[0];
        self.cursor = Some(first);
        let state = self.list_state_mut();
        state.set_selected(landed, cx);
        state.scroll_to(first, Align::Nearest, px(0.), cx);
        if all {
            self.pending = None;
        }
        if rename {
            self.begin_rename(first, cx);
            self.focus_next = Some(RENAME_FIELD);
        }
        cx.notify();
    }

    /// Reads the `.trashinfo` of entries that have none yet.
    fn ask_trash_info(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.location.dir().map(Path::to_path_buf) else { return };
        let wanted: Vec<(EntryId, OsString)> = self
            .entries
            .iter()
            .filter(|e| !self.trash_asked.contains(&e.id))
            .map(|e| (e.id, e.raw_name().to_os_string()))
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.trash_asked.extend(wanted.iter().map(|(id, _)| *id));
        let generation = self.generation;
        let names: Vec<OsString> = wanted.into_iter().map(|(_, n)| n).collect();
        ops::background(
            cx.weak_entity(),
            move || trash::read_infos(&dir, &names),
            move |t: &mut Tab, infos, cx| {
                if t.generation != generation {
                    return;
                }
                for (name, info) in infos {
                    let id = EntryId::of(&name);
                    t.trash_info.insert(id, info);
                    t.bump(id);
                }
                t.sync_view(cx);
                cx.notify();
            },
        );
    }
}

/// How long after a single click opened something its double click's second press is ignored,
/// so a double click from habit does not open what lands under the pointer next.
const SECOND_PRESS: f64 = 0.5;

impl Tab {
    /// A plain click on a row, tile or the columns' own column: opens it under the single
    /// click setting. While picking, a click only selects, as a file chooser's does.
    fn tapped(&mut self, id: EntryId, e: &TapEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The columns view lists a folder as soon as it is selected; only files open there.
        if self.view == View::Columns && !self.extras.results.shown() && self.is_folder(&id) {
            return;
        }
        if self.single_click(e, window, cx) {
            self.open(id, window, cx);
        }
    }

    /// Whether `e` opens what it landed on under the single click setting; notes when it did.
    pub(crate) fn single_click(&mut self, e: &TapEvent, window: &Window, cx: &App) -> bool {
        if e.count != 1 || e.modifiers.any() || self.pick.is_some() || !crate::settings::get(cx).single_click {
            return false;
        }
        self.tapped_at = window.now();
        true
    }

    /// Whether an activation is the second press of a double click whose first press already
    /// opened something under the single click setting.
    pub(crate) fn second_press(&self, window: &Window) -> bool {
        window.now() - self.tapped_at < SECOND_PRESS
    }

    /// A double click on a row.
    pub(crate) fn activate(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.second_press(window) {
            self.open(id, window, cx);
        }
    }

    pub fn open(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path_of(&id) else { return };
        if self.is_folder(&id) {
            self.navigate(Location::Dir(path), window, cx);
        } else if self.pick.is_some() {
            cx.dispatch_action(window.id(), &crate::picker::PickAccept);
        } else if let Err(e) = nd::system::open_path(&path) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let alert = Alert::new(format!("Could not open \u{201c}{name}\u{201d}")).message(e.to_string());
            window.alert(cx, alert, |_, _| {});
        }
    }
}

// Hooks for the search results, the columns view and the thumbnails.
impl Tab {
    pub fn cursor(&self) -> Option<EntryId> {
        if self.extras.results.shown() { self.extras.results.cursor() } else { self.cursor }
    }

    pub fn showing_results(&self) -> bool {
        self.extras.results.shown()
    }

    /// Where the keys go: the list shown, which moves its own selection, or the listing's box
    /// while a slow, failed or empty folder shows none. An empty list draws its empty state in
    /// its own place, so a focus left on it would go with it.
    pub fn focus_target(&self) -> &'static str {
        if self.extras.results.shown() {
            return crate::results::TABLE;
        }
        match (&self.load, self.view) {
            (Load::Slow | Load::Failed(_), _) => VIEW_ID,
            (_, View::List | View::Grid) if self.list_state().is_empty() => VIEW_ID,
            (_, View::List) => TABLE_ID,
            (_, View::Grid) => GRID_ID,
            (_, View::Columns) => crate::columns::CURRENT,
        }
    }

    /// Puts back a selection the search text hid.
    pub(crate) fn restore_selection(&mut self, keys: Vec<EntryId>, cursor: Option<EntryId>, cx: &mut Context<Self>) {
        let keys: Vec<EntryId> = keys.into_iter().filter(|k| self.visible.contains(k)).collect();
        self.cursor = cursor.filter(|c| keys.contains(c)).or_else(|| keys.first().copied());
        let at = self.cursor;
        let state = self.list_state_mut();
        state.set_selected(keys, cx);
        if let Some(id) = at {
            state.scroll_to(id, Align::Nearest, px(0.), cx);
        }
        cx.notify();
    }

    /// Shows `to` without a history step, as the columns view moves between its columns,
    /// selecting `select` once it lists.
    pub(crate) fn retarget(&mut self, to: Location, select: Option<EntryId>, cx: &mut Context<Self>) {
        if to == self.location {
            return;
        }
        self.location = to.clone();
        self.load(to, cx);
        self.reveal = select;
        self.apply_reveal(cx);
    }

    /// Whether `path` is in the listing, and if so whether it is a folder.
    pub(crate) fn listed(&self, path: &Path) -> Option<bool> {
        let id = self.id_of_path(path)?;
        Some(self.listed_folder(&id))
    }

    pub(crate) fn id_of_path(&self, path: &Path) -> Option<EntryId> {
        let dir = self.location.dir()?;
        if path.parent()? != dir {
            return self.paths.iter().find(|(_, p)| p.as_path() == path).map(|(id, _)| *id);
        }
        let id = EntryId::of(path.file_name()?);
        self.index.contains_key(&id).then_some(id)
    }

    /// Redraws these rows alone.
    pub(crate) fn bump_ids(&mut self, ids: &[EntryId], cx: &mut Context<Self>) {
        for id in ids {
            self.bump(*id);
        }
        self.sync_view(cx);
        cx.notify();
    }
}

/// Rows dragged out of the window carry their files to the app they land on, where the platform
/// can start that drag.
pub(crate) fn carry_files_out(list: List<EntryId>, ids: Rc<HashMap<EntryId, SharedString>>) -> List<EntryId> {
    if !nd::platform::has(nd::platform::Feature::DragFilesOut) {
        return list;
    }
    list.drag_out_files(move |id| ids.get(id).filter(|p| !p.is_empty()).map(|p| PathBuf::from(p.to_string())))
}
