use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use trove_fs::FileKind;

use crate::engine::Engine;
use crate::thumbs::ThumbSize;
use crate::{archive, audio, mime, text};

#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// The UI draws it with nd's `image()`; 0x0 when an SVG gives no size.
    Image {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    Text {
        text: String,
        truncated: bool,
        language: Option<&'static str>,
        lines: usize,
    },
    /// `size` stays `None`: folder sizes are a separate, opt-in walk.
    Folder {
        items: u32,
        hidden: u32,
        size: Option<u64>,
    },
    /// `total` is the full count, or a lower bound when counting hit its time budget.
    Archive {
        entries: Vec<String>,
        total: usize,
    },
    Audio {
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        duration: Option<f64>,
        cover: Option<PathBuf>,
    },
    /// A Large thumbnail of a video, PDF, font or document.
    Thumbnail {
        path: PathBuf,
    },
    None,
}

/// On a worker thread. The receiver yields exactly one value, or disconnects if the thread could
/// not start.
pub fn preview(path: PathBuf, max_text: usize) -> flume::Receiver<Preview> {
    preview_with(Engine::global(), path, max_text)
}

pub fn preview_with(engine: Arc<Engine>, path: PathBuf, max_text: usize) -> flume::Receiver<Preview> {
    let (tx, rx) = flume::bounded(1);
    let spawned = std::thread::Builder::new().name("trove-preview".into()).spawn(move || {
        let _ = tx.send(engine.preview(&path, max_text));
    });
    drop(spawned);
    rx
}

/// Formats the UI can draw as they are; every other image goes through a thumbnailer.
const DRAWABLE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "svg"];

impl Engine {
    /// Blocking form of [`preview`].
    pub fn preview(&self, path: &Path, max_text: usize) -> Preview {
        let Ok(meta) = fs::metadata(path) else { return Preview::None };
        if meta.is_dir() {
            return folder(path);
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let thumbnail = || {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            self.thumbnail(path, mtime, ThumbSize::Large).map_or(Preview::None, |path| Preview::Thumbnail { path })
        };
        match FileKind::from_name(name) {
            FileKind::Image => {
                let ext = trove_fs::entry::extension_of(name).to_ascii_lowercase();
                match DRAWABLE.contains(&ext.as_str()).then(|| image_size(path)).flatten() {
                    Some((width, height)) => Preview::Image { path: path.to_path_buf(), width, height },
                    None => thumbnail(),
                }
            }
            FileKind::Text | FileKind::Code => text_preview(path, name, max_text).unwrap_or(Preview::None),
            FileKind::Unknown => text_preview(path, name, max_text).unwrap_or_else(|| {
                // Binary with a name that says nothing: a thumbnailer may know it by content.
                if self.registry().handles(&mime::mime_of(path, false)) { thumbnail() } else { Preview::None }
            }),
            FileKind::Audio => match audio::read(path, &self.config.covers_dir) {
                Some(a) => Preview::Audio {
                    title: a.title,
                    artist: a.artist,
                    album: a.album,
                    duration: a.duration,
                    cover: a.cover,
                },
                None => Preview::None,
            },
            FileKind::Archive => match archive::list(path) {
                Some(l) => Preview::Archive { entries: l.entries, total: l.total },
                None => Preview::None,
            },
            FileKind::Video
            | FileKind::Pdf
            | FileKind::Document
            | FileKind::Spreadsheet
            | FileKind::Presentation
            | FileKind::Font
            | FileKind::DiskImage
            | FileKind::Executable => thumbnail(),
            FileKind::Folder => Preview::None,
        }
    }
}

fn text_preview(path: &Path, name: &str, max_text: usize) -> Option<Preview> {
    let t = text::read_text(path, max_text).ok()??;
    Some(Preview::Text { text: t.text, truncated: t.truncated, language: text::language_of(name), lines: t.lines })
}

fn folder(path: &Path) -> Preview {
    let Ok(rd) = fs::read_dir(path) else { return Preview::None };
    let (mut items, mut hidden) = (0u32, 0u32);
    for e in rd.map_while(Result::ok) {
        items = items.saturating_add(1);
        let name = e.file_name();
        let b = name.as_encoded_bytes();
        if b.first() == Some(&b'.') || b.last() == Some(&b'~') {
            hidden = hidden.saturating_add(1);
        }
    }
    Preview::Folder { items, hidden, size: None }
}

/// Pixel size from the header alone.
pub(crate) fn image_size(path: &Path) -> Option<(u32, u32)> {
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if is_svg {
        return Some(svg_size(path).unwrap_or((0, 0)));
    }
    let s = imagesize::size(path).ok()?;
    Some((u32::try_from(s.width).ok()?, u32::try_from(s.height).ok()?))
}

fn svg_size(path: &Path) -> Option<(u32, u32)> {
    let mut head = Vec::new();
    fs::File::open(path).ok()?.take(4096).read_to_end(&mut head).ok()?;
    let head = String::from_utf8_lossy(&head);
    let start = head.find("<svg")?;
    let tag = &head[start..head[start..].find('>').map_or(head.len(), |i| start + i)];
    let attr = |name: &str| -> Option<&str> {
        let mut from = 0;
        while let Some(i) = tag[from..].find(name) {
            let at = from + i;
            let rest = &tag[at + name.len()..];
            let boundary = at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace();
            if boundary && let Some(rest) = rest.trim_start().strip_prefix('=') {
                let rest = rest.trim_start();
                let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
                return rest[1..].split(q).next();
            }
            from = at + name.len();
        }
        None
    };
    let px = |v: &str| -> Option<u32> {
        let v = v.trim();
        let num = v.strip_suffix("px").unwrap_or(v);
        num.parse::<f64>().ok().filter(|n| *n > 0.0).map(|n| n.round() as u32)
    };
    if let (Some(w), Some(h)) = (attr("width").and_then(px), attr("height").and_then(px)) {
        return Some((w, h));
    }
    let mut vb = attr("viewBox")?.split([' ', ',']).filter(|s| !s.is_empty()).skip(2);
    Some((px(vb.next()?)?, px(vb.next()?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::io::Write;

    fn engine(root: &Path) -> Arc<Engine> {
        Engine::new(Config::rooted(root))
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::DynamicImage::new_rgb8(w, h)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn image_reports_header_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.png", &png_bytes(30, 20));
        let e = engine(dir.path());
        assert_eq!(e.preview(&p, 100), Preview::Image { path: p, width: 30, height: 20 });
    }

    #[test]
    fn svg_dimensions_from_attributes_or_view_box() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.svg", br#"<?xml version="1.0"?><svg xmlns="x" width="48px" height='32'></svg>"#);
        let b = write(dir.path(), "b.svg", br#"<svg viewBox="0 0 24 12"></svg>"#);
        let c = write(dir.path(), "c.svg", br#"<svg width="100%"></svg>"#);
        let e = engine(dir.path());
        let dims = |p: &Path| match e.preview(p, 10) {
            Preview::Image { width, height, .. } => (width, height),
            other => panic!("{other:?}"),
        };
        assert_eq!(dims(&a), (48, 32));
        assert_eq!(dims(&b), (24, 12));
        assert_eq!(dims(&c), (0, 0));
    }

    #[test]
    fn text_code_and_unknown_with_text_content() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path());
        let rs = write(dir.path(), "main.rs", b"fn main() {}\n");
        assert_eq!(
            e.preview(&rs, 1000),
            Preview::Text { text: "fn main() {}\n".into(), truncated: false, language: Some("rust"), lines: 1 }
        );
        let unknown = write(dir.path(), "notes.zzz", b"hello\nworld\n");
        assert_eq!(
            e.preview(&unknown, 5),
            Preview::Text { text: "hello".into(), truncated: true, language: None, lines: 1 }
        );
        let bin = write(dir.path(), "blob.zzz", &[0, 1, 2, 3, 255]);
        assert_eq!(e.preview(&bin, 100), Preview::None);
        let fake_text = write(dir.path(), "x.txt", &[0, 0, 0, 1]);
        assert_eq!(e.preview(&fake_text, 100), Preview::None);
    }

    #[test]
    fn folder_counts_entries_and_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("d");
        fs::create_dir(&d).unwrap();
        for n in ["a", "b", ".h", "c~"] {
            fs::write(d.join(n), "").unwrap();
        }
        fs::create_dir(d.join("sub")).unwrap();
        assert_eq!(engine(dir.path()).preview(&d, 10), Preview::Folder { items: 5, hidden: 2, size: None });
    }

    #[test]
    fn archive_preview() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.zip");
        let mut w = zip::ZipWriter::new(fs::File::create(&p).unwrap());
        w.start_file(
            "x/y.txt",
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
        assert_eq!(engine(dir.path()).preview(&p, 10), Preview::Archive { entries: vec!["x/y.txt".into()], total: 1 });
    }

    #[test]
    fn missing_path_and_unthumbnailable_kinds_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine(dir.path());
        assert_eq!(e.preview(&dir.path().join("nope"), 10), Preview::None);
        let pdf = write(dir.path(), "a.pdf", b"%PDF-1.4");
        assert_eq!(e.preview(&pdf, 10), Preview::None, "no thumbnailer installed in the test root");
    }

    #[test]
    fn channel_form_delivers_one_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.txt", b"hi");
        let got = preview_with(engine(dir.path()), p, 10).recv().unwrap();
        assert!(matches!(got, Preview::Text { lines: 1, .. }));
    }

