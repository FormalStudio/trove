//! The status bar under the listing: how many items, what the selection holds, the free space
//! of the volume, and room at the trailing end for running jobs.

use nd::prelude::*;

use crate::format;
use crate::tab::{Load, Tab};

pub struct StatusBar {
    tab: Option<Entity<Tab>>,
    _watch: Option<Subscription>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self { tab: None, _watch: None }
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

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (left, right) = self.summary(cx);
        let line = |s: String, id: &'static str| {
            text(s).id(id).text_size(theme.type_scale.footnote).text_color(theme.text.secondary).tabular_nums()
        };
        v_flex().w_full().child(separator()).child(
            h_flex()
                .id("status")
                .w_full()
                .h(theme.control.small)
                .px(theme.spacing.md)
                .gap(theme.spacing.md)
                .items_center()
                .child(div().flex_1().min_w_0().child(line(left, "status-items").truncate()))
                .child(h_flex().id("jobs").gap(theme.spacing.sm))
                .child(line(right, "status-free")),
        )
    }
}
