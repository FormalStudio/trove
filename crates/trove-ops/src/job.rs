use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::trash::TrashConfig;
use crate::{JobId, Op, OpEvent, Resolution, ResolutionChoice};

const PROGRESS_EVERY: Duration = Duration::from_millis(50);

/// What a queued job runs: a public op, or an exact set of renames (the inverse of a move).
#[derive(Clone, Debug)]
pub(crate) enum Job {
    Op(Op),
    Pairs(Vec<(PathBuf, PathBuf)>),
}

#[derive(Debug)]
pub(crate) enum Stop {
    Cancelled,
    Failed(String),
}

pub(crate) type R<T> = Result<T, Stop>;

pub(crate) fn fail<T>(msg: impl Into<String>) -> R<T> {
    Err(Stop::Failed(msg.into()))
}

pub(crate) fn io_message(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

pub(crate) fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Attaches the path an I/O error happened on.
pub(crate) trait At<T> {
    fn at(self, path: &Path) -> R<T>;
}

impl<T> At<T> for io::Result<T> {
    fn at(self, path: &Path) -> R<T> {
        self.map_err(|e| Stop::Failed(format!("{}: {}", name_of(path), io_message(&e))))
    }
}

/// Everything one running job reads and writes.
pub(crate) struct Ctx {
    pub job: JobId,
    pub events: flume::Sender<OpEvent>,
    pub resolutions: flume::Receiver<(JobId, ResolutionChoice)>,
    pub cancel: Arc<AtomicBool>,
    pub trash: Arc<TrashConfig>,
    pub sticky: Option<Resolution>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_files: u64,
    pub total_files: u64,
    pub current: PathBuf,
    pub last_emit: Instant,
    /// Top-level results the UI should select.
    pub created: Vec<PathBuf>,
    /// Moves and renames that happened, as (from, to).
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// Items sent to the trash, as (original, trashed).
    pub trashed: Vec<(PathBuf, PathBuf)>,
    /// Items taken out of the trash, as (trashed, restored).
    pub restored: Vec<(PathBuf, PathBuf)>,
    /// Permission bits changed, as (path, the bits before).
    pub modes: Vec<(PathBuf, u32)>,
    /// Something existing was overwritten or merged into: no inverse can bring it back.
    pub tainted: bool,
    /// Fifos, sockets and device nodes a copy passed over.
    pub skipped: u64,
    pub reflink: bool,
    #[cfg(test)]
    pub cancel_after_bytes: Option<u64>,
    /// Makes every move's `rename(2)` fail with EXDEV, the cross-device path on one file system.
    #[cfg(test)]
    pub force_exdev: bool,
}

impl Ctx {
    pub fn new(
        job: JobId,
        events: flume::Sender<OpEvent>,
        resolutions: flume::Receiver<(JobId, ResolutionChoice)>,
        cancel: Arc<AtomicBool>,
        trash: Arc<TrashConfig>,
    ) -> Self {
        Self {
            job,
            events,
            resolutions,
            cancel,
            trash,
            sticky: None,
            done_bytes: 0,
            total_bytes: 0,
            done_files: 0,
            total_files: 0,
            current: PathBuf::new(),
            last_emit: Instant::now() - PROGRESS_EVERY,
            created: Vec::new(),
            pairs: Vec::new(),
            trashed: Vec::new(),
            restored: Vec::new(),
            modes: Vec::new(),
            tainted: false,
            skipped: 0,
            reflink: true,
            #[cfg(test)]
            cancel_after_bytes: None,
            #[cfg(test)]
            force_exdev: false,
        }
    }

    pub fn check_cancel(&self) -> R<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(Stop::Cancelled) } else { Ok(()) }
    }

    pub fn emit(&self, ev: OpEvent) {
        let _ = self.events.send(ev);
    }

    pub fn progress(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_emit) < PROGRESS_EVERY {
            return;
        }
        self.last_emit = now;
        self.emit(OpEvent::Progress {
            job: self.job,
            done_bytes: self.done_bytes,
            total_bytes: self.total_bytes.max(self.done_bytes),
            done_files: self.done_files,
            total_files: self.total_files.max(self.done_files),
            current: self.current.clone(),
        });
    }

    pub fn set_current(&mut self, path: &Path) {
        if self.current != path {
            self.current = path.to_path_buf();
        }
    }

    pub fn add_bytes(&mut self, n: u64) {
        self.done_bytes += n;
        #[cfg(test)]
        if self.cancel_after_bytes.is_some_and(|limit| self.done_bytes >= limit) {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.progress(false);
    }

    pub fn finish_item(&mut self) {
        self.done_files += 1;
        self.progress(false);
    }

    /// Asks the UI what to do about `existing` and blocks until it answers or the job is
    /// cancelled.
    pub fn resolve_conflict(&mut self, source: &Path, existing: &Path) -> R<Resolution> {
        if let Some(r) = self.sticky {
            return Ok(r);
        }
        self.check_cancel()?;
        while self.resolutions.try_recv().is_ok() {}
        self.emit(OpEvent::Conflict { job: self.job, source: source.to_path_buf(), existing: existing.to_path_buf() });
        let choice = loop {
            match self.resolutions.recv() {
                Ok((job, choice)) if job == self.job => break choice,
                Ok(_) => {}
                Err(_) => return Err(Stop::Cancelled),
            }
        };
        self.check_cancel()?;
        if choice.resolution == Resolution::Cancel {
            return Err(Stop::Cancelled);
        }
        if choice.apply_to_all {
            self.sticky = Some(choice.resolution);
        }
        Ok(choice.resolution)
    }
}
