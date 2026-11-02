//! `TROVE_DEMO=1`: a fixed home under `$XDG_RUNTIME_DIR/trove-demo` (or the temp dir), made
//! on start, so tests and screenshots show the same folders, sizes and dates everywhere. Files
//! are only ever created or rewritten here, never deleted.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn enabled() -> bool {
    std::env::var_os("TROVE_DEMO").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn root() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("trove-demo")
}

/// Recent's fixed list, relative to the root, newest first.
pub const RECENT: &[&str] = &[
    "Documents/Quarterly report.docx",
    "Pictures/Holiday/IMG_0142.jpg",
    "Projects/trove/README.md",
    "Downloads/trove-0.1.0.tar.gz",
    "Music/Album/03 Night drive.flac",
];

/// When a file was last changed: days before today at a time of day, so "Today" and
/// "Yesterday" stay true whatever day the demo runs.
#[derive(Clone, Copy)]
enum When {
    DaysAgo(u64, u64, u64),
    /// A fixed date (seconds since the epoch), for the older files.
    At(u64),
}

const DIRS: &[&str] = &[
    "Desktop",
    "Documents",
    "Documents/Taxes 2025",
    "Downloads",
    "Music",
    "Music/Album",
    "Pictures",
    "Pictures/Holiday",
    "Pictures/Screenshots",
    "Projects",
    "Projects/trove",
    "Projects/trove/src",
    "Projects/website",
    "Videos",
    "Logs",
    ".config",
    ".local/share/Trash/files",
];

/// 2026-03-14 10:00 UTC, 2025-11-02 16:30 UTC, 2024-07-21 08:15 UTC.
const MARCH: u64 = 1_773_482_400;
const NOVEMBER: u64 = 1_762_101_000;
const JULY: u64 = 1_721_549_700;

const FILES: &[(&str, u64, When)] = &[
    ("Desktop/Meeting notes.txt", 1_240, When::DaysAgo(0, 9, 41)),
    ("Desktop/Screenshot 2026-10-30 at 09.41.png", 418_000, When::DaysAgo(2, 9, 41)),
    ("Documents/Quarterly report.docx", 48_600, When::DaysAgo(0, 14, 2)),
    ("Documents/Budget 2026.xlsx", 21_400, When::DaysAgo(1, 9, 10)),
    ("Documents/Roadmap.pptx", 2_300_000, When::At(MARCH)),
    ("Documents/Invoice 2026-03.pdf", 96_200, When::At(MARCH)),
    ("Documents/chapter1.md", 6_100, When::At(NOVEMBER)),
    ("Documents/chapter2.md", 7_400, When::At(NOVEMBER)),
    ("Documents/chapter10.md", 5_200, When::At(NOVEMBER)),
    ("Documents/Taxes 2025/Return.pdf", 210_000, When::At(MARCH)),
    ("Downloads/trove-0.1.0.tar.gz", 4_100_000, When::DaysAgo(0, 11, 26)),
    ("Downloads/Inter.ttf", 804_000, When::At(JULY)),
    ("Downloads/openSUSE-Tumbleweed.iso", 1_600_000, When::At(NOVEMBER)),
    ("Downloads/photos.zip", 3_200_000, When::DaysAgo(1, 20, 5)),
    ("Downloads/setup.AppImage", 2_700_000, When::At(JULY)),
    ("Music/Album/01 Opening.flac", 3_100_000, When::At(JULY)),
    ("Music/Album/02 Coastline.flac", 2_900_000, When::At(JULY)),
    ("Music/Album/03 Night drive.flac", 3_400_000, When::At(JULY)),
    ("Music/Album/10 Encore.flac", 2_200_000, When::At(JULY)),
    ("Pictures/sunset.jpg", 1_200_000, When::DaysAgo(1, 18, 20)),
    ("Pictures/diagram.svg", 12_300, When::At(MARCH)),
    ("Pictures/Holiday/IMG_0142.jpg", 2_400_000, When::DaysAgo(3, 12, 0)),
    ("Pictures/Holiday/IMG_0143.jpg", 2_600_000, When::DaysAgo(3, 12, 1)),
    ("Pictures/Holiday/IMG_0150.jpg", 2_100_000, When::DaysAgo(3, 12, 4)),
    ("Pictures/Screenshots/Screenshot 1.png", 220_000, When::At(NOVEMBER)),
    ("Projects/trove/Cargo.toml", 1_600, When::DaysAgo(0, 13, 37)),
    ("Projects/trove/README.md", 4_276, When::DaysAgo(0, 13, 40)),
    ("Projects/trove/src/main.rs", 120, When::DaysAgo(1, 9, 12)),
    ("Projects/trove/src/app.rs", 18_400, When::DaysAgo(0, 13, 55)),
    ("Projects/website/index.html", 3_900, When::At(MARCH)),
    ("Projects/website/style.css", 2_100, When::At(MARCH)),
    ("Videos/Launch.mp4", 4_800_000, When::At(NOVEMBER)),
    ("Notes.md", 2_048, When::DaysAgo(0, 8, 15)),
    ("todo.txt", 312, When::DaysAgo(1, 22, 47)),
    (".bashrc", 3_771, When::At(JULY)),
    (".config/settings.toml", 220, When::At(JULY)),
];

/// Builds or refreshes the fixture and returns its root.
pub fn ensure() -> io::Result<PathBuf> {
    ensure_at(root())
}

/// The fixture at `root` instead of the shared one: tests that change files get their own.
pub fn ensure_at(root: PathBuf) -> io::Result<PathBuf> {
    for dir in DIRS {
        fs::create_dir_all(root.join(dir))?;
    }
    let midnight = local_midnight();
    for (rel, size, when) in FILES {
        write(&root.join(rel), *size, stamp(*when, midnight))?;
    }
    // Enough rows to scroll, in natural order: log 2 before log 10.
    for n in 1..=120u64 {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(MARCH + n * 3_600);
        write(&root.join(format!("Logs/build {n}.log")), 600 + n * 37, at)?;
    }
    for dir in ["Desktop", "Documents", "Downloads", "Pictures", "Projects", "Projects/trove", "Logs"] {
        set_mtime(&root.join(dir), stamp(When::DaysAgo(0, 9, 0), midnight))?;
    }
    Ok(root)
}

fn write(path: &Path, size: u64, modified: SystemTime) -> io::Result<()> {
    let fresh = fs::metadata(path).map(|m| m.len() == size).unwrap_or(false);
    if !fresh {
        // A line of text, then a hole up to the size: big files cost no memory on tmpfs.
        let mut f = File::create(path)?;
        let line = b"Trove demo file.\n";
        f.write_all(&line[..line.len().min(size as usize)])?;
        f.set_len(size)?;
    }
    set_mtime(path, modified)
}

fn set_mtime(path: &Path, modified: SystemTime) -> io::Result<()> {
    File::options().read(true).open(path)?.set_modified(modified)
}

fn stamp(when: When, midnight: SystemTime) -> SystemTime {
    match when {
        When::DaysAgo(days, h, m) => {
            midnight - Duration::from_secs(days * 86_400) + Duration::from_secs(h * 3_600 + m * 60)
        }
        When::At(secs) => SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
    }
}

/// Today's local midnight.
fn local_midnight() -> SystemTime {
    let tz = jiff::tz::TimeZone::system();
    let now = jiff::Timestamp::now().to_zoned(tz);
    now.start_of_day().map(|z| SystemTime::from(z.timestamp())).unwrap_or_else(|_| SystemTime::now())
}
