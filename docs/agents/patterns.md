# Patterns

How an nd app is put together, and the shape that every new screen should take. The short list
of hard rules is in [donts.md](donts.md).

## Let the OS draw its own structure

Big structural pieces are intents. You say what the window has, and each backend maps it to the
highest-level component that its OS offers. The next OS release restyles the app with no change
to your code.

| You write | macOS draws | Linux and Windows draw |
| --- | --- | --- |
| `scaffold().sidebar(sidebar())` | `NSSplitViewItem` sidebar with a source list | GPUI sidebar on the skin's sidebar surface |
| `scaffold().inspector(inspector())` | the inspector split item | a right pane |
| `toolbar()` | `NSToolbar` with system items | a header row |
| `source_list(id)` / `source_tree(id)` | source-list `NSOutlineView` | drawn tree on the sidebar surface |
| `titlebar()` + `window_controls()` | real traffic lights in the slot | caption buttons, or nothing on Hyprland |
| `.context_menu(..)` | `NSMenu` | menu in the overlay layer |
| `window.open_sheet(..)` | a window sheet | a dialog over a scrim |
| `window.alert(..)`, `window.confirm(..)` | `NSAlert` | task dialog or portal |
| `glass()` | `NSGlassEffectView` (26+), `NSVisualEffectView` below | Mica, Acrylic or compositor blur |

Do:

```rust
scaffold()
    .toolbar(toolbar().title("Inbox").sidebar_toggle()
        .item(icon_button("new", Icon::SquarePen).tooltip("New Message").dispatch(NewMessage)))
    .sidebar(sidebar().min_w(px(180.)).child(
        source_list("folders")
            .section("Mailboxes", [source_item("inbox", "Inbox").icon(Icon::Inbox).badge("12")])
            .selected(self.folder.clone())
            .on_select(cx.listener(|this, e: &SourceSelected, _, cx| {
                this.folder = e.id.clone();
                cx.notify();
            })),
    ))
    .content(self.detail.clone())
```

Don't:

```rust
// A sidebar replica: a fixed-width div with a hand-picked grey and hand-drawn rows.
h_flex()
    .child(v_flex().w(px(220.)).bg(rgb(0xf2f2f7)).children(folders.iter().map(row)))
    .child(detail)
```

The replica never picks up the system sidebar material, selection style, keyboard behavior, or
accessibility, and it trips `colour-literal` and `scroll-list` in `nd check`.

A custom titlebar is `WindowOptions::main("main").titlebar(Titlebar::Custom)` plus a
`titlebar()` element. `titlebar()` reserves the side and size of the OS controls as insets
(traffic lights on the left on macOS, caption buttons on the right on Windows, nothing on
Hyprland), so you never pad content past the controls by hand. To move the traffic lights, use
`window_controls().at(point(px(18.), px(20.)))`. To let toolbar content carry them, use
`window_controls().embedded()`.

## State lives in entities

- Create one entity for each piece of state that has its own lifetime (`cx.new(|cx| ..)`). A view
  reads state in `render`, changes it in handlers, and then calls `cx.notify()`.
- `cx.notify()` re-renders only that view. Child entities are boundaries: split a screen into
  views so that typing in the editor doesn't rebuild the sidebar.
- `cx.observe(&entity, |this, entity, cx| ..)` reacts to another entity's `notify`. Keep the
  returned `Subscription` in the view, or `.detach()` it on purpose.
- Handlers come from `cx.listener(|this, event: &E, window, cx| ..)`. Row renderers for lists
  come from `cx.processor(Self::row)`.

## Actions, keys and menus

Commands are actions. One action drives the button, the shortcut, and the menu item, so they
never disagree.

```rust
actions!(mail, [NewMessage, DeleteMessage]);

cx.bind_keys([KeyBinding::new("secondary-n", NewMessage, Some("Mail"))]);

div().key_context("Mail").on_action(cx.listener(|this, _: &NewMessage, window, cx| this.compose(window, cx)))
```

