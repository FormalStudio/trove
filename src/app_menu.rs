//! The app's menu: the header's menu button on Linux and Windows, the menu bar on macOS. New
//! window, Reopen closed tab, Show hidden files, Preferences (the settings form over
//! `TroveSettings`), Keyboard shortcuts (a sheet read off the key bindings) and About.

use nd::platform::{Feature, Target};
use nd::prelude::*;

use crate::actions::*;
use crate::keys::{ColumnsView, QuickLook, ShowInFolder, ZoomIn, ZoomOut, ZoomReset};
use crate::location::Location;
use crate::root::{Root, Start};
use crate::settings::TroveSettings;

actions!(trove, [NewWindow, Preferences, KeyboardShortcuts, About, ReopenClosedTab]);

/// The keys GNOME Files and Finder give these commands.
pub fn bind(cx: &mut App) {
    let w = Some(WINDOW);
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewWindow, w),
        KeyBinding::new("secondary-shift-t", ReopenClosedTab, w),
        KeyBinding::new("secondary-,", Preferences, w),
        KeyBinding::new("secondary-?", KeyboardShortcuts, w),
    ]);
}

/// The menu's items. `can_reopen` is whether a closed tab is waiting to come back.
pub fn items(menu: ContextMenu, show_hidden: bool, can_reopen: bool) -> ContextMenu {
    menu.item("New window", NewWindow)
        .item("New tab", NewTab)
        .entry(MenuItem::action("Reopen closed tab", ReopenClosedTab).enabled(can_reopen))
        .separator()
        .entry(MenuItem::action("Show hidden files", ToggleHidden).checked(show_hidden))
        .separator()
        .item("Preferences", Preferences)
        .item("Keyboard shortcuts", KeyboardShortcuts)
        .item("About Trove", About)
}

/// The header's menu button, where the platform has no menu bar to hold the same items.
pub fn header_button(root: WeakEntity<Root>) -> Option<Button> {
    if nd::platform::has(Feature::MenuBar) {
        return None;
    }
    let button = menu_button("app-menu", Icon::Menu, move |menu, _, cx| {
        let Some(root) = root.upgrade() else { return menu };
        let r = root.read(cx);
        items(menu, r.shows_hidden(), r.can_reopen())
    });
    Some(button.tooltip("Main menu"))
}

/// The macOS menu bar: the same commands under the menus the platform expects them in.
pub fn set_menu_bar(cx: &mut App) {
    nd::chrome::set_menus(
        cx,
        [
            Menu::app([
                MenuItem::action("About Trove", About),
                MenuItem::separator(),
                MenuItem::action("Preferences\u{2026}", Preferences),
            ]),
            Menu::new("File").items([
                MenuItem::action("New window", NewWindow),
                MenuItem::action("New tab", NewTab),
                MenuItem::action("Reopen closed tab", ReopenClosedTab),
            ]),
            Menu::new("View").items([MenuItem::action("Show hidden files", ToggleHidden)]),
            Menu::help([MenuItem::action("Keyboard shortcuts", KeyboardShortcuts)]),
        ],
    );
}

/// The window's handlers for the menu's commands.
pub fn on_root(root: Scaffold, cx: &mut Context<Root>) -> Scaffold {
    root.on_action(cx.listener(|this, _: &NewWindow, _, cx| {
        let start = match this.active_tab().read(cx).location() {
            Location::Dir(dir) => dir.clone(),
            Location::Recent => crate::env::get().home.clone(),
        };
        crate::open(cx, Start::Path(start));
    }))
    .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| this.reopen_tab(window, cx)))
    .on_action(cx.listener(|_, _: &Preferences, _, cx| {
        open_preferences(cx);
    }))
    .on_action(cx.listener(|_, _: &KeyboardShortcuts, window, cx| {
        open_shortcuts(cx, window.id());
    }))
    .on_action(cx.listener(|_, _: &About, window, cx| {
        let about = nd::chrome::AboutPanel::new()
            .name("Trove")
            .version(env!("CARGO_PKG_VERSION"))
            .credits("A file manager for Linux.");
        window.show_about(cx, about);
    }))
}

/// Opens Preferences, or brings the open one forward.
pub fn open_preferences(cx: &mut App) -> Option<WindowHandle<PreferencesWindow>> {
    let handle = nd::chrome::open_settings(cx, |_, cx| cx.new(|_| PreferencesWindow))?;
    cx.set_window_title(handle.id(), "Preferences");
    Some(handle)
}

pub struct PreferencesWindow;

impl Render for PreferencesWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex().id("preferences").size_full().p(theme.spacing.lg).child(settings_form::<TroveSettings>())
    }
}

/// Opens the keyboard shortcuts as a sheet over `parent`.
pub fn open_shortcuts(cx: &mut App, parent: nd::core::WindowId) -> WindowHandle<ShortcutsSheet> {
    let options =
        WindowOptions::main("main.shortcuts").kind(WindowKind::Sheet { parent }).size(size(px(520.), px(640.)));
    cx.open_window(options, |_, cx| cx.new(|cx| ShortcutsSheet::new(cx)))
}

