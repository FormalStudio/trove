//! The window: toolbar, tab strip, sidebar, the active tab's content, the status bar and the
//! preview pane. It owns the tabs and routes every command to the active one; each piece that
//! changes on its own (sidebar, path bar, tab content, status, preview) is its own view.

use std::path::PathBuf;

use nd::core::WindowId;
use nd::prelude::*;

use crate::actions::*;
use crate::icons;
use crate::location::Location;
use crate::pathbar::PathBar;
use crate::preview::Preview;
use crate::sidebar::Sidebar;
use crate::status::StatusBar;
use crate::tab::{Tab, View};

/// Where the first tab opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    Home,
    /// A path from the command line: a folder opens, a file opens its folder with the file
    /// selected. Which one it is shows when the listing answers, never by a stat up front.
    Path(PathBuf),
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
    show_preview: bool,
    show_hidden: bool,
    chrome: Option<Chrome>,
    titles: Vec<String>,
    _watch: Option<Subscription>,
    _volumes: Subscription,
}

impl Root {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            status: cx.new(|_| StatusBar::new()),
            preview: cx.new(|_| Preview::new()),
            show_preview: false,
            show_hidden: false,
            chrome: None,
            titles: Vec::new(),
            _watch: None,
            _volumes: volumes,
        };
        let (location, file_check) = match start {
            Start::Home => (home, false),
            Start::Path(p) => (Location::Dir(p), true),
        };
        let tab = this.make_tab(location, View::List, cx);
        if file_check {
            tab.update(cx, |t, _| t.open_parent_if_file());
        }
        let id = this.take_id();
        this.tabs.push((id, tab));
        this.activate(0, window, cx);
        this
    }

    // For tests and tooling.

    pub fn tabs(&self) -> Vec<Entity<Tab>> {
        self.tabs.iter().map(|(_, t)| t.clone()).collect()
    }

    pub fn active_tab(&self) -> &Entity<Tab> {
        &self.tabs[self.active].1
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

    /// Applies everything each tab's listing has sent, waiting up to `timeout` per tab: the
    /// host's pump, for tests and the appearance audit, which run none.
    pub fn drain(&mut self, timeout: std::time::Duration, cx: &mut Context<Self>) {
        for (_, tab) in &self.tabs {
            tab.update(cx, |t, cx| t.drain(timeout, cx));
        }
    }

    // Tabs.

    fn take_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn make_tab(&mut self, location: Location, view: View, cx: &mut Context<Self>) -> Entity<Tab> {
        let show_hidden = self.show_hidden;
        let tab = cx.new(|cx| Tab::new(location, show_hidden, view, cx));
        cx.observe(&tab, |this: &mut Root, _, cx| this.refresh_titles(cx)).detach();
        tab
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index.min(self.tabs.len() - 1);
        let tab = self.active_tab().clone();
        self.status.update(cx, |s, cx| s.set_tab(&tab, cx));
        self.preview.update(cx, |p, cx| p.set_tab(&tab, cx));
        self._watch = Some(cx.observe(&tab, |this: &mut Root, _, cx| this.sync_chrome(cx)));
        self.chrome = None;
        self.sync_chrome(cx);
        window.focus("files");
        cx.notify();
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
        cx.set_window_title(self.window, chrome.title.clone());
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
        self.tabs.remove(index);
        let next = if index < self.active || self.active >= self.tabs.len() {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        self.refresh_titles(cx);
        self.activate(next, window, cx);
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        let next = (self.active as isize + step).rem_euclid(n) as usize;
        self.activate(next, window, cx);
    }

    // Navigation, always in the active tab.

    fn go(&mut self, location: Location, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab().clone().update(cx, |t, cx| t.navigate(location, window, cx));
        window.focus("files");
    }

    fn with_tab(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Tab, &mut Context<Tab>)) {
        self.active_tab().clone().update(cx, f);
    }

    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
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

    fn toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let show = self.show_hidden;
        for (_, tab) in &self.tabs {
            tab.update(cx, |t, cx| t.set_show_hidden(show, cx));
        }
    }

    fn view_switch(&self) -> impl IntoElement + use<> {
        let view = self.chrome.as_ref().map_or(View::List, |c| c.view);
        button_group()
            .child(
                icon_button("view-list", icons::list_view())
                    .checked(view == View::List)
                    .tooltip("List")
                    .dispatch(ListView),
            )
            .child(
                icon_button("view-grid", icons::grid_view())
                    .checked(view == View::Grid)
                    .tooltip("Grid")
                    .dispatch(GridView),
            )
    }

    fn tab_strip(&self) -> TabStrip {
        let active = format!("tab-{}", self.tabs[self.active].0);
        let specs = self.tabs.iter().zip(&self.titles).map(|((id, _), title)| nd::components::TabSpec {
            id: format!("tab-{id}").into(),
            title: title.clone().into(),
            icon: None,
            closable: true,
        });
        let (select, close, add) = (self.me(), self.me(), self.me());
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
            .on_add(move |_: &TabAddRequested, window, cx| {
                let _ = add.update(cx, |this, cx| {
                    let here = this.active_tab().read(cx).location().clone();
                    this.new_tab(here, true, window, cx);
                });
            })
    }

    fn me(&self) -> WeakEntity<Root> {
        self.weak.clone()
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|(n, _)| format!("tab-{n}") == id)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Toolbar {
        let chrome = self.chrome.clone();
        let (can_back, can_forward) = chrome.as_ref().map_or((false, false), |c| (c.can_back, c.can_forward));
        let title = chrome.map(|c| c.title).unwrap_or_default();
        let search = self.active_tab().read(cx).search().clone();
        toolbar()
            .hide_title()
            .navigation(icon_button("back", Icon::ChevronLeft).tooltip("Back").disabled(!can_back).dispatch(Back))
            .navigation(
                icon_button("forward", Icon::ChevronRight).tooltip("Forward").disabled(!can_forward).dispatch(Forward),
            )
            .item(self.pathbar.clone())
            .flexible_space()
            .item(self.view_switch())
            .item(
                search_field("search", &search).placeholder(format!("Search {title}")).key_context(SEARCH).w(px(220.)),
            )
            .item(
                icon_button("preview-toggle", Icon::PanelRight)
                    .tooltip("Preview")
                    .checked(self.show_preview)
                    .dispatch(TogglePreview),
            )
    }

    fn actions(&self, root: Scaffold, cx: &mut Context<Self>) -> Scaffold {
        root.on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::open_in_new_tab))
            .on_action(cx.listener(Self::enter_folder))
            .on_action(cx.listener(Self::toggle_hidden))
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                this.with_tab(cx, |t, cx| t.go_back(window, cx));
                window.focus("files");
            }))
            .on_action(cx.listener(|this, _: &Forward, window, cx| {
                this.with_tab(cx, |t, cx| t.go_forward(window, cx));
                window.focus("files");
            }))
            .on_action(cx.listener(|this, _: &Parent, window, cx| this.with_tab(cx, |t, cx| t.go_up(window, cx))))
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.with_tab(cx, |t, cx| t.reload(cx))))
            .on_action(
                cx.listener(|this, GoTo(path): &GoTo, window, cx| this.go(Location::Dir(path.clone()), window, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.with_tab(cx, |t, cx| t.select_all(cx))))
            .on_action(cx.listener(|this, _: &Deselect, window, cx| this.with_tab(cx, |t, cx| t.deselect(window, cx))))
            .on_action(cx.listener(|this, _: &ListView, _, cx| this.with_tab(cx, |t, cx| t.set_view(View::List, cx))))
            .on_action(cx.listener(|this, _: &GridView, _, cx| this.with_tab(cx, |t, cx| t.set_view(View::Grid, cx))))
            .on_action(cx.listener(|this, _: &TogglePreview, _, cx| {
                this.show_preview = !this.show_preview;
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
                window.focus("files");
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
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.step(Step::Rows(-1), false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.step(Step::Rows(1), false, cx)))
            .on_action(cx.listener(|this, _: &ExtendUp, _, cx| this.step(Step::Rows(-1), true, cx)))
            .on_action(cx.listener(|this, _: &ExtendDown, _, cx| this.step(Step::Rows(1), true, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.step(Step::Cells(-1), false, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.step(Step::Cells(1), false, cx)))
            .on_action(cx.listener(|this, _: &MoveHome, _, cx| this.with_tab(cx, |t, cx| t.move_to_end(false, cx))))
            .on_action(cx.listener(|this, _: &MoveEnd, _, cx| this.with_tab(cx, |t, cx| t.move_to_end(true, cx))))
            .on_action(cx.listener(|this, TypeSelect(c): &TypeSelect, window, cx| {
                let now = window.now();
                this.with_tab(cx, |t, cx| t.type_select(*c, now, cx));
            }))
    }

    /// Up and Down move a row of the list or a row of tiles; Left and Right move one tile and
    /// do nothing in the list.
    fn step(&mut self, step: Step, extend: bool, cx: &mut Context<Self>) {
        self.with_tab(cx, |t, cx| {
            let by = match (step, t.view()) {
                (Step::Rows(n), View::List) => n,
                (Step::Rows(n), View::Grid) => n * t.lanes() as isize,
                (Step::Cells(n), View::Grid) => n,
                (Step::Cells(_), View::List) => return,
            };
            t.move_cursor(by, extend, cx);
        });
    }
}

#[derive(Clone, Copy)]
enum Step {
    Rows(isize),
    Cells(isize),
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace::first_render();
        let tabs = (self.tabs.len() > 1).then(|| self.tab_strip());
        let content = v_flex()
            .size_full()
            .children(tabs)
            .child(div().flex_1().min_h_0().child(self.active_tab().clone()))
            .child(self.status.clone());
        let root = scaffold()
            .key_context(WINDOW)
            .toolbar(self.toolbar(cx))
            .sidebar(sidebar().width(px(208.)).child(self.sidebar.clone()))
            .content(content)
            .inspector(
                inspector()
                    .width(px(260.))
                    .collapsed(!self.show_preview)
                    .on_collapse_change(cx.listener(|this, e: &PaneCollapsed, _, cx| {
                        this.show_preview = !e.collapsed;
                        cx.notify();
                    }))
                    .child(self.preview.clone()),
            );
        self.actions(root, cx)
    }
}
