//! Quick Look: Space shows the selection large over the window. macOS's own panel when the
//! system has one, else a cover over the window with the same content as the preview pane,
//! videos and sounds playing. Left and Right step through the selection, or through the
//! folder (moving the selection with it) when one item is selected.

use std::time::Duration;

use nd::components::{VideoFailed, video};
use nd::prelude::*;
use nd::system::PreviewItem;
use trove_fs::FileKind;
use trove_preview::Preview as Content;

use crate::format::{self, Today};
use crate::keys::{CloseQuickLook, QUICKLOOK, QuickLookNext, QuickLookOpen, QuickLookPrevious};
use crate::preview::{self, Item, Job, Loaded, Loader, Scale, TextView};
use crate::pump::pump;
use crate::sound::{self, Sound};
use crate::tab::Tab;

/// Text Quick Look reads: more than the pane, still bounded.
const TEXT: usize = 256 * 1024;
const LINES: usize = 5000;
/// The card's largest size, and its footer's height.
const CARD: (f32, f32) = (1280., 900.);
const FOOTER: f32 = 38.;

pub struct QuickLook {
    tab: Option<WeakEntity<Tab>>,
    open: bool,
    /// The items stepped through when several were selected; empty to step through the folder.
    picked: Vec<Item>,
    index: usize,
    item: Option<Item>,
    loader: Loader,
    loaded: Loaded,
    text: Entity<TextView>,
    sound: Entity<Sound>,
    video_failed: bool,
    _sound: Subscription,
}

impl QuickLook {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let loader = Loader::new();
        pump(loader.rx.clone(), cx.weak_entity(), |this: &mut QuickLook, parts, cx| {
            this.receive(parts, cx);
            true
        });
        let sound = sound::get(cx);
        let watch = cx.observe(&sound, |_, _, cx| cx.notify());
        Self {
            tab: None,
            open: false,
            picked: Vec::new(),
            index: 0,
            item: None,
            loader,
            loaded: Loaded::default(),
            text: cx.new(TextView::new),
            sound,
            video_failed: false,
            _sound: watch,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn item(&self) -> Option<&Item> {
        self.item.as_ref()
    }

    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    pub fn toggle(&mut self, tab: &Entity<Tab>, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.show(tab, window, cx);
        }
    }

    fn show(&mut self, tab: &Entity<Tab>, window: &mut Window, cx: &mut Context<Self>) {
        let items = preview::selection(tab.read(cx));
        if items.is_empty() {
            return;
        }
        let native: Vec<PreviewItem> =
            items.iter().map(|i| PreviewItem { path: i.path.clone(), source: None }).collect();
        if nd::system::quick_look(window.id(), native, 0, cx) {
            return;
        }
        self.tab = Some(tab.downgrade());
        self.picked = if items.len() > 1 { items.clone() } else { Vec::new() };
        self.index = 0;
        self.open = true;
        self.load(items.into_iter().next(), cx);
        window.focus("quicklook");
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.item = None;
        self.loader.forget();
        self.loaded = Loaded::default();
        sound::stop_unless(None, cx);
        window.focus("files");
        cx.notify();
    }

    fn load(&mut self, item: Option<Item>, cx: &mut Context<Self>) {
        if self.item == item {
            return;
        }
        sound::stop_unless(None, cx);
        self.loaded = Loaded::default();
        self.video_failed = false;
        match &item {
            Some(i) => {
                self.loader.request(Job::One { path: i.path.clone(), max_text: TEXT, app: true });
            }
            None => {
                self.loader.forget();
            }
        }
        self.text.update(cx, |t, cx| t.clear(cx));
        self.item = item;
        cx.notify();
    }

