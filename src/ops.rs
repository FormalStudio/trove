//! The app's one `Ops`: operations go in through [`submit`], their events come back on the main
//! thread into [`Jobs`], which keeps what the status bar shows, opens the conflict sheet, reports
//! failures and hands each finished job's results to the tab that shows their folder.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use nd::core::WindowId;
use nd::prelude::*;
use trove_ops::{Config, JobId, Op, OpEvent, Ops, ResolutionChoice, TrashConfig};

use crate::pump::{live, pump};
use crate::tab::Tab;

/// How long a finished job stays in the status bar.
const LINGER: Duration = Duration::from_millis(1500);

struct Hub {
    ops: Rc<Ops>,
    jobs: Entity<Jobs>,
    clip: Entity<Clip>,
}

/// Starts the app's `Ops`, once. In the demo every trash is the fixture's own.
pub fn init(cx: &mut App) {
    if cx.has_global::<Hub>() {
        return;
    }
    let env = crate::env::get();
    let mut trash = TrashConfig::from_env();
    if env.demo {
        if let Some(share) = env.trash.parent().and_then(Path::parent) {
            trash.data_home = share.to_path_buf();
        }
        trash.mounts = Some(Vec::new());
    }
    let (ops, rx) = Ops::with_config(Config { trash });
    let jobs = cx.new(|_| Jobs::new(rx.clone()));
    pump(rx, jobs.downgrade(), |jobs: &mut Jobs, batch, cx| {
        jobs.apply(batch, cx);
        true
    });
    let clip = cx.new(|_| Clip::default());
    cx.set_global(Hub { ops: Rc::new(ops), jobs, clip });
    warm_open_with();
}

fn hub(cx: &App) -> &Hub {
    cx.global::<Hub>()
}

pub fn jobs(cx: &App) -> Entity<Jobs> {
    hub(cx).jobs.clone()
}

pub fn clip(cx: &App) -> Entity<Clip> {
    hub(cx).clip.clone()
}

/// Who asked: the window that shows the job's sheet and messages, and the tab that selects
/// what it made.
#[derive(Clone)]
pub struct Origin {
    pub window: WindowId,
    pub tab: Option<WeakEntity<Tab>>,
}

pub fn submit(cx: &mut App, origin: Origin, op: Op) -> JobId {
    let ops = hub(cx).ops.clone();
    let job = ops.submit(op.clone());
    hub(cx).jobs.clone().update(cx, |j, _| j.track(job, origin, Some(op)));
    job
}

pub fn undo(cx: &mut App, origin: Origin) -> Option<JobId> {
    let job = hub(cx).ops.clone().undo()?;
    hub(cx).jobs.clone().update(cx, |j, _| j.track(job, origin, None));
    Some(job)
}

pub fn redo(cx: &mut App, origin: Origin) -> Option<JobId> {
    let job = hub(cx).ops.clone().redo()?;
    hub(cx).jobs.clone().update(cx, |j, _| j.track(job, origin, None));
    Some(job)
}

pub fn cancel(cx: &App, job: JobId) {
    hub(cx).ops.cancel(job);
}

pub fn resolve(cx: &App, job: JobId, choice: ResolutionChoice) {
    hub(cx).ops.resolve(job, choice);
}

/// One line of the status bar's jobs area.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRow {
    pub id: JobId,
    pub title: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_files: u64,
    pub total_files: u64,
    pub finished: bool,
}

/// A conflict waiting for the user.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub job: JobId,
    pub source: PathBuf,
    pub existing: PathBuf,
}

/// A message for one window's toast overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub window: WindowId,
    pub id: SharedString,
    pub title: String,
}

struct Tracked {
    origin: Origin,
    op: Option<Op>,
}

pub struct Jobs {
    rx: flume::Receiver<OpEvent>,
    rows: Vec<JobRow>,
    tracked: HashMap<JobId, Tracked>,
    open: HashSet<JobId>,
    asks: VecDeque<Ask>,
    sheet: Option<(JobId, WindowId)>,
    notices: Vec<Notice>,
    next_notice: u64,
    tabs: Vec<WeakEntity<Tab>>,
    /// Every event seen, for tests.
    seen: Vec<OpEvent>,
}

impl Jobs {
    fn new(rx: flume::Receiver<OpEvent>) -> Self {
        Self {
            rx,
            rows: Vec::new(),
            tracked: HashMap::new(),
            open: HashSet::new(),
            asks: VecDeque::new(),
            sheet: None,
            notices: Vec::new(),
            next_notice: 0,
            tabs: Vec::new(),
            seen: Vec::new(),
        }
    }