/// A command the sheet lists: what it says and the action whose keys it shows.
type Command = (&'static str, &'static dyn Action);

/// One group of the sheet: its title and its commands, each with what the bindings give it.
pub type Group = (&'static str, Vec<(&'static str, String)>);

pub struct ShortcutsSheet {
    groups: Vec<Group>,
}

impl ShortcutsSheet {
    pub fn new(cx: &App) -> Self {
        Self { groups: shortcuts(cx) }
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }
}

/// The commands the sheet lists, as `docs/design.md`'s Keys table orders them. The keys come
/// from the bindings in force, so the sheet never disagrees with what a key does; a command
/// with no binding is left out.
pub fn shortcuts(cx: &App) -> Vec<Group> {
    let rows: [(&'static str, Vec<Command>); 4] = [
        (
            "Navigation",
            vec![
                ("Open", &Open),
                ("Parent folder", &Parent),
                ("Back", &Back),
                ("Forward", &Forward),
                ("Open the selected folder", &IntoFolder),
                ("Edit the path", &EditPath),
                ("Search", &FocusSearch),
                ("Show in enclosing folder", &ShowInFolder),
                ("Reload", &Reload),
            ],
        ),
        (
            "Files",
            vec![
                ("Quick Look", &QuickLook),
                ("Rename", &Rename),
                ("Copy", &Copy),
                ("Cut", &Cut),
                ("Paste", &Paste),
                ("Paste into folder", &PasteInto),
                ("Undo", &Undo),
                ("Redo", &Redo),
                ("New folder", &NewFolder),
                ("Duplicate", &Duplicate),
                ("Move to Trash", &Trash),
                ("Delete permanently", &DeletePermanently),
                ("Restore from Trash", &Restore),
            ],
        ),
        (
            "View",
            vec![
                ("List", &ListView),
                ("Grid", &GridView),
                ("Columns", &ColumnsView),
                ("Show hidden files", &ToggleHidden),
                ("Preview pane", &TogglePreview),
                ("Zoom in", &ZoomIn),
                ("Zoom out", &ZoomOut),
                ("Normal size", &ZoomReset),
            ],
        ),
        (
            "Windows and tabs",
            vec![
                ("New window", &NewWindow),
                ("New tab", &NewTab),
                ("Close tab", &CloseTab),
                ("Reopen closed tab", &ReopenClosedTab),
                ("Next tab", &NextTab),
                ("Previous tab", &PreviousTab),
                ("First tab", &SelectTab(0)),
                ("Preferences", &Preferences),
                ("Keyboard shortcuts", &KeyboardShortcuts),
            ],
        ),
    ];
    rows.into_iter()
        .map(|(title, commands)| {
            let found = commands
                .into_iter()
                .filter_map(|(label, action)| {
                    let keys = keys_for(cx, action);
                    (!keys.is_empty()).then_some((label, keys))
                })
                .collect();
            (title, found)
        })
        .collect()
}

/// Every single-stroke binding of `action`, as the platform writes it ("Ctrl+Shift+T"), in the
/// order they were bound and each once.
fn keys_for(cx: &App, action: &dyn Action) -> String {
    let mut seen: Vec<String> = Vec::new();
    for binding in cx.key_bindings() {
        if !binding.action().partial_eq(action) {
            continue;
        }
        let [stroke] = binding.keystrokes() else { continue };
        let m = stroke.modifiers();
        let label = keystroke(m.control, m.alt, m.shift, m.platform, stroke.key());
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen.join(", ")
}

fn keystroke(control: bool, alt: bool, shift: bool, platform: bool, key: &str) -> String {
    let platform_name = if nd::platform::target() == Target::MacOs { "Cmd" } else { "Super" };
    let mut parts: Vec<String> = [(control, "Ctrl"), (platform, platform_name), (alt, "Alt"), (shift, "Shift")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_owned())
        .collect();
    let named = match key {
        "enter" => "Enter",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "escape" => "Esc",
        "space" => "Space",
        "tab" => "Tab",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        _ => "",
    };
    parts.push(if named.is_empty() { key.to_uppercase() } else { named.to_owned() });
    parts.join("+")
}

impl Render for ShortcutsSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let sections = self.groups.iter().filter(|(_, rows)| !rows.is_empty()).map(|(title, rows)| {
            form_section().title(*title).children(rows.iter().map(|(label, keys)| {
                form_row(*label).child(text(keys.clone()).text_color(theme.text.secondary)).into_any_element()
            }))
        });
        sheet_layout("Keyboard shortcuts").child(form().children(sections)).footer(
            button("shortcuts-done", "Done")
                .primary()
                .on_click(cx.listener(|_, _: &ClickEvent, window, cx| window.close_sheet(cx))),
        )
    }
}