    /// One item on: through the picked items, or the folder's next row.
    pub fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if !self.picked.is_empty() {
            let n = self.picked.len() as isize;
            self.index = (self.index as isize + by).rem_euclid(n) as usize;
            let item = self.picked.get(self.index).cloned();
            self.load(item, cx);
            return;
        }
        let Some(tab) = self.tab.as_ref().and_then(|t| t.upgrade()) else { return };
        tab.update(cx, |t, cx| t.move_cursor(by, false, cx));
        let item = preview::selection(tab.read(cx)).into_iter().next();
        if item.is_some() {
            self.load(item, cx);
        }
    }

    fn receive(&mut self, parts: Vec<preview::Part>, cx: &mut Context<Self>) {
        let current = self.loader.current();
        let mut changed = false;
        for part in parts {
            if let preview::Part::Content(g, c) = &part
                && *g == current
            {
                match c {
                    Content::Text { text, truncated, .. } => {
                        let (text, truncated) = (text.clone(), *truncated);
                        self.text.update(cx, |t, cx| t.set_text(&text, truncated, LINES, cx));
                    }
                    Content::Archive { entries, total } => {
                        let (entries, total) = (entries.clone(), *total);
                        self.text.update(cx, |t, cx| t.set_names(&entries, total, cx));
                    }
                    _ => {}
                }
                if self.item.as_ref().is_some_and(|i| i.kind == FileKind::Audio) {
                    let path = self.item.as_ref().map(|i| i.path.clone());
                    if let Some(path) = path
                        && matches!(c, Content::Audio { .. })
                    {
                        self.sound.update(cx, |s, cx| {
                            if s.state_of(&path).is_none() {
                                s.toggle(&path, cx);
                            }
                        });
                    }
                }
            }
            changed |= self.loaded.take(part, current);
        }
        if changed {
            cx.notify();
        }
    }

    /// Waits up to `timeout` for the shown item to load: the host's pump, for tests.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
        let deadline = std::time::Instant::now() + timeout;
        while self.item.is_some() && !self.loaded.gone && (self.loaded.content.is_none() || self.loaded.app.is_none()) {
            // nd-allow(clock-in-ui): as above
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(part) = self.loader.rx.recv_timeout(left) else { return };
            let mut parts = vec![part];
            parts.extend(self.loader.rx.try_iter());
            self.receive(parts, cx);
        }
    }

    fn open_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.item.clone() else { return };
        if item.is_dir {
            if let Some(tab) = self.tab.as_ref().and_then(|t| t.upgrade()) {
                tab.update(cx, |t, cx| t.navigate(crate::location::Location::Dir(item.path.clone()), window, cx));
            }
            self.close(window, cx);
            return;
        }
        crate::keys::open_file(&item.path, window, cx);
    }

    fn position(&self, cx: &App) -> Option<String> {
        if !self.picked.is_empty() {
            return Some(format!("{} of {}", self.index + 1, self.picked.len()));
        }
        let tab = self.tab.as_ref()?.upgrade()?;
        let tab = tab.read(cx);
        if tab.extras.results.shown() {
            return None;
        }
        let at = tab.target()?;
        let i = tab.visible().iter().position(|id| *id == at)?;
        Some(format!("{} of {}", format::group(i + 1), format::group(tab.visible().len())))
    }

    fn body(&mut self, item: &Item, room: (f32, f32), cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if item.kind == FileKind::Video && !self.video_failed && self.loaded.info.is_some() {
            let me = cx.weak_entity();
            return video(item.path.to_string_lossy().into_owned())
                .id("quicklook-video")
                .playing(true)
                .alt(item.name.clone())
                .on_error(move |_: &VideoFailed, _, cx| {
                    let _ = me.update(cx, |q, cx| {
                        q.video_failed = true;
                        cx.notify();
                    });
                })
                .size_full()
                .rounded(theme.radius.md)
                .into_any_element();
        }
        let sound = self.sound.read(cx).state_of(&item.path);
        let art =
            preview::art(item, &self.loaded, &self.text, sound, Scale::Large { width: room.0, height: room.1 }, cx);
        match &self.loaded.content {
            Some(Content::Audio { title, artist, album, .. }) => {
                let title = title.clone().unwrap_or_else(|| item.name.clone());
                let by: Vec<String> = [artist.clone(), album.clone()].into_iter().flatten().collect();
                h_flex()
                    .size_full()
                    .justify_center()
                    .items_center()
                    .gap(theme.spacing.xl)
                    .child(art)
                    .child(
                        v_flex()
                            .gap(theme.spacing.xs)
                            .max_w(px(360.))
                            .child(text(title).text_size(theme.type_scale.title).font_weight(FontWeight::SEMIBOLD))
                            .children(
                                by.into_iter().map(|l| text(l).text_color(theme.text.secondary).into_any_element()),
                            ),
                    )
                    .into_any_element()
            }
            Some(Content::Folder { items, .. }) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(theme.spacing.md)
                .child(icon(Icon::Folder, 160.).text_color(theme.accent))
                .child(text(format::items(*items as usize)).text_color(theme.text.secondary))
                .into_any_element(),
            _ => art,
        }
    }
}

