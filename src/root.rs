//! The window: toolbar, tab strip, sidebar, the active tab's content, the status bar and the
//! preview pane. It owns the tabs and routes every command to the active one; each piece that
//! changes on its own (sidebar, path bar, tab content, status, preview) is its own view.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use nd::core::WindowId;
use nd::prelude::*;

use crate::actions::*;
use crate::icons;
use crate::location::Location;
use crate::pathbar::PathBar;
use crate::picker::{Choice, Decision, Outcome, PickAccept, PickCancel, Picker};
use crate::preview::Preview;
use crate::sidebar::Sidebar;
use crate::status::StatusBar;
use crate::tab::{Load, Tab, View};

/// The widest a tab gets, as in Explorer: a few tabs keep their titles instead of each taking
/// a share of the window.
const TAB_W: f32 = 220.;
/// How many closed tabs Reopen closed tab can bring back.
const CLOSED_MAX: usize = 16;
/// The strip's `+` past the last tab.
const ADD_W: f32 = 44.;
/// The sidebar's width until the user drags it.
const SIDEBAR_W: f32 = 232.;
/// The least the files get beside the preview pane while the pane can still narrow.
const FILES_MIN: f32 = 440.;
/// The header over the files keeps its search field from this width up.
const SEARCH_ROOM: f32 = 860.;
/// Below this width the header's three view buttons fold into one menu, for the path bar.
const VIEW_ROOM: f32 = 440.;

/// Where the first tab opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Start {
    Home,
    /// The tabs the last run left, from the session.
    Restore(crate::session::Session),
    /// A path from the command line: a folder opens, a file opens its folder with the file
    /// selected. Which one it is shows when the listing answers, never by a stat up front.
    Path(PathBuf),
    /// A tab for each, the first active: what "Show in folder" asks for.
    Places(Vec<Place>),
}

/// One tab of [`Start::Places`].
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// As [`Start::Path`].
    Path(PathBuf),
    /// A folder with these items of it selected, the first scrolled to.
    Items { folder: PathBuf, items: Vec<PathBuf> },
}

/// What the toolbar and tab strip show of the active tab; the window re-renders only when this
/// moves, not on every listing batch.
#[derive(Clone, Debug, PartialEq)]
struct Chrome {
    location: Location,
    title: String,
    can_back: bool,
    can_forward: bool,
    view: View,
}

/// A closed tab as Reopen closed tab brings it back, where it stood in the strip.
#[derive(Clone, Debug)]
struct ClosedTab {
    at: usize,
    location: Location,
    view: View,
    sort: trove_fs::SortOrder,
    selection: Vec<PathBuf>,
}

pub struct Root {
    weak: WeakEntity<Root>,
    window: WindowId,
    tabs: Vec<(u64, Entity<Tab>)>,
    active: usize,
    next_id: u64,
    sidebar: Entity<Sidebar>,
    pathbar: Entity<PathBar>,
    status: Entity<StatusBar>,
    preview: Entity<Preview>,
    quicklook: Entity<crate::quicklook::QuickLook>,
    show_preview: bool,
    show_hidden: bool,
    sidebar_w: f32,
    preview_w: f32,
    chrome: Option<Chrome>,
    titles: Vec<String>,
    /// Closed tabs, the last closed at the end.
    closed: Vec<ClosedTab>,
    size: (f32, f32),
    /// Set when the window picks files for another app.
    picker: Option<Entity<Picker>>,
    /// The filter the tabs list under, as last handed to them.
    pick_filter: Option<usize>,
    _watch: Option<Subscription>,
    _volumes: Subscription,
    _picker: Option<Subscription>,
    _settings: Option<Subscription>,
}