    pub fn rows(&self) -> &[JobRow] {
        &self.rows
    }

    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    pub fn asking(&self) -> Option<&Ask> {
        self.asks.front()
    }

    pub fn seen(&self) -> &[OpEvent] {
        &self.seen
    }

    pub fn is_idle(&self) -> bool {
        self.open.is_empty()
    }

    /// A tab that wants to hear what jobs made.
    pub fn register(&mut self, tab: WeakEntity<Tab>) {
        self.tabs.retain(|t| t.upgrade().is_some());
        self.tabs.push(tab);
    }

    fn track(&mut self, job: JobId, origin: Origin, op: Option<Op>) {
        self.open.insert(job);
        self.tracked.insert(job, Tracked { origin, op });
    }

    pub fn dismiss_notice(&mut self, id: &str, cx: &mut Context<Self>) {
        self.notices.retain(|n| n.id.as_ref() != id);
        cx.notify();
    }

    fn notify_window(&mut self, window: WindowId, title: String) {
        self.next_notice += 1;
        self.notices.push(Notice { window, id: format!("notice-{}", self.next_notice).into(), title });
    }

    /// Applies everything sent until every job submitted so far has ended or one waits on a
    /// conflict, waiting up to `timeout`: the host's pump, for tier 1 tests.
    /// Returns whether anything was applied.
    pub fn drain(&mut self, timeout: Duration, cx: &mut Context<Self>) -> bool {
        // nd-allow(clock-in-ui): a test drain's deadline, run with no executor
        let deadline = std::time::Instant::now() + timeout;
        let mut moved = false;
        loop {
            let mut batch: Vec<OpEvent> = self.rx.try_iter().collect();
            if batch.is_empty() {
                if self.open.is_empty() || !self.asks.is_empty() {
                    return moved;
                }
                // nd-allow(clock-in-ui): as above
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(e) => batch.push(e),
                    Err(_) => return moved,
                }
            }
            moved = true;
            self.apply(batch, cx);
        }
    }

    fn apply(&mut self, batch: Vec<OpEvent>, cx: &mut Context<Self>) {
        for event in batch {
            self.seen.push(event.clone());
            match event {
                OpEvent::Started { job, title } => {
                    if self.shows(job) {
                        self.rows.push(JobRow {
                            id: job,
                            title,
                            done_bytes: 0,
                            total_bytes: 0,
                            done_files: 0,
                            total_files: 0,
                            finished: false,
                        });
                    }
                }
                OpEvent::Progress { job, done_bytes, total_bytes, done_files, total_files, .. } => {
                    if let Some(row) = self.rows.iter_mut().find(|r| r.id == job) {
                        row.done_bytes = done_bytes;
                        row.total_bytes = total_bytes;
                        row.done_files = done_files;
                        row.total_files = total_files;
                    }
                }
                OpEvent::Conflict { job, source, existing } => {
                    self.asks.push_back(Ask { job, source, existing });
                    self.ask_next(cx);
                }
                OpEvent::Finished { job, created } => {
                    self.end(job, cx);
                    if let Some(row) = self.rows.iter_mut().find(|r| r.id == job) {
                        row.finished = true;
                        row.done_files = row.total_files;
                        row.done_bytes = row.total_bytes;
                        let me = cx.weak_entity();
                        pump(after(LINGER), me, move |this: &mut Jobs, _, cx| {
                            this.rows.retain(|r| r.id != job);
                            cx.notify();
                            false
                        });
                    }
                    self.deliver(job, &created, cx);
                    self.tracked.remove(&job);
                }
                OpEvent::Failed { job, error } => {
                    self.end(job, cx);
                    self.rows.retain(|r| r.id != job);
                    self.failed(job, error, cx);
                    self.tracked.remove(&job);
                }
                OpEvent::Cancelled { job } => {
                    self.end(job, cx);
                    self.rows.retain(|r| r.id != job);
                    if let Some(tab) = self.rename_tab(job) {
                        let _ = tab.update(cx, |t, cx| t.rename_ended(job, cx));
                    }
                    self.tracked.remove(&job);
                }
            }
        }
        cx.notify();
    }

    /// Quick jobs (a rename, a new folder) never show in the status bar.
    fn shows(&self, job: JobId) -> bool {
        !matches!(
            self.tracked.get(&job).and_then(|t| t.op.as_ref()),
            Some(Op::Rename { .. } | Op::NewFolder { .. } | Op::NewFile { .. })
        )
    }

    fn end(&mut self, job: JobId, cx: &mut Context<Self>) {
        self.open.remove(&job);
        let before = self.asks.len();
        self.asks.retain(|a| a.job != job);
        if self.sheet.is_some_and(|(j, _)| j == job) {
            self.close_sheet(cx);
        }
        if self.asks.len() != before {
            self.ask_next(cx);
        }
    }

    fn rename_tab(&self, job: JobId) -> Option<WeakEntity<Tab>> {
        let t = self.tracked.get(&job)?;
        matches!(t.op, Some(Op::Rename { .. })).then(|| t.origin.tab.clone()).flatten()
    }

    /// Selects what `job` made in the tab that asked when it shows their folder, else in every
    /// tab that does. A new folder or file also starts its rename.
    fn deliver(&mut self, job: JobId, created: &[PathBuf], cx: &mut Context<Self>) {
        let tracked = self.tracked.get(&job);
        let origin_tab = tracked.and_then(|t| t.origin.tab.clone());
        let rename = matches!(tracked.and_then(|t| t.op.as_ref()), Some(Op::NewFolder { .. } | Op::NewFile { .. }));
        if let Some(tab) = self.rename_tab(job) {
            let _ = tab.update(cx, |t, cx| t.rename_ended(job, cx));
        }
        if created.is_empty() {
            return;
        }
        let Some(dir) = created[0].parent().map(Path::to_path_buf) else { return };
        let shows = |tab: &WeakEntity<Tab>, cx: &App| {
            tab.upgrade().is_some_and(|t| t.read(cx).location().dir() == Some(dir.as_path()))
        };
        let targets: Vec<WeakEntity<Tab>> = match origin_tab.filter(|t| shows(t, cx)) {
            Some(t) => vec![t],
            None => self.tabs.iter().filter(|t| shows(t, cx)).cloned().collect(),
        };
        let paths: Vec<PathBuf> = created.iter().filter(|p| p.parent() == Some(dir.as_path())).cloned().collect();
        for tab in targets {
            let paths = paths.clone();
            let _ = tab.update(cx, |t, cx| t.select_created(paths, rename, cx));
        }
    }

    fn failed(&mut self, job: JobId, error: String, cx: &mut Context<Self>) {
        let Some(tracked) = self.tracked.get(&job) else { return };
        let window = tracked.origin.window;
        if let Some(tab) = self.rename_tab(job) {
            let _ = tab.update(cx, |t, cx| t.rename_failed(job, error, cx));
            return;
        }
        if let Some(Op::Trash { paths }) = &tracked.op
            && error.starts_with(trove_ops::trash::NO_VOLUME_TRASH)
        {
            let (paths, origin) = (paths.clone(), tracked.origin.clone());
            offer_delete(cx, origin, paths);
            return;
        }
        let what = match &tracked.op {
            Some(op) => verb(op),
            None => "Could not undo",
        };
        self.notify_window(window, format!("{what}: {error}"));
    }

    fn ask_next(&mut self, cx: &mut Context<Self>) {
        if self.sheet.is_some() {
            return;
        }
        let Some(ask) = self.asks.front().cloned() else { return };
        let Some(window) = self.tracked.get(&ask.job).map(|t| t.origin.window) else { return };
        let job = ask.job;
        let sheet = crate::conflict::open(cx, window, ask);
        self.sheet = Some((job, sheet.id()));
    }

    fn close_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some((_, window)) = self.sheet.take() {
            cx.close_window(window);
        }
    }

    /// The sheet's answer: passes it on and shows the next conflict, if any.
    pub fn answer(&mut self, choice: ResolutionChoice, cx: &mut Context<Self>) {
        let Some(ask) = self.asks.pop_front() else { return };
        resolve(cx, ask.job, choice);
        if choice.apply_to_all || choice.resolution == trove_ops::Resolution::Cancel {
            self.asks.retain(|a| a.job != ask.job);
        }
        self.close_sheet(cx);
        self.ask_next(cx);
        cx.notify();
    }
}

