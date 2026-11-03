//! File operations: copy, move, trash, delete, rename, new folder, with progress, conflicts and undo.

mod archive;
pub mod batch;
pub mod clipboard;
mod desktop;
mod fsops;
mod job;
mod names;
pub mod open_with;
pub mod props;
mod queue;
pub mod trash;

use std::path::PathBuf;

pub use clipboard::{copy_files, paste_files};
pub use open_with::{AppInfo, apps_for, mime_of, open, open_terminal, open_with, set_default};
pub use queue::{Config, Ops};
pub use trash::TrashConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Copy {
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    Move {
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    /// Permanent; the UI confirms first.
    Delete {
        paths: Vec<PathBuf>,
    },
    Rename {
        path: PathBuf,
        new_name: String,
    },
    /// `name` is the wanted name; a taken one gets " 2", " 3", ...
    NewFolder {
        parent: PathBuf,
        name: String,
    },
    NewFile {
        parent: PathBuf,
        name: String,
    },
    /// Copies `template` into `parent` under its own name, or the first free " 2", " 3", ... of it.
    NewFromTemplate {
        template: PathBuf,
        parent: PathBuf,
    },
    /// Paths inside a trash's `files` directory.
    Restore {
        trashed: Vec<PathBuf>,
    },
    EmptyTrash,
    /// Copies next to the original: "name copy.ext", "name copy 2.ext".
    Duplicate {
        paths: Vec<PathBuf>,
    },
    /// `dest` is the zip file to write, or an existing folder to write "<name>.zip" or
    /// "Archive.zip" into. A taken name gets " 2", " 3", ...
    Compress {
        paths: Vec<PathBuf>,
        dest: PathBuf,
    },
    Extract {
        archive: PathBuf,
        dest_dir: PathBuf,
    },
    /// Each path's permission bits (the low 12 bits of its mode) set to the value beside it.
    /// A link is followed, as chmod(1) does.
    SetPermissions {
        modes: Vec<(PathBuf, u32)>,
    },
    /// Renames in one job, undone together. A name that is taken fails the job where it is.
    RenameMany {
        renames: Vec<(PathBuf, String)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum OpEvent {
    Started {
        job: JobId,
        title: String,
    },
    Progress {
        job: JobId,
        done_bytes: u64,
        total_bytes: u64,
        done_files: u64,
        total_files: u64,
        current: PathBuf,
    },
    Conflict {
        job: JobId,
        source: PathBuf,
        existing: PathBuf,
    },
    /// `created` is what the UI should select afterwards.
    Finished {
        job: JobId,
        created: Vec<PathBuf>,
    },
    Failed {
        job: JobId,
        error: String,
    },
    Cancelled {
        job: JobId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Replace,
    Skip,
    KeepBoth,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolutionChoice {
    pub resolution: Resolution,
    /// Answer every later conflict of the same job the same way.
    pub apply_to_all: bool,
}

impl ResolutionChoice {
    pub fn once(resolution: Resolution) -> Self {
        Self { resolution, apply_to_all: false }
    }

    pub fn all(resolution: Resolution) -> Self {
        Self { resolution, apply_to_all: true }
    }
}
