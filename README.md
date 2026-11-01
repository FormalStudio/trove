<p align="center">
  <!-- nd: icon, 160 px wide: <img src="assets/icon.png" width="160" alt="Trove icon"> -->
</p>

# Trove

[![Built with NativeDesktop](https://img.shields.io/badge/built_with-NativeDesktop-4a9eda?style=for-the-badge&labelColor=161616)](https://github.com/FormalStudio/NativeDesktop)
[![Rust](https://img.shields.io/badge/rust-2024-4a9eda?style=for-the-badge&labelColor=161616&logo=rust&logoColor=white)](src/)
[![Status](https://img.shields.io/badge/status-first_scaffold-d35f5f?style=for-the-badge&labelColor=161616)](#features)

A file manager for Linux that opens instantly, even with a dead network mount in the sidebar.

<!-- nd: hero screenshot from `nd docs capture`: ![Trove](docs/images/main.png) -->

```
┌──────────────────────┐   chunks of names,   ┌──────────────────────────┐
│ trove-fs             │   then sizes, dates  │ src/tab.rs               │
│ listing threads      │ ───────────────────▶ │ one tab: history, sort,  │
│ per folder           │ ◀─────────────────── │ filter, selection, view  │
└──────────────────────┘   drop = cancel      └──────────────────────────┘
```

Each tab streams its folder from worker threads: names land first, sizes and dates fill in row
by row as they are read, and leaving a folder drops its listing instead of waiting for it. The
first frame waits on nothing but a few small config files.

A release build reaches its first frame about 150 ms after the process starts (g815, headless
Hyprland, median of ten launches).

<!-- nd: two-column screenshot table from `nd docs capture`:
| | |
|---|---|
| ![Light](docs/images/light.png) | ![Dark](docs/images/dark.png) |
-->

## Features

- List and grid views, Finder-style dates, natural order with folders first, sort by any column.
- Tabs with their own history, folder, selection and view: Ctrl+T, Ctrl+W, Ctrl+Tab, Alt+1..9.
- A breadcrumb path bar that turns into a text field on Ctrl+L, with Tab completion.
- Search that filters the current folder as you type, type-select, Ctrl+H for hidden files.
- A sidebar of places, bookmarks, devices with free space, and network mounts with eject.
- `TROVE_DEMO=1` opens a fixed fixture folder, which is what the tests and screenshots use.
- Follows the system: light and dark, the accent colour, and matugen palettes on Linux, live.
- Writes an incident bundle when it crashes or hangs, so a bug report comes with the last inputs
  and what was on screen.

Not working yet: copy, move, rename and trash; previews of file contents; search beyond the
current folder; Shift-click ranges, rubber-band selection and the mouse's back and forward
buttons.

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
