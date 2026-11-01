use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::fsops::{self, Mode};
use crate::job::{Ctx, Job, R, Stop, name_of};
use crate::trash::{self, TrashConfig};
use crate::{JobId, Op, OpEvent, Resolution, ResolutionChoice, archive};

#[derive(Clone, Debug)]
pub struct Config {
    pub trash: TrashConfig,
}

impl Config {
    pub fn from_env() -> Self {
        Self { trash: TrashConfig::from_env() }
    }
}

/// Inverses of finished jobs. Undoing a job runs its inverse and pushes that run's own inverse
/// onto the other stack, so redo is just the same walk the other way.
#[derive(Default)]
struct Journal {
    undo: Vec<Job>,
    redo: Vec<Job>,
    /// Submitted fresh jobs that have not finished: their inverse is not on a stack yet.
    in_flight: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Fresh,
    Undo,
    Redo,
}

struct Queued {
    id: JobId,
    job: Option<Job>,
    origin: Origin,
    cancel: Arc<AtomicBool>,
}

struct Shared {
    next_id: AtomicU64,
    journal: Mutex<Journal>,
    cancels: Mutex<HashMap<JobId, Arc<AtomicBool>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One per app. Jobs run one at a time, in submission order, on a worker thread.
pub struct Ops {
    shared: Arc<Shared>,
    jobs: flume::Sender<Queued>,
    resolutions: flume::Sender<(JobId, ResolutionChoice)>,
}

impl Ops {
    pub fn new() -> (Ops, flume::Receiver<OpEvent>) {
        Self::with_config(Config::from_env())
    }

    pub fn with_config(config: Config) -> (Ops, flume::Receiver<OpEvent>) {
        let (job_tx, job_rx) = flume::unbounded();
        let (res_tx, res_rx) = flume::unbounded();
        let (ev_tx, ev_rx) = flume::unbounded();
        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            journal: Mutex::new(Journal::default()),
            cancels: Mutex::new(HashMap::new()),
        });
        let worker =
            Worker { shared: Arc::clone(&shared), events: ev_tx, resolutions: res_rx, trash: Arc::new(config.trash) };
        std::thread::Builder::new().name("trove-ops".into()).spawn(move || worker.run(job_rx)).ok();
        (Ops { shared, jobs: job_tx, resolutions: res_tx }, ev_rx)
    }

    pub fn submit(&self, op: Op) -> JobId {
        lock(&self.shared.journal).in_flight += 1;
        self.enqueue(Some(Job::Op(op)), Origin::Fresh)
    }

    fn enqueue(&self, job: Option<Job>, origin: Origin) -> JobId {
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.shared.cancels).insert(id, Arc::clone(&cancel));
        let _ = self.jobs.send(Queued { id, job, origin, cancel });
        id
    }

    pub fn cancel(&self, job: JobId) {
        if let Some(flag) = lock(&self.shared.cancels).get(&job) {
            flag.store(true, Ordering::Relaxed);
        }
        let _ = self.resolutions.send((job, ResolutionChoice::once(Resolution::Cancel)));
    }

    /// Answers an `OpEvent::Conflict`.
    pub fn resolve(&self, job: JobId, choice: ResolutionChoice) {
        let _ = self.resolutions.send((job, choice));
    }

    /// Runs the inverse of the newest undoable job. `None` when there is nothing to undo.
    pub fn undo(&self) -> Option<JobId> {
        {
            let j = lock(&self.shared.journal);
            if j.undo.is_empty() && j.in_flight == 0 {
                return None;
            }
        }
        Some(self.enqueue(None, Origin::Undo))
    }

    pub fn redo(&self) -> Option<JobId> {
        {
            let j = lock(&self.shared.journal);
            if j.redo.is_empty() && j.in_flight == 0 {
                return None;
            }
        }
        Some(self.enqueue(None, Origin::Redo))
    }
}

