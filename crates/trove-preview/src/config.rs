use std::env;
use std::path::PathBuf;
use std::time::Duration;

/// Where the caches live and where thumbnailers are looked up. Tests build one over a tempdir so
/// nothing touches the real `~/.cache`.
#[derive(Clone, Debug)]
pub struct Config {
    /// `$XDG_CACHE_HOME/thumbnails`, shared with every freedesktop app.
    pub thumbnails_dir: PathBuf,
    /// Extracted album art, `$XDG_CACHE_HOME/trove/covers`.
    pub covers_dir: PathBuf,
    /// Directories holding `*.thumbnailer` files, highest priority first.
    pub thumbnailer_dirs: Vec<PathBuf>,
    pub thumbnailer_timeout: Duration,
    /// Larger files are not decoded by the image path.
    pub max_image_bytes: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let dir_var = |name: &str, fallback: &str| match env::var_os(name) {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => home.join(fallback),
        };
        let cache = dir_var("XDG_CACHE_HOME", ".cache");
        let data_home = dir_var("XDG_DATA_HOME", ".local/share");
        let data_dirs = match env::var("XDG_DATA_DIRS") {
            Ok(v) if !v.is_empty() => v,
            _ => "/usr/local/share:/usr/share".to_owned(),
        };
        let mut thumbnailer_dirs = vec![data_home.join("thumbnailers")];
        thumbnailer_dirs
            .extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("thumbnailers")));
        let system = PathBuf::from("/usr/share/thumbnailers");
        if !thumbnailer_dirs.contains(&system) {
            thumbnailer_dirs.push(system);
        }
        Self {
            thumbnails_dir: cache.join("thumbnails"),
            covers_dir: cache.join("trove").join("covers"),
            thumbnailer_dirs,
            thumbnailer_timeout: Duration::from_secs(10),
            max_image_bytes: 100 * 1024 * 1024,
        }
    }

    /// A config rooted in one directory, with no system thumbnailers.
    pub fn rooted(root: &std::path::Path) -> Self {
        Self {
            thumbnails_dir: root.join("thumbnails"),
            covers_dir: root.join("trove").join("covers"),
            thumbnailer_dirs: vec![root.join("thumbnailers")],
            thumbnailer_timeout: Duration::from_secs(10),
            max_image_bytes: 100 * 1024 * 1024,
        }
    }
}
