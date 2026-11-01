use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, Tag};

pub(crate) struct AudioInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<f64>,
    pub cover: Option<PathBuf>,
}

pub(crate) fn read(path: &Path, covers_dir: &Path) -> Option<AudioInfo> {
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let duration = file.properties().duration().as_secs_f64();
    Some(AudioInfo {
        title: tag.and_then(|t| t.title().map(|s| s.into_owned())),
        artist: tag.and_then(|t| t.artist().map(|s| s.into_owned())),
        album: tag.and_then(|t| t.album().map(|s| s.into_owned())),
        duration: (duration > 0.0).then_some(duration),
        cover: tag.and_then(|t| extract_cover(t, covers_dir)),
    })
}

fn extract_cover(tag: &Tag, covers_dir: &Path) -> Option<PathBuf> {
    let pics = tag.pictures();
    let pic = pics.iter().find(|p| p.pic_type() == PictureType::CoverFront).or_else(|| pics.first())?;
    let target = covers_dir.join(format!("{:x}.{}", md5::compute(pic.data()), extension(pic)));
    if !target.exists() {
        write_once(&target, pic.data())?;
    }
    Some(target)
}

fn extension(pic: &Picture) -> &'static str {
    match pic.mime_type() {
        Some(MimeType::Png) => "png",
        Some(MimeType::Gif) => "gif",
        Some(MimeType::Bmp) => "bmp",
        Some(MimeType::Tiff) => "tiff",
        Some(MimeType::Jpeg) => "jpg",
        _ => match infer::get(pic.data()).map(|t| t.extension()) {
            Some("png") => "png",
            Some("gif") => "gif",
            Some("webp") => "webp",
            _ => "jpg",
        },
    }
}

fn write_once(target: &Path, data: &[u8]) -> Option<()> {
    fs::create_dir_all(target.parent()?).ok()?;
    let tmp = target.with_extension(format!("tmp{}", std::process::id()));
    let mut f = fs::File::create(&tmp).ok()?;
    f.write_all(data).ok()?;
    drop(f);
    fs::rename(&tmp, target).ok()
}
