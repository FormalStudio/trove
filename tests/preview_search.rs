#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: the preview pane, Quick Look, thumbnails, search results and the columns view on
//! `TestBackend`, over one copy of the demo fixture per process with real images and text
//! added under `look/`. Thumbnails land in the fixture's own cache, never `~/.cache`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use nd::prelude::*;
use nd::test::{Size, TestApp, TextProps, by_test_id};
use trove::actions::*;
use trove::keys::{
    CloseQuickLook, ColumnsView, FocusResults, QuickLook, QuickLookNext, QuickLookPrevious, ShowInFolder,
};
use trove::{Location, Root, Scope, Tab, View};
use trove_preview::Preview as Content;

const WAIT: Duration = Duration::from_secs(5);

/// The fixture's home, written once per process.
fn home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::Builder::new().prefix("trove-ps").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap().keep();
        let home = trove::demo_root_in(&dir);
        let look = home.join("look");
        fs::create_dir_all(look.join("sub")).unwrap();
        for (name, w, h, rgb) in [("photo.png", 64, 48, [200, 60, 40]), ("second.png", 32, 32, [40, 120, 220])] {
            image::RgbImage::from_pixel(w, h, image::Rgb(rgb)).save(look.join(name)).unwrap();
        }
        let pics = look.join("pics");
        fs::create_dir_all(&pics).unwrap();
        for (name, rgb) in [("a.png", [220, 180, 40]), ("b.png", [60, 170, 90]), ("c.png", [120, 80, 200])] {
            image::RgbImage::from_pixel(400, 300, image::Rgb(rgb)).save(pics.join(name)).unwrap();
        }
        let text: String = (1..=300).map(|n| format!("line {n}\n")).collect();
        fs::write(look.join("long.txt"), text).unwrap();
        fs::write(look.join("report-final.txt"), "final\n").unwrap();
        fs::write(look.join("sub/report-draft.md"), "# draft\n").unwrap();
        fs::write(look.join("sub/notes.md"), "notes\n").unwrap();
        fs::write(home.join("report-home.txt"), "home\n").unwrap();
        home
    })
}

fn look() -> PathBuf {
    home().join("look")
}

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
}

impl Harness {
    fn at(dir: PathBuf) -> Self {
        let _ = home();
        let mut cx = TestApp::new();
        let window = cx.open_window(Size { width: 1400., height: 900. }, |window, cx| {
            trove::init(cx);
            cx.new(|cx| Root::new(trove::Start::Path(dir), window, cx))
        });
        let mut h = Self { cx, window };
        h.settle();
        h
    }

    fn root(&self) -> Entity<Root> {
        self.window.root().clone()
    }

    fn tab(&self) -> Entity<Tab> {
        self.root().read(&self.cx).active_tab().clone()
    }

    fn preview(&self) -> Entity<trove::Preview> {
        self.root().read(&self.cx).preview().clone()
    }

    fn quicklook(&self) -> Entity<trove::QuickLook> {
        self.root().read(&self.cx).quicklook().clone()
    }

    /// Everything the workers send, applied, with the observers and a frame run in between:
    /// the selection moves the preview, which then loads.
    fn settle(&mut self) {
        for _ in 0..4 {
            self.cx.run_until_idle();
            self.root().update(&mut self.cx, |r, cx| r.drain(WAIT, cx));
            self.tab().update(&mut self.cx, |t, cx| t.drain_extras(WAIT, cx));
            self.cx.run_until_idle();
            self.preview().update(&mut self.cx, |p, cx| p.drain(WAIT, cx));
            self.quicklook().update(&mut self.cx, |q, cx| q.drain(WAIT, cx));
            trove::drain_thumbnails(WAIT, &mut self.cx);
        }
        self.cx.run_until_idle();
    }

    fn act(&mut self, action: impl Action) {
        let id = self.window.id();
        self.cx.dispatch_action(id, &action);
        self.settle();
    }

    fn select(&mut self, names: &[&str]) {
        self.tab().update(&mut self.cx, |t, cx| t.select_names(names, cx));
        self.settle();
        assert_eq!(self.tab().read(&self.cx).selected_names(), names);
    }

