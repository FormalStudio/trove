//! The preview pane: the selection's contents (picture, text, folder summary, archive listing,
//! tags, thumbnail) over Finder's information rows. Contents and info load on a worker after
//! the selection rests for a moment, so arrowing through a folder starts no work at all; a
//! newer selection makes every older answer stale. The same loader and drawing serve Quick
//! Look and the columns view's last column.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nd::prelude::*;
use trove_fs::FileKind;
use trove_preview::{Info, Preview as Content};

use crate::format::{self, Today};
use crate::icons;
use crate::pump::pump;
use crate::sound::{self, Sound};
use crate::tab::Tab;

/// How long the selection has to rest before its preview loads.
pub const DEBOUNCE: Duration = Duration::from_millis(60);
/// Text the pane reads from a file: a screenful of code, not the whole file.
const PANE_TEXT: usize = 32 * 1024;
const PANE_LINES: usize = 200;

// The worker.

#[derive(Clone, Debug)]
pub enum Job {
    One {
        path: PathBuf,
        max_text: usize,
        app: bool,
    },
    /// The total apparent size of files picked together (folders count nothing).
    Total(Vec<PathBuf>),
}

#[derive(Debug)]
pub enum Part {
    Info(u64, Info),
    Content(u64, Content),
    /// The default app's name, for Quick Look's "Open with".
    App(u64, Option<String>),
    Total(u64, u64),
    /// The path could not be stat'd.
    Gone(u64),
}

impl Part {
    fn generation(&self) -> u64 {
        match self {
            Part::Info(g, _) | Part::Content(g, _) | Part::App(g, _) | Part::Total(g, _) | Part::Gone(g) => *g,
        }
    }
}

/// A debounced preview worker: only the newest request that rests for [`DEBOUNCE`] runs, each
/// on a thread of its own, so a slow thumbnailer never holds up the next file.
pub struct Loader {
    tx: flume::Sender<(u64, Job)>,
    pub rx: flume::Receiver<Part>,
    generation: Arc<AtomicU64>,
}

impl Loader {
    pub fn new() -> Self {
        let (tx, jobs) = flume::unbounded::<(u64, Job)>();
        let (out, rx) = flume::unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let _ = std::thread::Builder::new().name("trove-preview-debounce".into()).spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                loop {
                    match jobs.recv_timeout(DEBOUNCE) {
                        Ok(newer) => job = newer,
                        Err(flume::RecvTimeoutError::Timeout) => break,
                        Err(flume::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if job.0 != current.load(Ordering::Relaxed) {
                    continue;
                }
                let (out, current) = (out.clone(), current.clone());
                let _ = std::thread::Builder::new()
                    .name("trove-preview".into())
                    .spawn(move || run(job.0, job.1, &out, &current));
            }
        });
        Self { tx, rx, generation }
    }

    /// Asks for `job`, making every earlier answer stale; returns its generation.
    pub fn request(&self, job: Job) -> u64 {
        let g = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send((g, job));
        g
    }

