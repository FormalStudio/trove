//! The status bar under the listing: how many items, what the selection holds, the free space
//! of the volume, and above it the running jobs, each with its progress and a cancel button.
//! A job's failure shows as a toast over the window that asked for it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nd::core::WindowId;
use nd::prelude::*;

use crate::actions::ZoomTo;
use crate::format;
use crate::ops::{self, JobRow};
use crate::tab::{Load, Tab};

/// The selected folders, walked off the main thread for what they hold.
struct Measured {
    folders: Vec<PathBuf>,
    bytes: Option<u64>,
    stop: Arc<AtomicBool>,
}

pub struct StatusBar {
    window: WindowId,
    tab: Option<Entity<Tab>>,
    measured: Option<Measured>,
    _watch: Option<Subscription>,
    _jobs: Subscription,
}

impl StatusBar {
    pub fn new(window: WindowId, cx: &mut Context<Self>) -> Self {
        let jobs = cx.observe(&ops::jobs(cx), |_, _, cx| cx.notify());
        Self { window, tab: None, measured: None, _watch: None, _jobs: jobs }
    }

    pub fn set_tab(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        self._watch = Some(cx.observe(tab, |this, tab, cx| {
            this.measure(&tab, cx);
            cx.notify();
        }));
        self.tab = Some(tab.clone());
        self.measure(tab, cx);
        cx.notify();
    }

