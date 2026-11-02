//! Search results in a tab: from the second character of the toolbar's search, a recursive
//! search of the folder (or Home) replaces the listing. Every keystroke drops the run before
//! it; rows are keyed by path, so a refined set moves in place instead of flickering. Escape
//! puts the folder back with its selection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nd::prelude::*;
use nd::virtual_list::{RowDropped, SelectionChanged};
use trove_fs::{EntryId, FileKind};
use trove_search::{Hit, IndexState, Query, SearchEvent, SearchRun};

use crate::format;
use crate::icons;
use crate::location::Location;
use crate::pump::pump;
use crate::tab::Tab;

/// The fewest characters a recursive search starts at; one character only filters the folder.
pub const MIN_CHARS: usize = 2;
/// The results' table, which takes the keys while it shows.
pub const TABLE: &str = "results-table";
const LIMIT: usize = 500;
/// Rows whose modification time is looked up for each answer: the first screenfuls.
const STAT_ROWS: usize = 400;
const PLACE_W: f32 = 240.;
const KIND_W: f32 = 110.;
const DATE_W: f32 = 130.;
const NAME_MIN: f32 = 180.;
const PLACE_MIN: f32 = 110.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Folder,
    Home,
}

#[derive(Clone, Debug)]
struct Row {
    path: PathBuf,
    name: String,
    place: String,
    is_dir: bool,
    kind: FileKind,
    matches: Vec<(u32, u32)>,
    modified: Option<SystemTime>,
    version: u64,
}