struct Worker {
    shared: Arc<Shared>,
    events: flume::Sender<OpEvent>,
    resolutions: flume::Receiver<(JobId, ResolutionChoice)>,
    trash: Arc<TrashConfig>,
}

impl Worker {
    fn run(self, jobs: flume::Receiver<Queued>) {
        for queued in jobs {
            self.run_one(queued);
        }
    }

    fn run_one(&self, q: Queued) {
        let Queued { id, job, origin, cancel } = q;
        let job = match origin {
            Origin::Fresh => job,
            Origin::Undo => lock(&self.shared.journal).undo.pop(),
            Origin::Redo => lock(&self.shared.journal).redo.pop(),
        };
        let mut ctx = Ctx::new(id, self.events.clone(), self.resolutions.clone(), cancel, Arc::clone(&self.trash));
        let result = match &job {
            Some(job) => {
                ctx.emit(OpEvent::Started { job: id, title: title_of(job, origin) });
                ctx.check_cancel().and_then(|()| execute(&mut ctx, job))
            }
            None => {
                ctx.emit(OpEvent::Started { job: id, title: "Nothing to undo".into() });
                Ok(())
            }
        };

        let inverse = job.as_ref().and_then(|j| inverse_of(j, &ctx));
        {
            let mut guard = lock(&self.shared.journal);
            let journal = &mut *guard;
            match origin {
                Origin::Fresh => {
                    journal.in_flight = journal.in_flight.saturating_sub(1);
                    if let Some(inv) = inverse {
                        journal.undo.push(inv);
                        journal.redo.clear();
                    }
                }
                Origin::Undo | Origin::Redo => {
                    let (this, other) = if origin == Origin::Undo {
                        (&mut journal.undo, &mut journal.redo)
                    } else {
                        (&mut journal.redo, &mut journal.undo)
                    };
                    match (inverse, &job) {
                        (Some(inv), _) => other.push(inv),
                        // Nothing happened (it failed first or was cancelled): keep the entry.
                        (None, Some(j)) if result.is_err() => this.push(j.clone()),
                        _ => {}
                    }
                }
            }
        }
        lock(&self.shared.cancels).remove(&id);
        match result {
            Ok(()) => {
                let created = std::mem::take(&mut ctx.created);
                ctx.emit(OpEvent::Finished { job: id, created });
            }
            Err(Stop::Cancelled) => ctx.emit(OpEvent::Cancelled { job: id }),
            Err(Stop::Failed(error)) => ctx.emit(OpEvent::Failed { job: id, error }),
        }
    }
}

fn count_label(n: usize, one: &str) -> String {
    if n == 1 { format!("\"{one}\"") } else { format!("{n} items") }
}

fn first_name(paths: &[PathBuf]) -> String {
    paths.first().map(|p| name_of(p)).unwrap_or_default()
}

fn title_of(job: &Job, origin: Origin) -> String {
    let prefix = match origin {
        Origin::Fresh => "",
        Origin::Undo => "Undo: ",
        Origin::Redo => "Redo: ",
    };
    let body = match job {
        Job::Pairs(p) => {
            format!("Moving {}", count_label(p.len(), &p.first().map(|(f, _)| name_of(f)).unwrap_or_default()))
        }
        Job::Op(op) => match op {
            Op::Copy { sources, .. } => format!("Copying {}", count_label(sources.len(), &first_name(sources))),
            Op::Move { sources, .. } => format!("Moving {}", count_label(sources.len(), &first_name(sources))),
            Op::Trash { paths } => format!("Moving {} to Trash", count_label(paths.len(), &first_name(paths))),
            Op::Delete { paths } => format!("Deleting {}", count_label(paths.len(), &first_name(paths))),
            Op::Rename { path, .. } => format!("Renaming \"{}\"", name_of(path)),
            Op::NewFolder { name, .. } => format!("Creating folder \"{name}\""),
            Op::NewFile { name, .. } => format!("Creating \"{name}\""),
            Op::Restore { trashed } => format!("Restoring {}", count_label(trashed.len(), &first_name(trashed))),
            Op::EmptyTrash => "Emptying Trash".into(),
            Op::Duplicate { paths } => format!("Duplicating {}", count_label(paths.len(), &first_name(paths))),
            Op::Compress { paths, .. } => format!("Compressing {}", count_label(paths.len(), &first_name(paths))),
            Op::Extract { archive, .. } => format!("Extracting \"{}\"", name_of(archive)),
        },
    };
    format!("{prefix}{body}")
}