    /// Makes every answer in flight stale without asking for anything.
    pub fn forget(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn current(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

fn run(g: u64, job: Job, out: &flume::Sender<Part>, current: &AtomicU64) {
    let stale = || current.load(Ordering::Relaxed) != g;
    match job {
        Job::One { path, max_text, app } => {
            let Some(info) = trove_preview::info_blocking(&path) else {
                let _ = out.send(Part::Gone(g));
                return;
            };
            let mime = info.mime.clone();
            let _ = out.send(Part::Info(g, info));
            if stale() {
                return;
            }
            let content = crate::thumbs::engine().preview(&path, max_text);
            let _ = out.send(Part::Content(g, content));
            if app && !stale() {
                let name = trove_ops::apps_for(&mime).into_iter().next().map(|a| a.name);
                let _ = out.send(Part::App(g, name));
            }
        }
        Job::Total(paths) => {
            let total =
                paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| !m.is_dir()).map(|m| m.len()).sum();
            let _ = out.send(Part::Total(g, total));
        }
    }
}

// What is shown.

/// The item a preview is about, from what the listing already knows: enough to draw the title
/// and the icon before anything loads.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
    pub is_dir: bool,
    /// The folder it sits in, shown for search results and Recent, where it is not the tab's.
    pub place: Option<String>,
}

impl Item {
    pub fn of(path: PathBuf, is_dir: bool, place: Option<String>) -> Self {
        let name =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let kind = if is_dir { FileKind::Folder } else { FileKind::from_name(&name) };
        Self { path, name, kind, is_dir, place }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Subject {
    /// Nothing selected: the folder itself.
    Folder {
        title: String,
        items: usize,
    },
    One(Item),
    Many(Vec<PathBuf>),
}

/// What loaded for the subject so far.
#[derive(Default)]
pub struct Loaded {
    pub info: Option<Info>,
    pub content: Option<Content>,
    pub app: Option<Option<String>>,
    pub total: Option<u64>,
    pub gone: bool,
}

impl Loaded {
    /// Takes one part in; false when it belongs to an older request.
    pub fn take(&mut self, part: Part, current: u64) -> bool {
        if part.generation() != current {
            return false;
        }
        match part {
            Part::Info(_, i) => self.info = Some(i),
            Part::Content(_, c) => self.content = Some(c),
            Part::App(_, a) => self.app = Some(a),
            Part::Total(_, t) => self.total = Some(t),
            Part::Gone(_) => self.gone = true,
        }
        true
    }
}

/// Where `path`'s folder is, the way the results list says it: relative to Home.
pub fn place_of(path: &Path, home: &Path) -> String {
    let Some(dir) = path.parent() else { return String::new() };
    match dir.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "Home".to_owned(),
        Ok(rel) => rel.display().to_string(),
        Err(_) => dir.display().to_string(),
    }
}

/// The selection of `tab` as preview items, in order.
pub fn selection(tab: &Tab) -> Vec<Item> {
    let place = tab.extras.results.shown() || tab.location() == &crate::location::Location::Recent;
    let home = &crate::env::get().home;
    tab.selected()
        .iter()
        .filter_map(|id| {
            let path = tab.path_of(id)?;
            let place = place.then(|| place_of(&path, home));
            Some(Item::of(path, tab.is_folder(id), place))
        })
        .collect()
}

// The pane.

pub struct Preview {
    tab: Option<WeakEntity<Tab>>,
    subject: Option<Subject>,
    loader: Loader,
    loaded: Loaded,
    text: Entity<TextView>,
    sound: Entity<Sound>,
    visible: bool,
    _watch: Option<Subscription>,
    _sound: Subscription,
}

impl Preview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let loader = Loader::new();
        pump(loader.rx.clone(), cx.weak_entity(), |this: &mut Preview, parts, cx| {
            this.receive(parts, cx);
            true
        });
        let sound = sound::get(cx);
        let watch = cx.observe(&sound, |_, _, cx| cx.notify());
        Self {
            tab: None,
            subject: None,
            loader,
            loaded: Loaded::default(),
            text: cx.new(TextView::new),
            sound,
            visible: true,
            _watch: None,
            _sound: watch,
        }
    }

    pub fn set_tab(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        self._watch = Some(cx.observe(tab, |this, tab, cx| this.follow(&tab, cx)));
        self.tab = Some(tab.downgrade());
        self.subject = None;
        self.follow(tab, cx);
    }

