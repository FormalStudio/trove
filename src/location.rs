//! Where a tab is: a folder, or the Recent list, which is no folder at all.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    Dir(PathBuf),
    Recent,
}

impl Location {
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Self::Dir(p) => Some(p),
            Self::Recent => None,
        }
    }

    /// The tab's title and the path bar's last segment.
    pub fn title(&self, home: &Path, trash: &Path) -> String {
        match self {
            Self::Recent => "Recent".to_owned(),
            Self::Dir(p) if p == home => "Home".to_owned(),
            Self::Dir(p) if p == trash => "Trash".to_owned(),
            Self::Dir(p) => match p.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                None => "Computer".to_owned(),
            },
        }
    }

    pub fn parent(&self) -> Option<Location> {
        self.dir()?.parent().map(|p| Location::Dir(p.to_path_buf()))
    }
}

/// The volume `path` is shown under: the deepest one mounted above it, the root's aside. One
/// mounted inside Home (a network share at `~/.nas`) counts; Home itself sitting on a volume of
/// its own does not make that volume the place.
pub fn volume_above<'a>(path: &Path, home: &Path, volumes: &'a [trove_fs::Volume]) -> Option<&'a trove_fs::Volume> {
    let depth = |p: &Path| p.components().count();
    volumes
        .iter()
        .filter(|v| v.mount_point != Path::new("/") && path.starts_with(&v.mount_point))
        .max_by_key(|v| depth(&v.mount_point))
        .filter(|v| !path.starts_with(home) || depth(&v.mount_point) > depth(home))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        let home = Path::new("/home/u");
        let trash = Path::new("/home/u/.local/share/Trash/files");
        assert_eq!(Location::Dir(home.into()).title(home, trash), "Home");
        assert_eq!(Location::Dir("/".into()).title(home, trash), "Computer");
        assert_eq!(Location::Dir("/home/u/Music".into()).title(home, trash), "Music");
        assert_eq!(Location::Dir(trash.into()).title(home, trash), "Trash");
        assert_eq!(Location::Recent.title(home, trash), "Recent");
    }

    #[test]
    fn a_volume_inside_home_is_its_own_place() {
        use trove_fs::{Volume, VolumeKind};
        let volume = |at: &str, kind| Volume {
            mount_point: at.into(),
            device: String::new(),
            fs_type: String::new(),
            label: String::new(),
            kind,
            read_only: false,
        };
        let home = Path::new("/home/kyan");
        let volumes = [
            volume("/", VolumeKind::Root),
            volume("/home", VolumeKind::Internal),
            volume("/home/kyan/.nas", VolumeKind::Network),
            volume("/mnt/games", VolumeKind::Internal),
        ];
        let above = |p: &str| volume_above(Path::new(p), home, &volumes).map(|v| v.mount_point.clone());
        assert_eq!(above("/home/kyan/.nas/films"), Some("/home/kyan/.nas".into()));
        assert_eq!(above("/home/kyan/Documents"), None, "Home on a volume of its own stays Home");
        assert_eq!(above("/mnt/games/steam"), Some("/mnt/games".into()));
        assert_eq!(above("/etc"), None);
    }
}
