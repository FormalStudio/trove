//! One tab: its location and history, the streaming listing, sort, filter, selection, scroll
//! and view mode, and the content it draws. A notify here re-renders this tab's content and
//! whatever observes it (the status bar, the preview), never the sidebar or the other tabs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nd::prelude::*;
use trove_fs::{
    Change, Entry, EntryId, EntryKind, ListError, ListEvent, ListOptions, Listing, Meta, SortKey, SortOrder, Usage,
};

use crate::format::{self, Today};
use crate::icons;
use crate::location::Location;
use crate::pump::pump;

/// List rows, Finder's density.
pub const ROW_HEIGHT: f32 = 28.;
const TILE: (f32, f32) = (112., 104.);
const TYPE_SELECT_WINDOW: f64 = 0.8;
const SIZE_W: f32 = 96.;
const KIND_W: f32 = 132.;
const DATE_W: f32 = 168.;
const NAME_MIN: f32 = 220.;
/// What a still-unmeasured content area is taken to be on the first frame.
const DEFAULT_WIDTH: f32 = 760.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    List,
    Grid,
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
    index: HashMap<EntryId, usize>,
    versions: HashMap<EntryId, u64>,
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
    cursor: Option<EntryId>,
    anchor: Option<EntryId>,
    /// Selected and scrolled to as soon as the listing has it: the folder just left on the way
    /// up, or the file named on the command line.
    reveal: Option<EntryId>,
    width: f32,
    /// A row took the press, so the tap that ends it is not a click on empty space.
    row_hit: bool,
    /// The location came off the command line and may name a file: on `NotADirectory` the
    /// tab opens its folder with it selected instead of failing.
    maybe_file: bool,
    home: PathBuf,
    trash: PathBuf,
}