    /// The pane only loads while it shows.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible && let Some(tab) = self.tab.as_ref().and_then(|t| t.upgrade()) {
            self.subject = None;
            self.follow(&tab, cx);
        }
    }

    /// The title it shows, for tests.
    pub fn title(&self) -> Option<String> {
        match self.subject.as_ref()? {
            Subject::Folder { title, .. } => Some(title.clone()),
            Subject::One(item) => Some(item.name.clone()),
            Subject::Many(paths) => Some(format::items(paths.len())),
        }
    }

    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    fn follow(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let subject = {
            let t = tab.read(cx);
            let mut items = selection(t);
            match items.len() {
                0 => Subject::Folder { title: t.title(), items: t.visible().len() },
                1 => Subject::One(items.remove(0)),
                _ => Subject::Many(items.into_iter().map(|i| i.path).collect()),
            }
        };
        if self.subject.as_ref() != Some(&subject) {
            self.set_subject(Some(subject), cx);
        }
    }

    /// Shows `item` with no tab to follow: the columns view's last column.
    pub fn show(&mut self, item: Option<Item>, cx: &mut Context<Self>) {
        let subject = item.map(Subject::One);
        if self.subject != subject {
            self.set_subject(subject, cx);
        }
    }

    fn set_subject(&mut self, subject: Option<Subject>, cx: &mut Context<Self>) {
        self.loaded = Loaded::default();
        match &subject {
            Some(Subject::One(item)) => {
                self.loader.request(Job::One { path: item.path.clone(), max_text: PANE_TEXT, app: false });
                crate::sound::stop_unless(Some(&item.path), cx);
            }
            Some(Subject::Many(paths)) => {
                self.loader.request(Job::Total(paths.clone()));
            }
            Some(Subject::Folder { .. }) | None => {
                self.loader.forget();
            }
        }
        self.text.update(cx, |t, cx| t.clear(cx));
        self.subject = subject;
        cx.notify();
    }

    fn receive(&mut self, parts: Vec<Part>, cx: &mut Context<Self>) {
        let current = self.loader.current();
        let mut changed = false;
        for part in parts {
            if let Part::Content(g, Content::Text { text, truncated, .. }) = &part
                && *g == current
            {
                let (text, truncated) = (text.clone(), *truncated);
                self.text.update(cx, |t, cx| t.set_text(&text, truncated, PANE_LINES, cx));
            }
            if let Part::Content(g, Content::Archive { entries, total }) = &part
                && *g == current
            {
                let (entries, total) = (entries.clone(), *total);
                self.text.update(cx, |t, cx| t.set_names(&entries, total, cx));
            }
            changed |= self.loaded.take(part, current);
        }
        if changed {
            cx.notify();
        }
    }

    /// Waits up to `timeout` for the current subject to load: the host's pump, for tests.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let done = match &self.subject {
                Some(Subject::One(_)) => self.loaded.gone || self.loaded.content.is_some(),
                Some(Subject::Many(_)) => self.loaded.total.is_some(),
                _ => true,
            };
            if done {
                return;
            }
            // nd-allow(clock-in-ui): as above
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(part) = self.loader.rx.recv_timeout(left) else { return };
            let mut parts = vec![part];
            parts.extend(self.loader.rx.try_iter());
            self.receive(parts, cx);
        }
    }
}

/// The pane element, told whether the inspector shows so a hidden pane loads nothing.
pub fn pane(preview: &Entity<Preview>, visible: bool, cx: &mut App) -> Entity<Preview> {
    preview.update(cx, |p, cx| p.set_visible(visible, cx));
    preview.clone()
}

impl Render for Preview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(subject) = self.subject.clone() else { return v_flex().id("preview").size_full() };
        let today = Today::now();
        let body = match &subject {
            Subject::Folder { title, items } => v_flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(art_box(px(PANE_ART)).child(icon(Icon::Folder, 96.).text_color(theme.accent)))
                .child(title_text(title.clone()))
                .child(quiet(format::items(*items), theme)),
            Subject::Many(paths) => {
                let mut lines = vec![format::items(paths.len())];
                if let Some(total) = self.loaded.total {
                    lines.push(format::size(total));
                }
                v_flex()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .child(art_box(px(PANE_ART)).child(icon(Icon::Copy, 80.).text_color(theme.text.secondary)))
                    .child(title_text(lines.remove(0)))
                    .children(lines.into_iter().map(|l| quiet(l, theme).into_any_element()))
            }
            Subject::One(item) => {
                let sound = self.sound.read(cx).state_of(&item.path);
                let art = art(item, &self.loaded, &self.text, sound, Scale::Pane, cx);
                v_flex()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .child(art)
                    .child(title_text(item.name.clone()))
                    .child(quiet(subtitle(item, &self.loaded), theme))
                    .child(info_section(item, &self.loaded, &today, cx))
            }
        };
        v_flex().id("preview").size_full().overflow_y_scroll().p(theme.spacing.lg).pt(theme.spacing.xl).child(body)
    }
}