    fn search(&mut self, text: &str) {
        self.act(FocusSearch);
        let window = self.window.id();
        let field = by_test_id(&mut self.cx, window, "search").expect("a search field");
        self.cx.emit(field.id, nd::components::TextChanged { text: text.into() });
        self.settle();
    }

    fn snapshot(&mut self) -> String {
        self.cx.run_until_idle();
        self.cx.semantic_tree().snapshot()
    }

    fn has(&mut self, test_id: &str) -> bool {
        let window = self.window.id();
        by_test_id(&mut self.cx, window, test_id).is_some()
    }

    fn title(&self) -> Option<String> {
        self.preview().read(&self.cx).title()
    }

    fn result_names(&self) -> Vec<String> {
        self.tab().read(&self.cx).results().names()
    }

    fn location(&self) -> Location {
        self.tab().read(&self.cx).location().clone()
    }
}

#[test]
fn preview_pane_follows_the_selection() {
    let mut h = Harness::at(look());
    assert!(!h.root().read(&h.cx).shows_preview());
    h.act(TogglePreview);
    assert!(h.root().read(&h.cx).shows_preview());
    assert_eq!(h.title().as_deref(), Some("look"), "nothing selected shows the folder");

    h.select(&["photo.png"]);
    assert_eq!(h.title().as_deref(), Some("photo.png"));
    let content = h.preview().read(&h.cx).loaded().content.clone();
    assert!(matches!(content, Some(Content::Image { width: 64, height: 48, .. })), "{content:?}");
    let tree = h.snapshot();
    assert!(tree.contains("64 \u{d7} 48"), "dimensions row in\n{tree}");
    assert!(tree.contains("PNG image") || tree.contains("Image"), "kind row in\n{tree}");

    h.select(&["long.txt"]);
    let content = h.preview().read(&h.cx).loaded().content.clone();
    assert!(matches!(content, Some(Content::Text { .. })), "{content:?}");
    let tree = h.snapshot();
    assert!(tree.contains("line 1"), "the text's first line in\n{tree}");
    assert!(tree.contains("Showing the first 200 lines"), "the truncation note in\n{tree}");

    h.select(&["sub"]);
    let content = h.preview().read(&h.cx).loaded().content.clone();
    assert!(matches!(content, Some(Content::Folder { items: 2, .. })), "{content:?}");

    h.select(&["photo.png", "second.png"]);
    assert_eq!(h.title().as_deref(), Some("2 items"));
    let total = h.preview().read(&h.cx).loaded().total;
    let want =
        fs::metadata(look().join("photo.png")).unwrap().len() + fs::metadata(look().join("second.png")).unwrap().len();
    assert_eq!(total, Some(want));
}

#[test]
fn a_hidden_pane_loads_nothing() {
    let mut h = Harness::at(look());
    h.select(&["photo.png"]);
    assert!(h.preview().read(&h.cx).loaded().content.is_none());
    h.act(TogglePreview);
    assert!(h.preview().read(&h.cx).loaded().content.is_some(), "showing the pane loads the selection");
}

#[test]
fn a_quick_change_of_selection_keeps_only_the_last() {
    let mut h = Harness::at(look());
    h.act(TogglePreview);
    h.tab().update(&mut h.cx, |t, cx| t.select_names(&["long.txt"], cx));
    h.cx.run_until_idle();
    h.select(&["photo.png"]);
    assert_eq!(h.title().as_deref(), Some("photo.png"));
    let content = h.preview().read(&h.cx).loaded().content.clone();
    assert!(matches!(content, Some(Content::Image { .. })), "the text's answer was dropped: {content:?}");
}

