//! Picker mode: Trove as another app's file chooser. `trove --pick` reads a [`Request`] as JSON
//! on stdin, opens one window over it and writes one [`Outcome`] as a JSON line to stdout. The
//! portal (`portal.rs`) runs one such process per request.
//!
//! The window is the usual [`Root`](crate::Root) with a [`Picker`] bar under the listing: the
//! name field when saving, the filter, Cancel and the accept button. Activating a file accepts,
//! Escape cancels.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use nd::prelude::*;
use nd::settings::Store;
use serde::{Deserialize, Serialize};
use trove_fs::Entry;

actions!(trove, [PickAccept, PickCancel]);

/// The name field's id, which takes the keys first when saving.
pub const NAME_FIELD: &str = "pick-name";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// One or more files, or folders with [`Request::directory`].
    #[default]
    Open,
    /// One file: a folder and a name.
    Save,
    /// A folder to save [`Request::files`] into.
    SaveFiles,
}

/// A filter as the portal sends one: its name and its patterns, each a glob (`0`) or a MIME
/// type (`1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    pub patterns: Vec<(u32, String)>,
}

/// What the caller asked for. Paths and names are the portal's byte strings, nul included.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub mode: Mode,
    pub title: String,
    pub accept_label: Option<String>,
    pub multiple: bool,
    pub directory: bool,
    pub filters: Vec<Filter>,
    pub current_filter: Option<Filter>,
    pub current_name: Option<String>,
    pub current_folder: Option<Vec<u8>>,
    pub current_file: Option<Vec<u8>>,
    pub files: Vec<Vec<u8>>,
}

/// How a pick ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    Picked { uris: Vec<String>, filter: Option<Filter> },
    Cancelled,
}

/// A portal byte string as a path: up to its nul.
pub fn bytes_path(bytes: &[u8]) -> Option<PathBuf> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    (end > 0).then(|| PathBuf::from(OsString::from_vec(bytes[..end].to_vec())))
}

impl Request {
    pub fn saving(&self) -> bool {
        self.mode != Mode::Open
    }

    pub fn picks_folders(&self) -> bool {
        self.mode == Mode::Open && self.directory
    }

    /// The accept button's words: the caller's, its mnemonic underscores dropped.
    pub fn accept_label(&self) -> String {
        match self.accept_label.as_deref().filter(|l| !l.trim().is_empty()) {
            Some(label) => strip_mnemonic(label),
            None if self.saving() => "Save".to_owned(),
            None if self.directory => "Select".to_owned(),
            None => "Open".to_owned(),
        }
    }

    pub fn window_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.clone();
        }
        match self.mode {
            Mode::Open if self.directory => "Select Folder",
            Mode::Open if self.multiple => "Open Files",
            Mode::Open => "Open File",
            Mode::Save => "Save File",
            Mode::SaveFiles => "Save Files",
        }
        .to_owned()
    }

    /// The filters the dropdown lists: the caller's, with `current_filter` added when it is
    /// none of them. Empty when picking folders.
    pub fn filter_list(&self) -> (Vec<Filter>, usize) {
        if self.picks_folders() || self.mode == Mode::SaveFiles {
            return (Vec::new(), 0);
        }
        let mut list = self.filters.clone();
        let at = match &self.current_filter {
            Some(f) => list.iter().position(|g| g == f).unwrap_or_else(|| {
                list.push(f.clone());
                list.len() - 1
            }),
            None => 0,
        };
        (list, at)
    }

    /// The name a save starts with: `current_name`, else `current_file`'s.
    pub fn start_name(&self) -> String {
        if let Some(name) = self.current_name.as_ref().filter(|n| !n.is_empty()) {
            return name.clone();
        }
        let file = self.current_file.as_deref().and_then(bytes_path);
        file.and_then(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default()
    }

    pub fn names(&self) -> Vec<PathBuf> {
        self.files.iter().filter_map(|n| bytes_path(n)).collect()
    }
}

/// "_Open" reads "Open"; a doubled underscore is a real one.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The folder a pick opens on: `current_file`'s, then `current_folder`, then the last folder
/// picked, then home.
pub fn start_folder(request: &Request, last: Option<PathBuf>, home: &Path) -> PathBuf {
    let file_dir = request.current_file.as_deref().and_then(bytes_path).and_then(|f| f.parent().map(Path::to_path_buf));
    let folder = request.current_folder.as_deref().and_then(bytes_path);
    file_dir.or(folder).or(last).filter(|p| p.is_absolute()).unwrap_or_else(|| home.to_path_buf())
}

