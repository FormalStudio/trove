# Testing

Test in the cheapest tier that can see the behavior. Read the last incident before you try to
reproduce a bug. For the short list of rules, see [donts.md](donts.md).

## Three tiers, one semantic tree

Every tier reads the same semantic tree (role, name, value, test id, bounds, enabled, focused,
checked, selected), so a locator that you write for one tier reads the same in the others.

| Tier | Runs | Use it for | Command |
| --- | --- | --- | --- |
| 1, logic | `TestApp` on the recording `TestBackend`: no window, no OS, deterministic clock | state, actions, keyboard dispatch, focus, list diffs, what the tree contains | `nd test` |
| 2, backends | real backends in process: AppKit offscreen windows, headless GPUI | how it looks: the appearance audit, goldens, a native control's mapping | `nd test` |
| 3, real | the app as its own process with `NATIVE_AUTOMATION=1`, driven over the automation socket | what only the OS does: real input, focus between windows, resize, menus, capture | `nd test --tier real` |

`nd test` runs tiers 1 and 2 in parallel, with every test in its own process. A tier 3 test has
a name that starts with `real_` or sits in a `real` module (`tests/real.rs`). Tier 3 tests run one
at a time.

### Tier 1

```rust
use nd::test::{TestApp, click, text};

#[nd::test]
fn add_one_counts_each_click(cx: &mut TestApp) {
    let window = my_app::open(cx).id();
    click(cx, window, "add");
    assert!(text(cx, window).contains("1"), "{}", text(cx, window));
}
```

- `#[nd::test]` gives you a fresh `TestApp` and runs the same test natively and in a browser
  under `wasm-bindgen-test`. Tests are synchronous. `cx.run_until_idle()` settles frames and
  queued events.
- `nd::test::{tree, by_test_id, text, click}` read and drive the tree by test id (an element's
  explicit id). `cx.emit(node, event)` delivers an event the way the backend would.
- Time comes from the test clock. `cx.advance_clock(1. / 60.)` ticks every window one frame.
- `cx.render_count(entity_id)` proves that a view didn't re-render. Use it to test that a list
  update leaves clean rows alone.
- To run the tier 3 locators in process, use `nd_test::TestHarness::new(TestApp::new())` (add
  `nd-test` as a dev-dependency).

### Tier 2: the appearance audit

`tests/appearance.rs` opens each window in light and dark mode on every backend kind that the
target links (plus two matugen palettes on GPUI) and reads back every fill and text run. It fails
on a color that isn't a theme token (or a state derived from one), and on text under WCAG
contrast (4.5:1, or 3:1 for large text). Give every window that you add a `.window(..)` line:

```rust
fn main() {
    nd::audit::Audit::new("my-app", my_app::app)
        .window("main", |cx| my_app::open(cx).id())
        .window("settings", |cx| my_app::open_settings(cx).id())
        .run()
}
```

Pass colors that your `theme.rs` names on purpose through `.allow(theme::colors())`. To
re-record golden images, set `ND_BLESS=1`. A golden that drifts leaves the diff image next to the
test.

### Tier 3

```rust
use nd_test::{LaunchOptions, Role, expect, launch_app};

#[test]
fn real_add_one_updates_the_count() {
    let app = launch_app(LaunchOptions::new(env!("CARGO_BIN_EXE_my-app"))).expect("launch");
    app.get_by_test_id("add").click().unwrap();
    expect(&app.get_by_role(Role::Text).name("1").exact()).to_be_visible().unwrap();
}
```

Locators: `get_by_test_id`, `get_by_role(..).name(..)`, `get_by_text`, `get_by_label`,
`locator("role=button[name=\"Save\"] >> nth=1")`. Actions wait for the element to be
actionable, and `expect(..)` polls until the session timeout (5 s). Each launch gets its own data
directory (`ND_APP_DATA_DIR`) and socket, so two runs never share settings or logs.

## The machine lock

Real input, window activation, and screen capture share the machine's one cursor and one focus.
Tier 3 runs that need them take a machine-wide lock (`flock` on `real-input.lock` under
`~/Library/Caches/nd/` on macOS, `~/.cache/nd/` on Linux, `%LOCALAPPDATA%\nd\` on Windows). A
second agent's run, in this worktree or any other, queues behind the lock and prints the holder's
worktree and purpose instead of stealing focus in the middle of a test.

- `launch_app` holds the lock by default on macOS. Use `LaunchOptions::machine_lock(false)` for
  an app that only gets events posted into its own queue.
- A test that moves the real cursor holds `nd_test::real_input("why")` for as long as it moves it.
- `ndshell lock status` shows who holds the lock. `ndshell lock -- <command>` runs any command
  under it.
- A queued run is waiting, not hung. Don't kill the holder, and never delete the lock file.

## Before reproducing a bug: read the incident

Every nd app runs a flight recorder that keeps the last 5000 log events, the last 200 inputs,
the semantic tree, frame records, and open overlays. A panic, a native crash, a hang, or
`nd::report_issue("what happened")` writes all of it to an incident bundle under the app's data
directory (`diagnostics/<timestamp>-<kind>/`).

```sh
nd diagnose --last        # newest bundle: what crashed, the app's frames, last inputs, the tree
nd diagnose --list        # every bundle, newest first
nd diagnose <id> --json   # one bundle, machine-readable
```

Start there. The bundle already says which window had focus, what was on screen, and what the
user pressed, so most bugs need no reproduction run at all. When the app itself knows that
something went wrong, call `nd::report_issue(..)` (it returns the bundle path) instead of logging
and waiting for someone to run it again. Bundles clean themselves up: nd keeps the newest 20,
none older than 14 days, and no more than 200 MB in total.

## Scripts around tests

- Delete only what your code made, by a path that it built and checked. Never run
  `rm -rf "$DIR/"` on a variable that can be empty, or `remove_dir_all` outside a root that you
  own.
- Stop processes by the pid that you started. Never run `pkill -f <pattern>`, because other
  agents' apps match it too.
- Don't use VMs or sleeps to wait for a window. `expect(..)` and `wait_for_stderr` poll.