impl Tab {
    pub fn new(location: Location, show_hidden: bool, view: View, cx: &mut Context<Self>) -> Self {
        let table = ListState::new(cx).estimated_row_height(px(ROW_HEIGHT));
        let grid = ListState::new(cx).estimated_row_height(px(TILE.1));
        let search = cx.new(|_| TextFieldState::default().placeholder("Search"));
        cx.observe(&search, |this: &mut Tab, search, cx| {
            if search.read(cx).last_event() == Some(TextEvent::Edited) {
                this.query = search.read(cx).text().clone();
                this.refresh(cx);
            }
        })
        .detach();
        let mut tab = Self {
            location: location.clone(),
            back: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            index: HashMap::new(),
            versions: HashMap::new(),
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
            anchor: None,
            reveal: None,
            width: DEFAULT_WIDTH,
            row_hit: false,
            maybe_file: false,
            home: crate::env::get().home.clone(),
            trash: crate::env::get().trash.clone(),
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

    fn list_state(&self) -> &ListState<EntryId> {
        match self.view {
            View::List => &self.table,
            View::Grid => &self.grid,
        }
    }

    fn list_state_mut(&mut self) -> &mut ListState<EntryId> {
        match self.view {
            View::List => &mut self.table,
            View::Grid => &mut self.grid,
        }
    }

    pub fn selected(&self) -> Vec<EntryId> {
        self.list_state().selected()
    }

    pub fn selected_names(&self) -> Vec<String> {
        self.selected().iter().filter_map(|id| self.entry(id)).map(|e| e.name.to_string()).collect()
    }

    pub fn path_of(&self, id: &EntryId) -> Option<PathBuf> {
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
        self.reveal = file.file_name().map(|n| EntryId::of(&n.to_string_lossy()));
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
        self.anchor = None;
        self.typed.clear();
        self.load = Load::Loading;
        self.free = None;
        self.today = Today::now();
        for state in [&mut self.table, &mut self.grid] {
            state.set_items(Vec::new(), cx);
            state.scroll_to_top(cx);
        }
        let generation = self.generation;
        let me = cx.weak_entity();
        match &location {
            Location::Dir(dir) => {
                let listing = trove_fs::list(dir.clone(), ListOptions { show_hidden: self.show_hidden });
                self.events = Some(listing.events.clone());
                pump(listing.events.clone(), me.clone(), move |tab: &mut Tab, batch, cx| {
                    tab.generation == generation && tab.apply(batch, cx)
                });
                self.listing = Some(listing);
                let usage = trove_fs::usage(dir.clone(), Duration::from_secs(2));
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
        let query = self.query.trim().to_owned();
        let smart_case = query.chars().any(char::is_uppercase);
        let needle = if smart_case { query.clone() } else { query.to_lowercase() };
        let show_hidden = self.show_hidden;
        self.visible = self
            .entries
            .iter()
            .filter(|e| show_hidden || !e.hidden)
            .filter(|e| {
                needle.is_empty()
                    || if smart_case { e.name.contains(&needle) } else { e.name.to_lowercase().contains(&needle) }
            })
            .map(|e| e.id)
            .collect();
        self.sync_view(cx);
        self.apply_reveal(cx);
        cx.notify();
    }

    fn sync_view(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<(EntryId, u64)> =
            self.visible.iter().map(|id| (*id, self.versions.get(id).copied().unwrap_or(0))).collect();
        self.list_state_mut().set_items_versioned(rows, cx);
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
        let vols = trove_fs::volumes();
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
        self.resort();
        self.refresh(cx);
    }

    fn on_sort(&mut self, e: &SortChanged, cx: &mut Context<Self>) {
        self.row_hit = true;
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
        self.refresh(cx);
        true
    }

    // Selection.

    fn select_only(&mut self, id: EntryId, cx: &mut Context<Self>) {
        self.cursor = Some(id);
        self.anchor = Some(id);
        let state = self.list_state_mut();
        state.set_selected(vec![id], cx);
        state.scroll_to(id, Align::Nearest, px(0.), cx);
        cx.notify();
    }

    fn on_select(&mut self, e: &SelectEvent<EntryId>, cx: &mut Context<Self>) {
        self.row_hit = true;
        self.cursor = Some(e.key);
        if e.keys.len() <= 1 {
            self.anchor = Some(e.key);
        }
        cx.notify();
    }

    fn on_tap(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.row_hit) {
            return;
        }
        if !self.selected().is_empty() {
            self.list_state_mut().set_selected(Vec::new(), cx);
            self.cursor = None;
            self.anchor = None;
            cx.notify();
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let all = self.visible.clone();
        self.list_state_mut().set_selected(all, cx);
        cx.notify();
    }

    pub fn deselect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clear_search(window, cx) {
            return;
        }
        self.list_state_mut().set_selected(Vec::new(), cx);
        self.cursor = None;
        cx.notify();
    }

    /// Moves the keyboard cursor by `step` rows (a grid's lane count for up and down);
    /// `extend` grows the selection from its anchor instead of replacing it.
    pub fn move_cursor(&mut self, step: isize, extend: bool, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() as isize - 1;
        let at = self.cursor.and_then(|c| self.visible.iter().position(|id| *id == c));
        let next = match at {
            Some(i) => (i as isize + step).clamp(0, last),
            None if step < 0 => last,
            None => 0,
        } as usize;
        let id = self.visible[next];
        if !extend {
            self.select_only(id, cx);
            return;
        }
        let anchor = self.anchor.and_then(|a| self.visible.iter().position(|v| *v == a)).unwrap_or(next);
        let (lo, hi) = (anchor.min(next), anchor.max(next));
        let range = self.visible[lo..=hi].to_vec();
        self.cursor = Some(id);
        let state = self.list_state_mut();
        state.set_selected(range, cx);
        state.scroll_to(id, Align::Nearest, px(0.), cx);
        cx.notify();
    }

    pub fn move_to_end(&mut self, end: bool, cx: &mut Context<Self>) {
        let id = if end { self.visible.last() } else { self.visible.first() };
        if let Some(id) = id.copied() {
            self.select_only(id, cx);
        }
    }

    /// Grid lanes: how many tiles one row of the grid holds.
    pub fn lanes(&self) -> usize {
        match self.view {
            View::List => 1,
            View::Grid => self.grid.handle().0.borrow().lanes().max(1),
        }
    }

    /// Type-select: characters typed within a short pause build a prefix, and the first name
    /// in natural order starting with it is selected.
    pub fn type_select(&mut self, c: char, now: f64, cx: &mut Context<Self>) {
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
        let selected = self.selected();
        self.cursor.filter(|c| selected.contains(c)).or_else(|| selected.first().copied())
    }

    /// Whether `id` names a folder this tab can go into.
    pub fn is_folder(&self, id: &EntryId) -> bool {
        self.entry(id).is_some_and(|e| e.is_dir() || matches!(e.kind, EntryKind::Symlink { .. }) && e.meta.is_none())
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if (self.width - width).abs() < 0.5 {
            return;
        }
        self.width = width;
        self.table.invalidate_all(cx);
        cx.notify();
    }

    // Drawing.

    fn columns(&self) -> Vec<Column> {
        let sort = self.sort();
        let dir = if sort.ascending { SortDirection::Ascending } else { SortDirection::Descending };
        let col = |id: &'static str, title: &'static str, key: SortKey, width: f32| {
            let c = column(id, title).width(width).min_width(60.);
            if sort.key == key { c.sorted(dir) } else { c.sortable() }
        };
        vec![
            col("name", "Name", SortKey::Name, self.name_width()),
            col("size", "Size", SortKey::Size, SIZE_W),
            col("kind", "Kind", SortKey::Kind, KIND_W),
            col("modified", "Modified", SortKey::Modified, DATE_W),
        ]
    }

    fn name_width(&self) -> f32 {
        (self.width - SIZE_W - KIND_W - DATE_W).max(NAME_MIN)
    }

    fn cell(&mut self, id: EntryId, col: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(e) = self.entry(&id) else { return div().into_any_element() };
        let quiet = |s: String| {
            text(s)
                .text_size(theme.type_scale.footnote)
                .text_color(theme.text.secondary)
                .tabular_nums()
                .truncate()
                .into_any_element()
        };
        let content = match col {
            0 => {
                let symlinked = matches!(e.kind, EntryKind::Symlink { .. }) && e.is_dir();
                let glyph = if symlinked { icons::symlinked_folder() } else { icons::for_kind(e.file_kind) };
                h_flex()
                    .gap(theme.spacing.sm)
                    .min_w_0()
                    .child(icon(glyph, 16.).text_color(icons::tint(e.file_kind, theme)))
                    .child(div().flex_1().min_w_0().child(
                        text(e.name.to_string()).truncate().when(e.hidden, |t| t.text_color(theme.text.secondary)),
                    ))
                    .into_any_element()
            }
            1 if e.is_dir() && e.meta.as_ref().and_then(|m| m.child_count).is_none() => {
                text("--").text_size(theme.type_scale.footnote).text_color(theme.text.secondary).into_any_element()
            }
            1 => quiet(size_text(e)),
            2 => quiet(e.file_kind.label().to_owned()),
            _ => quiet(e.meta.as_ref().and_then(|m| m.modified).map(|t| self.today.modified(t)).unwrap_or_default()),
        };
        h_flex()
            .h(px(ROW_HEIGHT))
            .px(theme.spacing.sm)
            .items_center()
            .when(col == 1, |d| d.justify_end())
            .child(content)
            .into_any_element()
    }

    fn tile(&mut self, id: EntryId, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(e) = self.entry(&id) else { return div().into_any_element() };
        v_flex()
            .size_full()
            .items_center()
            .gap(theme.spacing.xs)
            .p(theme.spacing.sm)
            .child(tile_art(e, theme))
            .child(
                div().w_full().flex().justify_center().child(
                    text(e.name.to_string())
                        .text_size(theme.type_scale.footnote)
                        .text_center()
                        .line_clamp(2)
                        .when(e.hidden, |t| t.text_color(theme.text.secondary)),
                ),
            )
            .into_any_element()
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
        let path = self.location.dir().map(|p| p.display().to_string()).unwrap_or_default();
        let (title, icon) = match e {
            ListError::NotFound => ("Folder not found", Icon::TriangleAlert),
            ListError::PermissionDenied => ("Permission denied", Icon::Lock),
            ListError::NotADirectory => ("Not a folder", Icon::TriangleAlert),
            ListError::Io(_) => ("Could not read the folder", Icon::TriangleAlert),
        };
        let description = match e {
            ListError::Io(why) => format!("{path}\n{why}"),
            _ => path,
        };
        status_page(title).id("listing-error").icon(icon).description(description).into_any_element()
    }
}

/// The size column: bytes for files, the count for a folder once something counted it.
fn size_text(e: &Entry) -> String {
    match &e.meta {
        Some(m) if m.kind.is_dir() => m.child_count.map_or_else(|| "--".to_owned(), |n| format::items(n as usize)),
        Some(m) => format::size(m.size),
        None if e.is_dir() => "--".to_owned(),
        None => String::new(),
    }
}

/// The picture of a grid tile: the kind's icon for now; thumbnails take this slot.
fn tile_art(e: &Entry, theme: &Theme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .child(icon(icons::for_kind(e.file_kind), 48.).text_color(icons::tint(e.file_kind, theme)))
        .into_any_element()
}

/// The entry of `child` when it sits directly inside `parent`: selected after going up.
fn child_of(child: &Location, parent: &Location) -> Option<EntryId> {
    let (c, p) = (child.dir()?, parent.dir()?);
    (c.parent()? == p).then(|| c.file_name().map(|n| EntryId::of(&n.to_string_lossy()))).flatten()
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
                let name = path.file_name()?.to_string_lossy().into_owned();
                let kind = if md.is_dir() { EntryKind::Dir } else { EntryKind::File };
                let mut entry = Entry::new(&name, kind);
                entry.id = EntryId::of(&path.to_string_lossy());
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let me = cx.weak_entity();
        let body: AnyElement = match &self.load {
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
                    let (sel, act, sort) = (me.clone(), me.clone(), me.clone());
                    let cell = cx.processor(Self::table_cell);
                    table("files-table", &self.table, self.columns(), move |id, col, w, cx| cell((id, col), w, cx))
                        .selection(Selection::multiple())
                        .estimated_row_height(px(ROW_HEIGHT))
                        .on_select(move |e: &SelectEvent<EntryId>, _, cx| {
                            let _ = sel.update(cx, |t, cx| t.on_select(e, cx));
                        })
                        .on_activate(move |e: &ActivateEvent<EntryId>, window, cx| {
                            activate(&act, e.key, window, cx);
                        })
                        .on_sort(move |e: &SortChanged, _, cx| {
                            let _ = sort.update(cx, |t, cx| t.on_sort(e, cx));
                        })
                        .empty(self.empty_state())
                        .into_any_element()
                }
                View::Grid => {
                    let (sel, act) = (me.clone(), me.clone());
                    grid("files-grid", &self.grid, (px(TILE.0), px(TILE.1)), cx.processor(Self::tile))
                        .gap(theme.spacing.xs)
                        .selection(Selection::multiple())
                        .on_select(move |e: &SelectEvent<EntryId>, _, cx| {
                            let _ = sel.update(cx, |t, cx| t.on_select(e, cx));
                        })
                        .on_activate(move |e: &ActivateEvent<EntryId>, window, cx| {
                            activate(&act, e.key, window, cx);
                        })
                        .empty(self.empty_state())
                        .into_any_element()
                }
            },
        };
        let (resize, tap) = (me.clone(), me);
        div()
            .id("files")
            .key_context(crate::actions::FILES)
            .focusable()
            .size_full()
            .flex()
            .flex_col()
            .on_resize(move |r, _, cx| {
                let _ = resize.update(cx, |t, cx| t.set_width(r.width, cx));
            })
            .on_tap(move |_: &TapEvent, _, cx| {
                let _ = tap.update(cx, |t, cx| t.on_tap(cx));
            })
            .child(body)
    }
}

impl Tab {
    fn table_cell(&mut self, (id, col): (EntryId, usize), window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.cell(id, col, window, cx)
    }
}

/// A double click or Enter on a row: folders open in this tab, files in their app.
fn activate(tab: &WeakEntity<Tab>, id: EntryId, window: &mut Window, cx: &mut App) {
    let _ = tab.update(cx, |t, cx| {
        t.row_hit = true;
        t.open(id, window, cx);
    });
}

impl Tab {
    pub fn open(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path_of(&id) else { return };
        if self.is_folder(&id) {
            self.navigate(Location::Dir(path), window, cx);
        } else if let Err(e) = nd::system::open_path(&path) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let alert = Alert::new(format!("Could not open \u{201c}{name}\u{201d}")).message(e.to_string());
            window.alert(cx, alert, |_, _| {});
        }
    }
}