/// A filter compiled for the listing.
pub struct Matcher {
    globs: Vec<glob::Pattern>,
    mimes: Vec<String>,
}

impl Matcher {
    pub fn new(filter: &Filter) -> Self {
        let mut globs = Vec::new();
        let mut mimes = Vec::new();
        for (kind, pattern) in &filter.patterns {
            match kind {
                0 => globs.extend(glob::Pattern::new(pattern).ok()),
                _ => mimes.push(pattern.to_ascii_lowercase()),
            }
        }
        Self { globs, mimes }
    }

    /// Whether a file named `name` passes: a glob matching its name in any case (callers list
    /// `*.jpg` and rarely `*.JPG`), or a MIME type its name gives.
    pub fn matches(&self, name: &str) -> bool {
        let fold = glob::MatchOptions { case_sensitive: false, ..glob::MatchOptions::new() };
        if self.globs.iter().any(|g| g.matches_with(name, fold)) {
            return true;
        }
        if self.mimes.iter().any(|m| m == "*/*" || m == "*") {
            return true;
        }
        let guessed = mime_guess::from_path(name);
        guessed.iter_raw().any(|mime| self.mimes.iter().any(|m| mime_matches(m, mime)))
    }
}

fn mime_matches(pattern: &str, mime: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(major) => mime.split('/').next().is_some_and(|m| m.eq_ignore_ascii_case(major)),
        None => pattern.eq_ignore_ascii_case(mime),
    }
}

/// What a picking tab lists: every folder, and the files the filter passes; no files at all
/// when picking folders.
pub struct Shown {
    pub folders_only: bool,
    pub multiple: bool,
    pub matcher: Option<Matcher>,
}

impl Shown {
    pub fn shows(&self, entry: &Entry, folder: bool) -> bool {
        if folder {
            return true;
        }
        !self.folders_only && self.matcher.as_ref().is_none_or(|m| m.matches(&entry.name))
    }
}

/// What the tab holds when the accept button is pressed.
pub struct Choice {
    pub dir: Option<PathBuf>,
    /// The selection, each path with whether it is a folder.
    pub selected: Vec<(PathBuf, bool)>,
    pub target: Option<(PathBuf, bool)>,
    pub name: String,
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Go into this folder instead of accepting.
    Enter(PathBuf),
    Accept(Vec<PathBuf>),
    /// Accept once the user agrees to replace what exists.
    Replace {
        paths: Vec<PathBuf>,
        title: String,
    },
    Nothing,
}

/// What the accept button does. `listed` says whether a path is in the folder shown, and
/// whether it is a folder: the listing answers, never the disk.
pub fn decide(request: &Request, choice: &Choice, listed: impl Fn(&Path) -> Option<bool>) -> Decision {
    let target_folder = choice.target.as_ref().filter(|(_, folder)| *folder).map(|(p, _)| p.clone());
    match request.mode {
        Mode::Open if request.directory => {
            let folders: Vec<PathBuf> = choice.selected.iter().filter(|(_, f)| *f).map(|(p, _)| p.clone()).collect();
            if folders.is_empty() {
                return choice.dir.clone().map_or(Decision::Nothing, |d| Decision::Accept(vec![d]));
            }
            if request.multiple {
                return Decision::Accept(folders);
            }
            Decision::Accept(vec![target_folder.unwrap_or_else(|| folders[0].clone())])
        }
        Mode::Open => {
            let files: Vec<PathBuf> = choice.selected.iter().filter(|(_, f)| !*f).map(|(p, _)| p.clone()).collect();
            if files.is_empty() {
                return target_folder.map_or(Decision::Nothing, Decision::Enter);
            }
            if request.multiple {
                return Decision::Accept(files);
            }
            let target_file = choice.target.as_ref().filter(|(_, f)| !*f).map(|(p, _)| p.clone());
            Decision::Accept(vec![target_file.unwrap_or_else(|| files[0].clone())])
        }
        Mode::Save => {
            let name = choice.name.trim();
            let Some(dir) = choice.dir.as_ref() else { return Decision::Nothing };
            if name.is_empty() || name == "." || name == ".." {
                return target_folder.map_or(Decision::Nothing, Decision::Enter);
            }
            let path = dir.join(name);
            match listed(&path) {
                Some(true) => Decision::Enter(path),
                Some(false) => Decision::Replace {
                    title: format!("\u{201c}{name}\u{201d} already exists. Replace it?"),
                    paths: vec![path],
                },
                None => Decision::Accept(vec![path]),
            }
        }
        Mode::SaveFiles => {
            if let Some(folder) = target_folder {
                return Decision::Enter(folder);
            }
            let Some(dir) = choice.dir.as_ref() else { return Decision::Nothing };
            let paths: Vec<PathBuf> = request.names().iter().map(|n| dir.join(n)).collect();
            let existing = paths.iter().filter(|p| listed(p).is_some()).count();
            if existing == 0 {
                return Decision::Accept(paths);
            }
            let title = match existing {
                1 => "One of the files already exists. Replace it?".to_owned(),
                n => format!("{n} of the files already exist. Replace them?"),
            };
            Decision::Replace { paths, title }
        }
    }
}

