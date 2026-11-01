# Trove design

Trove is a file manager for Linux (and macOS through nd's AppKit backend) built on NativeDesktop.
Finder's calm layout and Quick Look, GNOME Files' sidebar and bookmarks, Explorer's tabs, and a
search that finds a file anywhere in the home directory before the user finishes typing.

## Budgets (release build, on e1504g: i3-N305, iGPU, 60 Hz)

| Measure | Budget |
| --- | --- |
| Launch to first frame with the window's chrome | under 200 ms, whatever is mounted |
| First names of a 10k-entry local folder on screen | under 50 ms after navigation |
| A folder on a dead NFS/SMB mount | the window stays responsive; "Waiting for server" after 1 s |
| Search across an indexed home of 500k files, per keystroke | under 30 ms to ranked results |
| Idle CPU, window open, nothing changing | 0% |
| Scrolling a 100k-entry folder | no missed frames at 60 Hz |
| Resident memory with a 100k-entry folder open | under 150 MB |

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

### trove-ops

```rust
pub struct Ops;                                   // one per app, owns a worker thread
impl Ops {
    pub fn new() -> (Ops, flume::Receiver<OpEvent>);
    pub fn submit(&self, op: Op) -> JobId;
    pub fn cancel(&self, job: JobId);
    pub fn resolve(&self, job: JobId, r: Resolution); // answer to OpEvent::Conflict
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
    Compress { paths: Vec<PathBuf>, dest: PathBuf }, Extract { archive: PathBuf, dest_dir: PathBuf },
}
pub enum OpEvent {
    Started { job: JobId, title: String },
    Progress { job: JobId, done_bytes: u64, total_bytes: u64, done_files: u64, total_files: u64, current: PathBuf },
    Conflict { job: JobId, source: PathBuf, existing: PathBuf },
    Finished { job: JobId, created: Vec<PathBuf> },  // created = what to select afterwards
    Failed { job: JobId, error: String },
    Cancelled { job: JobId },
}
pub enum Resolution { Replace, Skip, KeepBoth, Cancel, /* apply_to_all */ }
pub struct ResolutionChoice { pub resolution: Resolution, pub apply_to_all: bool }

// clipboard.rs: x-special/gnome-copied-files and text/uri-list through wl-copy/wl-paste when
// present (interop with GNOME Files, Dolphin, terminals), an in-process copy always.
pub fn copy_files(paths: &[PathBuf], cut: bool);
pub fn paste_files() -> Option<(Vec<PathBuf>, bool /* cut */)>;

// open_with.rs: desktop entries from XDG_DATA_DIRS, defaults from mimeapps.list.
pub fn mime_of(path: &Path) -> String;           // by name; content sniff only when the name says nothing
pub fn apps_for(mime: &str) -> Vec<AppInfo>;     // default first
pub fn open(paths: &[PathBuf]) -> Result<(), String>;               // default app per file
pub fn open_with(app: &AppInfo, paths: &[PathBuf]) -> Result<(), String>; // Exec field codes
pub fn set_default(app: &AppInfo, mime: &str) -> io::Result<()>;
pub struct AppInfo { pub id: String, pub name: String, pub icon: Option<String>, pub exec: String }
```

Rules: a move within one file system is `rename(2)`; across file systems it is copy then
delete, with the delete only after every byte landed. Copies use `std::fs::copy`
(`copy_file_range`, so reflinks where the file system has them) and keep mtime and mode. A
conflict pauses the job until resolved. Trash follows the freedesktop spec (per-volume
`.Trash-$uid` for other mounts) and Restore reads `.trashinfo`. Undo covers rename, move, copy,
trash, new folder, new file and duplicate. Permanent delete and empty trash are not undoable.

### trove-preview

```rust
pub fn preview(path: PathBuf, max_text: usize) -> flume::Receiver<Preview>; // worker thread
pub enum Preview {
    Image { path: PathBuf, width: u32, height: u32 },        // the UI draws it with nd's image()
    Text { text: String, truncated: bool, language: Option<&'static str>, lines: usize },
    Folder { items: u32, hidden: u32, size: Option<u64> /* only when cheap */ },
    Archive { entries: Vec<String>, total: usize },
    Audio { title: Option<String>, artist: Option<String>, album: Option<String>, duration: Option<f64>, cover: Option<PathBuf> },
    Thumbnail { path: PathBuf },                             // video, PDF, fonts... via thumbnailers
    None,
}
pub fn info(path: PathBuf) -> flume::Receiver<Info>;  // size, dates, mode string, owner, mime, link target
pub struct Thumbnails;                                  // bounded worker pool, newest request first
impl Thumbnails {
    pub fn new(threads: usize) -> (Thumbnails, flume::Receiver<(PathBuf, Option<PathBuf>)>);
    pub fn request(&self, path: PathBuf, mtime: SystemTime, size: ThumbSize);
    pub fn cancel_all(&self);
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
    pub fn query(&self, q: Query) -> SearchRun;                 // drop = cancel
}
pub struct Query { pub text: String, pub scope: PathBuf, pub show_hidden: bool, pub limit: usize }
pub struct SearchRun { pub events: flume::Receiver<SearchEvent> }
pub enum SearchEvent { Hits(Vec<Hit>) /* best so far, replaces the previous set */, Done { scanned: u64, elapsed_ms: u64 } }
pub struct Hit { pub path: PathBuf, pub is_dir: bool, pub score: i32, pub name_matches: Vec<(u32, u32)> }
```

Query language: words match the name, all must match, smart case; `"quoted"` is exact;
`*` and `?` make a glob; `ext:pdf`, `kind:image|video|audio|doc|code|folder|archive`,
`size:>10mb`, `modified:today|yesterday|week|month|year|<2026-01-01`, `in:<folder name>`.
Ranking: exact name > name prefix > word boundary > substring > subsequence; then shallower
depth, then newer mtime. Excluded by default: `.cache`, `node_modules`, `.git`, `target`,
`.local/share/Trash`, `.cargo/registry`, `.rustup`, `.npm`, `.bun`, `__pycache__`; hidden
entries only when the query starts with `.` or hidden files are shown. Network mounts are never
indexed; a search scoped inside one walks it live.

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
- Preview pane: the selection's preview and its info; Space opens Quick Look (a large sheet with
  the same content, arrows step through the selection, Space or Escape closes).
- Status bar: item count, selection count and size, free space, running jobs with progress and
  cancel. Conflicts open a sheet: Replace, Skip, Keep both, Apply to all.

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
| Delete | move to Trash; Shift+Delete deletes after a confirm |
| Ctrl+Shift+N | new folder, named inline |
| Ctrl+D | duplicate |
| Ctrl+F | search; Escape clears and returns to the folder |
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

Settings through `nd-settings` (`~/.config/trove/settings.toml`): show hidden, folders first,
default view, single-click to open, search exclusions, thumbnails on network mounts. Session in
the app's state dir: the open tabs with their paths, views and sorts, the window size, the
sidebar and preview widths. Restored at launch without waiting on any of the paths.
