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
}