fn execute(ctx: &mut Ctx, job: &Job) -> R<()> {
    let pair_up = |sources: &[PathBuf], dest_dir: &PathBuf| -> Vec<(PathBuf, PathBuf)> {
        sources.iter().map(|s| (s.clone(), dest_dir.clone())).collect()
    };
    match job {
        Job::Pairs(pairs) => fsops::move_exact(ctx, pairs),
        Job::Op(op) => match op {
            Op::Copy { sources, dest_dir } => fsops::transfer(ctx, Mode::Copy, &pair_up(sources, dest_dir)),
            Op::Move { sources, dest_dir } => fsops::transfer(ctx, Mode::Move, &pair_up(sources, dest_dir)),
            Op::Duplicate { paths } => {
                let items: Vec<_> = paths
                    .iter()
                    .map(|p| (p.clone(), p.parent().map_or_else(|| PathBuf::from("/"), PathBuf::from)))
                    .collect();
                fsops::transfer(ctx, Mode::Copy, &items)
            }
            Op::Trash { paths } => trash::trash_paths(ctx, paths),
            Op::Delete { paths } => fsops::delete_paths(ctx, paths),
            Op::Rename { path, new_name } => fsops::rename(ctx, path, new_name),
            Op::NewFolder { parent, name } => fsops::create(ctx, parent, name, true),
            Op::NewFile { parent, name } => fsops::create(ctx, parent, name, false),
            Op::Restore { trashed } => trash::restore_paths(ctx, trashed),
            Op::EmptyTrash => trash::empty_trash(ctx),
            Op::Compress { paths, dest } => archive::compress(ctx, paths, dest),
            Op::Extract { archive, dest_dir } => archive::extract(ctx, archive, dest_dir),
        },
    }
}

/// What brings back the state before `job`, from what it got done. A job that overwrote
/// something has none.
fn inverse_of(job: &Job, ctx: &Ctx) -> Option<Job> {
    if ctx.tainted {
        return None;
    }
    let reverse = |pairs: &[(PathBuf, PathBuf)]| {
        (!pairs.is_empty()).then(|| Job::Pairs(pairs.iter().rev().map(|(a, b)| (b.clone(), a.clone())).collect()))
    };
    let trash_of = |paths: &[PathBuf]| (!paths.is_empty()).then(|| Job::Op(Op::Trash { paths: paths.to_vec() }));
    match job {
        Job::Pairs(_) => reverse(&ctx.pairs),
        Job::Op(op) => match op {
            Op::Move { .. } | Op::Rename { .. } => reverse(&ctx.pairs),
            Op::Copy { .. }
            | Op::Duplicate { .. }
            | Op::NewFolder { .. }
            | Op::NewFile { .. }
            | Op::Compress { .. }
            | Op::Extract { .. } => trash_of(&ctx.created),
            Op::Trash { .. } => {
                let trashed: Vec<PathBuf> = ctx.trashed.iter().rev().map(|(_, t)| t.clone()).collect();
                (!trashed.is_empty()).then_some(Job::Op(Op::Restore { trashed }))
            }
            Op::Restore { .. } => {
                let restored: Vec<PathBuf> = ctx.restored.iter().rev().map(|(_, r)| r.clone()).collect();
                trash_of(&restored)
            }
            Op::Delete { .. } | Op::EmptyTrash => None,
        },
    }
}
