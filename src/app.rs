use nd::prelude::*;

#[derive(Default)]
pub struct Counter {
    pub count: u32,
}

impl Render for Counter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        scaffold()
            .toolbar(toolbar().item(button("add", "Add one").on_click(cx.listener(
                |this, _: &ClickEvent, _window, cx| {
                    this.count += 1;
                    cx.notify();
                },
            ))))
            .content(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(window_controls().at(point(px(16.), px(10.))))
                    .child(text(self.count.to_string()).text_size(px(48.)).tabular_nums())
                    .child(text("clicks").text_color(theme.text.secondary)),
            )
    }
}
