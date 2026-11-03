use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};

/// Extensions the `image` crate is built with here, plus HEIC through heic-rs.
pub(crate) fn decodable(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif")
}

pub(crate) struct Rendered {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The size to draw at: fit inside `edge` x `edge`, never upscaled.
fn fit(w: u32, h: u32, edge: u32) -> (u32, u32) {
    if w <= edge && h <= edge {
        return (w.max(1), h.max(1));
    }
    let scale = f64::from(edge) / f64::from(w.max(h));
    (((f64::from(w) * scale).round() as u32).max(1), ((f64::from(h) * scale).round() as u32).max(1))
}

/// Decode `path` to at most `edge` pixels on the long side. JPEGs are decoded at 1/2, 1/4 or 1/8
/// size straight from the DCT, which is most of the cost of a 12 MP photo.
pub(crate) fn render(path: &Path, edge: u32) -> Option<Rendered> {
    let is_heif = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"));
    if is_heif {
        return render_heif(path, edge);
    }
    let is_jpeg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    if is_jpeg && let Some(r) = render_jpeg_scaled(path, edge) {
        return Some(r);
    }
    render_generic(path, edge)
}

pub(crate) fn render_generic(path: &Path, edge: u32) -> Option<Rendered> {
    let mut decoder = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let img = DynamicImage::from_decoder(decoder).ok()?;
    finish(img, edge, orientation)
}

/// heic-rs applies clap/irot/imir itself, so no orientation is passed on.
fn render_heif(path: &Path, edge: u32) -> Option<Rendered> {
    let bytes = std::fs::read(path).ok()?;
    let options = heic_rs::DecodeOptions { layout: heic_rs::PixelLayout::Rgba8, ..heic_rs::DecodeOptions::default() };
    let decoded = heic_rs::decode(&bytes, &options).ok()?;
    let img = DynamicImage::ImageRgba8(RgbaImage::from_raw(decoded.width, decoded.height, decoded.data)?);
    finish(img, edge, None)
}

fn render_jpeg_scaled(path: &Path, edge: u32) -> Option<Rendered> {
    let orientation = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?.orientation().ok();
    let mut dec = jpeg_decoder::Decoder::new(BufReader::new(File::open(path).ok()?));
    dec.read_info().ok()?;
    let info = dec.info()?;
    let (tw, th) = fit(u32::from(info.width), u32::from(info.height), edge);
    let (sw, sh) = dec.scale(u16::try_from(tw).ok()?, u16::try_from(th).ok()?).ok()?;
    let pixels = dec.decode().ok()?;
    let (sw, sh) = (u32::from(sw), u32::from(sh));
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            let mut out = Vec::with_capacity(pixels.len() / 3 * 4);
            for px in pixels.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        _ => return None,
    };
    let img = DynamicImage::ImageRgba8(RgbaImage::from_raw(sw, sh, rgba)?);
    // Orientation came from the full-size header; the box is square so the fit above holds.
    finish_to(img, tw, th, orientation)
}

fn finish(img: DynamicImage, edge: u32, orientation: Option<image::metadata::Orientation>) -> Option<Rendered> {
    let (tw, th) = fit(img.width(), img.height(), edge);
    finish_to(img, tw, th, orientation)
}

fn finish_to(
    img: DynamicImage,
    tw: u32,
    th: u32,
    orientation: Option<image::metadata::Orientation>,
) -> Option<Rendered> {
    let mut img = if (img.width(), img.height()) == (tw, th) { img } else { img.thumbnail_exact(tw, th) };
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let rgba = img.into_rgba8();
    Some(Rendered { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_upscales_and_keeps_aspect() {
        assert_eq!(fit(4000, 3000, 256), (256, 192));
        assert_eq!(fit(100, 50, 256), (100, 50));
        assert_eq!(fit(1, 5000, 256), (1, 256));
    }
}