/// One line of facts under the content.
fn facts(item: &Item, loaded: &Loaded) -> String {
    let today = Today::now();
    let mut parts = vec![item.kind.label().to_owned()];
    if let Some(info) = &loaded.info {
        if !info.is_dir {
            parts.push(format::size(info.size));
        }
        if let Some((w, h)) = info.dimensions {
            parts.push(format!("{w} \u{d7} {h}"));
        }
        if let Some(Content::Audio { duration: Some(d), .. }) = &loaded.content {
            parts.push(preview::duration_text(*d));
        }
        if let Some(t) = info.modified {
            parts.push(format!("Modified {}", today.long(t)));
        }
    }
    parts.join(" \u{b7} ")
}

impl Render for QuickLook {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (true, Some(item)) = (self.open, self.item.clone()) else {
            return div().id("quicklook-anchor").into_any_element();
        };
        let room = {
            let theme = cx.theme();
            let viewport = window.viewport_size();
            let edge = 2. * f32::from(theme.spacing.xl);
            let inset = 2. * f32::from(theme.spacing.lg);
            let card = ((viewport.width - edge).min(CARD.0), (viewport.height - edge).min(CARD.1));
            (card.0 - inset, card.1 - f32::from(theme.control.large) - FOOTER - inset)
        };
        let body = self.body(&item, room, cx);
        let theme = cx.theme();
        let me = cx.weak_entity();
        let open_label = match &self.loaded.app {
            _ if item.is_dir => "Open".to_owned(),
            Some(Some(app)) => format!("Open with {app}"),
            _ => "Open".to_owned(),
        };
        let position = self.position(cx);
        let (close, dismiss) = (me.clone(), me.clone());
        let header =
            h_flex()
                .id("quicklook-header")
                .w_full()
                .h(theme.control.large)
                .px(theme.spacing.sm)
                .gap(theme.spacing.sm)
                .items_center()
                .child(icon_button("quicklook-close", Icon::X).tooltip("Close").on_click(
                    move |_: &ClickEvent, window, cx| {
                        let _ = close.update(cx, |q, cx| q.close(window, cx));
                    },
                ))
                .child(
                    div().flex_1().min_w_0().flex().justify_center().child(
                        text(item.name.clone()).id("quicklook-title").font_weight(FontWeight::SEMIBOLD).truncate(),
                    ),
                )
                .child(button("quicklook-open", open_label).dispatch(QuickLookOpen));
        let footer = h_flex()
            .w_full()
            .px(theme.spacing.md)
            .py(theme.spacing.sm)
            .gap(theme.spacing.md)
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    text(facts(&item, &self.loaded))
                        .id("quicklook-facts")
                        .text_size(theme.type_scale.footnote)
                        .text_color(theme.text.secondary)
                        .tabular_nums()
                        .truncate(),
                ),
            )
            .children(position.map(|p| {
                text(p)
                    .id("quicklook-position")
                    .text_size(theme.type_scale.footnote)
                    .text_color(theme.text.secondary)
                    .tabular_nums()
                    .into_any_element()
            }));
        let card = v_flex()
            .id("quicklook")
            .key_context(QUICKLOOK)
            .focusable()
            .size_full()
            .max_w(px(CARD.0))
            .max_h(px(CARD.1))
            .bg(theme.surface.raised)
            .rounded(theme.radius.xl)
            .border_1()
            .border_color(theme.border.subtle)
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &CloseQuickLook, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &QuickLookNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &QuickLookPrevious, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &QuickLookOpen, window, cx| this.open_item(window, cx)))
            .child(header)
            .child(separator())
            .child(div().flex_1().min_h_0().w_full().p(theme.spacing.lg).child(body))
            .child(separator())
            .child(footer);
        div()
            .id("quicklook-anchor")
            .child(
                cover("quicklook-cover")
                    .modality(Modality::LightDismiss)
                    .on_dismiss(move |_: &Dismissed, window, cx| {
                        let _ = dismiss.update(cx, |q, cx| q.close(window, cx));
                    })
                    .child(div().size_full().flex().items_center().justify_center().p(theme.spacing.xl).child(card)),
            )
            .into_any_element()
    }
}
