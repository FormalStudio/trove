pub mod app;
mod demo;

use nd::prelude::*;

pub fn app() -> Application {
    Application::new(nd::manifest!())
}

pub fn open(cx: &mut App) -> WindowHandle<app::Counter> {
    let count = if demo::enabled() { demo::COUNT } else { 0 };
    cx.open_window(WindowOptions::main("main").title("Trove"), move |_window, cx| cx.new(|_cx| app::Counter { count }))
}

pub fn run() -> nd::Result<()> {
    app().run(|cx| {
        open(cx);
    })
}
