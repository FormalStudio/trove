# Don'ts

The hard list. Each line says what to do instead and, where `nd check` enforces it, the rule id
that fires. Silence a finding only with a reason: put `// nd-allow(rule-id): why` on the line or
the line above.

## Structure

- Don't draw a replica of a sidebar, toolbar, traffic light, caption button, sheet, menu, alert,
  or system control. Use `scaffold`, `toolbar`, `sidebar`, `source_list`, `titlebar`,
  `window_controls`, `window.open_sheet`, `.context_menu`, `window.alert`, or `glass`.
  ([patterns.md](patterns.md))
- Don't put `.drawn()` on a structural element to change its look. Anywhere else, it needs a
  comment that says why the native control can't do the job. (`drawn-reason`)
- Don't pad content past the window controls by hand. For a custom titlebar, use `titlebar()`,
  which reserves the controls' inset on every OS.
- Don't branch on the OS in view code. Use intents, the `.on_macos(..)` family, or
  `nd::platform::has(Feature::..)`. (`cfg-target-os`)
- Don't name `gpui` or `objc2` outside a native widget module. (`framework-internals`)

## Looks

- Don't write a color, radius, spacing, or font size literal in a view. Read `cx.theme()`. Color
  constructors live only in `src/theme.rs`. (`colour-literal`, and the appearance audit)
- Don't add a window without adding it to `tests/appearance.rs`.
- Don't fake glass with a translucent fill. Use `glass()`. ([styling.md](styling.md))
- Don't inline SVG. Use `Icon`. (`inline-svg`)

## Behavior

- Don't feed a scroll container with `.children(..)` for anything that can grow past 50 rows.
  Use `list`, `table`, `grid`, or `tree` over a keyed `ListState`. (`scroll-list`,
  [widgets.md](widgets.md))
- Don't key list rows by index, and don't rebuild the item list to force a refresh. Bump the
  row's version with `set_items_versioned`.
- Don't build popups from absolutely positioned divs or GPUI's `deferred`/`anchored`. Use
  `popover`, `panel`, `tooltip`, `.context_menu`, or a built-in. (`floating-element`)
- Don't guard controlled values with "ignore the next change" flags. Store what the event says
  and render it back. To refuse a change, don't store it.
- Don't call app code from a native callback. Emit through `Emit`, wrap setters in
  `cx.programmatic(..)`, and keep one `Synced<T>` for each controlled prop.
- Don't use `Instant::now`, `thread::sleep`, or `smol::Timer` in UI code. Use the frame clock
  (`window.now()`) and `nd::system::spawn`. (`clock-in-ui`)
- Don't block the main thread on disk, network, or decoding.
- Don't use `unwrap`, `expect`, `dbg!`, `todo!`, or `println!` outside tests. Return errors, or
  report them with `nd::report_issue(..)`. (clippy, denied by `nd check`)

## Dependencies

- Don't edit the `nd` git tag or the `[patch.crates-io]` GPUI block in `Cargo.toml` by hand.
  `nd upgrade` moves both together. A mismatch builds two GPUIs or fails to resolve.

## Testing and debugging

- Don't try to reproduce a bug before you read `nd diagnose --last`.
- Don't test in a real window what tier 1 can see. Tier 3 is for what only the OS does.
  ([testing.md](testing.md))
- Don't kill a tier 3 run that holds the machine lock, and don't delete the lock file. A queued
  run is waiting its turn.
- Don't sleep in tests. `expect(..)` and `run_until_idle()` wait for the condition.

## Shell and files

- Don't run `rm` or `mv` on a variable path: `rm -rf "$DIR/"` with an empty `DIR` is
  `rm -rf /`. Delete only directories that your code created, through one helper that refuses
  empty paths and anything outside its root.
- Don't run `pkill -f <pattern>`. It matches other agents' processes. Stop what you started by its
  pid.
- Don't discard uncommitted work that you didn't write (`git reset --hard`, `git checkout -- .`,
  `git stash`, `git clean`). Another agent might be in the middle of an edit in the same tree.