    #[test]
    fn audio_tags_and_cover_extraction() {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::tag::{Accessor, Tag, TagExt, TagType};
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("song.wav");
        let data_len = 8000u32 * 2;
        let mut b = Vec::new();
        b.extend(b"RIFF");
        b.extend((36 + data_len).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(8000u32.to_le_bytes());
        b.extend(16000u32.to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(data_len.to_le_bytes());
        b.extend(vec![0u8; data_len as usize]);
        fs::write(&wav, b).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Song".into());
        tag.set_artist("Band".into());
        tag.set_album("Record".into());
        let art = png_bytes(4, 4);
        tag.push_picture(
            Picture::unchecked(art.clone()).pic_type(PictureType::CoverFront).mime_type(MimeType::Png).build(),
        );
        tag.save_to_path(&wav, WriteOptions::default()).unwrap();

        let e = engine(dir.path());
        let Preview::Audio { title, artist, album, duration, cover } = e.preview(&wav, 10) else { panic!("not audio") };
        assert_eq!(
            (title.as_deref(), artist.as_deref(), album.as_deref()),
            (Some("Song"), Some("Band"), Some("Record"))
        );
        assert!((duration.unwrap() - 1.0).abs() < 0.01);
        let cover = cover.unwrap();
        assert!(cover.starts_with(&e.config.covers_dir) && cover.extension().unwrap() == "png");
        assert_eq!(fs::read(&cover).unwrap(), art);
        let mtime = fs::metadata(&cover).unwrap().modified().unwrap();
        let Preview::Audio { cover: again, .. } = e.preview(&wav, 10) else { panic!() };
        assert_eq!(again.as_deref(), Some(cover.as_path()));
        assert_eq!(fs::metadata(&cover).unwrap().modified().unwrap(), mtime, "written once");
    }
}
