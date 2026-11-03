#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Tier 1: the views themselves (the list's columns, the Trash, the columns view's rows, the
//! zoom steps and single click), on `TestBackend`, each test on its own copy of the demo
//! fixture: zoom and single click write the settings file, which lives inside it.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use nd::prelude::*;
use nd::test::{Size, TestApp, by_test_id};
use nd::virtual_list::{RowActivated, RowsSelected};
use trove::actions::*;
use trove::keys::{ColumnsView, ZoomIn, ZoomOut, ZoomReset};
use trove::zoom::{ROWS, TILES, Zoom};
use trove::{Location, Root, Tab};
use trove_preview::ThumbSize;

const WAIT: Duration = Duration::from_secs(5);

struct Harness {
    cx: TestApp,
    window: WindowHandle<Root>,
    home: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// A window on a fresh fixture, with `prepare` run on its home before the window opens.
    fn with(prepare: impl FnOnce(&std::path::Path)) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let home = trove::demo_root_in(dir.path());
        prepare(&home);
        let mut cx = TestApp::new();
        let start = home.clone();
        let window = cx.open_window(Size { width: 1200., height: 800. }, |window, cx| {
            trove::init(cx);
            cx.new(|cx| Root::new(trove::Start::Path(start), window, cx))
        });
        let mut h = Self { cx, window, home, _dir: dir };
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

    fn settle(&mut self) {
        for _ in 0..3 {
            self.cx.run_until_idle();
            self.root().update(&mut self.cx, |r, cx| r.drain(WAIT, cx));
            self.tab().update(&mut self.cx, |t, cx| t.drain_extras(WAIT, cx));
            self.cx.run_until_idle();
            self.preview().update(&mut self.cx, |p, cx| p.drain(WAIT, cx));
            trove::drain_thumbnails(WAIT, &mut self.cx);
        }
        self.cx.run_until_idle();
    }

    fn act(&mut self, action: impl Action) {
        let id = self.window.id();
        self.cx.dispatch_action(id, &action);
        self.settle();
    }

    fn go(&mut self, rel: &str) {
        let to = self.home.join(rel);
        self.act(GoTo(to));
    }

    fn location(&self) -> Location {
        self.tab().read(&self.cx).location().clone()
    }

    fn has(&mut self, test_id: &str) -> bool {
        let id = self.window.id();
        by_test_id(&mut self.cx, id, test_id).is_some()
    }

    fn node(&mut self, test_id: &str) -> nd::core::NodeId {
        let id = self.window.id();
        by_test_id(&mut self.cx, id, test_id)
            .unwrap_or_else(|| panic!("no `{test_id}` in\n{}", self.cx.semantic_tree().snapshot()))
            .id
    }

    fn text(&mut self) -> String {
        let id = self.window.id();
        nd::test::text(&mut self.cx, id)
    }

    fn select(&mut self, names: &[&str]) {
        self.tab().update(&mut self.cx, |t, cx| t.select_names(names, cx));
        self.settle();
        assert_eq!(self.tab().read(&self.cx).selected_names(), names);
    }

    /// A plain click on the row or tile showing `name`.
    fn tap(&mut self, name: &str) {
        let node = self.node(&trove::row_id(name));
        self.cx.emit(node, TapEvent::new(nd::core::Point { x: 4., y: 4. }, 1));
        self.settle();
    }

    fn single_click(&mut self, on: bool) {
        trove::settings::set(&self.cx, "single_click", &on);
        assert_eq!(trove::settings::get(&self.cx).single_click, on);
    }
}

#[test]
fn a_wide_list_grows_the_names_and_leaves_no_empty_band() {
    let mut h = Harness::new();
    h.tab().update(&mut h.cx, |t, cx| t.set_width(1700., cx));
    let columns = h.tab().read(&h.cx).columns();
    let width = |id: &str| columns.iter().find(|c| c.id.as_ref() == id).map(|c| c.width).unwrap();
    assert_eq!(width("name"), 850., "the names take half of a wide window");
    let total: f32 = columns.iter().map(|c| c.width).sum();
    assert!((total - 1700.).abs() < 0.5, "the columns fill the width: {total}");
    let details = [width("size"), width("kind"), width("modified")];
    let spread = details.iter().map(|w| w - details[0]).collect::<Vec<_>>();
    assert!(width("size") > 96. && spread[1] > 0. && spread[2] > 0., "the details share the rest: {details:?}");

    h.tab().update(&mut h.cx, |t, cx| t.set_width(900., cx));
    let columns = h.tab().read(&h.cx).columns();
    let name = columns.iter().find(|c| c.id.as_ref() == "name").map(|c| c.width).unwrap();
    assert_eq!(name, 900. - 96. - 132. - 168., "a narrow window gives the names what is left");
}