#[test]
fn quick_look_opens_steps_through_the_folder_and_closes() {
    let mut h = Harness::at(look());
    h.select(&["photo.png"]);
    h.act(QuickLook);
    assert!(h.quicklook().read(&h.cx).is_open());
    assert!(h.has("quicklook"));
    assert_eq!(h.quicklook().read(&h.cx).item().map(|i| i.name.clone()).as_deref(), Some("photo.png"));
    let content = h.quicklook().read(&h.cx).loaded().content.clone();
    assert!(matches!(content, Some(Content::Image { .. })), "{content:?}");

    let names = h.tab().read(&h.cx).visible_names();
    let at = names.iter().position(|n| n == "photo.png").unwrap();
    h.act(QuickLookNext);
    let next = &names[at + 1];
    assert_eq!(h.quicklook().read(&h.cx).item().map(|i| i.name.clone()).as_ref(), Some(next));
    assert_eq!(&h.tab().read(&h.cx).selected_names(), std::slice::from_ref(next), "the selection moves with it");
    h.act(QuickLookPrevious);
    assert_eq!(h.quicklook().read(&h.cx).item().map(|i| i.name.clone()).as_deref(), Some("photo.png"));

    h.act(CloseQuickLook);
    assert!(!h.quicklook().read(&h.cx).is_open());
    assert!(!h.has("quicklook"));
}

#[test]
fn quick_look_steps_through_a_selection_of_several() {
    let mut h = Harness::at(look());
    h.select(&["long.txt", "second.png"]);
    h.act(QuickLook);
    let name = |h: &Harness| h.quicklook().read(&h.cx).item().map(|i| i.name.clone()).unwrap();
    assert_eq!(name(&h), "long.txt");
    h.act(QuickLookNext);
    assert_eq!(name(&h), "second.png");
    h.act(QuickLookNext);
    assert_eq!(name(&h), "long.txt", "it wraps round the selection");
    assert_eq!(h.tab().read(&h.cx).selected_names(), ["long.txt", "second.png"], "the selection stays");
}

#[test]
fn grid_tiles_get_thumbnails_in_the_fixture_cache() {
    let mut h = Harness::at(look().join("pics"));
    h.act(GridView);
    assert_eq!(h.tab().read(&h.cx).view(), View::Grid);
    let thumb = trove::thumbnail(&h.cx, &look().join("pics/a.png")).expect("a thumbnail for a rendered tile");
    assert!(thumb.exists());
    assert!(!thumb.starts_with(std::env::home_dir().unwrap().join(".cache")), "{}", thumb.display());
    assert!(thumb.starts_with(home().parent().unwrap()), "{}", thumb.display());
}

#[test]
fn search_lists_matches_below_the_folder_and_escape_restores_it() {
    let mut h = Harness::at(look());
    h.select(&["photo.png"]);
    h.search("r");
    assert!(!h.tab().read(&h.cx).showing_results(), "one character only filters");

    h.search("report");
    assert!(h.tab().read(&h.cx).showing_results());
    let mut names = h.result_names();
    names.sort();
    assert_eq!(names, ["report-draft.md", "report-final.txt"]);
    let results = h.tab().read(&h.cx).results();
    assert_eq!(results.place_of("report-draft.md").as_deref(), Some("look/sub"));
    assert_eq!(results.matches_of("report-final.txt"), Some(vec![(0, 6)]));
    assert!(h.has("results-table"));
    assert!(h.has("search-scope"), "a folder other than Home offers the scope switch");

    h.act(ClearSearch);
    assert!(!h.tab().read(&h.cx).showing_results());
    assert_eq!(h.tab().read(&h.cx).selected_names(), ["photo.png"], "the folder's selection is back");
}

/// The spans of the rich text drawn for `name`, as (text, accent, semibold).
fn drawn_spans(h: &TestApp, name: &str) -> Vec<(String, bool, bool)> {
    let backend = h.backend();
    let found = backend.find(|_, p| p.downcast_ref::<TextProps>().is_some_and(|t| t.rich.is_some() && t.text == name));
    let id = *found.first().unwrap_or_else(|| panic!("no rich text drawn for {name}"));
    let props = backend.props(id).and_then(|p| p.downcast_ref::<TextProps>()).and_then(|t| t.rich.clone()).unwrap();
    props
        .spans()
        .iter()
        .map(|s| (s.text.to_string(), s.style.color() == Some(TextColor::Accent), s.style.semibold))
        .collect()
}

#[test]
fn result_names_draw_their_matches_in_the_accent() {
    let mut h = Harness::at(look());
    h.search("draft");
    assert_eq!(
        drawn_spans(&h.cx, "report-draft.md"),
        [("report-".to_owned(), false, false), ("draft".to_owned(), true, true), (".md".to_owned(), false, false)]
    );
}

