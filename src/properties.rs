//! The Properties sheet: what an item is, its size (a folder's added up off the main thread),
//! where it is, its dates, owner and group, and its permissions as checkboxes that apply through
//! `Op::SetPermissions`, so Undo takes a change back. Over several items it shows what they share
//! and their total size; a checkbox the items disagree on shows mixed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use nd::prelude::*;
use trove_fs::FileKind;
use trove_ops::props::Usage;
use trove_ops::{JobId, Op, OpEvent};

use crate::conflict::home_relative;
use crate::format::{self, Today};
use crate::ops::{self, Origin};

/// Who a permission row is for, by the shift of its three bits.
const WHO: [(&str, &str, u32); 3] = [("Owner", "owner", 6), ("Group", "group", 3), ("Others", "others", 0)];
const WHAT: [(&str, &str, u32); 3] = [("Read", "read", 4), ("Write", "write", 2), ("Execute", "execute", 1)];

/// What the sheet reads about its items, off the main thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub files: usize,
    pub folders: usize,
    /// The one kind every item shares.
    pub kind: Option<String>,
    /// The folder every item is in.
    pub location: Option<PathBuf>,
    pub link_target: Option<PathBuf>,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// Each item's permission bits, links followed.
    pub modes: Vec<(PathBuf, u32)>,
    /// This process may change every item's permissions.
    pub editable: bool,
}

/// One value when every item gives the same, else `None`.
fn shared<T: PartialEq + Clone>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.all(|v| v == first).then_some(first)
}

fn read_facts(paths: &[PathBuf]) -> Facts {
    let infos: Vec<trove_preview::Info> = paths.iter().filter_map(|p| trove_preview::info_blocking(p)).collect();
    let folders = infos.iter().filter(|i| i.is_dir).count();
    let kind_of = |i: &trove_preview::Info| {
        if i.is_dir { FileKind::Folder } else { FileKind::from_name(&ops::name_of(&i.path)) }.label().to_owned()
    };
    let one = (infos.len() == 1).then(|| &infos[0]);
    let modes: Vec<(PathBuf, u32)> = paths
        .iter()
        .filter_map(|p| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).ok().map(|m| (p.clone(), m.mode() & 0o7777))
        })
        .collect();
    let editable = !modes.is_empty()
        && modes.iter().all(|(p, _)| trove_ops::props::owner_of(p).is_some_and(trove_ops::props::may_change_mode));
    Facts {
        files: infos.len() - folders,
        folders,
        kind: shared(infos.iter().map(kind_of)),
        location: shared(paths.iter().map(|p| p.parent().map(Path::to_path_buf))).flatten(),
        link_target: one.and_then(|i| i.link_target.clone()),
        created: one.and_then(|i| i.created),
        modified: one.and_then(|i| i.modified),
        accessed: one.and_then(|i| i.accessed),
        owner: shared(infos.iter().map(|i| i.owner.clone())),
        group: shared(infos.iter().map(|i| i.group.clone())),
        modes,
        editable,
    }
}

/// A permission checkbox's state over every item: on, off, or mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bit {
    On,
    Off,
    Mixed,
}

pub fn bit(modes: &[(PathBuf, u32)], mask: u32) -> Bit {
    let on = modes.iter().filter(|(_, m)| m & mask != 0).count();
    match on {
        0 => Bit::Off,
        n if n == modes.len() => Bit::On,
        _ => Bit::Mixed,
    }
}

pub struct PropertiesSheet {
    paths: Vec<PathBuf>,
    origin: Origin,
    facts: Option<Facts>,
    usage: Option<Usage>,
    stop: Arc<AtomicBool>,
    /// A permission change on its way: the facts are read again once it ends.
    pending: Option<JobId>,
    _jobs: Subscription,
}

/// Opens the sheet for `paths` over the window `parent`.
pub fn open(cx: &mut App, parent: nd::core::WindowId, paths: Vec<PathBuf>) -> WindowHandle<PropertiesSheet> {
    let title = match paths.as_slice() {
        [one] => format!("Properties of {}", ops::name_of(one)),
        many => format!("Properties of {} items", many.len()),
    };
    let options = WindowOptions::main("main.properties")
        .title(title)
        .kind(WindowKind::Sheet { parent })
        .size(size(px(480.), px(500.)));
    let origin = Origin { window: parent, tab: None };
    cx.open_window(options, move |_, cx| cx.new(|cx| PropertiesSheet::new(paths, origin, cx)))
}

/// The sheet over a demo window, for the appearance audit: a folder and a file in Documents.
pub fn open_demo(cx: &mut App) -> nd::core::WindowId {
    let parent = crate::open_demo(cx);
    let root = crate::demo::root();
    let sheet = open(cx, parent.id(), vec![root.join("Documents/Quarterly report.docx")]);
    ops::run_deferred(cx);
    sheet.id()
}