// Drawing, shared with Quick Look and the columns view.

/// The pane's picture box height.
pub const PANE_ART: f32 = 190.;

/// The inspector's width, which nd does not let the user change.
pub const PANE_W: f32 = 260.;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scale {
    Pane,
    /// Quick Look, with the room its body has.
    Large {
        width: f32,
        height: f32,
    },
}

/// The size `natural` is drawn at inside `room`: whole, never enlarged.
// nd: ImageFit::Contain still fills the box on GPUI, so the box takes the picture's shape.
fn fitted(natural: (u32, u32), room: (f32, f32)) -> (f32, f32) {
    let (w, h) = (natural.0 as f32, natural.1 as f32);
    let s = (room.0 / w).min(room.1 / h).min(1.);
    ((w * s).max(1.), (h * s).max(1.))
}

pub fn art_box(height: Pixels) -> Div {
    div().w_full().h(height).flex().items_center().justify_center()
}

pub fn title_text(s: String) -> Text {
    text(s).id("preview-title").font_weight(FontWeight::SEMIBOLD).text_center().line_clamp(3)
}

fn quiet(s: String, theme: &Theme) -> Text {
    text(s).text_size(theme.type_scale.footnote).text_color(theme.text.secondary).text_center().tabular_nums()
}

/// "Image · 1.2 MB" under the name.
pub fn subtitle(item: &Item, loaded: &Loaded) -> String {
    let kind = item.kind.label().to_owned();
    match (&loaded.content, &loaded.info) {
        (Some(Content::Folder { items, .. }), _) => format!("{kind} \u{b7} {}", format::items(*items as usize)),
        (_, Some(info)) if !info.is_dir => format!("{kind} \u{b7} {}", format::size(info.size)),
        _ => kind,
    }
}

/// The picture area: the file's own picture, text, listing, cover or icon.
pub fn art(
    item: &Item,
    loaded: &Loaded,
    text: &Entity<TextView>,
    sound: Option<bool>,
    scale: Scale,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let large = matches!(scale, Scale::Large { .. });
    let height = px(if large { 0. } else { PANE_ART });
    let room = match scale {
        Scale::Pane => (PANE_W - 2. * f32::from(theme.spacing.lg), PANE_ART),
        Scale::Large { width, height } => (width, height),
    };
    let natural = loaded.info.as_ref().and_then(|i| i.dimensions);
    let boxed = |child: AnyElement| -> AnyElement {
        if large {
            div().size_full().flex().items_center().justify_center().child(child).into_any_element()
        } else {
            art_box(height).child(child).into_any_element()
        }
    };
    let glyph = |size: f32| -> AnyElement {
        icon(icons::for_kind(item.kind), size).text_color(icons::tint(item.kind, theme)).into_any_element()
    };
    let picture = |path: &Path, hint: Option<(u32, u32)>| -> AnyElement {
        let mut img = image(ImageSource::path(path)).contain().alt(item.name.clone());
        let dims = hint.or(natural).filter(|(w, h)| *w > 0 && *h > 0);
        if let Some((w, h)) = dims {
            img = img.size_hint(w, h);
        }
        let img = match dims {
            Some(dims) => {
                let (w, h) = fitted(dims, room);
                img.w(px(w)).h(px(h))
            }
            None if large => img.size_full(),
            None => img.w_full().h(height),
        };
        img.rounded(theme.radius.sm).into_any_element()
    };
    match &loaded.content {
        Some(Content::Image { path, width, height: h }) => boxed(picture(path, Some((*width, *h)))),
        Some(Content::Thumbnail { path }) => boxed(picture(path, None)),
        Some(Content::Text { .. } | Content::Archive { .. }) => {
            let h = if large { None } else { Some(px(PANE_ART + 70.)) };
            div()
                .w_full()
                .when_some(h, |d, h| d.h(h))
                .when(large, |d| d.size_full())
                .rounded(theme.radius.md)
                .border_1()
                .border_color(theme.border.subtle)
                .bg(theme.surface.base)
                .overflow_hidden()
                .child(text.clone())
                .into_any_element()
        }
        Some(Content::Audio { cover, .. }) => {
            let size = if large { 320. } else { PANE_ART };
            let cover_art = match cover {
                Some(c) => {
                    image(ImageSource::path(c)).w(px(size)).h(px(size)).rounded(theme.radius.md).into_any_element()
                }
                None => div()
                    .w(px(size))
                    .h(px(size))
                    .rounded(theme.radius.md)
                    .bg(theme.surface.raised)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(Icon::Music, size * 0.4).text_color(theme.text.secondary))
                    .into_any_element(),
            };
            let play = sound::play_button(&item.path, sound.unwrap_or(false));
            let column = v_flex().items_center().gap(theme.spacing.md).child(cover_art).child(play);
            if large {
                column.into_any_element()
            } else {
                div().w_full().flex().justify_center().child(column).into_any_element()
            }
        }
        Some(Content::Folder { .. }) => {
            boxed(icon(Icon::Folder, if large { 160. } else { 96. }).text_color(theme.accent).into_any_element())
        }
        _ => boxed(glyph(if large { 160. } else { 96. })),
    }
}