#[test]
fn the_scope_row_marks_the_scope_searched() {
    let mut h = Harness::at(look());
    h.search("report");
    let window = h.window.id();
    let checked = |h: &mut Harness, id: &str| by_test_id(&mut h.cx, window, id).expect(id).checked;
    assert_eq!(checked(&mut h, "search-scope-folder"), Some(true));
    assert_eq!(checked(&mut h, "search-scope-home"), Some(false));
    nd::test::click(&mut h.cx, window, "search-scope-home");
    h.settle();
    assert_eq!(h.tab().read(&h.cx).results().scope(), Scope::Home);
    assert_eq!(checked(&mut h, "search-scope-folder"), Some(false));
    assert_eq!(checked(&mut h, "search-scope-home"), Some(true));
    assert!(h.result_names().contains(&"report-home.txt".to_owned()), "{:?}", h.result_names());
}

#[test]
fn search_scope_home_reaches_past_the_folder() {
    let mut h = Harness::at(look());
    h.search("report");
    h.tab().update(&mut h.cx, |t, cx| t.set_search_scope(Scope::Home, cx));
    h.settle();
    assert!(h.result_names().contains(&"report-home.txt".to_owned()), "{:?}", h.result_names());
    assert_eq!(h.tab().read(&h.cx).results().scope(), Scope::Home);
}

#[test]
fn results_preview_and_show_in_folder() {
    let mut h = Harness::at(look());
    h.act(TogglePreview);
    h.search("draft");
    assert_eq!(h.result_names(), ["report-draft.md"]);
    h.act(FocusResults);
    assert_eq!(h.title().as_deref(), Some("report-draft.md"), "the preview follows the result");
    let tree = h.snapshot();
    assert!(tree.contains("Where"), "a result's preview says where it is\n{tree}");

    h.act(ShowInFolder);
    assert_eq!(h.location(), Location::Dir(look().join("sub")));
    assert!(!h.tab().read(&h.cx).showing_results());
    assert_eq!(h.tab().read(&h.cx).query(), "");
    assert_eq!(h.tab().read(&h.cx).selected_names(), ["report-draft.md"]);
}

#[test]
fn columns_view_walks_folders_left_and_right() {
    let mut h = Harness::at(look());
    h.act(ColumnsView);
    assert_eq!(h.tab().read(&h.cx).view(), View::Columns);
    assert!(h.has("files-columns"));
    let ancestors = h.tab().read(&h.cx).column_view().ancestors();
    assert_eq!(ancestors, [home().to_path_buf()], "one column per folder above, from Home");

    h.select(&["sub"]);
    assert_eq!(h.tab().read(&h.cx).column_view().child(), Some(look().join("sub")));
    let mut inside = h.tab().read(&h.cx).column_view().names_in(&look().join("sub"));
    inside.sort();
    assert_eq!(inside, ["notes.md", "report-draft.md"]);

    h.act(MoveRight);
    assert_eq!(h.location(), Location::Dir(look().join("sub")));
    assert_eq!(h.tab().read(&h.cx).column_view().ancestors(), [home().to_path_buf(), look()]);

    h.select(&["notes.md"]);
    assert!(h.has("column-preview"), "a file shows its preview in the last column");

    h.act(MoveLeft);
    assert_eq!(h.location(), Location::Dir(look()));
    assert_eq!(h.tab().read(&h.cx).selected_names(), ["sub"], "going left selects the folder left");
}

#[test]
fn the_status_line_counts_results_while_they_show() {
    let mut h = Harness::at(home().to_path_buf());
    h.search("c");
    h.search("ch");
    h.search("chap");
    assert!(h.tab().read(&h.cx).showing_results());
    h.act(FocusResults);
    let found = h.tab().read(&h.cx).results().len();
    let window = h.window.id();
    let line = by_test_id(&mut h.cx, window, "status-items").expect("the status line");
    let drawn = line.name.clone();
    assert_eq!(drawn, format!("{found} results \u{b7} 1 selected"), "{}", h.snapshot());
}