impl PropertiesSheet {
    fn new(paths: Vec<PathBuf>, origin: Origin, cx: &mut Context<Self>) -> Self {
        let jobs = ops::jobs(cx);
        let watch = cx.observe(&jobs, |this: &mut Self, jobs, cx| {
            let Some(job) = this.pending else { return };
            let ended = jobs.read(cx).seen().iter().rev().any(|e| match e {
                OpEvent::Finished { job: j, .. } | OpEvent::Failed { job: j, .. } | OpEvent::Cancelled { job: j } => {
                    *j == job
                }
                _ => false,
            });
            if ended {
                this.pending = None;
                this.read(cx);
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (walk, flag) = (paths.clone(), stop.clone());
        ops::background(
            cx.weak_entity(),
            move || trove_ops::props::usage(&walk, &flag),
            |this: &mut Self, usage, cx| {
                this.usage = Some(usage);
                cx.notify();
            },
        );
        let mut sheet = Self { paths, origin, facts: None, usage: None, stop, pending: None, _jobs: watch };
        sheet.read(cx);
        sheet
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        ops::background(
            cx.weak_entity(),
            move || read_facts(&paths),
            |this: &mut Self, facts, cx| {
                this.facts = Some(facts);
                cx.notify();
            },
        );
    }

    pub fn facts(&self) -> Option<&Facts> {
        self.facts.as_ref()
    }

    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    /// Sets or clears `mask` on every item, as one job.
    pub fn set_bit(&mut self, mask: u32, on: bool, cx: &mut Context<Self>) {
        let Some(facts) = self.facts.as_mut().filter(|f| f.editable) else { return };
        let modes: Vec<(PathBuf, u32)> =
            facts.modes.iter().map(|(p, m)| (p.clone(), if on { m | mask } else { m & !mask })).collect();
        if modes == facts.modes {
            return;
        }
        facts.modes = modes.clone();
        self.pending = Some(ops::submit(cx, self.origin.clone(), Op::SetPermissions { modes }));
        cx.notify();
    }

    fn title(&self) -> String {
        match self.paths.as_slice() {
            [one] => ops::name_of(one),
            many => format!("{} items", many.len()),
        }
    }

    /// The label and value rows above the permissions.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let Some(f) = &self.facts else { return Vec::new() };
        let today = Today::now();
        let mut rows = Vec::new();
        let kind = match (&f.kind, self.paths.len()) {
            (Some(kind), 1) => kind.clone(),
            _ => match (f.files, f.folders) {
                (0, d) => format!("{d} folders"),
                (n, 0) => format!("{n} files"),
                (n, d) => format!("{n} {}, {d} {}", plural(n, "file"), plural(d, "folder")),
            },
        };
        rows.push(("Kind", kind));
        let size = match self.usage {
            None => "Calculating\u{2026}".to_owned(),
            Some(u) if f.folders > 0 => format!("{}, {}", format::size_long(u.bytes), format::items(u.items as usize)),
            Some(u) => format::size_long(u.bytes),
        };
        rows.push(("Size", size));
        if let Some(dir) = &f.location {
            rows.push(("Where", home_relative(dir)));
        }
        if let Some(target) = &f.link_target {
            rows.push(("Points to", target.display().to_string()));
        }
        for (label, t) in [("Created", f.created), ("Modified", f.modified), ("Accessed", f.accessed)] {
            if let Some(t) = t {
                rows.push((label, today.long(t)));
            }
        }
        if let Some(owner) = &f.owner {
            rows.push(("Owner", owner.clone()));
        }
        if let Some(group) = &f.group {
            rows.push(("Group", group.clone()));
        }
        rows
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_owned() } else { format!("{word}s") }
}

impl Drop for PropertiesSheet {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Render for PropertiesSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let row = |label: &'static str, value: Div| {
            h_flex()
                .w_full()
                .gap(theme.spacing.md)
                .items_start()
                .child(text(label).text_color(theme.text.secondary).flex_none().w(px(84.)).text_right())
                .child(value.flex_1().min_w_0())
        };
        let facts = v_flex().id("properties-facts").gap(theme.spacing.sm).children(
            self.rows()
                .into_iter()
                .map(|(label, value)| row(label, div().child(text(value).tabular_nums().line_clamp(2))).id(label)),
        );
        let modes = self.facts.as_ref().map(|f| f.modes.clone()).unwrap_or_default();
        let editable = self.facts.as_ref().is_some_and(|f| f.editable);
        let mut perms = v_flex().id("properties-permissions").gap(theme.spacing.sm);
        for (who, who_id, shift) in WHO {
            let mut boxes = h_flex().gap(theme.spacing.lg);
            for (what, what_id, bit_value) in WHAT {
                let mask = bit_value << shift;
                let state = bit(&modes, mask);
                let me = cx.weak_entity();
                boxes = boxes.child(
                    checkbox(format!("perm-{who_id}-{what_id}"), what)
                        .checked(state == Bit::On)
                        .mixed(state == Bit::Mixed)
                        .disabled(!editable)
                        .on_toggle(move |e: &Toggled, _, cx| {
                            let _ = me.update(cx, |this, cx| this.set_bit(mask, e.checked, cx));
                        }),
                );
            }
            perms = perms.child(row(who, boxes));
        }
        let note = (self.facts.is_some() && !editable).then(|| {
            text("Only the owner can change permissions.")
                .id("properties-note")
                .text_size(theme.type_scale.footnote)
                .text_color(theme.text.secondary)
        });
        let body = sheet_layout(self.title())
            .child(
                v_flex()
                    .gap(theme.spacing.md)
                    .child(facts)
                    .child(separator())
                    .child(text("Permissions").font_weight(FontWeight::SEMIBOLD))
                    .child(perms)
                    .children(note),
            )
            .footer(
                button("properties-done", "Done")
                    .primary()
                    .on_click(|_: &ClickEvent, window, cx| cx.close_window(window.id())),
            );
        div()
            .id("properties-sheet")
            .size_full()
            .on_action(|_: &nd::actions::Cancel, window, cx| cx.close_window(window.id()))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bit_is_mixed_when_the_items_disagree() {
        let m = |modes: &[u32]| modes.iter().map(|m| (PathBuf::from("x"), *m)).collect::<Vec<_>>();
        assert_eq!(bit(&m(&[0o755, 0o700]), 0o100), Bit::On);
        assert_eq!(bit(&m(&[0o755, 0o700]), 0o004), Bit::Mixed);
        assert_eq!(bit(&m(&[0o600]), 0o001), Bit::Off);
        assert_eq!(shared([1, 1, 1].into_iter()), Some(1));
        assert_eq!(shared([1, 2].into_iter()), None);
    }
}
