//! The icons nd does not ship, registered from Lucide's own SVGs (`src/icons/`, ISC, same
//! release as nd's vendored set), and which icon each kind of file and place gets.

use std::sync::OnceLock;

use nd::prelude::*;
use trove_fs::{FileKind, PlaceIcon, VolumeKind};

struct Extra {
    list: Icon,
    grid: Icon,
    drive: Icon,
    usb: Icon,
    server: Icon,
    phone: Icon,
    desktop: Icon,
    code: Icon,
    archive: Icon,
    spreadsheet: Icon,
    presentation: Icon,
    film: Icon,
    font: Icon,
    app: Icon,
    eject: Icon,
    recent: Icon,
    documents: Icon,
    symlink: Icon,
    no_results: Icon,
}

static EXTRA: OnceLock<Extra> = OnceLock::new();

fn reg(name: &'static str, svg: &str, symbols: &[&'static str], fallback: Icon) -> Icon {
    nd::icons::register(name, svg, symbols).unwrap_or(fallback)
}

fn extra() -> &'static Extra {
    EXTRA.get_or_init(|| Extra {
        list: reg("list", include_str!("icons/list.svg"), &["list.bullet"], Icon::Menu),
        grid: reg("layout-grid", include_str!("icons/layout-grid.svg"), &["square.grid.2x2"], Icon::Square),
        drive: reg("hard-drive", include_str!("icons/hard-drive.svg"), &["internaldrive"], Icon::Disc3),
        usb: reg("usb", include_str!("icons/usb.svg"), &["externaldrive"], Icon::Disc3),
        server: reg("server", include_str!("icons/server.svg"), &["server.rack"], Icon::Globe),
        phone: reg("smartphone", include_str!("icons/smartphone.svg"), &["iphone"], Icon::Disc3),
        desktop: reg("monitor", include_str!("icons/monitor.svg"), &["menubar.dock.rectangle"], Icon::Folder),
        code: reg(
            "file-code",
            include_str!("icons/file-code.svg"),
            &["chevron.left.forwardslash.chevron.right"],
            Icon::Code,
        ),
        archive: reg("file-archive", include_str!("icons/file-archive.svg"), &["doc.zipper"], Icon::Archive),
        spreadsheet: reg(
            "file-spreadsheet",
            include_str!("icons/file-spreadsheet.svg"),
            &["tablecells"],
            Icon::FileText,
        ),
        presentation: reg("presentation", include_str!("icons/presentation.svg"), &["play.rectangle"], Icon::FileText),
        film: reg("film", include_str!("icons/film.svg"), &["film"], Icon::Video),
        font: reg("type", include_str!("icons/type.svg"), &["textformat"], Icon::File),
        app: reg("app-window", include_str!("icons/app-window.svg"), &["macwindow"], Icon::File),
        eject: reg("eject", include_str!("icons/eject.svg"), &["eject"], Icon::CircleArrowUp),
        recent: reg("history", include_str!("icons/history.svg"), &["clock.arrow.circlepath"], Icon::Clock),
        documents: reg("book-open", include_str!("icons/book-open.svg"), &["doc.text"], Icon::FileText),
        symlink: reg(
            "folder-symlink",
            include_str!("icons/folder-symlink.svg"),
            &["folder.badge.questionmark"],
            Icon::Folder,
        ),
        no_results: reg("search-x", include_str!("icons/search-x.svg"), &["magnifyingglass"], Icon::Search),
    })
}

/// Registers every extra icon; called once at startup, before the first frame needs one.
pub fn init() {
    let _ = extra();
}

pub fn list_view() -> Icon {
    extra().list
}

pub fn grid_view() -> Icon {
    extra().grid
}

pub fn eject() -> Icon {
    extra().eject
}

pub fn recent() -> Icon {
    extra().recent
}

pub fn no_results() -> Icon {
    extra().no_results
}

pub fn symlinked_folder() -> Icon {
    extra().symlink
}

pub fn for_kind(kind: FileKind) -> Icon {
    let x = extra();
    match kind {
        FileKind::Folder => Icon::Folder,
        FileKind::Image => Icon::Image,
        FileKind::Video => x.film,
        FileKind::Audio => Icon::Music,
        FileKind::Text | FileKind::Document | FileKind::Pdf => Icon::FileText,
        FileKind::Code => x.code,
        FileKind::Spreadsheet => x.spreadsheet,
        FileKind::Presentation => x.presentation,
        FileKind::Archive => x.archive,
        FileKind::DiskImage => Icon::Disc3,
        FileKind::Font => x.font,
        FileKind::Executable => x.app,
        FileKind::Unknown => Icon::File,
    }
}

pub fn for_place(icon: PlaceIcon) -> Icon {
    let x = extra();
    match icon {
        PlaceIcon::Home => Icon::House,
        PlaceIcon::Desktop => x.desktop,
        PlaceIcon::Documents => x.documents,
        PlaceIcon::Downloads => Icon::Download,
        PlaceIcon::Music => Icon::Music,
        PlaceIcon::Pictures => Icon::Image,
        PlaceIcon::Videos => x.film,
        PlaceIcon::Folder => Icon::Folder,
        PlaceIcon::Network => x.server,
        PlaceIcon::Trash => Icon::Trash,
        PlaceIcon::Recent => x.recent,
    }
}

pub fn for_volume(kind: VolumeKind) -> Icon {
    let x = extra();
    match kind {
        VolumeKind::Root | VolumeKind::Internal => x.drive,
        VolumeKind::Removable => x.usb,
        VolumeKind::Network | VolumeKind::Gvfs => x.server,
        VolumeKind::Phone => x.phone,
    }
}

/// The tint of a file's icon: folders in the accent, a few families in a role colour, the rest
/// quiet.
pub fn tint(kind: FileKind, theme: &Theme) -> Hsla {
    match kind {
        FileKind::Folder => theme.accent,
        FileKind::Image | FileKind::Video => theme.text.success,
        FileKind::Archive | FileKind::DiskImage => theme.text.warning,
        _ => theme.text.secondary,
    }
}