impl Root {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::build(start, None, window, cx)
    }

    /// A window picking files for another app, opening where the request says or on the last
    /// folder picked.
    pub fn picking(
        request: crate::picker::Request,
        done: crate::picker::Done,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let last = crate::picker::last_folder(cx);
        let start = crate::picker::start_folder(&request, last, &crate::env::get().home);
        let picker = cx.new(|_| Picker::new(request, done));
        let mut this = Self::build(Start::Path(start), Some(picker.clone()), window, cx);
        this._picker = Some(cx.observe(&picker, |this: &mut Root, _, cx| this.sync_pick_filter(cx)));
        this
    }

    fn build(start: Start, picker: Option<Entity<Picker>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::settings::init(cx);
        crate::ops::init(cx);
        let prefs = crate::settings::get(cx);
        let places = match &start {
            Start::Path(p) => vec![Place::Path(p.clone())],
            Start::Places(places) => places.clone(),
            Start::Home | Start::Restore(_) => Vec::new(),
        };
        let me = cx.weak_entity();
        let go = move |location: Location, window: &mut Window, cx: &mut App| {
            let _ = me.update(cx, |this, cx| this.go(location, window, cx));
        };
        let sidebar = cx.new({
            let go = go.clone();
            |cx| Sidebar::new(go, cx)
        });
        let home = Location::Dir(crate::env::get().home.clone());
        let pathbar = cx.new(|_| PathBar::new(home.clone(), go));
        let volumes = cx.observe(&sidebar, |this: &mut Root, sidebar, cx| {
            let volumes = sidebar.read(cx).volumes().to_vec();
            this.pathbar.update(cx, |p, cx| p.set_volumes(&volumes, cx));
        });
        let volumes_now = sidebar.read(cx).volumes().to_vec();
        pathbar.update(cx, |p, cx| p.set_volumes(&volumes_now, cx));
        let mut this = Self {
            weak: cx.weak_entity(),
            window: window.id(),
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            sidebar,
            pathbar,
            status: cx.new(|cx| StatusBar::new(window.id(), cx)),
            preview: cx.new(Preview::new),
            quicklook: cx.new(crate::quicklook::QuickLook::new),
            show_preview: false,
            show_hidden: prefs.show_hidden,
            sidebar_w: SIDEBAR_W,
            preview_w: crate::preview::PANE_W,
            chrome: None,
            titles: Vec::new(),
            closed: Vec::new(),
            size: (0., 0.),
            pick_filter: picker.as_ref().map(|p| p.read(cx).filter_index()),
            picker,
            _watch: None,
            _volumes: volumes,
            _picker: None,
            _settings: crate::settings::changes(cx)
                .map(|changes| cx.observe(&changes, |this: &mut Root, _, cx| this.apply_settings(cx))),
        };
        let view = match prefs.default_view {
            crate::settings::DefaultView::List => View::List,
            crate::settings::DefaultView::Grid => View::Grid,
            crate::settings::DefaultView::Columns => View::Columns,
        };
        let mut active = 0;
        match start {
            Start::Restore(session) => {
                for saved in &session.tabs {
                    let tab = this.make_tab(saved.location(), saved.view(), cx);
                    let sort = saved.sort();
                    tab.update(cx, |t, cx| t.set_sort(sort, cx));
                    let id = this.take_id();
                    this.tabs.push((id, tab));
                }
                this.show_preview = session.show_preview;
                if session.sidebar_width > 0. {
                    this.sidebar_w = session.sidebar_width;
                }
                if session.preview_width > 0. {
                    this.preview_w = session.preview_width;
                }
                active = session.active;
            }
            Start::Home | Start::Path(_) | Start::Places(_) => {}
        }
        for place in places {
            let tab = match place {
                Place::Path(p) => {
                    let tab = this.make_tab(Location::Dir(p), view, cx);
                    tab.update(cx, |t, _| t.open_parent_if_file());
                    tab
                }
                Place::Items { folder, items } => {
                    let tab = this.make_tab(Location::Dir(folder), view, cx);
                    tab.update(cx, |t, cx| t.reveal_items(&items, cx));
                    tab
                }
            };
            let id = this.take_id();
            this.tabs.push((id, tab));
        }
        if this.tabs.is_empty() {
            let tab = this.make_tab(home, view, cx);
            let id = this.take_id();
            this.tabs.push((id, tab));
        }
        this.register_drops();
        this.activate(active, window, cx);
        this
    }

    // For tests and tooling.

    pub fn tabs(&self) -> Vec<Entity<Tab>> {
        self.tabs.iter().map(|(_, t)| t.clone()).collect()
    }

    /// The tabs' ids, which the tab menus' actions carry, in strip order.
    pub fn tab_ids(&self) -> Vec<u64> {
        self.tabs.iter().map(|(id, _)| *id).collect()
    }

    pub fn widths(&self) -> (f32, f32) {
        (self.sidebar_w, self.preview_w)
    }

    pub fn active_tab(&self) -> &Entity<Tab> {
        &self.tabs[self.active].1
    }

    pub fn preview(&self) -> &Entity<Preview> {
        &self.preview
    }

    pub fn quicklook(&self) -> &Entity<crate::quicklook::QuickLook> {
        &self.quicklook
    }

    pub fn window_id(&self) -> WindowId {
        self.window
    }

    pub fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn pathbar(&self) -> &Entity<PathBar> {
        &self.pathbar
    }

    pub fn status(&self) -> &Entity<StatusBar> {
        &self.status
    }

    pub fn shows_preview(&self) -> bool {
        self.show_preview
    }

    pub fn shows_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn can_reopen(&self) -> bool {
        !self.closed.is_empty()
    }

    pub fn picker(&self) -> Option<&Entity<Picker>> {
        self.picker.as_ref()
    }

    /// What takes the keys once the window shows: the name field when saving, else the files.
    pub fn first_focus(&self, cx: &App) -> &'static str {
        if self.picker.as_ref().is_some_and(|p| p.read(cx).request().mode == crate::picker::Mode::Save) {
            return crate::picker::NAME_FIELD;
        }
        self.active_tab().read(cx).focus_target()
    }

    /// Applies everything each tab's listing has sent, waiting up to `timeout` per tab: the
    /// host's pump, for tests and the appearance audit, which run none.
    pub fn drain(&mut self, timeout: std::time::Duration, cx: &mut Context<Self>) {
        loop {
            let ran = crate::ops::run_deferred(cx) | crate::settings::drain(cx);
            let jobs = crate::ops::jobs(cx);
            let moved = jobs.update(cx, |j, cx| j.drain(timeout, cx));
            for (_, tab) in &self.tabs {
                tab.update(cx, |t, cx| t.drain(timeout, cx));
            }
            if !ran && !moved && !crate::ops::run_deferred(cx) {
                break;
            }
        }
    }

    // Tabs.

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn make_tab(&mut self, location: Location, view: View, cx: &mut Context<Self>) -> Entity<Tab> {
        let show_hidden = self.show_hidden;
        let sort = crate::settings::get(cx).sort();
        let window = self.window;
        let shown = self.picker.as_ref().map(|p| Rc::new(p.read(cx).shown()));
        let tab = cx.new(|cx| {
            let mut t = Tab::new(location, show_hidden, view, cx);
            t.set_window(window);
            t.set_folder_sort(sort, cx);
            if let Some(shown) = shown {
                t.set_pick(shown, cx);
            }
            t
        });
        cx.observe(&tab, |this: &mut Root, _, cx| this.refresh_titles(cx)).detach();
        tab
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index.min(self.tabs.len() - 1);
        let tab = self.active_tab().clone();
        self.status.update(cx, |s, cx| s.set_tab(&tab, cx));
        self.preview.update(cx, |p, cx| p.set_tab(&tab, cx));
        self._watch = Some(cx.observe(&tab, |this: &mut Root, _, cx| {
            this.sync_chrome(cx);
            this.sync_pick_name(cx);
        }));
        self.chrome = None;
        self.sync_chrome(cx);
        self.save_session(cx);
        self.focus_files(window, cx);
        cx.notify();
    }

    /// Keys go to the list the active tab shows.
    pub(crate) fn focus_files(&self, window: &mut Window, cx: &App) {
        window.focus(self.active_tab().read(cx).focus_target());
    }

    fn sync_chrome(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab().read(cx);
        let chrome = Chrome {
            location: tab.location().clone(),
            title: tab.title(),
            can_back: tab.can_go_back(),
            can_forward: tab.can_go_forward(),
            view: tab.view(),
        };
        if self.chrome.as_ref() == Some(&chrome) {
            return;
        }
        let location = chrome.location.clone();
        let title = match &self.picker {
            Some(p) => p.read(cx).request().window_title(),
            None => chrome.title.clone(),
        };
        cx.set_window_title(self.window, title);
        self.sidebar.update(cx, |s, cx| s.set_current(&location, cx));
        self.pathbar.update(cx, |p, cx| p.set_location(&location, cx));
        self.chrome = Some(chrome);
        cx.notify();
    }

    fn refresh_titles(&mut self, cx: &mut Context<Self>) {
        let titles: Vec<String> = self.tabs.iter().map(|(_, t)| t.read(cx).title()).collect();
        if titles != self.titles {
            self.titles = titles;
            cx.notify();
        }
        self.save_session(cx);
    }

    fn new_tab(&mut self, location: Location, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.active_tab().read(cx).view();
        let tab = self.make_tab(location, view, cx);
        let at = self.active + 1;
        let id = self.take_id();
        self.tabs.insert(at, (id, tab));
        self.refresh_titles(cx);
        if select {
            self.activate(at, window, cx);
        } else {
            cx.notify();
        }
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 {
            cx.close_window(self.window);
            return;
        }
        self.remember_closed(index, cx);
        self.tabs.remove(index);
        let next = if index < self.active || self.active >= self.tabs.len() {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        self.refresh_titles(cx);
        self.activate(next, window, cx);
    }

    /// Keeps the tab at `index` for Reopen closed tab.
    fn remember_closed(&mut self, index: usize, cx: &App) {
        let Some((_, tab)) = self.tabs.get(index) else { return };
        let t = tab.read(cx);
        let dir = t.location().dir().map(Path::to_path_buf);
        let selection =
            t.selected_paths().into_iter().filter(|p| dir.as_deref().is_some_and(|d| p.parent() == Some(d))).collect();
        let closed = ClosedTab { at: index, location: t.location().clone(), view: t.view(), sort: t.sort(), selection };
        if self.closed.len() == CLOSED_MAX {
            self.closed.remove(0);
        }
        self.closed.push(closed);
    }

    /// Brings back the last closed tab where it stood, with its view, sort and selection.
    pub(crate) fn reopen_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.closed.pop() else { return };
        let tab = self.make_tab(closed.location, closed.view, cx);
        let (sort, selection) = (closed.sort, closed.selection);
        tab.update(cx, |t, cx| {
            t.set_sort(sort, cx);
            t.select_created(selection, false, cx);
        });
        let at = closed.at.min(self.tabs.len());
        let id = self.take_id();
        self.tabs.insert(at, (id, tab));
        self.refresh_titles(cx);
        self.activate(at, window, cx);
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        let next = (self.active as isize + step).rem_euclid(n) as usize;
        self.activate(next, window, cx);
    }

    // Navigation, always in the active tab.

    fn go(&mut self, location: Location, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab().clone().update(cx, |t, cx| t.navigate(location, window, cx));
        self.focus_files(window, cx);
    }

    fn with_tab(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Tab, &mut Context<Tab>)) {
        self.active_tab().clone().update(cx, f);
    }

    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        let picks_file = self.picker.is_some() && {
            let t = self.active_tab().read(cx);
            t.target().is_some_and(|id| !t.is_folder(&id))
        };
        if picks_file {
            return self.pick_accept(window, cx);
        }
        self.active_tab().clone().update(cx, |t, cx| {
            if let Some(id) = t.target() {
                t.open(id, window, cx);
            }
        });
    }

    fn open_in_new_tab(&mut self, _: &OpenInNewTab, window: &mut Window, cx: &mut Context<Self>) {
        let target = {
            let tab = self.active_tab().read(cx);
            tab.target().filter(|id| tab.is_folder(id)).and_then(|id| tab.path_of(&id))
        };
        if let Some(path) = target {
            self.new_tab(Location::Dir(path), false, window, cx);
        }
    }

    fn enter_folder(&mut self, _: &IntoFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab().clone().update(cx, |t, cx| {
            if let Some(id) = t.target().filter(|id| t.is_folder(id)) {
                t.open(id, window, cx);
            }
        });
    }

    /// Brings every tab up to the settings after a change made anywhere: this window, another
    /// one, Preferences or the file.
    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let prefs = crate::settings::get(cx);
        let zoom = crate::zoom::Zoom::of(&prefs);
        self.show_hidden = prefs.show_hidden;
        for (_, tab) in &self.tabs {
            tab.update(cx, |t, cx| {
                t.set_show_hidden(prefs.show_hidden, cx);
                t.set_folders_first(prefs.folders_first, cx);
                t.set_zoom(zoom, cx);
            });
        }
        cx.notify();
    }

    fn toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let show = self.show_hidden;
        for (_, tab) in &self.tabs {
            tab.update(cx, |t, cx| t.set_show_hidden(show, cx));
        }
        crate::settings::set(cx, "show_hidden", &show);
    }

    /// A copy of the tab with id `of`, right after it and selected.
    fn duplicate_tab(&mut self, of: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.position(of) else { return };
        let (location, view, sort) = {
            let tab = self.tabs[from].1.read(cx);
            (tab.location().clone(), tab.view(), tab.sort())
        };
        let tab = self.make_tab(location, view, cx);
        tab.update(cx, |t, cx| t.set_sort(sort, cx));
        let at = from + 1;
        let id = self.take_id();
        self.tabs.insert(at, (id, tab));
        self.refresh_titles(cx);
        self.activate(at, window, cx);
    }

    fn close_other_tabs(&mut self, keep: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.position(keep) else { return };
        for i in (0..self.tabs.len()).rev().filter(|i| *i != at) {
            self.remember_closed(i, cx);
        }
        let keep = self.tabs.remove(at);
        self.tabs = vec![keep];
        self.refresh_titles(cx);
        self.activate(0, window, cx);
    }

    fn position(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|(n, _)| *n == id)
    }

    /// The tabs as the next launch reopens them. Kept on every change; the store writes it a
    /// moment later off the main thread, and only when it moved.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        if crate::env::get().demo || self.picker.is_some() {
            return;
        }
        let tabs = self
            .tabs
            .iter()
            .map(|(_, t)| {
                let t = t.read(cx);
                crate::session::TabState::new(t.location(), t.view(), t.sort())
            })
            .collect();
        let session = crate::session::Session {
            tabs,
            active: self.active,
            width: self.size.0,
            height: self.size.1,
            show_preview: self.show_preview,
            sidebar_width: self.sidebar_w,
            preview_width: self.preview_w,
        };
        crate::session::save(cx, session);
    }

    /// Files from another app dropped where nothing takes them land in the folder the active
    /// tab shows.
    fn register_drops(&self) {
        let me = self.me();
        let window = self.window;
        // nd: on macOS files from other apps arrive only as FileDrop, with a position but no
        // element, so there a drop on a folder row or a sidebar place lands here too.
        nd::system::on_event(&nd::system::system(), move |event, cx| {
            let nd::system::SystemEvent::FileDrop { window: on, paths, .. } = event else { return };
            if *on != window || paths.is_empty() {
                return;
            }
            let Some(root) = me.upgrade() else { return };
            let at = me.clone();
            let (origin, dest) = {
                let r = root.read(cx);
                let tab = r.active_tab().read(cx);
                let dest = tab.location().dir().map(PathBuf::from).filter(|_| !tab.is_trash());
                (crate::ops::Origin { window, tab: Some(r.active_tab().downgrade()) }, dest)
            };
            if let Some(dest) = dest {
                crate::fileops::transfer(cx, at, origin, paths.clone(), dest, crate::fileops::Transfer::Auto);
            }
        });
    }

    fn view_switch(&self) -> impl IntoElement + use<> {
        let view = self.chrome.as_ref().map_or(View::List, |c| c.view);
        let at = match view {
            View::List => 0,
            View::Grid => 1,
            View::Columns => 2,
        };
        segmented("view", ["List", "Grid", "Columns"])
            .icons([icons::list_view(), icons::grid_view(), icons::columns_view()])
            .label("View")
            .selected(at)
            .on_change(|e: &SelectChanged, window, cx| {
                let action: &dyn Action = match e.index {
                    0 => &ListView,
                    1 => &GridView,
                    _ => &crate::keys::ColumnsView,
                };
                cx.dispatch_action(window.id(), action);
            })
    }

    fn view_menu(&self) -> impl IntoElement + use<> {
        let view = self.chrome.as_ref().map_or(View::List, |c| c.view);
        let glyph = match view {
            View::List => icons::list_view(),
            View::Grid => icons::grid_view(),
            View::Columns => icons::columns_view(),
        };
        menu_button("view", glyph, |menu, _, _| {
            menu.item("List", ListView).item("Grid", GridView).item("Columns", crate::keys::ColumnsView)
        })
        .tooltip("View")
    }

    /// The tabs share the band's width up to [`TAB_W`] each; the band runs the files' whole
    /// width on the strip's own surface, its hairline carried on past the last tab.
    fn tab_strip(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let count = self.tabs.len();
        h_flex()
            .id("tab-strip")
            .w_full()
            .items_stretch()
            .bg(theme.surface.sidebar)
            .child(div().min_w_0().w(px(TAB_W * count as f32 + ADD_W)).child(self.tab_bar()))
            .child(div().flex_1().border_b_1().border_color(theme.border.subtle))
    }

    fn tab_bar(&self) -> TabStrip {
        let active = format!("tab-{}", self.tabs[self.active].0);
        let specs = self.tabs.iter().zip(&self.titles).map(|((id, _), title)| nd::components::TabSpec {
            id: format!("tab-{id}").into(),
            title: title.clone().into(),
            icon: None,
            closable: true,
        });
        let (select, close, add, drop) = (self.me(), self.me(), self.me(), self.me());
        let count = self.tabs.len();
        tab_strip("tabs")
            .presentation(TabStyle::Strip)
            .add_button(true)
            .label("Tabs")
            .tabs(specs.collect::<Vec<_>>())
            .selected(active)
            .on_select(move |e: &TabChanged, window, cx| {
                let _ = select.update(cx, |this, cx| {
                    if let Some(i) = this.index_of(&e.id) {
                        this.activate(i, window, cx);
                    }
                });
            })
            .on_close(move |e: &TabCloseRequested, window, cx| {
                let _ = close.update(cx, |this, cx| {
                    if let Some(i) = this.index_of(&e.id) {
                        this.close_tab(i, window, cx);
                    }
                });
            })
            .context_menu(move |id, menu, _, _| match tab_id(id) {
                Some(id) => crate::menus::tab(menu, id, count),
                None => menu,
            })
            .on_add(move |_: &TabAddRequested, window, cx| {
                let _ = add.update(cx, |this, cx| {
                    let here = this.active_tab().read(cx).location().clone();
                    this.new_tab(here, true, window, cx);
                });
            })
            .accepts_drops([DRAG_FILES, DragItem::FILES])
            .on_drop(move |e: &Dropped, window, cx| {
                let Some(this) = drop.upgrade() else { return };
                let dest = {
                    let r = this.read(cx);
                    let tab = e.slot.as_ref().and_then(|s| s.on.as_deref()).and_then(|id| r.index_of(id));
                    tab.and_then(|i| r.tabs[i].1.read(cx).location().dir().map(PathBuf::from))
                };
                let Some(dest) = dest else { return };
                let paths = e.item.ids.iter().map(|id| PathBuf::from(id.as_ref())).collect();
                let mode = crate::fileops::Transfer::from_modifiers(e.modifiers);
                cx.dispatch_action(window.id(), &DropOnto { paths, dest, mode });
            })
    }

    fn me(&self) -> WeakEntity<Root> {
        self.weak.clone()
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        tab_id(id).and_then(|id| self.position(id))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Toolbar {
        let chrome = self.chrome.clone();
        let (can_back, can_forward) = chrome.as_ref().map_or((false, false), |c| (c.can_back, c.can_forward));
        let title = chrome.as_ref().map(|c| c.title.clone()).unwrap_or_default();
        let search = self.active_tab().read(cx).search().clone();
        let field =
            search_field("search", &search).placeholder(format!("Search {title}")).key_context(SEARCH).w(px(240.));
        let toolbar = toolbar()
            .hide_title()
            .navigation(icon_button("back", Icon::ChevronLeft).tooltip("Back").disabled(!can_back).dispatch(Back))
            .navigation(
                icon_button("forward", Icon::ChevronRight).tooltip("Forward").disabled(!can_forward).dispatch(Forward),
            )
            .flexible_item(self.pathbar.clone());
        let toolbar =
            if self.files_w() >= VIEW_ROOM { toolbar.item(self.view_switch()) } else { toolbar.item(self.view_menu()) };
        // A header too narrow for the field keeps libadwaita's search toggle instead, its bar
        // opening under the header (Ctrl+F opens it too).
        let toolbar =
            if self.files_w() >= SEARCH_ROOM { toolbar.persistent_search(field) } else { toolbar.item(field) };
        let toolbar = toolbar.item(
            icon_button("preview-toggle", Icon::PanelRight)
                .tooltip("Preview")
                .checked(self.show_preview)
                .dispatch(TogglePreview),
        );
        let menu = if self.picker.is_some() { None } else { crate::app_menu::header_button(self.me()) };
        match menu {
            Some(button) => toolbar.item(button),
            None => toolbar,
        }
    }

    /// The preview pane's width as drawn: it gives up its own width, down to its least, before
    /// the files beside it go under [`FILES_MIN`].
    fn shown_preview_w(&self) -> f32 {
        self.preview_w.min((self.size.0 - self.sidebar_w - FILES_MIN).max(crate::preview::PANE_MIN))
    }

    /// The width the files and their header bar get between the sidebar and the preview pane.
    fn files_w(&self) -> f32 {
        let preview = if self.show_preview { self.shown_preview_w() } else { 0. };
        self.size.0 - self.sidebar_w - preview
    }

    fn actions(&self, root: Scaffold, cx: &mut Context<Self>) -> Scaffold {
        let root = crate::fileops::register(root, cx);
        let root = crate::keys::on_root(root, cx);
        let root = if self.picker.is_some() { root } else { crate::app_menu::on_root(root, cx) };
        root.on_action(cx.listener(Self::open))
            .on_action(
                cx.listener(|this, DuplicateTab(id): &DuplicateTab, window, cx| this.duplicate_tab(*id, window, cx)),
            )
            .on_action(cx.listener(|this, CloseOtherTabs(id): &CloseOtherTabs, window, cx| {
                this.close_other_tabs(*id, window, cx)
            }))
            .on_action(cx.listener(|this, CloseTabWithId(id): &CloseTabWithId, window, cx| {
                if let Some(at) = this.position(*id) {
                    this.close_tab(at, window, cx);
                }
            }))
            .on_action(cx.listener(|this, OpenPlaceInNewTab(at): &OpenPlaceInNewTab, window, cx| {
                this.new_tab(at.clone(), false, window, cx)
            }))
            .on_action(cx.listener(Self::open_in_new_tab))
            .on_action(cx.listener(Self::enter_folder))
            .on_action(cx.listener(Self::toggle_hidden))
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                this.with_tab(cx, |t, cx| t.go_back(window, cx));
                this.focus_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Forward, window, cx| {
                this.with_tab(cx, |t, cx| t.go_forward(window, cx));
                this.focus_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusFiles, window, cx| this.focus_files(window, cx)))
            .on_action(cx.listener(|this, _: &Parent, window, cx| this.with_tab(cx, |t, cx| t.go_up(window, cx))))
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.with_tab(cx, |t, cx| t.reload(cx))))
            .on_action(
                cx.listener(|this, GoTo(path): &GoTo, window, cx| this.go(Location::Dir(path.clone()), window, cx)),
            )
            .on_action(cx.listener(|this, _: &Deselect, window, cx| {
                if this.picker.is_some() {
                    let cleared = this.active_tab().clone().update(cx, |t, cx| t.clear_search(window, cx));
                    if !cleared {
                        this.pick_end(Outcome::Cancelled, cx);
                    }
                    return;
                }
                this.with_tab(cx, |t, cx| t.deselect(window, cx));
                this.focus_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PickAccept, window, cx| this.pick_accept(window, cx)))
            .on_action(cx.listener(|this, _: &PickCancel, _, cx| this.pick_end(Outcome::Cancelled, cx)))
            .on_action(cx.listener(|this, _: &ListView, _, cx| this.with_tab(cx, |t, cx| t.set_view(View::List, cx))))
            .on_action(cx.listener(|this, _: &GridView, _, cx| this.with_tab(cx, |t, cx| t.set_view(View::Grid, cx))))
            .on_action(cx.listener(|this, _: &TogglePreview, _, cx| {
                this.show_preview = !this.show_preview;
                this.save_session(cx);
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &EditPath, window, cx| {
                    this.pathbar.clone().update(cx, |p, cx| p.edit(window, cx))
                }),
            )
            .on_action(cx.listener(|this, _: &CancelPathEdit, window, cx| {
                this.pathbar.clone().update(cx, |p, cx| p.cancel(window, cx))
            }))
            .on_action(
                cx.listener(|this, _: &CompletePath, _, cx| this.pathbar.clone().update(cx, |p, cx| p.complete(cx))),
            )
            .on_action(cx.listener(|_, _: &FocusSearch, window, _| window.focus("search")))
            .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                this.with_tab(cx, |t, cx| {
                    t.clear_search(window, cx);
                });
                this.focus_files(window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                let here = this.active_tab().read(cx).location().clone();
                this.new_tab(here, true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let at = this.active;
                this.close_tab(at, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, SelectTab(n): &SelectTab, window, cx| {
                if *n < this.tabs.len() {
                    this.activate(*n, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &MoveLeft, _, cx| this.with_tab(cx, |t, cx| crate::columns::step(t, -1, cx))),
            )
            .on_action(
                cx.listener(|this, _: &MoveRight, _, cx| this.with_tab(cx, |t, cx| crate::columns::step(t, 1, cx))),
            )
            .on_action(cx.listener(|this, TypeSelect(c): &TypeSelect, window, cx| {
                let now = window.now();
                this.with_tab(cx, |t, cx| t.type_select(*c, now, cx));
            }))
    }
}

// Picking files for another app.
impl Root {
    /// The picker's filter moved: every tab lists under the new one.
    fn sync_pick_filter(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        let at = picker.read(cx).filter_index();
        if self.pick_filter == Some(at) {
            return;
        }
        self.pick_filter = Some(at);
        let shown = Rc::new(picker.read(cx).shown());
        for (_, tab) in &self.tabs {
            tab.update(cx, |t, cx| t.set_pick(shown.clone(), cx));
        }
    }

    /// A save takes the name of the file selected.
    fn sync_pick_name(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        let file = {
            let t = self.active_tab().read(cx);
            t.target().filter(|id| !t.is_folder(id)).and_then(|id| t.path_of(&id))
        };
        picker.update(cx, |p, cx| p.selected_file(file, cx));
    }

    fn pick_accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        let decision = {
            let p = picker.read(cx);
            let t = self.active_tab().read(cx);
            let path_kind = |id: &trove_fs::EntryId| t.path_of(id).map(|p| (p, t.is_folder(id)));
            let choice = Choice {
                dir: t.location().dir().map(Path::to_path_buf),
                selected: t.selected().iter().filter_map(path_kind).collect(),
                target: t.target().as_ref().and_then(path_kind),
                name: p.name().to_owned(),
            };
            // A save checks what exists against the listing, so it waits for every name.
            let listed = matches!(t.load_state(), Load::Listed | Load::Complete);
            if p.finished() || p.request().saving() && !listed {
                Decision::Nothing
            } else {
                crate::picker::decide(p.request(), &choice, |path| t.listed(path))
            }
        };
        match decision {
            Decision::Enter(dir) => self.go(Location::Dir(dir), window, cx),
            Decision::Go { dir, name } => {
                picker.update(cx, |p, cx| p.set_name(name, cx));
                self.go(Location::Dir(dir), window, cx);
            }
            Decision::Refuse(title) => nd::chrome::alert(cx, self.window, Alert::new(title), |_, _| {}),
            Decision::Accept(paths) => self.pick_finish(paths, cx),
            Decision::Replace { paths, title } => {
                let mut alert = Alert::confirm(title, "Replace");
                alert.message = Some("Replacing overwrites what is there now.".into());
                let me = self.me();
                nd::chrome::alert(cx, self.window, alert, move |res, cx| {
                    if res.0 == 0 {
                        let _ = me.update(cx, |r, cx| r.pick_finish(paths, cx));
                    }
                });
            }
            Decision::Nothing => {}
        }
    }

    fn pick_finish(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        let here = self.active_tab().read(cx).location().dir().map(Path::to_path_buf);
        let folder = here.or_else(|| paths.first().and_then(|p| p.parent()).map(Path::to_path_buf));
        if let Some(folder) = folder {
            crate::picker::remember(cx, &folder);
        }
        let outcome = picker.read(cx).picked(&paths);
        self.pick_end(outcome, cx);
    }

    /// Hands `outcome` to the caller, once; a pick already ended ignores the rest.
    fn pick_end(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.clone() else { return };
        if let Some(done) = picker.update(cx, |p, _| p.take_done()) {
            done(outcome, cx);
        }
    }
}

/// The mouse's back and forward buttons drive the active tab's history.
pub fn history(e: &NavigateEvent, window: &mut Window, cx: &mut App) {
    let action: &dyn Action = match e.direction {
        SwipeDirection::Back => &Back,
        SwipeDirection::Forward => &Forward,
    };
    cx.dispatch_action(window.id(), action);
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace::first_render();
        crate::search::after_first_frame(cx);
        let viewport = window.viewport_size();
        let size = (viewport.width, viewport.height);
        if size != self.size {
            self.size = size;
            self.save_session(cx);
        }
        let preview_w = self.shown_preview_w();
        if self.preview.read(cx).width() != preview_w {
            self.preview.update(cx, |p, cx| p.set_width(preview_w, cx));
        }
        let tabs = (self.tabs.len() > 1).then(|| self.tab_strip(cx));
        // The toolbar is built from the window's size, which a resize changes without asking
        // this view to render.
        let content = v_flex()
            .size_full()
            .on_resize(cx.listener(|_, _, _, cx| cx.notify()))
            .on_navigate(|e: &NavigateEvent, window, cx| history(e, window, cx))
            .children(tabs)
            .child(div().flex_1().min_h_0().child(self.active_tab().clone()))
            .child(self.status.clone())
            .children(self.picker.clone());
        let root = scaffold()
            .key_context(WINDOW)
            .toolbar(self.toolbar(cx))
            .sidebar(
                sidebar()
                    .resizable()
                    .min_w(px(180.))
                    .max_w(px(400.))
                    .width(px(self.sidebar_w))
                    .on_resize(cx.listener(|this, e: &nd::components::PaneResized, _, cx| {
                        this.sidebar_w = e.width;
                        this.save_session(cx);
                        cx.notify();
                    }))
                    .child(self.sidebar.clone()),
            )
            .content(content)
            .inspector(
                inspector()
                    .resizable()
                    .min_w(px(crate::preview::PANE_MIN))
                    .max_w(px(480.))
                    .width(px(preview_w))
                    .on_resize(cx.listener(|this, e: &nd::components::PaneResized, _, cx| {
                        this.preview_w = e.width;
                        this.save_session(cx);
                        cx.notify();
                    }))
                    .collapsed(!self.show_preview)
                    .on_collapse_change(cx.listener(|this, e: &PaneCollapsed, _, cx| {
                        this.show_preview = !e.collapsed;
                        this.save_session(cx);
                        cx.notify();
                    }))
                    .child(crate::preview::pane(&self.preview, self.show_preview, cx)),
            )
            // Over the whole window, so its scrim dims the sidebar and the toolbar too.
            .overlay(self.quicklook.clone());
        self.actions(root, cx)
    }
}

/// The tab a strip id names: `tab-<n>`, as [`Root::tab_bar`] mints them.
fn tab_id(strip_id: &str) -> Option<u64> {
    strip_id.strip_prefix("tab-")?.parse().ok()
}
