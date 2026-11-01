<p align="center">
  <!-- nd: icon, 160 px wide: <img src="assets/icon.png" width="160" alt="Trove icon"> -->
</p>

# Trove

[![Built with NativeDesktop](https://img.shields.io/badge/built_with-NativeDesktop-4a9eda?style=for-the-badge&labelColor=161616)](https://github.com/FormalStudio/NativeDesktop)
[![Rust](https://img.shields.io/badge/rust-2024-4a9eda?style=for-the-badge&labelColor=161616&logo=rust&logoColor=white)](src/)
[![Status](https://img.shields.io/badge/status-first_scaffold-d35f5f?style=for-the-badge&labelColor=161616)](#features)

A counter with one button. <!-- nd: replace with one plain sentence: what it is and who it is for, e.g. "iMessage on Linux." -->

<!-- nd: hero screenshot from `nd docs capture`: ![Trove](docs/images/main.png) -->

```
┌──────────────────────┐   ops: create, update   ┌──────────────────────────┐
│ src/app.rs (Rust)    │ ──────────────────────▶ │ backend                  │
│ Counter view         │ ◀────────────────────── │ AppKit on macOS, GPUI on │
└──────────────────────┘   events: clicks, keys  │ Linux, Windows and web   │
                                                 └──────────────────────────┘
```

The window is one Rust view tree. On macOS the toolbar is a real `NSToolbar` and the button a
real `NSButton`; on Linux the same tree draws through GPUI in your matugen colours, and on Windows
in Fluent. Clicking sends one event back, the count goes up, and only the text that changed is
touched.

<!-- nd: one measured number in a plain sentence, e.g. "It opens in 140 ms and idles at 0% CPU." Measure a release build on the slowest machine you have. -->

<!-- nd: two-column screenshot table from `nd docs capture`:
| | |
|---|---|
| ![Light](docs/images/light.png) | ![Dark](docs/images/dark.png) |
-->

## Features

- Counts clicks. Starts at 42 with `TROVE_DEMO=1`, which is what the tests and screenshots use.
- Follows the system: light and dark, the accent colour, and matugen palettes on Linux, live.
- Writes an incident bundle when it crashes or hangs, so a bug report comes with the last inputs
  and what was on screen.

Not working yet: nothing, because it does nothing yet. Keep this line honest as it grows.

<!-- nd: replace these with what the app does. Numbers beat adjectives: "100k rows", "under 150 ms". A yes/no table works when features differ by platform. Say what does not work yet. -->

## Install

### Nix

<!-- nd: `nix run github:<owner>/trove`, then a Home Manager snippet:
```nix
programs.trove = {
  enable = true;
  settings = { };   # written to ~/.config/trove/
};
```
-->

### Other Linux

```sh
git clone <!-- nd: repository URL -->
cd trove
nd package linux            # AppImage in target/nd/
```

### macOS

<!-- nd: download link for the .dmg, or build it: `nd package mac`. Note the minimum macOS version. -->

### Windows

<!-- nd: download link for Setup.exe, or build it: `nd package windows`. -->

## Configuration

<!-- nd: the settings file path and a short sample, once the app has settings. -->

| Variable | Effect |
| --- | --- |
| `TROVE_DEMO=1` | start on fixed demo state |

## Keyboard

| | |
|---|---|
| `Tab` / `Shift+Tab` | move focus |
| `Esc` | close the open popover |

`Cmd` on macOS. <!-- nd: one row per shortcut the app binds with `KeyBinding::new("secondary-..", ..)`. -->

## Development

```sh
just dev                  # run it; TROVE_DEMO=1 just dev for demo state
nd check                  # fmt, clippy, framework lints, appearance audit
nd test                   # tiers 1 and 2, parallel, no window
nd test --tier real       # tier 3, real windows
nd serve --target web     # the web build on localhost:8080
```

`src/app.rs` is the window, `src/demo.rs` the fixed state for tests and screenshots, and
`build.rs` links the Swift side on macOS. `CLAUDE.md` and `docs/agents/` have the rules for
agents working here.

## License

<!-- nd: [MIT](LICENSE), plus any bundled fonts or assets under their own licences. -->