/// Where the outcome goes: stdout for `trove --pick`, a test's record otherwise.
pub type Done = Box<dyn FnOnce(Outcome, &mut App)>;

/// The bar under a picking window's listing, and the request it answers.
pub struct Picker {
    request: Request,
    filters: Vec<Filter>,
    filter: usize,
    name: SharedString,
    /// The file whose name the field took last, so a re-render of the same selection leaves a
    /// typed name alone.
    named_from: Option<PathBuf>,
    done: Option<Done>,
}

impl Picker {
    pub fn new(request: Request, done: Done) -> Self {
        let (filters, filter) = request.filter_list();
        let name = request.start_name().into();
        Self { request, filters, filter, name, named_from: None, done: Some(done) }
    }

    pub fn request(&self) -> &Request {
        &self.request
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.name = name.into();
        cx.notify();
    }

    /// The selection moved onto `file`: a save takes its name, as GTK's does.
    pub fn selected_file(&mut self, file: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.request.mode != Mode::Save || file == self.named_from {
            return;
        }
        self.named_from = file.clone();
        if let Some(name) = file.as_ref().and_then(|f| f.file_name()) {
            self.set_name(name.to_string_lossy().into_owned(), cx);
        }
    }

    pub fn filter_index(&self) -> usize {
        self.filter
    }

    pub fn filter(&self) -> Option<&Filter> {
        self.filters.get(self.filter)
    }

    pub fn set_filter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.filters.len() && index != self.filter {
            self.filter = index;
            cx.notify();
        }
    }

    /// What the tabs list under the current filter.
    pub fn shown(&self) -> Shown {
        Shown {
            folders_only: self.request.picks_folders() || self.request.mode == Mode::SaveFiles,
            multiple: self.request.multiple && self.request.mode == Mode::Open,
            matcher: self.filter().map(Matcher::new),
        }
    }

    pub fn finished(&self) -> bool {
        self.done.is_none()
    }

    /// Ends the pick: the caller gets `outcome`, once.
    pub fn take_done(&mut self) -> Option<Done> {
        self.done.take()
    }

    pub fn picked(&self, paths: &[PathBuf]) -> Outcome {
        Outcome::Picked { uris: paths.iter().map(|p| trove_fs::file_uri(p)).collect(), filter: self.filter().cloned() }
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let me = cx.weak_entity();
        let name = (self.request.mode == Mode::Save).then(|| {
            let stem = crate::tab::stem_chars(&self.name, false);
            h_flex()
                .gap(theme.spacing.sm)
                .items_center()
                .flex_1()
                .min_w_0()
                .child(text("Name").text_color(theme.text.secondary))
                .child(
                    text_field(NAME_FIELD)
                        .text(self.name.clone())
                        .label("Name")
                        .select_on_focus(0..stem)
                        .flex_1()
                        .min_w_0()
                        .on_change({
                            let me = me.clone();
                            move |e: &TextChanged, _, cx| {
                                let _ = me.update(cx, |p, cx| p.set_name(e.text.clone(), cx));
                            }
                        })
                        .on_submit(|_: &TextSubmitted, window, cx| {
                            cx.dispatch_action(window.id(), &PickAccept);
                        })
                        .on_cancel(|_: &TextCancelled, window, cx| {
                            cx.dispatch_action(window.id(), &PickCancel);
                        }),
                )
        });
        let filters = (!self.filters.is_empty()).then(|| {
            let me = me.clone();
            select("pick-filter", self.filters.iter().map(|f| f.name.clone()))
                .label("File type")
                .selected(self.filter)
                .on_change(move |e: &SelectChanged, _, cx| {
                    let _ = me.update(cx, |p, cx| p.set_filter(e.index, cx));
                })
        });
        let spacer = name.is_none().then(|| div().flex_1());
        v_flex().w_full().child(separator()).child(
            h_flex()
                .id("pick-bar")
                .w_full()
                .px(theme.spacing.md)
                .py(theme.spacing.sm)
                .gap(theme.spacing.md)
                .items_center()
                .children(name)
                .children(spacer)
                .children(filters)
                .child(button("pick-cancel", "Cancel").dispatch(PickCancel))
                .child(button("pick-accept", self.request.accept_label()).primary().dispatch(PickAccept)),
        )
    }
}

