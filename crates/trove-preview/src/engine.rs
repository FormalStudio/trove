use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use crate::config::Config;
use crate::thumbs::{self, ThumbSize, thumbnailer::Registry};

/// A config plus the state built from it on first use (the thumbnailer table). Everything the
/// crate does synchronously hangs off it; the free functions run these on a worker thread.
pub struct Engine {
    pub(crate) config: Config,
    registry: OnceLock<Registry>,
}

impl Engine {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self { config, registry: OnceLock::new() })
    }

    /// The process-wide engine over the real environment.
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<Engine>> = OnceLock::new();
        GLOBAL.get_or_init(|| Engine::new(Config::from_env())).clone()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn registry(&self) -> &Registry {
        self.registry.get_or_init(|| Registry::load(&self.config.thumbnailer_dirs))
    }

    /// Blocking: the thumbnail for `path`, from the cache or freshly made.
    pub fn thumbnail(&self, path: &Path, mtime: SystemTime, size: ThumbSize) -> Option<PathBuf> {
        thumbs::thumbnail(self, path, mtime, size)
    }
}
