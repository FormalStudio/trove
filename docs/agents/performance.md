# Performance

The reference machine is e1504g: an Intel Core i3-N305 laptop with an integrated GPU and a 60 Hz
panel. If an app is smooth there, it's smooth everywhere. For the short list of rules, see
[donts.md](donts.md).

## Budgets

| Measure | Budget | Measured so far |
| --- | --- | --- |
| Cold start to first frame | under 300 ms | 106 ms median for a bare GPUI window |
| Idle CPU over 60 s, focused and unfocused | 0.0% | 0.000% for a bare GPUI window |
| Key press to present, p95 | under 16.7 ms at 60 Hz | |
| Scrolling 100k rows | no missed frames at the display rate | p95 17.0 ms, 1 to 2 missed of about 600 frames at 60 Hz (the first frame) |
| Resident memory | under 80 MB | 177 MB for a bare GPUI window while the GL path is still linked |
| One view's render | under 1 ms and under 500 nodes | |

A still window does no work. No frame runs unless a view is dirty or an animation is live, so a
timer that polls or a view that notifies itself every frame shows up as CPU use on a window that
nobody is touching.

## How rendering stays cheap

- `cx.notify()` re-renders one view. Its child entities are boundaries and stay as they were.
  The cost is the size of the dirty view, never the window, so split a busy screen into views
  along the lines where state changes (for example, the sidebar, the list, the editor, and the
  status bar).
- nd compares the new element tree with the retained one and sends only changed properties to
  the backend. Building the same tree again is cheap. Building a bigger tree than you need isn't.
- Lists render the rows near the viewport. With `set_items_versioned`, a re-render of the owner
  reuses every row whose version didn't change. To prove it in tier 1, call `cx.render_count(..)`
  on a row view before and after an unrelated update.

## Keep the main thread free

The main thread runs input, layout, and every frame. Move anything that can take more than a
frame somewhere else:

- For network, disk, parsing, and decoding, use `nd::system::spawn(future, |result, cx| ..)`. The
  future runs on a worker, and the closure brings the result back to the main thread.
- For images, `image(..)` decodes at display size on worker threads, newest request first, with an
  LRU in memory and a disk cache for downloads. Never decode a full-size image in `render`.
- Don't make blocking calls in a handler: no `std::fs::read` of a large file, no synchronous HTTP,
  and no `thread::sleep` (`clock-in-ui` catches the last one).

## Frame timing and monitors

Each window has its own frame clock at its display's refresh rate: 60 Hz on e1504g, 120 Hz
ProMotion on a MacBook, and 240 Hz and 144 Hz side by side on g815. A window that you drag to
another display switches clocks. So:

- Animate against time, never frame counts. Sample `window.now()`, and let the frame clock decide
  how often that happens.
- Use the theme's `motion.{fast, normal, slow}` and `motion.spring`. They also respect the user's
  reduced-motion setting.
- An animation re-renders its view every frame while it runs, so keep animated content in its
  own small view.

## Measuring

```sh
nd diag scroll --rows 100000          # scroll a virtual list every frame, check the frame budget
cargo build --release                 # budgets are release numbers; debug builds are not comparable
```

`nd diag scroll` passes at 2 or fewer missed frames, with p95 within 1.25 display periods. On
the GPUI hosts, `ND_ANIMATE=<seconds>` re-renders every window every frame for that many seconds
and prints the frame line, which shows what a heavy view costs. Measure on the slowest machine
that you have, from a release build, before and after the change. Put the number in the commit
or the README.