/// "Could not copy", for a failure message.
fn verb(op: &Op) -> &'static str {
    match op {
        Op::Copy { .. } | Op::Duplicate { .. } => "Could not copy",
        Op::Move { .. } => "Could not move",
        Op::Trash { .. } => "Could not move to Trash",
        Op::Delete { .. } => "Could not delete",
        Op::Rename { .. } => "Could not rename",
        Op::NewFolder { .. } => "Could not create the folder",
        Op::NewFile { .. } => "Could not create the file",
        Op::Restore { .. } => "Could not restore",
        Op::EmptyTrash => "Could not empty the Trash",
        Op::Compress { .. } => "Could not compress",
        Op::Extract { .. } => "Could not extract",
    }
}

/// A trash refused on a volume with no trash: asks before deleting what is still there.
fn offer_delete(cx: &mut App, origin: Origin, paths: Vec<PathBuf>) {
    let jobs = jobs(cx);
    background(
        jobs.downgrade(),
        move || paths.into_iter().filter(|p| p.symlink_metadata().is_ok()).collect::<Vec<_>>(),
        move |_: &mut Jobs, left: Vec<PathBuf>, cx| {
            if left.is_empty() {
                return;
            }
            let title = format!("{} can\u{2019}t be moved to Trash", describe(&left));
            let message = "This volume has no Trash. Delete permanently? This can\u{2019}t be undone.";
            let mut alert = Alert::confirm(title, "Delete permanently");
            alert.message = Some(message.into());
            let window = origin.window;
            nd::chrome::alert(cx, window, alert, move |r, cx| {
                if r.0 == 0 {
                    submit(cx, origin, Op::Delete { paths: left });
                }
            });
        },
    );
}

