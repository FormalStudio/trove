//! The sheet a conflict opens: both files with their size and date, Replace (a folder over a
//! folder merges), Skip, Keep both, Cancel, and "Apply to all" for the rest of the job.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nd::prelude::*;
use trove_ops::{Resolution, ResolutionChoice};

use crate::format;
use crate::ops::{self, Ask};

/// What the sheet says about one side.
#[derive(Clone, Debug, Default, PartialEq)]
struct Side {
    is_dir: bool,
    size: Option<u64>,
    modified: Option<SystemTime>,
}

pub struct ConflictSheet {
    ask: Ask,
    all: bool,
    source: Option<Side>,
    existing: Option<Side>,
}

fn side(path: &PathBuf) -> Option<Side> {
    let md = std::fs::symlink_metadata(path).ok()?;
    Some(Side { is_dir: md.is_dir(), size: (!md.is_dir()).then_some(md.len()), modified: md.modified().ok() })
}

/// Opens the sheet for `ask` over `parent`.
pub fn open(cx: &mut App, parent: nd::core::WindowId, ask: Ask) -> WindowHandle<ConflictSheet> {
    let options =
        WindowOptions::main("main.conflict").kind(WindowKind::Sheet { parent }).size(size(px(520.), px(300.)));
    cx.open_window(options, move |_, cx| cx.new(|cx| ConflictSheet::new(ask, cx)))
}

impl ConflictSheet {
    pub fn new(ask: Ask, cx: &mut Context<Self>) -> Self {
        let (source, existing) = (ask.source.clone(), ask.existing.clone());
        ops::background(
            cx.weak_entity(),
            move || (side(&source), side(&existing)),
            |this: &mut ConflictSheet, (s, e), cx| {
                this.source = s;
                this.existing = e;
                cx.notify();
            },
        );
        Self { ask, all: false, source: None, existing: None }
    }

    fn both_folders(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.is_dir) && self.existing.as_ref().is_some_and(|e| e.is_dir)
    }

    fn describe(&self, side: &Option<Side>) -> String {
        let Some(side) = side else { return String::new() };
        let today = format::Today::now();
        let size = if side.is_dir { "Folder".to_owned() } else { side.size.map(format::size).unwrap_or_default() };
        match side.modified {
            Some(t) => format!("{size} \u{b7} modified {}", today.modified(t)),
            None => size,
        }
    }
}

/// `~/Documents/Notes.md` for a path under Home, else the path as it is.
fn home_relative(path: &Path) -> String {
    match path.strip_prefix(&crate::env::get().home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

impl Render for ConflictSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let name = ops::name_of(&self.ask.existing);
        let folder = self.ask.existing.parent().map(ops::name_of).unwrap_or_default();
        let title = if self.both_folders() {
            format!("A folder named \u{201c}{name}\u{201d} already exists in \u{201c}{folder}\u{201d}")
        } else {
            format!("An item named \u{201c}{name}\u{201d} already exists in \u{201c}{folder}\u{201d}")
        };
        let replace = if self.both_folders() { "Merge" } else { "Replace" };
        let line = |label: &'static str, path: &Path, about: String, id: &'static str| {
            v_flex()
                .id(id)
                .gap(theme.spacing.xs)
                .child(text(label).text_size(theme.type_scale.footnote).text_color(theme.text.secondary))
                .child(text(home_relative(path)).truncate())
                .child(text(about).text_size(theme.type_scale.footnote).text_color(theme.text.secondary).tabular_nums())
        };
        let me = cx.weak_entity();
        let pick = move |r: Resolution| {
            let me = me.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                if let Some(this) = me.upgrade() {
                    let choice = ResolutionChoice { resolution: r, apply_to_all: this.read(cx).all };
                    ops::jobs(cx).update(cx, |j, cx| j.answer(choice, cx));
                }
            }
        };
        let toggle = cx.weak_entity();
        sheet_layout(title)
            .child(
                v_flex()
                    .gap(theme.spacing.md)
                    .child(line("Existing", &self.ask.existing, self.describe(&self.existing), "conflict-existing"))
                    .child(line("Incoming", &self.ask.source, self.describe(&self.source), "conflict-incoming"))
                    .child(checkbox("conflict-all", "Apply to all").checked(self.all).on_toggle(
                        move |e: &Toggled, _, cx| {
                            let _ = toggle.update(cx, |this, cx| {
                                this.all = e.checked;
                                cx.notify();
                            });
                        },
                    )),
            )
            .footer(button("conflict-cancel", "Cancel").on_click(pick(Resolution::Cancel)))
            .footer(button("conflict-skip", "Skip").on_click(pick(Resolution::Skip)))
            .footer(button("conflict-replace", replace).on_click(pick(Resolution::Replace)))
            .footer(button("conflict-keep", "Keep both").primary().on_click(pick(Resolution::KeepBoth)))
    }
}
