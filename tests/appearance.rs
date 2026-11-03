//! Tier 2: the appearance audit. Every window in light, dark and matugen themes on each backend
//! kind; fails on a colour that is not a theme token and on text under WCAG contrast. Runs in
//! `nd check` and `nd test`.

fn main() {
    nd::audit::Audit::new("trove", trove::app)
        .window("main", |cx| trove::open_demo(cx).id())
        .window("conflict", trove::open_conflict_demo)
        .window("picker", trove::open_picker_demo)
        .run()
}