`secondary` is Cmd on macOS and Ctrl elsewhere. Buttons fire actions with `.dispatch(Action)`,
and `set_menus(cx, ..)` builds the menu bar from the same actions.

## Controlled values

The app owns every value that a control shows. A control reports a change as an event, and the
app stores the value and renders it back. Feeding the reported value straight back is always
safe. nd doesn't set a native control to the value that it already holds, and it drops events
that fire while it applies a value. You get no echo loop and need no "ignore the next change"
flags.

```rust
slider("volume", self.volume).range(0.0..=1.0).on_change(cx.listener(|this, e: &SliderChanged, _, cx| {
    this.volume = e.value;
    cx.notify();
}))
```

To refuse a change, leave the state alone. The node emitted an event, so nd reaffirms it on the
next frame, and the control shows the app's value again. If you write a native widget yourself,
you keep the same contract with `Synced<T>`. See [widgets.md](widgets.md).

Text fields are native-owned while they have focus. `TextFieldState::set_text` from the app
waits for IME composition to finish, and `TextFieldState::last_event()` says whether the user
(`TextEvent::Edited`) or the app (`TextEvent::Replaced`) made the last change.

## Floating things are overlays

Popovers, panels, and tooltips are elements in the tree: `popover(id)`, `panel(id)`, and
`tooltip()`. The parent is the anchor. The overlay takes no room in the parent's layout and
closes with it. The app owns dismissal: `on_dismiss` receives `Dismissed { reason }`, and the app
stops rendering the overlay.

```rust
.when(self.popover, |d| d.child(
    popover("filters").p_3().child(filters)
        .on_dismiss(cx.listener(|this, _: &Dismissed, _, cx| {
            this.popover = false;
            cx.notify();
        })),
))
```

Modality decides what happens to input outside the overlay. `LightDismiss` (the popover
default) closes on an outside press and consumes it. `Modal` (the panel default) consumes it and
stays open. `Passive` (tooltips) takes no input. Input inside an overlay never reaches what's
under it, including web views. Menus, the command palette, toasts, and sheets are built-ins on
the same layer. An absolutely positioned div, or GPUI's `deferred`/`anchored` used as a popup,
leaks clicks and hover into what's beneath it and fails `floating-element`.

## Async and time

- To work off the main thread, use `nd::system::spawn(future, |output, cx| ..)`. The future runs
  on a worker. The closure runs on the main thread and can touch entities.
- Time in a view comes from the frame clock: `window.now()` (seconds, the frame's target
  present time) or `cx.now()`. Tier 1 tests move it with `cx.advance_clock(1. / 60.)`, so
  anything driven by it is deterministic.
- For wall-clock timestamps, use `nd::time::SystemTime`, which also works in a browser.
- Don't use `Instant::now`, `thread::sleep`, or `smol::Timer` in UI code (`clock-in-ui`). A sleep
  on the main thread freezes the window, and a wall clock in a view makes its test flaky.

## Platform differences

Use intents and the `.on_macos(..)` family, never `cfg!(target_os)` in view code. When a
capability might be missing on a target, ask: `nd::platform::has(Feature::MultiWindow)`. `cfg`
belongs in native widget modules (`src/native/`, or files named `appkit.rs`, `swift.rs`,
`win32.rs`, `wayland.rs`). Anywhere else, it needs a reason comment (`cfg-target-os`).

## Files and processes

Code and scripts delete only what they created, by a path that they built and checked. Never
run `rm -rf "$DIR/"` or `remove_dir_all(dir)` on a variable that can be empty or point outside a
directory that you own. Keep one helper that refuses an empty path and anything outside your
root, the way `nd`'s own CLI only clears directories under `target/nd`. Never run
`pkill -f <pattern>`, because it matches other agents' processes too. Stop what you started by
its pid.