/// Finder's information rows.
pub fn info_rows(item: &Item, loaded: &Loaded, today: &Today) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    rows.push(("Kind", item.kind.label().to_owned()));
    if let Some(info) = &loaded.info {
        if !info.is_dir {
            rows.push(("Size", size_long(info.size)));
        } else if let Some(Content::Folder { items, .. }) = &loaded.content {
            rows.push(("Contains", format::items(*items as usize)));
        }
        if let Some(Content::Audio { title, artist, album, duration, .. }) = &loaded.content {
            for (label, value) in [("Title", title), ("Artist", artist), ("Album", album)] {
                if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
                    rows.push((label, v.clone()));
                }
            }
            if let Some(d) = duration {
                rows.push(("Duration", duration_text(*d)));
            }
        }
        if let Some((w, h)) = info.dimensions {
            rows.push(("Dimensions", format!("{w} \u{d7} {h}")));
        }
        if let Some(t) = info.created {
            rows.push(("Created", today.long(t)));
        }
        if let Some(t) = info.modified {
            rows.push(("Modified", today.long(t)));
        }
        rows.push(("Permissions", info.mode.clone()));
        let owner = if info.group.is_empty() || info.group == info.owner {
            info.owner.clone()
        } else {
            format!("{} ({})", info.owner, info.group)
        };
        rows.push(("Owner", owner));
        if let Some(target) = &info.link_target {
            rows.push(("Points to", target.display().to_string()));
        }
    }
    if let Some(place) = &item.place {
        rows.push(("Where", place.clone()));
    }
    rows
}

pub fn info_section(item: &Item, loaded: &Loaded, today: &Today, cx: &App) -> Div {
    let theme = cx.theme();
    let rows = info_rows(item, loaded, today);
    v_flex()
        .id("preview-info")
        .w_full()
        .pt(theme.spacing.md)
        .gap(theme.spacing.xs)
        .child(
            text("Information")
                .text_size(theme.type_scale.footnote)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text.secondary),
        )
        .child(separator())
        .children(rows.into_iter().map(|(label, value)| {
            let mono = label == "Permissions";
            h_flex()
                .w_full()
                .gap(theme.spacing.sm)
                .items_start()
                .child(
                    text(label)
                        .text_size(theme.type_scale.footnote)
                        .text_color(theme.text.secondary)
                        .flex_none()
                        .w(px(72.)),
                )
                .child(
                    div().flex_1().min_w_0().flex().justify_end().child(
                        text(value)
                            .text_size(theme.type_scale.footnote)
                            .tabular_nums()
                            .text_right()
                            .line_clamp(3)
                            .when(mono, |t| t.font_family(theme.fonts.mono.family.clone())),
                    ),
                )
                .into_any_element()
        }))
}

