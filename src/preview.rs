//! The preview pane: what the selection is, from what the listing already knows (kind, size,
//! dates). File contents and thumbnails come from trove-preview into the art slot later.

use nd::prelude::*;

use crate::format;
use crate::icons;
use crate::tab::Tab;

pub struct Preview {
    tab: Option<Entity<Tab>>,
    _watch: Option<Subscription>,
}

impl Preview {
    pub fn new() -> Self {
        Self { tab: None, _watch: None }
    }

    pub fn set_tab(&mut self, tab: &Entity<Tab>, cx: &mut Context<Self>) {
        self._watch = Some(cx.observe(tab, |_, _, cx| cx.notify()));
        self.tab = Some(tab.clone());
        cx.notify();
    }
}

impl Default for Preview {
    fn default() -> Self {
        Self::new()
    }
}

struct Shown {
    glyph: Icon,
    tint: Hsla,
    title: String,
    lines: Vec<String>,
}

impl Render for Preview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shown = self.tab.as_ref().map(|tab| {
            let tab = tab.read(cx);
            let selected = tab.selected();
            match selected.as_slice() {
                [] => Shown {
                    glyph: Icon::Folder,
                    tint: theme.accent,
                    title: tab.title(),
                    lines: vec![format::items(tab.visible().len())],
                },
                [one] => match tab.entry(one) {
                    Some(e) => {
                        let mut lines = Vec::new();
                        let mut first = e.file_kind.label().to_owned();
                        if let Some(m) = &e.meta
                            && !m.kind.is_dir()
                        {
                            first = format!("{first} \u{b7} {}", format::size(m.size));
                        }
                        lines.push(first);
                        if let Some(t) = e.meta.as_ref().and_then(|m| m.modified) {
                            lines.push(format!("Modified {}", tab.today().long(t)));
                        }
                        if let Some(t) = e.meta.as_ref().and_then(|m| m.created) {
                            lines.push(format!("Created {}", tab.today().long(t)));
                        }
                        Shown {
                            glyph: icons::for_kind(e.file_kind),
                            tint: icons::tint(e.file_kind, theme),
                            title: e.name.to_string(),
                            lines,
                        }
                    }
                    None => {
                        Shown { glyph: Icon::File, tint: theme.text.secondary, title: String::new(), lines: vec![] }
                    }
                },
                many => {
                    let bytes: u64 = many
                        .iter()
                        .filter_map(|id| tab.entry(id))
                        .filter(|e| !e.is_dir())
                        .filter_map(|e| e.meta.as_ref().map(|m| m.size))
                        .sum();
                    Shown {
                        glyph: Icon::Copy,
                        tint: theme.text.secondary,
                        title: format::items(many.len()),
                        lines: vec![format::size(bytes)],
                    }
                }
            }
        });
        let Some(shown) = shown else { return v_flex().id("preview").size_full() };
        v_flex()
            .id("preview")
            .size_full()
            .items_center()
            .gap(theme.spacing.sm)
            .p(theme.spacing.lg)
            .pt(theme.spacing.xl)
            .child(div().py(theme.spacing.lg).child(icon(shown.glyph, 96.).text_color(shown.tint)))
            .child(text(shown.title).id("preview-title").font_weight(FontWeight::SEMIBOLD).text_center().line_clamp(3))
            .children(shown.lines.into_iter().map(|l| {
                text(l)
                    .text_size(theme.type_scale.footnote)
                    .text_color(theme.text.secondary)
                    .text_center()
                    .tabular_nums()
                    .into_any_element()
            }))
    }
}
