//! What the preview pane and Quick Look show for a path, and the freedesktop thumbnail cache.

mod archive;
mod audio;
mod config;
mod engine;
mod info;
mod mime;
mod preview;
mod text;
pub mod thumbs;

pub use config::Config;
pub use engine::Engine;
pub use info::{Info, info, info_blocking};
pub use preview::{Preview, preview, preview_with};
pub use thumbs::{ThumbSize, Thumbnails};