/// "“name”" for one path, "3 items" for more.
pub fn describe(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("\u{201c}{}\u{201d}", name_of(one)),
        many => format!("{} items", many.len()),
    }
}

pub fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// The paths cut to the clipboard: drawn dimmed until they are pasted or something else is
/// copied.
#[derive(Default)]
pub struct Clip {
    cut: HashSet<PathBuf>,
}

impl Clip {
    pub fn is_cut(&self, path: &Path) -> bool {
        self.cut.contains(path)
    }

    pub fn cut(&self) -> &HashSet<PathBuf> {
        &self.cut
    }

    pub fn set_cut(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        let cut: HashSet<PathBuf> = paths.into_iter().collect();
        if cut != self.cut {
            self.cut = cut;
            cx.notify();
        }
    }
}

/// Puts `paths` on the file clipboard, off the main thread (`wl-copy` can take a moment).
pub fn copy_to_clipboard(cx: &mut App, paths: Vec<PathBuf>, cut: bool) {
    let clip = clip(cx);
    clip.update(cx, |c, cx| c.set_cut(if cut { paths.clone() } else { Vec::new() }, cx));
    in_order(clip.downgrade(), move || trove_ops::copy_files(&paths, cut), |_: &mut Clip, (), _| {});
}

/// Reads the file clipboard off the main thread (`wl-paste`) and hands its paths to `then`,
/// which pastes them. Nothing is cut after a paste: a cut is spent once it moves, and a copy
/// on the clipboard means the cut was replaced.
pub fn read_clipboard(cx: &mut App, then: impl FnOnce(Option<(Vec<PathBuf>, bool)>, &mut App) + 'static) {
    let clip = clip(cx);
    in_order(clip.downgrade(), trove_ops::paste_files, move |c: &mut Clip, found, cx| {
        c.set_cut(Vec::new(), cx);
        if matches!(found, Some((_, true))) {
            in_order(cx.weak_entity(), trove_ops::clipboard::clear, |_: &mut Clip, (), _| {});
        }
        then(found, cx);
    });
}

type Deferred = Box<dyn FnOnce(&mut App)>;

thread_local! {
    static DEFERRED: RefCell<Vec<Deferred>> = const { RefCell::new(Vec::new()) };
}

/// Runs `work` off the main thread and `done` with its result on `entity`. With no host
/// running (tier 1) the work runs at once and `done` waits for [`run_deferred`], so it never
/// lands inside the update that asked for it.
pub fn background<V: 'static, T: Send + 'static>(
    entity: WeakEntity<V>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut V, T, &mut Context<V>) + 'static,
) {
    run_off_main(entity, work, done, |job| {
        let _ = std::thread::Builder::new().name("trove-work".into()).spawn(job);
    });
}

type Job = Box<dyn FnOnce() + Send>;

/// [`background`] on one worker thread shared by every call, so the work runs in the order it
/// was asked for: a paste right after a copy reads what the copy put on the clipboard, though
/// `wl-copy` takes a moment to start.
fn in_order<V: 'static, T: Send + 'static>(
    entity: WeakEntity<V>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut V, T, &mut Context<V>) + 'static,
) {
    static QUEUE: std::sync::OnceLock<flume::Sender<Job>> = std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = flume::unbounded::<Job>();
        let _ = std::thread::Builder::new().name("trove-clipboard".into()).spawn(move || {
            for job in rx.iter() {
                job();
            }
        });
        tx
    });
    run_off_main(entity, work, done, |job| {
        let _ = queue.send(job);
    });
}

