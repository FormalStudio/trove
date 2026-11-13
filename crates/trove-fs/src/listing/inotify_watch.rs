use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use inotify::{EventMask, Inotify, WatchMask};

use super::{Batch, COALESCE, Ctx, EntryId, ListError, ListEvent, MAX_CHUNK};
use std::collections::HashSet;

pub(super) struct Cancel {
    flag: AtomicBool,
    /// Wakes the watcher's poll so a dropped listing releases its inotify instance at once.
    wake: Option<OwnedFd>,
}

impl Cancel {
    pub(super) fn new() -> Self {
        // SAFETY: eventfd takes plain integers; a valid result is an fd we own.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        // SAFETY: `fd` is a fresh descriptor not owned elsewhere.
        let wake = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) });
        Self { flag: AtomicBool::new(false), wake }
    }

    pub(super) fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    pub(super) fn trigger(&self) {
        self.flag.store(true, Ordering::Relaxed);
        if let Some(fd) = &self.wake {
            let one = 1u64;
            // SAFETY: writing 8 bytes from a live u64 to an eventfd we own.
            unsafe { libc::write(fd.as_raw_fd(), (&raw const one).cast(), 8) };
        }
    }
}

pub(super) struct Watcher {
    ino: Inotify,
}

impl Watcher {
    pub(super) fn new(dir: &Path, _cancel: &Cancel) -> Option<Self> {
        let ino = Inotify::init().ok()?;
        let mask = WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MODIFY
            | WatchMask::ATTRIB
            | WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF
            | WatchMask::ONLYDIR;
        ino.watches().add(dir, mask).ok()?;
        Some(Self { ino })
    }

    pub(super) fn run(mut self, dir: &Path, mut known: HashSet<EntryId>, ctx: &Ctx) {
        let ino_fd = self.ino.as_raw_fd();
        let wake_fd = ctx.cancel.wake.as_ref().map(AsRawFd::as_raw_fd);
        let mut buf = vec![0u8; 32 * 1024];
        let mut batch = Batch::default();
        let mut deadline: Option<Instant> = None;
        loop {
            let timeout = match (deadline, wake_fd) {
                (Some(d), _) => d.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32,
                (None, Some(_)) => -1,
                (None, None) => 1000,
            };
            if !wait(ino_fd, wake_fd, timeout) || ctx.cancel.is_set() {
                return;
            }
            loop {
                match self.ino.read_events(&mut buf) {
                    Ok(events) => {
                        for ev in events {
                            batch.add(ev.mask, ev.name, ev.cookie);
                            deadline.get_or_insert_with(|| Instant::now() + COALESCE);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                deadline = None;
                let gone = batch.dir_gone;
                let changes = std::mem::take(&mut batch).resolve(dir, &mut known);
                for part in changes.chunks(MAX_CHUNK) {
                    if !ctx.send(ListEvent::Changed(part.to_vec())) {
                        return;
                    }
                }
                if gone {
                    ctx.send(ListEvent::Error(ListError::NotFound));
                    return;
                }
            }
        }
    }
}

/// Blocks until the inotify fd is readable, the cancel fd fires or the timeout passes. False on a
/// poll error.
fn wait(ino_fd: RawFd, wake_fd: Option<RawFd>, timeout_ms: i32) -> bool {
    let mut fds = [
        libc::pollfd { fd: ino_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake_fd.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
    ];
    loop {
        // SAFETY: `fds` is a valid array of two pollfds; a negative fd is ignored by poll.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if r >= 0 {
            return true;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

impl Batch {
    fn add(&mut self, mask: EventMask, name: Option<&OsStr>, cookie: u32) {
        if mask.contains(EventMask::Q_OVERFLOW) {
            self.overflow = true;
        }
        if mask.intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF | EventMask::UNMOUNT) {
            self.dir_gone = true;
        }
        let Some(name) = name else { return };
        self.touch(name);
        if mask.contains(EventMask::MOVED_FROM) {
            self.moved_from.push((cookie, name.to_os_string()));
        } else if mask.contains(EventMask::MOVED_TO)
            && let Some(i) = self.moved_from.iter().position(|(c, _)| *c == cookie)
        {
            let (_, from) = self.moved_from.swap_remove(i);
            self.renames.push((from, name.to_os_string()));
        }
    }
}