type Stats = Vec<(EntryId, Option<SystemTime>)>;
/// A row known before the first answer: path, whether it is a folder, the matched ranges.
type Seed = (PathBuf, bool, Vec<(u32, u32)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done {
    pub scanned: u64,
    pub elapsed_ms: u64,
    pub truncated: bool,
}

pub struct Results {
    pub list: ListState<EntryId>,
    rows: Vec<EntryId>,
    by_id: HashMap<EntryId, Row>,
    query: String,
    scope: Scope,
    shown: bool,
    run: Option<SearchRun>,
    events: Option<flume::Receiver<SearchEvent>>,
    stats: Option<flume::Receiver<Stats>>,
    generation: u64,
    done: Option<Done>,
    /// The folder's selection and cursor from before the search, put back when it ends.
    saved: Option<(Vec<EntryId>, Option<EntryId>)>,
    cursor: Option<EntryId>,
    hint: bool,
    typed: String,
    typed_at: f64,
}

impl Results {
    pub fn new<T: 'static>(cx: &mut Context<T>) -> Self {
        Self {
            list: ListState::new(cx).estimated_row_height(px(crate::tab::ROW_HEIGHT)),
            rows: Vec::new(),
            by_id: HashMap::new(),
            query: String::new(),
            scope: Scope::Folder,
            shown: false,
            run: None,
            events: None,
            stats: None,
            generation: 0,
            done: None,
            saved: None,
            cursor: None,
            hint: false,
            typed: String::new(),
            typed_at: f64::NEG_INFINITY,
        }
    }

    /// Whether the results replace the listing.
    pub fn cursor(&self) -> Option<EntryId> {
        self.cursor
    }

    /// The rows' names, in order.
    pub fn names(&self) -> Vec<String> {
        self.rows.iter().filter_map(|id| self.by_id.get(id)).map(|r| r.name.clone()).collect()
    }

    /// The Folder column of the row named `name`.
    pub fn place_of(&self, name: &str) -> Option<String> {
        self.by_id.values().find(|r| r.name == name).map(|r| r.place.clone())
    }

    /// The highlighted byte ranges of the row named `name`.
    pub fn matches_of(&self, name: &str) -> Option<Vec<(u32, u32)>> {
        self.by_id.values().find(|r| r.name == name).map(|r| r.matches.clone())
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn done(&self) -> Option<Done> {
        self.done
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// "3 results", or "1,000+ results" once the search stopped at its limit.
    pub fn count_label(&self) -> String {
        let n = self.rows.len();
        if self.done.is_some_and(|d| d.truncated) || n >= LIMIT {
            format!("{}+ results", format::group(n))
        } else if n == 1 {
            "1 result".to_owned()
        } else {
            format!("{} results", format::group(n))
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Result paths in rank order, for tests.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.rows.iter().filter_map(|id| self.by_id.get(id)).map(|r| r.path.clone()).collect()
    }

    pub fn selected(&self) -> Vec<EntryId> {
        self.list.selected()
    }

    /// The row Enter opens: the cursor's, else the first selected, else the best.
    pub fn target(&self) -> Option<EntryId> {
        let selected = self.selected();
        self.cursor
            .filter(|c| selected.contains(c))
            .or_else(|| selected.first().copied())
            .or_else(|| self.rows.first().copied())
    }

    pub fn path_of(&self, id: &EntryId) -> Option<PathBuf> {
        self.shown.then(|| self.by_id.get(id).map(|r| r.path.clone())).flatten()
    }

    pub fn is_dir(&self, id: &EntryId) -> bool {
        self.by_id.get(id).is_some_and(|r| r.is_dir)
    }

    fn select_only(&mut self, id: EntryId, cx: &mut App) {
        self.cursor = Some(id);
        self.list.set_selected(vec![id], cx);
        self.list.scroll_to(id, Align::Nearest, px(0.), cx);
    }

    /// Quick Look's arrows over the results.
    pub fn select_step(&mut self, by: isize, cx: &mut App) {
        if let Some(id) = crate::tab::step_in(&self.rows, self.cursor, by) {
            self.select_only(id, cx);
        }
    }

    /// Down in the search field: the best result, selected.
    pub fn select_first(&mut self, cx: &mut App) {
        if let Some(id) = self.rows.first().copied() {
            self.select_only(id, cx);
        }
    }

    pub fn type_select(&mut self, c: char, now: f64, cx: &mut App) {
        if now - self.typed_at > 0.8 {
            self.typed.clear();
        }
        self.typed_at = now;
        self.typed.push(c);
        let prefix = self.typed.to_lowercase();
        let found =
            self.rows.iter().find(|id| self.by_id.get(id).is_some_and(|r| r.name.to_lowercase().starts_with(&prefix)));
        if let Some(id) = found.copied() {
            self.select_only(id, cx);
        }
    }

    fn on_selection_change(&mut self, e: &SelectionChanged<EntryId>) {
        self.cursor = e.cursor.or_else(|| e.keys.last().copied());
    }

    fn stop(&mut self) {
        self.run = None;
        self.events = None;
        self.stats = None;
        self.generation += 1;
    }

    fn sync(&mut self, cx: &mut App) {
        let rows: Vec<(EntryId, u64)> =
            self.rows.iter().map(|id| (*id, self.by_id.get(id).map_or(0, |r| r.version))).collect();
        self.list.set_items_versioned(rows, cx);
        if self.cursor.is_some_and(|c| !self.by_id.contains_key(&c)) {
            self.cursor = None;
        }
    }

    /// Replaces the rows with `hits`, best first, keeping what is known of rows that stay.
    fn set_hits(&mut self, hits: Vec<Hit>, home: &Path) -> Vec<(EntryId, PathBuf)> {
        let mut by_id = HashMap::with_capacity(hits.len());
        let mut rows = Vec::with_capacity(hits.len());
        let mut unknown = Vec::new();
        for hit in hits {
            let id = id_of(&hit.path);
            if by_id.contains_key(&id) {
                continue;
            }
            let row = match self.by_id.remove(&id) {
                Some(mut old) => {
                    if old.matches != hit.name_matches || old.is_dir != hit.is_dir {
                        old.matches = hit.name_matches;
                        old.is_dir = hit.is_dir;
                        old.version += 1;
                    }
                    old
                }
                None => {
                    let row = row_of(hit.path, hit.is_dir, hit.name_matches, home);
                    if rows.len() < STAT_ROWS {
                        unknown.push((id, row.path.clone()));
                    }
                    row
                }
            };
            rows.push(id);
            by_id.insert(id, row);
        }
        self.rows = rows;
        self.by_id = by_id;
        unknown
    }

    /// Takes the run's answers in; starts looking up the dates of rows not seen before.
    fn apply(&mut self, events: Vec<SearchEvent>, cx: &mut App) -> Option<flume::Receiver<Stats>> {
        let home = crate::env::get().home.clone();
        let mut unknown = Vec::new();
        for event in events {
            match event {
                SearchEvent::Hits(hits) => unknown = self.set_hits(hits, &home),
                SearchEvent::Done { scanned, elapsed_ms, truncated } => {
                    self.done = Some(Done { scanned, elapsed_ms, truncated });
                }
            }
        }
        self.sync(cx);
        if unknown.is_empty() {
            return None;
        }
        let rx = stat_rows(unknown);
        self.stats = Some(rx.clone());
        Some(rx)
    }

    fn apply_stats(&mut self, stats: Vec<(EntryId, Option<SystemTime>)>, cx: &mut App) {
        let mut changed = false;
        for (id, modified) in stats {
            if let Some(row) = self.by_id.get_mut(&id) {
                row.modified = modified;
                row.version += 1;
                changed = true;
            }
        }
        if changed {
            self.sync(cx);
        }
    }

    /// Applies what the current run and its stat lookups sent within `timeout`: the host's
    /// pump, for tests.
    pub fn drain(&mut self, timeout: Duration, cx: &mut App) {
        // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
        let deadline = std::time::Instant::now() + timeout;
        while let Some(rx) = self.events.clone() {
            if self.done.is_some() {
                break;
            }
            // nd-allow(clock-in-ui): as above
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(first) = rx.recv_timeout(left) else { break };
            let mut batch = vec![first];
            batch.extend(rx.try_iter());
            if let Some(stats) = self.apply(batch, cx) {
                let all: Vec<_> = stats.iter().flatten().collect();
                self.apply_stats(all, cx);
            }
        }
    }
}

fn id_of(path: &Path) -> EntryId {
    EntryId::of(path)
}

fn row_of(path: PathBuf, is_dir: bool, matches: Vec<(u32, u32)>, home: &Path) -> Row {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let kind = if is_dir { FileKind::Folder } else { FileKind::from_name(&name) };
    let place = crate::preview::place_of(&path, home);
    Row { path, name, place, is_dir, kind, matches, modified: None, version: 0 }
}

/// Modification times, on a thread of their own and in small chunks, so the first rows fill
/// in first and a dropped receiver stops the rest.
fn stat_rows(rows: Vec<(EntryId, PathBuf)>) -> flume::Receiver<Stats> {
    let (tx, rx) = flume::unbounded();
    let _ = std::thread::Builder::new().name("trove-result-stat".into()).spawn(move || {
        for chunk in rows.chunks(64) {
            let stats = chunk
                .iter()
                .map(|(id, p)| (*id, std::fs::symlink_metadata(p).ok().and_then(|m| m.modified().ok())))
                .collect();
            if tx.send(stats).is_err() {
                return;
            }
        }
    });
    rx
}

/// Byte ranges of `needle` in `name`, ignoring case unless the needle has a capital, for the
/// rows shown before the first answer.
fn substring_matches(name: &str, needle: &str) -> Option<Vec<(u32, u32)>> {
    let sensitive = needle.chars().any(char::is_uppercase);
    let at = if sensitive { name.find(needle) } else { name.to_lowercase().find(&needle.to_lowercase()) }?;
    let end = (at + needle.len()).min(name.len());
    (name.is_char_boundary(at) && name.is_char_boundary(end)).then(|| vec![(at as u32, end as u32)])
}

// Hooks the tab calls around each change of its search text.

/// Before the tab filters for a new query: remembers the folder's selection on the first
/// keystroke, and starts or stops the recursive search.
pub fn before_refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let query = tab.query().trim().to_owned();
    if !query.is_empty() && tab.extras.results.saved.is_none() {
        let saved = (tab.selected(), tab.cursor());
        tab.extras.results.saved = Some(saved);
    }
    if query.chars().count() < MIN_CHARS {
        let r = &mut tab.extras.results;
        r.stop();
        r.shown = false;
        r.query.clear();
        r.done = None;
        return;
    }
    start(tab, query, cx);
}

/// After the tab filtered: once the search text is gone, the folder's selection comes back.
pub fn after_refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    if !tab.query().trim().is_empty() {
        return;
    }
    let r = &mut tab.extras.results;
    r.stop();
    r.shown = false;
    r.query.clear();
    r.done = None;
    r.hint = false;
    r.rows.clear();
    r.by_id.clear();
    r.list.set_items(Vec::new(), cx);
    if let Some((selected, cursor)) = r.saved.take() {
        tab.restore_selection(selected, cursor, cx);
    }
}

fn scope_dir(tab: &Tab) -> PathBuf {
    let home = crate::env::get().home.clone();
    match (tab.extras.results.scope, tab.location()) {
        (Scope::Folder, Location::Dir(dir)) => dir.clone(),
        _ => home,
    }
}

fn start(tab: &mut Tab, query: String, cx: &mut Context<Tab>) {
    let scope = scope_dir(tab);
    let seed: Vec<Seed> = if tab.extras.results.shown {
        Vec::new()
    } else {
        let dir = tab.location().dir().map(Path::to_path_buf);
        tab.visible()
            .iter()
            .filter_map(|id| {
                let e = tab.entry(id)?;
                let m = substring_matches(&e.name, &query)?;
                Some((e.path_in(dir.as_deref()?), e.is_dir(), m))
            })
            .take(LIMIT)
            .collect()
    };
    let show_hidden = tab.show_hidden();
    let search = crate::search::get(cx);
    let run = search.query(Query { text: query.clone(), scope, show_hidden, limit: LIMIT });
    let me = cx.weak_entity();
    let r = &mut tab.extras.results;
    r.stop();
    r.query = query;
    r.done = None;
    if !r.shown {
        r.shown = true;
        r.cursor = None;
        let home = crate::env::get().home.clone();
        let hits = seed.into_iter().map(|(path, is_dir, name_matches)| Hit { path, is_dir, score: 0, name_matches });
        let unknown = r.set_hits(hits.collect(), &home);
        r.sync(cx);
        if !unknown.is_empty() {
            let rx = stat_rows(unknown);
            r.stats = Some(rx.clone());
            pump_stats(rx, r.generation, me.clone());
        }
    }
    let generation = r.generation;
    r.events = Some(run.events.clone());
    pump(run.events.clone(), me.clone(), move |tab: &mut Tab, events, cx| {
        if tab.extras.results.generation != generation {
            return false;
        }
        if let Some(rx) = tab.extras.results.apply(events, cx) {
            pump_stats(rx, generation, cx.weak_entity());
        }
        cx.notify();
        true
    });
    r.run = Some(run);
}

fn pump_stats(rx: flume::Receiver<Stats>, generation: u64, me: WeakEntity<Tab>) {
    pump(rx, me, move |tab: &mut Tab, batches, cx| {
        let r = &mut tab.extras.results;
        if r.generation != generation {
            return false;
        }
        r.apply_stats(batches.into_iter().flatten().collect(), cx);
        cx.notify();
        true
    });
}

/// Runs the search again in the other scope.
pub fn set_scope(tab: &mut Tab, scope: Scope, cx: &mut Context<Tab>) {
    if tab.extras.results.scope == scope {
        return;
    }
    tab.extras.results.scope = scope;
    let query = tab.extras.results.query.clone();
    if tab.extras.results.shown && !query.is_empty() {
        start(tab, query, cx);
    }
    cx.notify();
}

// Drawing.

/// The columns across `width`: Kind and Modified fixed, Folder taking up to a third of the rest
/// so a narrow window still shows whole names.
fn columns(width: f32) -> Vec<Column> {
    let rest = (width - KIND_W - DATE_W).max(NAME_MIN + PLACE_MIN);
    let place = (rest / 3.).clamp(PLACE_MIN, PLACE_W);
    let name = rest - place;
    vec![
        column("name", "Name").width(name).min_width(120.),
        column("place", "Folder").width(place).min_width(80.),
        column("kind", "Kind").width(KIND_W).min_width(60.),
        column("modified", "Modified").width(DATE_W).min_width(60.),
    ]
}

/// The name with its matched ranges in the accent.
fn highlighted(name: &str, matches: &[(u32, u32)], hidden: bool) -> Text {
    let mut spans = Vec::new();
    let mut at = 0usize;
    let mut ranges: Vec<(usize, usize)> = matches
        .iter()
        .map(|(s, e)| (*s as usize, (*e as usize).min(name.len())))
        .filter(|(s, e)| s < e && name.is_char_boundary(*s) && name.is_char_boundary(*e))
        .collect();
    ranges.sort_unstable();
    for (s, e) in ranges {
        if s < at {
            continue;
        }
        if s > at {
            spans.push(plain(&name[at..s], hidden));
        }
        spans.push(span(name[s..e].to_owned()).semibold().color(TextColor::Accent));
        at = e;
    }
    if at < name.len() {
        spans.push(plain(&name[at..], hidden));
    }
    rich_text(spans)
}

fn plain(s: &str, hidden: bool) -> Span {
    let sp = span(s.to_owned());
    if hidden { sp.color(TextColor::Secondary) } else { sp }
}

impl Tab {
    fn result_cell(&mut self, (id, col): (EntryId, usize), _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(row) = self.extras.results.by_id.get(&id) else { return div().into_any_element() };
        let quiet = |s: String| {
            text(s)
                .text_size(theme.type_scale.footnote)
                .text_color(theme.text.secondary)
                .tabular_nums()
                .truncate()
                .into_any_element()
        };
        let content = match col {
            0 => h_flex()
                .gap(theme.spacing.sm)
                .min_w_0()
                .child(icon(icons::for_kind(row.kind), 16.).text_color(icons::tint(row.kind, theme)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(highlighted(&row.name, &row.matches, row.name.starts_with('.')).truncate()),
                )
                .into_any_element(),
            1 => quiet(row.place.clone()),
            2 => quiet(row.kind.label().to_owned()),
            _ => quiet(row.modified.map(|t| self.today().modified(t)).unwrap_or_default()),
        };
        let row = h_flex().h(px(crate::tab::ROW_HEIGHT)).px(theme.spacing.sm).items_center().child(content);
        self.file_events(row, id, cx).into_any_element()
    }
}

/// The results in place of the listing: the scope switch and a quiet state line over the table.
pub fn render(tab: &mut Tab, width: f32, cx: &mut Context<Tab>) -> AnyElement {
    let index = crate::search::get(cx).index_state();
    let theme = cx.theme();
    let me = cx.weak_entity();
    let home = crate::env::get().home.clone();
    let folder = match tab.location() {
        Location::Dir(d) if *d != home => Some(tab.title()),
        _ => None,
    };
    let r = &tab.extras.results;
    let scope = r.scope;
    let scope_dir = scope_dir(tab);
    let state = state_line(&scope_dir, &home, r.done, &index, cx);

    let switch = folder.clone().map(|name| {
        let to = me.clone();
        segmented("search-scope", [name, "Home".to_owned()])
            .label("Search in")
            .selected(if scope == Scope::Folder { 0 } else { 1 })
            .on_change(move |e: &SelectChanged, _, cx| {
                let scope = if e.index == 0 { Scope::Folder } else { Scope::Home };
                let _ = to.update(cx, |t, cx| set_scope(t, scope, cx));
            })
            .into_any_element()
    });
    let hint_open = r.hint;
    let (toggle, dismiss) = (me.clone(), me.clone());
    let info = div()
        .child(icon_button("search-help", Icon::Info).tooltip("Search syntax").on_click(
            move |_: &ClickEvent, _, cx| {
                let _ = toggle.update(cx, |t, cx| {
                    t.extras.results.hint = !t.extras.results.hint;
                    cx.notify();
                });
            },
        ))
        .when(hint_open, |d| {
            d.child(
                popover("search-hint")
                    .placement(Placement::Below)
                    .p(theme.spacing.md)
                    .w(px(320.))
                    .child(hint(cx))
                    .on_dismiss(move |_: &Dismissed, _, cx| {
                        let _ = dismiss.update(cx, |t, cx| {
                            t.extras.results.hint = false;
                            cx.notify();
                        });
                    }),
            )
        });
    let header = h_flex()
        .id("results-header")
        .w_full()
        .h(theme.control.large)
        .px(theme.spacing.md)
        .gap(theme.spacing.md)
        .items_center()
        .children(switch)
        .child(div().flex_1().min_w_0().children(state))
        .child(info);

    let (sel, act, drop) = (me.clone(), me.clone(), me.clone());
    let cell = cx.processor(Tab::result_cell);
    let query = r.query.clone();
    let empty = if r.done.is_some() {
        status_page("No results")
            .icon(icons::no_results())
            .description(format!("Nothing matches \u{201c}{query}\u{201d}."))
            .compact()
            .into_any_element()
    } else {
        div().into_any_element()
    };
    let ids = tab.drag_ids(&r.list);
    let folders: std::collections::HashSet<EntryId> =
        r.by_id.iter().filter(|(_, row)| row.is_dir).map(|(id, _)| *id).collect();
    let table = table(TABLE, &r.list, columns(width), move |id, col, w, cx| cell((id, col), w, cx))
        .selection(Selection::multiple())
        .estimated_row_height(px(crate::tab::ROW_HEIGHT))
        .on_selection_change(move |e: &SelectionChanged<EntryId>, _, cx| {
            let _ = sel.update(cx, |t, cx| {
                t.extras.results.on_selection_change(e);
                cx.notify();
            });
        })
        .on_activate(move |e: &ActivateEvent<EntryId>, window, cx| {
            let _ = act.update(cx, |t, cx| t.open(e.key, window, cx));
        })
        .drag_rows(crate::actions::DRAG_FILES, move |id| ids.get(id).cloned().unwrap_or_default())
        .accepts_drops([crate::actions::DRAG_FILES, DragItem::FILES])
        .drop_on(move |id| folders.contains(id))
        .on_drop(move |e: &RowDropped<EntryId>, window, cx| {
            let Some(tab) = drop.upgrade() else { return };
            if let Some(action) = tab.read(cx).drop_on_row(e) {
                cx.dispatch_action(window.id(), &action);
            }
        })
        .empty(empty);
    v_flex()
        .id("results")
        .key_context(crate::keys::RESULTS)
        .size_full()
        .child(header)
        .child(separator())
        .child(div().flex_1().min_h_0().child(table))
        .into_any_element()
}

/// One quiet line while it matters: the index still being built, or a live walk of a network
/// folder.
fn state_line(scope: &Path, home: &Path, done: Option<Done>, index: &IndexState, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let remote = trove_fs::is_network_path(scope, &crate::env::volumes());
    let message = if remote {
        Some(if done.is_some_and(|d| d.truncated) {
            "This is a live search of a network folder. It stopped early, so results may be incomplete."
        } else {
            "This is a live search of a network folder, so it may be slow."
        })
    } else if scope.starts_with(home) {
        matches!(index, IndexState::Absent | IndexState::Loading | IndexState::Building { .. })
            .then_some("Indexing your files, results may be incomplete.")
    } else {
        None
    }?;
    Some(
        h_flex()
            .id("results-state")
            .gap(theme.spacing.sm)
            .items_center()
            .child(icon(Icon::Info, 12.).text_color(theme.text.secondary))
            .child(text(message).text_size(theme.type_scale.footnote).text_color(theme.text.secondary).truncate())
            .into_any_element(),
    )
}

/// The query language, in short.
fn hint(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let rows: [(&str, &str); 8] = [
        ("report 2026", "every word in the name"),
        ("\"exact phrase\"", "the words as written"),
        ("*.rs, img_??", "a pattern over the whole name"),
        ("ext:pdf,docx", "by extension"),
        ("kind:image", "image, video, audio, doc, code, folder, archive"),
        ("size:>10mb", "larger than; also <, >=, <="),
        ("modified:week", "today, yesterday, week, month, year, or a date"),
        ("in:src", "inside a folder of that name"),
    ];
    v_flex()
        .id("search-hint-body")
        .gap(theme.spacing.sm)
        .child(text("Search syntax").font_weight(FontWeight::SEMIBOLD))
        .children(rows.into_iter().map(|(example, what)| {
            v_flex()
                .child(text(example).text_size(theme.type_scale.footnote).font_family(theme.fonts.mono.family.clone()))
                .child(text(what).text_size(theme.type_scale.footnote).text_color(theme.text.secondary))
                .into_any_element()
        }))
        .child(
            text("A capital letter makes that word match case.")
                .text_size(theme.type_scale.footnote)
                .text_color(theme.text.secondary),
        )
        .into_any_element()
}
