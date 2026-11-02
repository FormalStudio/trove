# Trove design

Trove is a file manager for Linux (and macOS through nd's AppKit backend) built on NativeDesktop.
Finder's calm layout and Quick Look, GNOME Files' sidebar and bookmarks, Explorer's tabs, and a
search that finds a file anywhere in the home directory before the user finishes typing.

## Budgets (release build, on e1504g: i3-N305, iGPU, 60 Hz)

| Measure | Budget | Measured |
| --- | --- | --- |
| Launch to first frame with the window's chrome | under 200 ms, whatever is mounted | 178 ms (175 to 190), 0 syscalls on the sshfs mount |
| First names of a 10k-entry local folder on screen | under 50 ms after navigation | 10 ms for 100k entries, all listed in 61 to 98 ms |
| A folder on a dead NFS/SMB mount | the window stays responsive; "Waiting for server" after 1 s | shown at 1064 ms, the window answering in 1 ms |
| Search across an indexed home of 500k files, per keystroke | under 30 ms to ranked results | 0.4 to 2.4 ms median per word (343k entries) |
| Idle CPU, window open, nothing changing | 0% | 0 ticks in 10 s; 2 to 3 ticks in 20 s with a shell `theme.json` present (nd's theme watcher) |
| Scrolling a 100k-entry folder | no missed frames at 60 Hz | p95 16.8 ms, 1 missed frame in 718 (the listing's last batch) |
| Resident memory with a 100k-entry folder open | under 150 MB | 145 MB |

Measured on g815 pinned to its E-cores (`taskset -c 8-15`), on its Intel GPU alone, with
`tests/real_perf.rs` (`ND_REAL=1`, ignored by default).

Nothing that can block runs on the main thread: no stat, no readdir, no statvfs, no thumbnail
decode, no index load. Startup reads only small procfs and config files (`/proc/self/mountinfo`,
`user-dirs.dirs`, the GTK bookmarks file) and never touches a mount point or a bookmark target.

## Crates

- `crates/trove-fs`: entries, kinds, natural sort, streaming listings, the directory watcher,
  volumes (mounts) and places (XDG dirs, bookmarks, recent). No UI types.
- `crates/trove-ops`: the operation queue (copy, move, trash, delete, rename, new folder, restore,
  empty trash), progress, conflicts, cancel, the undo journal, the file clipboard, Open With.
- `crates/trove-preview`: preview content for a path (image, text, folder summary, archive
  listing, audio tags, info) and thumbnails through the freedesktop thumbnail cache and the
  system's `.thumbnailer` entries.
- `crates/trove-search`: the home index (built in the background at idle priority, persisted,
  refreshed) and the query engine; a live parallel walk for scopes the index does not cover.
- root crate `trove`: the nd app.

Core crates take paths and return plain data. They never depend on `nd` or `gpui`. Background
work runs on their own threads and reports over `flume` channels, which the UI drains with
`recv_async` from a task it spawns (`cx.spawn`) or `nd::system::spawn`. Every long job has a
cancel handle; dropping the handle cancels.

## Contracts

These signatures are the seam between the crates. A crate may add to them; it does not change
them without updating this file and every caller.

### trove-fs

```rust
// entry.rs (exists): Entry, EntryId, EntryKind, Meta, FileKind, SortKey, SortOrder,
// natural_cmp, sort_entries.

// listing.rs
pub struct ListOptions { pub show_hidden: bool }       // hidden entries are still listed; the UI filters
pub fn list(dir: PathBuf, opts: ListOptions) -> Listing;
pub struct Listing { pub events: flume::Receiver<ListEvent>, /* drop = cancel */ }
pub enum ListEvent {
    /// Names and d_type from readdir, in chunks of at most 2048, first chunk as soon as it exists.
    Entries(Vec<Entry>),
    /// Metadata for entries already sent, in chunks, stat'd on a small worker pool.
    Meta(Vec<(EntryId, Meta)>),
    /// readdir finished: the full name set is known.
    Listed { total: usize },
    /// Every entry has metadata.
    Complete,
    /// The directory is slow to answer (no first chunk after 1 s). Still running.
    Slow,
    Error(ListError),
    /// Live changes after Listed, from the watcher (local file systems only).
    Changed(Vec<Change>),
}
pub enum Change { Added(Entry), Removed(EntryId), Updated(EntryId, Meta), Renamed { from: EntryId, to: Entry } }
pub enum ListError { NotFound, PermissionDenied, NotADirectory, Io(String) }

// volumes.rs
pub fn volumes() -> Vec<Volume>;                        // procfs only, no statvfs, no stat
pub struct Volume { pub mount_point: PathBuf, pub device: String, pub fs_type: String,
                    pub label: String, pub kind: VolumeKind, pub read_only: bool }
pub enum VolumeKind { Root, Internal, Removable, Network, Phone, Gvfs }
pub fn usage(path: PathBuf, timeout: Duration) -> flume::Receiver<Option<Usage>>; // statvfs on a detached thread
pub struct Usage { pub total: u64, pub free: u64 }
pub fn watch_volumes() -> flume::Receiver<Vec<Volume>>; // poll(POLLPRI) on mountinfo, sends the new set
pub fn is_network_path(path: &Path, vols: &[Volume]) -> bool;
pub fn unmount(v: &Volume) -> flume::Receiver<Result<(), String>>; // udisksctl / gio / fusermount

// places.rs
pub fn user_dirs() -> Vec<Place>;      // Home, Desktop, Documents, Downloads, Music, Pictures, Videos
pub fn bookmarks() -> Vec<Place>;      // ~/.config/gtk-3.0/bookmarks, shared with GNOME Files
pub fn add_bookmark(path: &Path, label: Option<&str>) -> io::Result<()>;
pub fn remove_bookmark(path: &Path) -> io::Result<()>;
pub fn trash_dir() -> PathBuf;
pub fn recent(limit: usize) -> Vec<PathBuf>; // recently-used.xbel, newest first, no stat
pub struct Place { pub label: String, pub path: PathBuf, pub icon: PlaceIcon }
pub enum PlaceIcon { Home, Desktop, Documents, Downloads, Music, Pictures, Videos, Folder, Network, Trash, Recent }
```

Listing rules: the first chunk lands before any stat. Stats run on up to 8 threads for local
file systems and 2 for network ones, nearest-first in the order readdir produced. A directory on
a network file system gets no watcher (inotify cannot see remote changes); the UI refreshes it on
focus and F5. Symlinks are listed with `EntryKind::Symlink`; their target is resolved in the
stat phase.

Implementation notes: `ListOptions` is `Default`. The watcher starts before readdir (so nothing
is missed) but delivers `Changed` only after `Complete`; a delete or move of the watched
directory ends the listing with `Error(NotFound)`. `Volume`s come from mountinfo only, so gvfs
locations (which are directories, not mounts) are not enumerated; `is_network_path` still treats
anything under `/run/user/<uid>/gvfs/` as remote. `unmount` also falls back to `umount` for
nfs/cifs. The inotify watcher makes trove-fs Linux-only for now.

### trove-ops

```rust
pub struct Ops;                                   // one per app, owns a worker thread
impl Ops {
    pub fn new() -> (Ops, flume::Receiver<OpEvent>);
    pub fn with_config(c: Config) -> (Ops, flume::Receiver<OpEvent>); // Config { trash: TrashConfig }, for tests
    pub fn submit(&self, op: Op) -> JobId;
    pub fn cancel(&self, job: JobId);
    pub fn resolve(&self, job: JobId, r: ResolutionChoice); // answer to OpEvent::Conflict
    pub fn undo(&self) -> Option<JobId>;              // inverse of the newest undoable job
    pub fn redo(&self) -> Option<JobId>;
}
pub enum Op {
    Copy { sources: Vec<PathBuf>, dest_dir: PathBuf },
    Move { sources: Vec<PathBuf>, dest_dir: PathBuf },
    Trash { paths: Vec<PathBuf> },
    Delete { paths: Vec<PathBuf> },               // permanent; the UI confirms first
    Rename { path: PathBuf, new_name: String },
    NewFolder { parent: PathBuf, name: String },  // "untitled folder", "untitled folder 2", ...
    NewFile { parent: PathBuf, name: String },
    Restore { trashed: Vec<PathBuf> },            // paths inside Trash/files
    EmptyTrash,
    Duplicate { paths: Vec<PathBuf> },            // "name copy.ext", "name copy 2.ext"
    Compress { paths: Vec<PathBuf>, dest: PathBuf }, // dest: the .zip to write, or a folder to write into
    Extract { archive: PathBuf, dest_dir: PathBuf },
}
pub enum OpEvent {
    Started { job: JobId, title: String },
    Progress { job: JobId, done_bytes: u64, total_bytes: u64, done_files: u64, total_files: u64, current: PathBuf },
    Conflict { job: JobId, source: PathBuf, existing: PathBuf },
    Finished { job: JobId, created: Vec<PathBuf> },  // created = what to select afterwards
    Failed { job: JobId, error: String },
    Cancelled { job: JobId },
}
pub enum Resolution { Replace, Skip, KeepBoth, Cancel }
pub struct ResolutionChoice { pub resolution: Resolution, pub apply_to_all: bool }

// clipboard.rs: x-special/gnome-copied-files and text/uri-list through wl-copy/wl-paste when
// present (interop with GNOME Files, Dolphin, terminals), an in-process copy always.
pub fn copy_files(paths: &[PathBuf], cut: bool);
pub fn paste_files() -> Option<(Vec<PathBuf>, bool /* cut */)>;
pub fn clear();                                  // forget the in-process copy, e.g. after pasting a cut

// open_with.rs: desktop entries from XDG_DATA_DIRS, defaults from mimeapps.list.
pub fn mime_of(path: &Path) -> String;           // by name; content sniff only when the name says nothing
pub fn apps_for(mime: &str) -> Vec<AppInfo>;     // default first
pub fn open(paths: &[PathBuf]) -> Result<(), String>;               // default app per file
pub fn open_with(app: &AppInfo, paths: &[PathBuf]) -> Result<(), String>; // Exec field codes
pub fn set_default(app: &AppInfo, mime: &str) -> io::Result<()>;
pub struct AppInfo {
    pub id: String, pub name: String, pub icon: Option<String>, pub exec: String,
    pub terminal: bool, pub desktop_file: Option<PathBuf>, // Terminal=true; %k
}
```

Rules: jobs run one at a time, in submission order; a conflict blocks the job (and so the
queue) until `resolve` or `cancel`. A move within one file system is `rename(2)`; across file
systems it is copy then delete per top-level source, with the delete only after every byte
landed. Files are copied in 1 MiB steps with `copy_file_range` (a `FICLONE` reflink first, a
read/write loop if the kernel refuses), checking for cancel between steps; a cancelled copy
removes the partial file it was writing and nothing else. mtime, atime and permission bits are
kept and symlinks are copied as symlinks. Fifos, sockets and device nodes are not copied: a move
across file systems copies the rest, keeps every source holding any, and fails saying the
originals were kept. Progress is throttled to about 20 events a second,
and a quick pre-walk (which reports too) fills in the totals. Copying or duplicating into the
folder the source lives in makes "name copy.ext", "name copy 2.ext" with no prompt. Any other
name clash is a conflict: KeepBoth picks "name 2.ext". Replace on a file overwrites it (through
a temporary name, so a failed copy keeps the old one). Replace on a folder merges: the source's
contents are copied or moved into the existing folder, files with the same name are replaced,
files only the target has are kept, and the choice applies to everything inside. Replacing a
folder with a file is refused. Moving or copying a folder into itself or a subfolder fails
with "Cannot move/copy "x" into itself". A rename keeping the same name is a no-op, a
case-only rename works, and an existing different file fails it. Trash follows the freedesktop
spec: the home trash for anything on its file system, else `$topdir/.Trash/$uid` (only when
`.Trash` is a sticky, non-symlink folder) or `$topdir/.Trash-$uid`, with `.trashinfo` files;
a volume with neither usable fails the item with "Cannot move to Trash on this volume" so the
UI can offer permanent delete. Restore reads `.trashinfo` and uses KeepBoth naming when the
original path is taken. EmptyTrash empties the home trash and every mounted volume's.
Extract writes into a hidden staging folder first: a lone top-level entry lands in `dest_dir`
under its own name, anything else in a new folder named after the archive. `.zip` is read
natively; other formats use `bsdtar`, else `tar` for tar archives, else the job fails naming
the missing tool.

Undo: each job records its inverse (rename and move back, trash what a copy, duplicate, new
item, compress or extract created, restore what a trash moved, trash what a restore brought
back). `undo` runs the newest inverse as a job and files that run's own inverse for `redo`; a
new job clears redo. A job that replaced or merged over existing files is not undoable.
Permanent delete and empty trash are not undoable and leave the journal alone.

Open with: apps come from `$XDG_DATA_HOME` and each `$XDG_DATA_DIRS` `applications` folder
(the flatpak export folders are always included), the first folder winning per desktop file
id. Defaults and associations follow `mimeapps.list` (user config, `$XDG_CONFIG_DIRS`, then
the data folders), `MimeType=` and `mimeinfo.cache`; `text/*` falls back to `text/plain`. NoDisplay,
Hidden and a missing TryExec hide an app from `apps_for` only; a NoDisplay default still
opens. `open` groups files by app and falls back to `xdg-open`, then `gio open`. Parsed
entries are cached and reloaded when an applications folder or a `mimeapps.list` changes.
Launched apps get their own session with no stdio, so closing Trove never ends them.
The clipboard mirrors a copy to the system as `x-special/gnome-copied-files` only (`wl-copy`
serves one type per process); `paste_files` also reads `text/uri-list` and a plain-text
absolute path, and an in-process copy yields to anything newer on the system clipboard.

### trove-preview

```rust
pub fn preview(path: PathBuf, max_text: usize) -> flume::Receiver<Preview>; // worker thread
pub enum Preview {
    Image { path: PathBuf, width: u32, height: u32 },        // the UI draws it with nd's image()
    Text { text: String, truncated: bool, language: Option<&'static str>, lines: usize },
    Folder { items: u32, hidden: u32, size: Option<u64> /* only when cheap */ },
    Archive { entries: Vec<String>, total: usize },    // first 200 names; total is a lower bound if counting hit 300 ms
    Audio { title: Option<String>, artist: Option<String>, album: Option<String>, duration: Option<f64>, cover: Option<PathBuf> },
    Thumbnail { path: PathBuf },                             // video, PDF, fonts... via thumbnailers
    None,
}
pub fn info(path: PathBuf) -> flume::Receiver<Info>;  // worker thread; disconnects if the path cannot be stat'd
pub struct Info { pub path: PathBuf, pub is_dir: bool, pub size: u64, pub modified: Option<SystemTime>,
                  pub created: Option<SystemTime>, pub accessed: Option<SystemTime>, pub mode: String /* drwxr-xr-x */,
                  pub owner: String, pub group: String, pub mime: String, pub link_target: Option<PathBuf>,
                  pub dimensions: Option<(u32, u32)> }
// Tests and embedders: Config::rooted(dir) / Config::from_env(), Engine::new(config) with blocking
// preview/thumbnail methods, preview_with(engine, ..), Thumbnails::with_engine(engine, threads).
pub struct Thumbnails;                                  // bounded worker pool, newest request first
impl Thumbnails {
    pub fn new(threads: usize) -> (Thumbnails, flume::Receiver<(PathBuf, Option<PathBuf>)>);
    pub fn request(&self, path: PathBuf, mtime: SystemTime, size: ThumbSize); // LIFO; deduped per (path, size); a repeat moves to the front
    pub fn cancel_all(&self);                                                 // drops queued jobs, running ones still answer
}
pub enum ThumbSize { Normal /* 128 */, Large /* 256 */ }
```

Thumbnails: look up `~/.cache/thumbnails/{normal,large}/<md5(uri)>.png` first (shared with
GNOME, KDE and every other freedesktop app), check `Thumb::MTime`, otherwise generate: images
with the `image` crate (scaled decode), everything else through
`/usr/share/thumbnailers/*.thumbnailer`. Failures go to `fail/trove-<version>/` so a broken file
is not retried on every scroll. Never thumbnail on a network mount unless the user turns it on.

### trove-search

```rust
pub struct Search;  // one per app
impl Search {
    pub fn new(cache_dir: PathBuf, home: PathBuf) -> Search;   // loads nothing yet
    pub fn start_indexing(&self);                               // idle-priority thread; loads or rebuilds
    pub fn index_state(&self) -> IndexState;                    // Absent, Loading, Building { scanned }, Ready { entries, age }
    pub fn query(&self, q: Query) -> SearchRun;                 // drop = cancel; returns at once
    pub fn indexed_root(&self) -> &Path;
    pub fn note_created(&self, path: PathBuf);                  // after the UI's own operations, so results
    pub fn note_removed(&self, path: PathBuf);                  // are right before the next rebuild; a folder
    pub fn note_renamed(&self, from: PathBuf, to: PathBuf);     // carries its contents
}
pub struct Query { pub text: String, pub scope: PathBuf, pub show_hidden: bool, pub limit: usize }
pub struct SearchRun { pub events: flume::Receiver<SearchEvent>, /* private cancel guard */ }
pub enum SearchEvent {
    Hits(Vec<Hit>),  // best so far, best first; replaces the previous set
    Done { scanned: u64, elapsed_ms: u64, truncated: bool },  // truncated: a network scope hit its walk cap
}
pub struct Hit { pub path: PathBuf, pub is_dir: bool, pub score: i32,
                 pub name_matches: Vec<(u32, u32)> }  // byte ranges [start, end) of the file name
```

Query language: words match the name, all must match, smart case (per word: an uppercase
letter makes that word case-sensitive); a word not in the name may match a folder on the path
below the scope, as long as one word matches the name (`.config hypr`); `"quoted"` is a literal
phrase, spaces kept, never fuzzy; `*` and `?` make a glob over the whole name; `ext:pdf` (or
`ext:jpg,png`), `kind:image|video|audio|doc|code|folder|archive` (also `text`, `pdf`, `font`,
`app`; a prefix like `kind:im` works), `size:>10mb` (`>`, `>=`, `<`, `<=`, `=`, bare means at
least; `k`/`m`/`g`/`t` decimal, `kib`/`mib`/`gib` binary),
`modified:today|yesterday|week|month|year|<2026-01-01|>2026-01-01|2026-01-01`,
`in:<folder name>` (a folder of that name, any case, on the path from the scope down). A filter whose value does not parse yet is ignored.
Ranking: exact name > name prefix (the name minus its extension first) > word boundary (after
`_ - . space`, camelCase humps) > substring > subsequence; then shallower depth, then newer
mtime; a folder whose whole name is the query gets a small boost. Subsequence matches are only
looked for when there are fewer substring hits than `limit`. Excluded by default: `.cache`,
`node_modules`, `.git`, `target`, `.local/share/Trash`, `.cargo/registry`, `.rustup`, `.npm`,
`.bun`, `__pycache__`; hidden entries only when the query starts with `.` or hidden files are
shown. Network, FUSE and pseudo mounts are never indexed, nor is any mount from another device
(a USB stick under the home directory); symlinks are never followed.

The index lives in `cache_dir/index-v1.bin` (rebuilt when missing, corrupt or another version).
`start_indexing` loads it (Loading, then Ready) and rebuilds in the background when it is over
30 minutes old, the old one serving queries meanwhile. A scope outside the index (outside the
root, excluded, on another mount, or before the index is Ready) is walked live, `Hits` improving
every 50 ms; a network scope's walk stops at depth 4 or 20k entries with `truncated`. Once the
index is over 5 minutes old a query also walks its scope 3 levels deep to catch new files.

## Interface

```
┌───────────────────────────────────────────────────────────────────────────────────┐
│ ◀ ▶  Home › Developer › ecosystem       [≡ ▦ ▥]   [⌕ Search Home      ]   [▣]      │  toolbar
│ ┌ ecosystem ×┐┌ Downloads ×┐ +                                                    │  tab strip
├──────────────┬───────────────────────────────────────────────┬────────────────────┤
│ Favourites   │ Name                     Size   Kind  Modified │                    │
│  ⌂ Home      │ ▸ crates                 12     Folder Today   │   preview          │
│  ▭ Desktop   │   Cargo.toml             1.6 kB Code  Today    │                    │
│  ↓ Downloads │   ...                                          │   Name.ext         │
│ Bookmarks    │                                                │   1.6 kB · Code    │
│ Devices      │                                                │   Modified ...     │
│  ◉ Root  45% │                                                │                    │
│  ◉ USB    ⏏  │                                                │                    │
│ Network      │                                                │                    │
│  Trash       │ 23 items · 3 selected (4.1 MB) · 120 GB free   │                    │
└──────────────┴───────────────────────────────────────────────┴────────────────────┘
```

- Toolbar: back, forward, the path bar (breadcrumb segments; click one to go there, right-click
  for its siblings, Ctrl+L or a click on the empty end turns it into a text field with path
  completion), view switch (list, grid, columns), the search field (scoped to the current
  folder, a toggle to search everywhere), the preview toggle.
- Tabs, Explorer-style: Ctrl+T new tab at the current folder, Ctrl+W close, Ctrl+Tab and
  Ctrl+Shift+Tab, Ctrl+1..9, middle-click a folder or Ctrl+Enter to open it in a new tab, drag a
  file onto a tab to move it there. Each tab keeps its own history, selection, scroll, view and
  sort. The tab strip hides with one tab, like GNOME Files.
- Sidebar: Favourites (XDG dirs, Recent), Bookmarks (GTK file, drag a folder in to add),
  Devices (internal and removable volumes, a usage bar filled in when statvfs answers, an eject
  button), Network (network mounts, never touched until clicked), Trash.
- List view: a `table` with Name, Size, Kind, Modified (Created optional), sortable, folders
  first, natural order, folders expandable inline (disclosure, Right/Left keys) like Finder.
- Grid view: icons with thumbnails, filled in as they arrive.
- Columns view: Finder's Miller columns, one `list` per level, the last column the preview.
- Preview pane: the selection's preview and its info, loaded 60 ms after the selection settles;
  several selected show their count and total size. Space opens Quick Look (a cover over the
  window with the same content larger and "Open with <app>"; arrows step through the selection,
  or through the folder with one selected; Space or Escape closes).
- Search results: from the second character, a recursive search of the folder (or Home, by the
  scope switch) replaces the listing with Name (matches in the accent), Folder, Kind, Modified.
  The preview follows the result selection; Escape puts the folder back with its selection.
- Status bar: item count, selection count and size, free space, running jobs with progress and
  cancel. Conflicts open a sheet: Replace (Merge for two folders), Skip, Keep both, Apply to
  all, Cancel. A failure shows as a toast with the reason.
- Context menus: on files (Open, Open with, Cut, Copy, Duplicate, Rename, Compress, Move to
  Trash, and Restore in the Trash), on empty space (New folder, New file, Paste, Show hidden,
  Sort by, View), on the sidebar and on the tab strip. The Trash lists Original location and
  Date deleted from `.trashinfo`, under a banner with Empty Trash.
- Drag and drop: rows and tiles onto folders, breadcrumb segments and the folder shown; one
  file system moves, another copies. Files dropped from other apps land in the folder shown.

### Keys

| Key | Action |
| --- | --- |
| Enter | open (folders navigate, files open in their default app) |
| F2 | rename inline (the name without the extension selected) |
| Space | Quick Look |
| Backspace, Alt+Up | parent folder |
| Alt+Left, Alt+Right, mouse back/forward | history |
| Alt+Down | open the selected folder |
| Ctrl+C, Ctrl+X, Ctrl+V | copy, cut, paste (Ctrl+Shift+V pastes into the selected folder) |
| Ctrl+Z, Ctrl+Shift+Z | undo, redo |
| Delete | move to Trash (in the Trash, delete after a confirm); Shift+Delete deletes after a confirm |
| Ctrl+Backspace, in the Trash | restore to the original location |
| Ctrl+Shift+N | new folder, named inline |
| Ctrl+D | duplicate |
| Ctrl+F | search; Escape clears and returns to the folder |
| Down, in the search field | into the results |
| Ctrl+Enter, Alt+Enter, on a result | show in its enclosing folder, selected |
| Ctrl+L | edit the path |
| Ctrl+H | show hidden files (per window, remembered) |
| Ctrl+1, Ctrl+2, Ctrl+3 | list, grid, columns view |
| Ctrl+I | show the preview pane |
| Ctrl+A | select all |
| F5, Ctrl+R | reload |
| typing letters | type-select: jump to the first name starting with what was typed |

Logic over habit: Enter opens (Finder renames on Enter; nobody expects that), Backspace goes up,
a click on empty space clears the selection, a rename that keeps the name changes nothing,
pasting into the folder a file came from makes "name copy", and every operation selects what it
produced.

## State

Settings through `nd-settings` (`~/.config/trove/config.toml`): show hidden, folders first,
default view, single-click to open, confirm before trash (off), search exclusions, thumbnails
on network mounts (off). Session in `$XDG_STATE_HOME/trove/session.json`, written a moment after
each change on its own thread: the open tabs with their paths, views and sorts, the active tab,
the window size and whether the preview shows. Restored at launch, when no path is given,
without waiting on any of the paths. The sidebar and preview widths are not kept: nd's panes
report no width change yet.