    /// Adds up the selected folders when the selection of them changes. Folders on a network
    /// mount are left alone: a walk of one can take as long as its server likes.
    fn measure(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        let tab = tab.read(cx);
        let mut folders: Vec<PathBuf> = if tab.showing_results() || tab.is_trash() {
            Vec::new()
        } else {
            let ids = tab.selected();
            ids.iter().filter(|id| tab.entry(id).is_some_and(|e| e.is_dir())).filter_map(|id| tab.path_of(id)).collect()
        };
        folders.sort();
        if self.measured.as_ref().map_or(&[][..], |m| &m.folders) == folders.as_slice() {
            return;
        }
        if let Some(old) = self.measured.take() {
            old.stop.store(true, Ordering::Relaxed);
        }
        let remote = folders.first().is_some_and(|f| trove_fs::is_network_path(f, &crate::env::volumes()));
        if folders.is_empty() || remote {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (walk, flag, key) = (folders.clone(), stop.clone(), folders.clone());
        self.measured = Some(Measured { folders, bytes: None, stop });
        ops::background(
            cx.weak_entity(),
            move || trove_ops::props::usage(&walk, &flag),
            move |this: &mut Self, usage, cx| {
                if let Some(m) = this.measured.as_mut().filter(|m| m.folders == key) {
                    m.bytes = Some(usage.bytes);
                    cx.notify();
                }
            },
        );
    }

    /// The line's words, for the view and for tests.
    pub fn summary(&self, cx: &App) -> (String, String) {
        let Some(tab) = &self.tab else { return (String::new(), String::new()) };
        let tab = tab.read(cx);
        if tab.showing_results() {
            let mut left = tab.results().count_label();
            let selected = tab.selected_paths().len();
            if selected > 0 {
                left.push_str(&format!(" \u{b7} {} selected", format::group(selected)));
            }
            return (left, String::new());
        }
        let shown = tab.visible().len();
        let mut left = if !tab.query().is_empty() {
            format!("{} of {}", format::group(shown), format::items(tab.total()))
        } else {
            match tab.load_state() {
                Load::Loading | Load::Slow if shown == 0 => String::new(),
                Load::Failed(_) => String::new(),
                _ => format::items(shown),
            }
        };
        // As Finder words it: "“Notes.md” selected, 1.6 kB", "3 of 10 selected, 4.1 MB". The
        // size counts the files, and the folders once their walk has ended.
        let selected = tab.selected();
        if !selected.is_empty() {
            let entries: Vec<_> = selected.iter().filter_map(|id| tab.entry(id)).collect();
            let files: Vec<_> = entries.iter().filter(|e| !e.is_dir()).collect();
            let bytes: u64 = files.iter().filter_map(|e| e.meta.as_ref().map(|m| m.size)).sum();
            left = match entries.as_slice() {
                [one] => format!("\u{201c}{}\u{201d} selected", one.name),
                _ => format!("{} of {} selected", format::group(selected.len()), format::group(shown)),
            };
            let folders = entries.len() - files.len();
            let walked = self.measured.as_ref().and_then(|m| m.bytes).filter(|_| folders > 0);
            if let Some(inside) = walked {
                left.push_str(&format!(", {}", format::size(bytes + inside)));
            } else if !files.is_empty() {
                left.push_str(&format!(", {}", format::size(bytes)));
            }
        }
        let right = tab.free().map(|u| format!("{} free", format::size(u.free))).unwrap_or_default();
        (left, right)
    }
}

impl Drop for StatusBar {
    fn drop(&mut self) {
        if let Some(m) = &self.measured {
            m.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// "1.2 MB of 4 MB · 3 of 10 files", or "Done" once the job finished.
pub fn job_detail(row: &JobRow) -> String {
    if row.finished {
        return "Done".to_owned();
    }
    let mut parts = Vec::new();
    if row.total_bytes > 0 {
        parts.push(format!("{} of {}", format::size(row.done_bytes), format::size(row.total_bytes)));
    }
    if row.total_files > 1 {
        parts.push(format!(
            "{} of {} files",
            format::group(row.done_files as usize),
            format::group(row.total_files as usize)
        ));
    }
    if parts.is_empty() {
        return "Preparing\u{2026}".to_owned();
    }
    parts.join(" \u{b7} ")
}

fn fraction(row: &JobRow) -> Option<f32> {
    if row.finished {
        return Some(1.);
    }
    if row.total_bytes > 0 {
        return Some((row.done_bytes as f64 / row.total_bytes as f64).clamp(0., 1.) as f32);
    }
    (row.total_files > 0).then(|| (row.done_files as f64 / row.total_files as f64).clamp(0., 1.) as f32)
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (left, right) = self.summary(cx);
        let line = |s: String, id: SharedString| {
            text(s).id(id).text_size(theme.type_scale.footnote).text_color(theme.text.secondary).tabular_nums()
        };
        let jobs = ops::jobs(cx);
        let (rows, toasts) = {
            let j = jobs.read(cx);
            let toasts: Vec<Toast> = j
                .notices()
                .iter()
                .filter(|n| n.window == self.window)
                .map(|n| toast(n.id.clone(), n.title.clone()))
                .collect();
            (j.rows().to_vec(), toasts)
        };
        let job_rows = rows.into_iter().map(|row| {
            let id = row.id;
            h_flex()
                .id(SharedString::from(format!("job-{}", id.0)))
                .w_full()
                .h(theme.control.small)
                .px(theme.spacing.md)
                .gap(theme.spacing.md)
                .items_center()
                .child(
                    div().flex_1().min_w_0().child(
                        text(row.title.clone())
                            .text_size(theme.type_scale.footnote)
                            .text_color(theme.text.primary)
                            .truncate(),
                    ),
                )
                .child(div().w(px(160.)).child(progress(fraction(&row)).id(format!("job-progress-{}", id.0)).w_full()))
                .child(line(job_detail(&row), format!("job-detail-{}", id.0).into()))
                .child(
                    icon_button(format!("job-cancel-{}", id.0), Icon::X)
                        .tooltip("Cancel")
                        .disabled(row.finished)
                        .on_click(move |_: &ClickEvent, _, cx| ops::cancel(cx, id)),
                )
        });
        let dismiss = jobs.downgrade();
        let size = self.tab.as_ref().map(|tab| {
            let tab = tab.read(cx);
            let (at, last) = tab.zoom().step_of(tab.view());
            slider("status-zoom", at as f32).range(0. ..=last as f32).step(1.).label("Size").w(px(96.)).on_change(
                |e: &SliderChanged, window, cx| {
                    cx.dispatch_action(window.id(), &ZoomTo(e.value.round().max(0.) as usize));
                },
            )
        });
        v_flex()
            .w_full()
            .child(separator())
            .children(job_rows)
            .child(
                h_flex()
                    .id("status")
                    .w_full()
                    .h(theme.control.small)
                    .px(theme.spacing.md)
                    .gap(theme.spacing.md)
                    .items_center()
                    .child(div().flex_1().min_w_0().child(line(left, "status-items".into()).truncate()))
                    .child(line(right, "status-free".into()))
                    .children(size),
            )
            .child(toast_overlay("job-toasts").toasts(toasts).on_dismiss(move |e: &ToastDismissed, _, cx| {
                let _ = dismiss.update(cx, |j, cx| j.dismiss_notice(&e.id, cx));
            }))
    }
}