fn run_off_main<V: 'static, T: Send + 'static>(
    entity: WeakEntity<V>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut V, T, &mut Context<V>) + 'static,
    spawn: impl FnOnce(Job),
) {
    if !live() {
        let value = work();
        DEFERRED.with(|d| {
            d.borrow_mut().push(Box::new(move |cx: &mut App| {
                let _ = entity.update(cx, |v, cx| done(v, value, cx));
            }))
        });
        return;
    }
    let (tx, rx) = flume::bounded(1);
    spawn(Box::new(move || {
        let _ = tx.send(work());
    }));
    let done = RefCell::new(Some(done));
    pump(rx, entity, move |v: &mut V, values, cx| {
        if let (Some(done), Some(value)) = (done.borrow_mut().take(), values.into_iter().next()) {
            done(v, value, cx);
        }
        false
    });
}

/// What [`background`] queued while no host runs. Returns whether anything ran.
pub fn run_deferred(cx: &mut App) -> bool {
    let mut ran = false;
    loop {
        let batch = DEFERRED.with(|d| std::mem::take(&mut *d.borrow_mut()));
        if batch.is_empty() {
            return ran;
        }
        ran = true;
        for f in batch {
            f(cx);
        }
    }
}

/// A message once `delay` has passed, from a thread of its own.
pub fn after(delay: Duration) -> flume::Receiver<()> {
    let (tx, rx) = flume::bounded(1);
    let _ = std::thread::Builder::new().name("trove-after".into()).spawn(move || {
        let (_hold, wait) = flume::bounded::<()>(1);
        let _ = wait.recv_timeout(delay);
        let _ = tx.send(());
    });
    rx
}

// Open with.

/// Apps per MIME type, filled off the main thread: the context menu reads it when it opens.
#[derive(Default)]
struct Apps {
    by_mime: HashMap<String, Vec<trove_ops::AppInfo>>,
    mime_of: HashMap<PathBuf, String>,
}

thread_local! {
    static APPS: RefCell<Apps> = RefCell::new(Apps::default());
}

/// Loads the desktop entries on a thread at start, so the first menu finds them parsed.
fn warm_open_with() {
    let _ = std::thread::Builder::new().name("trove-apps".into()).spawn(|| {
        let _ = trove_ops::open_with::registry();
    });
}

/// The apps for `path`, default first, when they have been looked up; otherwise starts the
/// lookup so the next menu has them.
pub fn apps_for(cx: &mut App, path: &Path) -> Option<Vec<trove_ops::AppInfo>> {
    let known = APPS.with(|a| {
        let a = a.borrow();
        a.mime_of.get(path).and_then(|m| a.by_mime.get(m)).cloned()
    });
    if known.is_some() {
        return known;
    }
    look_up_apps(cx, vec![path.to_path_buf()]);
    None
}

/// The MIME type a lookup found for `path`.
pub fn mime_of(path: &Path) -> Option<String> {
    APPS.with(|a| a.borrow().mime_of.get(path).cloned())
}

/// Looks up the apps for `paths` off the main thread.
pub fn look_up_apps(cx: &mut App, paths: Vec<PathBuf>) {
    let paths: Vec<PathBuf> =
        paths.into_iter().filter(|p| APPS.with(|a| !a.borrow().mime_of.contains_key(p))).collect();
    if paths.is_empty() {
        return;
    }
    let clip = clip(cx);
    background(
        clip.downgrade(),
        move || {
            paths
                .into_iter()
                .map(|p| {
                    let mime = trove_ops::mime_of(&p);
                    let apps = trove_ops::apps_for(&mime);
                    (p, mime, apps)
                })
                .collect::<Vec<_>>()
        },
        |_: &mut Clip, found, _| {
            APPS.with(|a| {
                let mut a = a.borrow_mut();
                for (path, mime, apps) in found {
                    a.mime_of.insert(path, mime.clone());
                    a.by_mime.insert(mime, apps);
                }
            })
        },
    );
}

/// The app with desktop id `id` among those known for `path`.
pub fn app_by_id(path: &Path, id: &str) -> Option<trove_ops::AppInfo> {
    APPS.with(|a| {
        let a = a.borrow();
        let mime = a.mime_of.get(path)?;
        a.by_mime.get(mime)?.iter().find(|app| app.id == id).cloned()
    })
}

/// Forgets the cached apps for a type after its default changed.
pub fn forget_apps(mime: &str) {
    APPS.with(|a| {
        let mut a = a.borrow_mut();
        a.by_mime.remove(mime);
        a.mime_of.retain(|_, m| m != mime);
    });
}