/// "1.2 MB (1 234 567 bytes)", Finder's size row.
fn size_long(bytes: u64) -> String {
    if bytes < 1000 {
        format::size(bytes)
    } else {
        format!("{} ({} bytes)", format::size(bytes), format::group(bytes as usize))
    }
}

pub fn duration_text(seconds: f64) -> String {
    let s = seconds.max(0.).round() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// Text and archive listings: a virtual list of lines.

pub struct TextView {
    lines: Vec<SharedString>,
    numbered: bool,
    note: Option<String>,
    list: ListState<u32>,
    generation: u64,
}

impl TextView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            lines: Vec::new(),
            numbered: true,
            note: None,
            list: ListState::new(cx).estimated_row_height(px(LINE_HEIGHT)),
            generation: 0,
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.lines.is_empty() && self.note.is_none() {
            return;
        }
        self.lines.clear();
        self.note = None;
        self.sync(cx);
    }

    /// At most `max_lines` lines of `text`, numbered; the note says when there was more.
    pub fn set_text(&mut self, text: &str, truncated: bool, max_lines: usize, cx: &mut Context<Self>) {
        let mut lines: Vec<SharedString> =
            text.lines().take(max_lines).map(|l| SharedString::from(l.replace('\t', "    "))).collect();
        if lines.is_empty() {
            lines.push(SharedString::default());
        }
        let cut = truncated || text.lines().nth(max_lines).is_some();
        self.note = cut.then(|| format!("Showing the first {} lines", format::group(lines.len())));
        self.lines = lines;
        self.numbered = true;
        self.sync(cx);
    }

    pub fn set_names(&mut self, names: &[String], total: usize, cx: &mut Context<Self>) {
        self.lines = names.iter().map(|n| SharedString::from(n.clone())).collect();
        self.numbered = false;
        self.note = (total > names.len()).then(|| format!("{} of {} items", names.len(), format::group(total)));
        self.sync(cx);
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let g = self.generation;
        self.list.set_items_versioned((0..self.lines.len() as u32).map(|i| (i, g)), cx);
        self.list.scroll_to_top(cx);
        cx.notify();
    }

    fn line(&mut self, i: u32, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(line) = self.lines.get(i as usize).cloned() else { return div().into_any_element() };
        let mono = theme.fonts.mono.family.clone();
        let digits = self.lines.len().to_string().len().max(2) as f32;
        h_flex()
            .h(px(LINE_HEIGHT))
            .px(theme.spacing.sm)
            .gap(theme.spacing.sm)
            .items_center()
            .when(self.numbered, |row| {
                row.child(
                    text((i + 1).to_string())
                        .text_size(theme.type_scale.caption)
                        .text_color(theme.text.tertiary)
                        .font_family(mono.clone())
                        .text_right()
                        .flex_none()
                        .w(px(digits * 7.5)),
                )
            })
            .when(!self.numbered, |row| {
                row.child(icon(icons::for_kind(FileKind::from_name(&line)), 12.).text_color(theme.text.secondary))
            })
            .child(text(line).text_size(theme.type_scale.caption).font_family(mono).truncate().flex_1().min_w_0())
            .into_any_element()
    }
}

const LINE_HEIGHT: f32 = 18.;

impl Render for TextView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let note = self.note.clone();
        v_flex()
            .id("preview-text")
            .size_full()
            .py(theme.spacing.xs)
            .child(div().flex_1().min_h_0().child(list("preview-lines", &self.list, cx.processor(Self::line))))
            .children(note.map(|n| {
                text(n)
                    .id("preview-note")
                    .text_size(theme.type_scale.caption)
                    .text_color(theme.text.secondary)
                    .px(theme.spacing.sm)
                    .pt(theme.spacing.xs)
                    .into_any_element()
            }))
    }
}
