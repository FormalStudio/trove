use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub(crate) const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const SOFTWARE: &str = concat!("Trove ", env!("CARGO_PKG_VERSION"));

/// The tEXt keys the thumbnail spec asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThumbMeta {
    pub uri: Option<String>,
    pub mtime: Option<i64>,
    pub size: Option<u64>,
}

impl ThumbMeta {
    pub fn new(uri: &str, mtime: i64, size: u64) -> Self {
        Self { uri: Some(uri.to_owned()), mtime: Some(mtime), size: Some(size) }
    }

    fn chunks(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(u) = &self.uri {
            v.push(("Thumb::URI", u.clone()));
        }
        if let Some(m) = self.mtime {
            v.push(("Thumb::MTime", m.to_string()));
        }
        if let Some(s) = self.size {
            v.push(("Thumb::Size", s.to_string()));
        }
        v.push(("Software", SOFTWARE.to_owned()));
        v
    }
}

/// How much of the file is read to find the tEXt chunks, which sit ahead of IDAT.
const HEAD: usize = 8192;

/// The `Thumb::*` chunks from the head of a PNG, without decoding it. `None` when the file is
/// not a PNG or the chunks do not fit in the head.
pub(crate) fn read_meta(path: &Path) -> Option<ThumbMeta> {
    let mut head = Vec::with_capacity(HEAD);
    File::open(path).ok()?.take(HEAD as u64).read_to_end(&mut head).ok()?;
    parse_meta(&head)
}

fn parse_meta(head: &[u8]) -> Option<ThumbMeta> {
    if head.get(..8)? != SIGNATURE {
        return None;
    }
    let mut meta = ThumbMeta::default();
    let mut at = 8;
    loop {
        let len = u32::from_be_bytes(head.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = head.get(at + 4..at + 8)?;
        match kind {
            b"IDAT" | b"IEND" => return Some(meta),
            b"tEXt" => {
                let data = head.get(at + 8..at + 8 + len)?;
                if let Some(nul) = data.iter().position(|&b| b == 0) {
                    let value: String = data[nul + 1..].iter().map(|&b| char::from(b)).collect();
                    match &data[..nul] {
                        b"Thumb::URI" => meta.uri = Some(value),
                        b"Thumb::MTime" => meta.mtime = value.trim().parse().ok(),
                        b"Thumb::Size" => meta.size = value.trim().parse().ok(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        at += 12 + len;
    }
}

/// Encode RGBA8 pixels as a PNG carrying the thumbnail keys.
pub(crate) fn encode_rgba(w: u32, h: u32, rgba: &[u8], meta: &ThumbMeta) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::with_capacity(rgba.len() / 4 + 512);
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    for (k, v) in meta.chunks() {
        enc.add_text_chunk(k.to_owned(), v)?;
    }
    enc.write_header()?.write_image_data(rgba)?;
    Ok(out)
}

/// The empty PNG a failure record is.
pub(crate) fn encode_empty(meta: &ThumbMeta) -> Result<Vec<u8>, png::EncodingError> {
    encode_rgba(1, 1, &[0, 0, 0, 0], meta)
}

/// Put the thumbnail keys into a PNG a thumbnailer wrote, replacing any `Thumb::*` it carries.
/// Works on the chunk stream, so the image is not decoded.
pub(crate) fn splice_meta(png: &[u8], meta: &ThumbMeta) -> Option<Vec<u8>> {
    if png.get(..8)? != SIGNATURE {
        return None;
    }
    let mut out = Vec::with_capacity(png.len() + 256);
    out.extend_from_slice(&SIGNATURE);
    let mut at = 8;
    let mut inserted = false;
    while at < png.len() {
        let len = u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = png.get(at..end)?;
        let kind = &chunk[4..8];
        let own = kind == b"tEXt" && chunk[8..8 + len].starts_with(b"Thumb::");
        if !own {
            out.extend_from_slice(chunk);
        }
        if kind == b"IHDR" && !inserted {
            for (k, v) in meta.chunks() {
                push_text(&mut out, k, &v);
            }
            inserted = true;
        }
        at = end;
    }
    inserted.then_some(out)
}

fn push_text(out: &mut Vec<u8>, key: &str, value: &str) {
    let mut data = Vec::with_capacity(key.len() + 1 + value.len());
    data.extend_from_slice(key.as_bytes());
    data.push(0);
    // tEXt is Latin-1; URIs are ASCII after percent-encoding.
    data.extend(value.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')));
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(b"tEXt");
    crc.update(&data);
    out.extend_from_slice(b"tEXt");
    out.extend_from_slice(&data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_read_back_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.png");
        let meta = ThumbMeta::new("file:///a/b.png", 1_700_000_000, 4242);
        std::fs::write(&p, encode_rgba(2, 2, &[255; 16], &meta).unwrap()).unwrap();
        assert_eq!(read_meta(&p), Some(meta));
    }

    #[test]
    fn splice_adds_keys_and_keeps_the_image_valid() {
        let plain =
            encode_rgba(3, 2, &[7; 24], &ThumbMeta { uri: Some("old".into()), mtime: Some(1), size: None }).unwrap();
        let meta = ThumbMeta::new("file:///x.mp4", 99, 5);
        let spliced = splice_meta(&plain, &meta).unwrap();
        assert_eq!(parse_meta(&spliced), Some(meta));
        let img = image::load_from_memory_with_format(&spliced, image::ImageFormat::Png).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        assert!(splice_meta(b"not a png at all", &ThumbMeta::default()).is_none());
    }

    #[test]
    fn a_file_without_keys_reads_as_empty_meta() {
        let mut out = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(parse_meta(&out), Some(ThumbMeta::default()));
        assert_eq!(parse_meta(b"junk"), None);
    }
}