#[test]
fn an_empty_trash_says_so_without_the_empty_button() {
    let mut h = Harness::new();
    h.go(".local/share/Trash/files");
    assert!(h.tab().read(&h.cx).is_trash());
    assert!(!h.has("trash-banner"), "nothing to empty");
    assert!(h.has("trash-empty"), "{}", h.text());
    let text = h.text();
    assert!(text.contains("Trash is empty") && !text.contains("Folder is empty"), "{text}");
}

#[test]
fn a_trash_with_items_offers_to_empty_it() {
    let mut h = Harness::with(|home| fs::write(home.join(".local/share/Trash/files/old.txt"), "x").unwrap());
    h.go(".local/share/Trash/files");
    assert!(h.has("trash-banner"));
    assert!(!h.has("trash-empty"));
}

#[test]
fn the_columns_view_draws_the_rename_field() {
    let mut h = Harness::new();
    h.act(ColumnsView);
    h.select(&["Notes.md"]);
    h.act(Rename);
    assert!(h.tab().read(&h.cx).renaming().is_some());
    assert!(h.has(trove::RENAME_FIELD), "F2 in the columns view shows the field it focuses");

    h.act(Deselect);
    h.act(NewFolder);
    let r = h.tab().read(&h.cx).renaming().cloned().expect("the new folder is in rename");
    assert_eq!(r.text.as_ref(), "untitled folder");
    assert!(h.has(trove::RENAME_FIELD), "and so does New folder");
}

#[test]
fn zoom_steps_each_view_and_is_remembered() {
    let mut h = Harness::new();
    assert_eq!(h.tab().read(&h.cx).zoom(), Zoom::default());

    h.act(ZoomIn);
    assert_eq!(h.tab().read(&h.cx).zoom().row(), ROWS[1]);
    h.act(ZoomIn);
    h.act(ZoomIn);
    assert_eq!(h.tab().read(&h.cx).zoom().list, ROWS.len() - 1, "held at the largest");
    assert_eq!(usize::from(trove::settings::get(&h.cx).list_zoom), ROWS.len() - 1);

    h.act(GridView);
    h.act(ZoomOut);
    let zoom = h.tab().read(&h.cx).zoom();
    assert_eq!((zoom.list, zoom.grid), (ROWS.len() - 1, 0), "the grid has its own size");
    assert_eq!(trove::settings::get(&h.cx).grid_zoom, 0);

    h.act(NewTab);
    assert_eq!(h.tab().read(&h.cx).zoom(), zoom, "a new tab opens at the sizes in use");

    h.act(GridView);
    h.act(ZoomReset);
    assert_eq!(h.tab().read(&h.cx).zoom().grid, Zoom::default().grid);
    h.act(ListView);
    h.act(ZoomReset);
    assert_eq!(h.tab().read(&h.cx).zoom(), Zoom::default());
    let saved = trove::settings::get(&h.cx);
    assert_eq!((saved.list_zoom, saved.grid_zoom), (0, Zoom::default().grid as u8));
}

#[test]
fn big_tiles_ask_for_large_thumbnails() {
    assert_eq!(trove::zoom::thumb_size(TILES[Zoom::default().grid].art.0), ThumbSize::Normal);
    assert_eq!(trove::zoom::thumb_size(TILES[TILES.len() - 1].art.0), ThumbSize::Large);

    let mut h = Harness::with(|home| {
        let dir = home.join("shots");
        fs::create_dir_all(&dir).unwrap();
        let img = image::RgbImage::from_fn(600, 400, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 90]));
        img.save(dir.join("wide.png")).unwrap();
    });
    h.go("shots");
    h.act(GridView);
    for _ in 0..TILES.len() {
        h.act(ZoomIn);
    }
    let large = h.home.with_file_name("trove-demo-cache").join("thumbnails/large");
    let made = fs::read_dir(&large).map(|d| d.count()).unwrap_or(0);
    assert_eq!(made, 1, "one large thumbnail in {}", large.display());
}

#[test]
fn single_click_opens_folders_in_the_list_and_grid() {
    let mut h = Harness::new();
    h.tap("Documents");
    assert_eq!(h.location(), Location::Dir(h.home.clone()), "a click only selects by default");

    h.single_click(true);
    h.tap("Documents");
    assert_eq!(h.location(), Location::Dir(h.home.join("Documents")));

    // The second press of a double click lands on the folder just opened; it opens nothing.
    let table = h.node("files-table");
    let row = {
        let t = h.tab().read(&h.cx);
        let id = t.visible().first().copied().unwrap();
        t.list_state().row_of(&id).unwrap()
    };
    h.cx.emit(table, RowActivated(row));
    h.settle();
    assert_eq!(h.location(), Location::Dir(h.home.join("Documents")));

    h.act(Back);
    h.act(GridView);
    h.cx.advance_clock(1.);
    h.tap("Desktop");
    assert_eq!(h.location(), Location::Dir(h.home.join("Desktop")));
}