/// The last folder a pick ended in, for the next pick with no folder of its own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Picked {
    folder: Option<Vec<u8>>,
}

struct Remembered(Store<Picked>);

fn store(cx: &mut App) -> &Store<Picked> {
    if !cx.has_global::<Remembered>() {
        let path = crate::session::state_file("picker.json");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        cx.set_global(Remembered(Store::open(path, 1, None)));
    }
    &cx.global::<Remembered>().0
}

pub fn last_folder(cx: &mut App) -> Option<PathBuf> {
    if crate::env::get().demo {
        return None;
    }
    store(cx).get().folder.as_deref().and_then(bytes_path)
}

/// Keeps `folder` for the next pick. Written at once: the process ends with the pick.
pub fn remember(cx: &mut App, folder: &Path) {
    if crate::env::get().demo {
        return;
    }
    let store = store(cx);
    store.update(|p| p.folder = Some(folder.as_os_str().as_bytes().to_vec()));
    if let Err(e) = store.flush() {
        eprintln!("trove: remembering the picked folder: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(patterns: &[(u32, &str)]) -> Filter {
        Filter { name: "f".into(), patterns: patterns.iter().map(|(k, p)| (*k, (*p).to_owned())).collect() }
    }

    #[test]
    fn globs_and_mime_types_match_names() {
        let images = Matcher::new(&filter(&[(1, "image/*")]));
        assert!(images.matches("sunset.jpg") && images.matches("diagram.svg"));
        assert!(!images.matches("notes.md"));
        let jpeg = Matcher::new(&filter(&[(1, "image/jpeg")]));
        assert!(jpeg.matches("a.JPG") && !jpeg.matches("a.png"));
        let md = Matcher::new(&filter(&[(0, "*.md"), (0, "*.[tT][xX][tT]")]));
        assert!(md.matches("chapter1.md") && md.matches("README.MD") && md.matches("todo.txt"));
        assert!(!md.matches("photo.jpg"));
        assert!(Matcher::new(&filter(&[(1, "*/*")])).matches("anything"));
    }

    #[test]
    fn labels_drop_mnemonics_and_default_per_mode() {
        let mut r = Request { accept_label: Some("_Upload".into()), ..Request::default() };
        assert_eq!(r.accept_label(), "Upload");
        r.accept_label = Some("a__b".into());
        assert_eq!(r.accept_label(), "a_b");
        r.accept_label = None;
        assert_eq!(r.accept_label(), "Open");
        r.directory = true;
        assert_eq!(r.accept_label(), "Select");
        r.mode = Mode::Save;
        assert_eq!(r.accept_label(), "Save");
    }

    #[test]
    fn starts_in_the_file_folder_then_the_folder_then_the_last_pick() {
        let home = Path::new("/home/u");
        let mut r = Request::default();
        assert_eq!(start_folder(&r, None, home), home);
        assert_eq!(start_folder(&r, Some("/srv".into()), home), Path::new("/srv"));
        r.current_folder = Some(b"/tmp/x\0".to_vec());
        assert_eq!(start_folder(&r, Some("/srv".into()), home), Path::new("/tmp/x"));
        r.current_file = Some(b"/data/report.pdf\0".to_vec());
        assert_eq!(start_folder(&r, None, home), Path::new("/data"));
        assert_eq!(r.start_name(), "report.pdf");
        r.current_name = Some("new.pdf".into());
        assert_eq!(r.start_name(), "new.pdf");
    }

    #[test]
    fn the_current_filter_joins_the_list_when_it_is_new() {
        let a = filter(&[(0, "*.a")]);
        let b = filter(&[(0, "*.b")]);
        let mut r = Request { filters: vec![a.clone(), b.clone()], current_filter: Some(b), ..Request::default() };
        assert_eq!(r.filter_list().1, 1);
        r.current_filter = Some(filter(&[(0, "*.c")]));
        let (list, at) = r.filter_list();
        assert_eq!((list.len(), at), (3, 2));
        r.directory = true;
        assert!(r.filter_list().0.is_empty());
    }
}
