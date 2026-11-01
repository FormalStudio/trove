fn main() {
    // Links the app's Swift (framework packages plus `[macos] swift_packages`) into one archive on
    // macOS; a no-op on every other target.
    nd::build::app();
}