#[test]
fn single_click_in_the_columns_view_leaves_folders_to_the_selection() {
    let mut h = Harness::new();
    h.single_click(true);
    h.act(ColumnsView);
    let table = h.node("files-columns");
    let row = {
        let t = h.tab().read(&h.cx);
        let id = t.visible().iter().copied().find(|id| t.entry(id).is_some_and(|e| e.name.as_ref() == "Documents"));
        t.list_state().row_of(&id.unwrap()).unwrap()
    };
    h.cx.emit(table, RowsSelected { rows: vec![row], cursor: Some(row) });
    h.tap("Documents");
    assert_eq!(h.location(), Location::Dir(h.home.clone()), "the tab stays, the folder lists beside it");
    assert_eq!(h.tab().read(&h.cx).column_view().child(), Some(h.home.join("Documents")));
}

#[test]
fn the_preview_says_what_you_may_do_with_a_file() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::with(|home| {
        let dir = home.join("modes");
        fs::create_dir_all(&dir).unwrap();
        for (name, mode) in [("open.txt", 0o644), ("frozen.txt", 0o444), ("locked.txt", 0o000)] {
            fs::write(dir.join(name), "x").unwrap();
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(mode)).unwrap();
        }
    });
    h.go("modes");
    h.act(TogglePreview);
    for (name, says) in [("open.txt", "Read and write"), ("frozen.txt", "Read only"), ("locked.txt", "No access")] {
        h.select(&[name]);
        let text = h.text();
        assert!(text.contains(says), "{name}: {text}");
        assert!(!text.contains("-r--"), "no mode string for {name}: {text}");
    }
    let info = h.preview().read(&h.cx).loaded().info.clone().expect("the info loaded");
    assert_eq!(info.access.label(), "No access");
}

/// Ctrl+scroll over the files zooms the view under it: a wheel notch a step, a trackpad's
/// fractions of one once they add up to a whole step.
#[test]
fn ctrl_scroll_zooms_the_view() {
    let mut h = Harness::new();
    let scroll = |h: &mut Harness, steps: f32| {
        let files = h.node("files");
        let modifiers = KeyModifiers {
            control: !cfg!(target_os = "macos"),
            platform: cfg!(target_os = "macos"),
            ..Default::default()
        };
        h.cx.emit(files, ScrollZoomEvent { position: nd::core::Point { x: 40., y: 40. }, steps, modifiers });
        h.settle();
    };
    let list = |h: &Harness| h.tab().read(&h.cx).zoom().list;

    scroll(&mut h, 1.);
    assert_eq!(list(&h), 1);
    scroll(&mut h, 0.4);
    assert_eq!(list(&h), 1, "a fraction of a notch waits for the rest");
    scroll(&mut h, 0.6);
    assert_eq!(list(&h), 2);
    scroll(&mut h, -2.);
    assert_eq!(list(&h), 0);
    assert_eq!(trove::settings::get(&h.cx).list_zoom, 0, "kept like Ctrl+-");

    h.act(GridView);
    let grid = h.tab().read(&h.cx).zoom().grid;
    scroll(&mut h, -1.);
    assert_eq!(h.tab().read(&h.cx).zoom().grid, grid - 1, "the grid steps its own size");
}

/// Rows dragged out of the window carry their files, from the list, the grid, the columns view
/// and search results, where the platform can start that drag.
#[test]
fn dragged_rows_carry_their_files_out() {
    use nd::virtual_list::ListProps;
    let mut h = Harness::new();
    let carried = |h: &Harness| -> Vec<PathBuf> {
        let backend = h.cx.backend();
        let ours = |p: &ListProps| p.drag_kind.as_deref() == Some(DRAG_FILES);
        backend
            .find(|_, p| p.downcast_ref::<ListProps>().is_some_and(ours))
            .into_iter()
            .filter_map(|id| backend.props(id)?.downcast_ref::<ListProps>().map(|l| l.drag_files.clone()))
            .flat_map(|files| files.into_values())
            .collect()
    };
    let out = nd::platform::has(nd::platform::Feature::DragFilesOut);
    // Only rows drawn or selected carry their files, and the test backend draws a few.
    let check = |h: &Harness, view: &str| {
        let files = carried(h);
        assert_eq!(!files.is_empty(), out, "{view}");
        assert!(files.iter().all(|f| f.starts_with(&h.home) && f.exists()), "{view}: {files:?}");
    };

    for view in ["list", "grid", "columns"] {
        match view {
            "grid" => h.act(GridView),
            "columns" => h.act(ColumnsView),
            _ => {}
        }
        check(&h, view);
    }
    h.act(ListView);
    h.act(FocusSearch);
    let field = h.node("search");
    h.cx.emit(field, nd::components::TextChanged { text: "notes".into() });
    h.settle();
    assert!(h.tab().read(&h.cx).showing_results());
    check(&h, "search results");
}
