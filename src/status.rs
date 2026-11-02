//! The status bar under the listing: how many items, what the selection holds, the free space
//! of the volume, and above it the running jobs, each with its progress and a cancel button.
//! A job's failure shows as a toast over the window that asked for it.

use nd::core::WindowId;
use nd::prelude::*;

use crate::format;
use crate::ops::{self, JobRow};
use crate::tab::{Load, Tab};

pub struct StatusBar {
    window: WindowId,
    tab: Option<Entity<Tab>>,
    _watch: Option<Subscription>,
    _jobs: Subscription,
}

impl StatusBar {
    pub fn new(window: WindowId, cx: &mut Context<Self>) -> Self {
        let jobs = cx.observe(&ops::jobs(cx), |_, _, cx| cx.notify());
        Self { window, tab: None, _watch: None, _jobs: jobs }
    }

    pub fn set_tab(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        self._watch = Some(cx.observe(tab, |_, _, cx| cx.notify()));
        self.tab = Some(tab.clone());
        cx.notify();
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
        let selected = tab.selected();
        if !selected.is_empty() {
            let bytes: u64 = selected
                .iter()
                .filter_map(|id| tab.entry(id))
                .filter(|e| !e.is_dir())
                .filter_map(|e| e.meta.as_ref().map(|m| m.size))
                .sum();
            let any_file = selected.iter().filter_map(|id| tab.entry(id)).any(|e| !e.is_dir());
            left.push_str(&format!(" \u{b7} {} selected", format::group(selected.len())));
            if any_file {
                left.push_str(&format!(" ({})", format::size(bytes)));
            }
        }
        let right = tab.free().map(|u| format!("{} free", format::size(u.free))).unwrap_or_default();
        (left, right)
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
                    .child(line(right, "status-free".into())),
            )
            .child(toast_overlay("job-toasts").toasts(toasts).on_dismiss(move |e: &ToastDismissed, _, cx| {
                let _ = dismiss.update(cx, |j, cx| j.dismiss_notice(&e.id, cx));
            }))
    }
}
