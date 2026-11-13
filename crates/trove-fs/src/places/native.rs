use std::path::PathBuf;

use super::{Place, PlaceIcon};

fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Home and the system's own folders (Known Folders on Windows, `~/Desktop` and friends on
/// macOS), in the order the Linux sidebar shows them.
pub fn user_dirs() -> Vec<Place> {
    let home = home();
    let mut places = vec![Place { label: "Home".into(), path: home.clone(), icon: PlaceIcon::Home }];
    let dirs = [
        (dirs::desktop_dir(), PlaceIcon::Desktop),
        (dirs::document_dir(), PlaceIcon::Documents),
        (dirs::download_dir(), PlaceIcon::Downloads),
        (dirs::audio_dir(), PlaceIcon::Music),
        (dirs::picture_dir(), PlaceIcon::Pictures),
        (dirs::video_dir(), PlaceIcon::Videos),
    ];
    for (path, icon) in dirs {
        let Some(path) = path.filter(|p| *p != home) else { continue };
        let label = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        places.push(Place { label, path, icon });
    }
    places
}

/// Windows' Templates known folder; macOS has none.
pub fn templates_dir() -> Option<PathBuf> {
    dirs::template_dir()
}

/// Trove's own list, in the same `file://` URI-per-line format as GTK's bookmarks file.
pub(super) fn bookmarks_file() -> PathBuf {
    dirs::config_dir().unwrap_or_else(home).join("trove").join("bookmarks")
}

/// The system keeps no list of recent files that Trove can read here.
pub fn recent(_limit: usize) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn trash_dir() -> PathBuf {
    home().join(".Trash")
}

#[cfg(target_os = "macos")]
pub fn trash_files_dir() -> PathBuf {
    trash_dir()
}

/// The system drive's Recycle Bin. Its items are named by the shell (`$R..`), so it lists
/// without their original names.
#[cfg(windows)]
pub fn trash_dir() -> PathBuf {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    PathBuf::from(format!("{drive}\\$Recycle.Bin"))
}

#[cfg(windows)]
pub fn trash_files_dir() -> PathBuf {
    trash_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_places_start_at_home() {
        let p = user_dirs();
        assert_eq!(p.first().map(|p| p.icon), Some(PlaceIcon::Home));
        assert!(p.iter().all(|p| !p.label.is_empty()));
        assert!(recent(5).is_empty());
        assert!(bookmarks_file().ends_with("trove/bookmarks"));
    }
}
