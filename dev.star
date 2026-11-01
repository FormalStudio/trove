# rustup's proxies act on the name they run as and install the pinned toolchain on first use.
RUST = ["cargo", "rustc", "rustdoc", "rustfmt", "cargo-fmt", "cargo-clippy", "clippy-driver"]

# gpui-pre links libxkbcommon and freetype at build time and dlopens wayland, vulkan,
# fontconfig and X11 at runtime.
LIBS = ["xkbcommon", "wayland-client", "wayland-protocols", "vulkan", "fontconfig", "freetype2",
        "xcb", "x11", "xcursor", "xi", "xrandr", "egl", "alsa", "libnotify"]

dev(
    packages = [pkg("rustup", links = {name: "rustup" for name in RUST})],
    libs = [lib(n) for n in LIBS] if platform.is_linux else [],
    bins = [bin(n) for n in ["pkg-config", "gcc", "grim", "wl-copy"]] if platform.is_linux else [],
    env = {"RUSTUP_TOOLCHAIN": "1.95.0"},
)
