use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use image::{DynamicImage, ImageFormat, RgbImage};

use super::*;
use crate::Config;

fn png_of(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
    out
}

fn jpeg_of(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg).unwrap();
    out
}

fn bump_mtime(path: &Path, by: u64) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(fs::metadata(path).unwrap().modified().unwrap() + Duration::from_secs(by)).unwrap();
}

fn mtime_of(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

struct Rig {
    _root: tempfile::TempDir,
    dir: PathBuf,
    engine: Arc<Engine>,
}

fn rig() -> Rig {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("files");
    fs::create_dir(&dir).unwrap();
    let engine = Engine::new(Config::rooted(root.path()));
    Rig { _root: root, dir, engine }
}

fn decode_dims(p: &Path) -> (u32, u32) {
    let img = image::open(p).unwrap();
    (img.width(), img.height())
}

#[test]
fn generates_png_with_spec_keys_and_private_modes() {
    let r = rig();
    let src = r.dir.join("photo.png");
    fs::write(&src, png_of(600, 400)).unwrap();
    let t = r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).unwrap();
    assert_eq!(t, cache_path(&r.engine, &src, ThumbSize::Large));
    assert_eq!(decode_dims(&t), (256, 171));
    let meta = pngmeta::read_meta(&t).unwrap();
    assert_eq!(meta.uri.as_deref(), Some(uri::file_uri(&src).as_str()));
    assert_eq!(meta.mtime, Some(secs(mtime_of(&src))));
    assert_eq!(meta.size, Some(fs::metadata(&src).unwrap().len()));
    assert_eq!(fs::metadata(&t).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(t.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    let n = r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Normal).unwrap();
    assert_eq!(decode_dims(&n), (128, 85));
}

#[test]
fn small_images_are_not_upscaled() {
    let r = rig();
    let src = r.dir.join("tiny.png");
    fs::write(&src, png_of(40, 30)).unwrap();
    let t = r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).unwrap();
    assert_eq!(decode_dims(&t), (40, 30));
}

#[test]
fn jpeg_is_decoded_scaled() {
    let r = rig();
    let src = r.dir.join("photo.JPG");
    fs::write(&src, jpeg_of(1200, 800)).unwrap();
    let t = r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).unwrap();
    assert_eq!(decode_dims(&t), (256, 171));
}

#[test]
fn exif_orientation_is_applied_on_both_decode_paths() {
    use image::ImageEncoder;
    let r = rig();
    // TIFF block with one IFD entry: Orientation (0x0112) = 6, rotate 90 degrees clockwise.
    let exif = vec![b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];
    let img = RgbImage::from_fn(600, 300, |_, _| image::Rgb([200, 30, 30]));
    let mut bytes = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new(&mut bytes);
    enc.set_exif_metadata(exif).unwrap();
    enc.write_image(img.as_raw(), 600, 300, image::ExtendedColorType::Rgb8).unwrap();
    let src = r.dir.join("rotated.jpg");
    fs::write(&src, bytes).unwrap();
    let scaled = imagegen::render(&src, 256).unwrap();
    let generic = imagegen::render_generic(&src, 256).unwrap();
    assert_eq!((scaled.width, scaled.height), (128, 256));
    assert_eq!((generic.width, generic.height), (128, 256));
}

#[test]
fn cache_hit_is_valid_only_while_mtime_matches() {
    let r = rig();
    let src = r.dir.join("a.png");
    fs::write(&src, png_of(500, 500)).unwrap();
    let first = mtime_of(&src);
    let t = r.engine.thumbnail(&src, first, ThumbSize::Large).unwrap();
    let made = mtime_of(&t);
    let inode = fs::metadata(&t).unwrap().ino_for_test();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(r.engine.thumbnail(&src, first, ThumbSize::Large), Some(t.clone()));
    assert_eq!(mtime_of(&t), made, "a hit does not rewrite the file");
    assert_eq!(fs::metadata(&t).unwrap().ino_for_test(), inode);

    bump_mtime(&src, 10);
    let second = mtime_of(&src);
    assert_eq!(r.engine.thumbnail(&src, second, ThumbSize::Large), Some(t.clone()));
    assert_eq!(pngmeta::read_meta(&t).unwrap().mtime, Some(secs(second)), "regenerated for the new mtime");
}

#[test]
fn foreign_cache_entries_are_honoured_and_a_size_mismatch_invalidates() {
    let r = rig();
    let src = r.dir.join("b.png");
    fs::write(&src, png_of(300, 300)).unwrap();
    let mtime = mtime_of(&src);
    let target = cache_path(&r.engine, &src, ThumbSize::Normal);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let foreign = ThumbMeta { uri: Some(uri::file_uri(&src)), mtime: Some(secs(mtime)), size: None };
    let marker = pngmeta::encode_rgba(1, 1, &[1, 2, 3, 255], &foreign).unwrap();
    fs::write(&target, &marker).unwrap();
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Normal), Some(target.clone()));
    assert_eq!(fs::read(&target).unwrap(), marker, "a current foreign thumbnail is used as it is");

    let wrong_size = ThumbMeta { size: Some(1), ..foreign };
    fs::write(&target, pngmeta::encode_rgba(1, 1, &[1, 2, 3, 255], &wrong_size).unwrap()).unwrap();
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Normal), Some(target.clone()));
    assert_eq!(decode_dims(&target), (128, 128), "regenerated over the stale one");
}

#[test]
fn failures_are_remembered_until_the_file_changes() {
    let r = rig();
    let src = r.dir.join("broken.png");
    fs::write(&src, b"definitely not a png").unwrap();
    let mtime = mtime_of(&src);
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Large), None);
    let key = uri::key_of(&uri::file_uri(&src));
    let fail = fail_path(&r.engine, &key);
    assert!(fail.starts_with(r.engine.config.thumbnails_dir.join("fail")), "{fail:?}");
    assert_eq!(pngmeta::read_meta(&fail).unwrap().mtime, Some(secs(mtime)));
    assert!(!cache_path(&r.engine, &src, ThumbSize::Large).exists());

    // Same mtime, now a real image: the record keeps it from being retried.
    fs::write(&src, png_of(100, 100)).unwrap();
    fs::File::options().write(true).open(&src).unwrap().set_modified(mtime).unwrap();
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Large), None);

    bump_mtime(&src, 5);
    assert!(r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).is_some());
}

#[test]
fn skips_the_cache_itself_and_oversized_images() {
    let r = rig();
    let inside = r.engine.config.thumbnails_dir.join("large").join("x.png");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, png_of(500, 500)).unwrap();
    assert_eq!(r.engine.thumbnail(&inside, mtime_of(&inside), ThumbSize::Large), None);
    assert!(!r.engine.config.thumbnails_dir.join("fail").exists());

    let mut cfg = r.engine.config.clone();
    cfg.max_image_bytes = 100;
    let small = Engine::new(cfg);
    let big = r.dir.join("big.png");
    fs::write(&big, png_of(500, 500)).unwrap();
    assert_eq!(small.thumbnail(&big, mtime_of(&big), ThumbSize::Large), None);
    assert!(!small.config.thumbnails_dir.join("fail").exists(), "too big is not a failure");
}

/// A thumbnailer that logs each input, takes `delay` seconds, and writes a fixture PNG (or fails).
fn install_thumbnailer(r: &Rig, delay: &str, body: &str) -> PathBuf {
    let tdir = r.engine.config.thumbnailer_dirs[0].clone();
    fs::create_dir_all(&tdir).unwrap();
    let fixture = tdir.join("fixture.png");
    fs::write(&fixture, png_of(10, 10)).unwrap();
    let log = tdir.join("log");
    let script = tdir.join("run.sh");
    fs::write(&script, format!("#!/bin/sh\necho \"$1 $3\" >> {log}\nsleep {delay}\n{body}\n", log = log.display()))
        .unwrap();
    fs::write(
        tdir.join("fake.thumbnailer"),
        format!("[Thumbnailer Entry]\nTryExec=sh\nExec=sh {} %i %o %s\nMimeType=video/mp4;\n", script.display()),
    )
    .unwrap();
    log
}

fn copy_body(r: &Rig) -> String {
    format!("cp {} \"$2\"", r.engine.config.thumbnailer_dirs[0].join("fixture.png").display())
}

fn fake_video(r: &Rig, name: &str) -> PathBuf {
    let p = r.dir.join(name);
    fs::write(&p, b"not really a video").unwrap();
    p
}

#[test]
fn system_thumbnailer_output_gets_the_spec_keys() {
    let r = rig();
    let body = copy_body(&r);
    let log = install_thumbnailer(&r, "0", &body);
    let src = fake_video(&r, "clip.mp4");
    let t = r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).unwrap();
    let meta = pngmeta::read_meta(&t).unwrap();
    assert_eq!(meta.uri.as_deref(), Some(uri::file_uri(&src).as_str()));
    assert_eq!(meta.mtime, Some(secs(mtime_of(&src))));
    assert_eq!(decode_dims(&t), (10, 10));
    assert_eq!(fs::read_to_string(&log).unwrap().trim(), format!("{} 256", src.display()));
    assert_eq!(fs::metadata(&t).unwrap().permissions().mode() & 0o777, 0o600);
    // Cached now: the thumbnailer is not run again.
    assert!(r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large).is_some());
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
}

#[test]
fn a_failing_thumbnailer_leaves_a_fail_record_and_no_retry() {
    let r = rig();
    let log = install_thumbnailer(&r, "0", "exit 1");
    let src = fake_video(&r, "bad.mp4");
    let mtime = mtime_of(&src);
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Large), None);
    assert_eq!(r.engine.thumbnail(&src, mtime, ThumbSize::Large), None);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1, "second call served from the fail record");
    let fail = fail_path(&r.engine, &uri::key_of(&uri::file_uri(&src)));
    assert_eq!(pngmeta::read_meta(&fail).unwrap().mtime, Some(secs(mtime)));
}

#[test]
fn a_hung_thumbnailer_is_killed() {
    let r = rig();
    install_thumbnailer(&r, "30", "true");
    let mut cfg = r.engine.config.clone();
    cfg.thumbnailer_timeout = Duration::from_millis(300);
    let engine = Engine::new(cfg);
    let src = fake_video(&r, "hang.mp4");
    let started = Instant::now();
    assert_eq!(engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large), None);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn no_thumbnailer_for_the_mime_is_none_without_a_record() {
    let r = rig();
    let src = r.dir.join("movie.mkv");
    fs::write(&src, "x").unwrap();
    assert_eq!(r.engine.thumbnail(&src, mtime_of(&src), ThumbSize::Large), None);
    assert!(!r.engine.config.thumbnails_dir.join("fail").exists());
}

fn wait_for_lines(log: &Path, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_to_string(log).map_or(0, |s| s.lines().count()) < n {
        assert!(Instant::now() < deadline, "thumbnailer never started");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn names(rx: &flume::Receiver<Made>, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let (p, _, t) = rx.recv_timeout(Duration::from_secs(15)).unwrap();
            assert!(t.is_some());
            p.file_name().unwrap().to_string_lossy().into_owned()
        })
        .collect()
}

#[test]
fn queue_is_lifo_deduped_and_bumps_a_repeat_to_the_front() {
    let r = rig();
    let body = copy_body(&r);
    let log = install_thumbnailer(&r, "0.25", &body);
    let (pool, rx) = Thumbnails::with_engine(r.engine.clone(), 1);
    let files: Vec<_> = ["a", "b", "c", "d"].iter().map(|n| fake_video(&r, &format!("{n}.mp4"))).collect();
    let req = |i: usize| pool.request(files[i].clone(), mtime_of(&files[i]), ThumbSize::Large);
    req(0);
    wait_for_lines(&log, 1);
    req(1);
    req(2);
    req(3);
    req(2);
    req(2);
    assert_eq!(names(&rx, 4), ["a.mp4", "c.mp4", "d.mp4", "b.mp4"]);
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err(), "c was asked for three times and made once");
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 4);
}

#[test]
fn request_for_a_running_path_answers_once() {
    let r = rig();
    let body = copy_body(&r);
    let log = install_thumbnailer(&r, "0.25", &body);
    let (pool, rx) = Thumbnails::with_engine(r.engine.clone(), 1);
    let f = fake_video(&r, "a.mp4");
    pool.request(f.clone(), mtime_of(&f), ThumbSize::Large);
    wait_for_lines(&log, 1);
    pool.request(f.clone(), mtime_of(&f), ThumbSize::Large);
    assert_eq!(names(&rx, 1), ["a.mp4"]);
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
}

#[test]
fn cancel_all_drops_what_has_not_started() {
    let r = rig();
    let body = copy_body(&r);
    let log = install_thumbnailer(&r, "0.25", &body);
    let (pool, rx) = Thumbnails::with_engine(r.engine.clone(), 1);
    let files: Vec<_> = ["a", "b", "c"].iter().map(|n| fake_video(&r, &format!("{n}.mp4"))).collect();
    pool.request(files[0].clone(), mtime_of(&files[0]), ThumbSize::Large);
    wait_for_lines(&log, 1);
    pool.request(files[1].clone(), mtime_of(&files[1]), ThumbSize::Large);
    pool.request(files[2].clone(), mtime_of(&files[2]), ThumbSize::Large);
    pool.cancel_all();
    assert_eq!(names(&rx, 1), ["a.mp4"], "the running one still answers");
    assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
    // The pool keeps working afterwards.
    pool.request(files[1].clone(), mtime_of(&files[1]), ThumbSize::Large);
    assert_eq!(names(&rx, 1), ["b.mp4"]);
}

#[test]
fn pool_answers_none_for_a_file_that_cannot_be_thumbnailed() {
    let r = rig();
    let (pool, rx) = Thumbnails::with_engine(r.engine.clone(), 2);
    let missing = r.dir.join("gone.png");
    pool.request(missing.clone(), SystemTime::now(), ThumbSize::Normal);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (missing, ThumbSize::Normal, None));
}

trait InoForTest {
    fn ino_for_test(&self) -> u64;
}
impl InoForTest for fs::Metadata {
    fn ino_for_test(&self) -> u64 {
        std::os::unix::fs::MetadataExt::ino(self)
    }
}

/// `cargo test -p trove-preview --release -- --ignored --nocapture`
#[test]
#[ignore = "benchmark"]
fn bench_cold_and_warm() {
    const COUNT: usize = 200;
    let r = rig();
    let base = RgbImage::from_fn(4000, 3000, |x, y| {
        let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503);
        let n = (n ^ (n >> 13)).wrapping_mul(1_274_126_177);
        let tex = ((n >> 24) & 0x3f) as u8;
        image::Rgb([
            ((x / 16) % 200) as u8 / 2 + tex,
            ((y / 12) % 200) as u8 / 2 + tex,
            (((x + y) / 20) % 200) as u8 / 2 + tex,
        ])
    });
    let mut jpeg = Vec::new();
    DynamicImage::ImageRgb8(base).write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg).unwrap();
    println!("fixture: 4000x3000 jpeg, {:.1} MB, {COUNT} copies", jpeg.len() as f64 / 1e6);
    let files: Vec<_> = (0..COUNT)
        .map(|i| {
            let p = r.dir.join(format!("img_{i:03}.jpg"));
            fs::write(&p, &jpeg).unwrap();
            p
        })
        .collect();

    let avg = |label: &str, n: usize, mut f: Box<dyn FnMut(&Path)>| {
        let t = Instant::now();
        for p in &files[..n] {
            f(p);
        }
        let per = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        println!("{label}: {per:.3} ms/image over {n}");
        per
    };
    let generic =
        avg("decode+resize, image crate only", 20, Box::new(|p| assert!(imagegen::render_generic(p, 256).is_some())));
    let scaled =
        avg("decode+resize, jpeg-decoder scaled", 20, Box::new(|p| assert!(imagegen::render(p, 256).is_some())));
    println!("scaled decode speedup: {:.1}x", generic / scaled);

    let engine = r.engine.clone();
    let cold = avg(
        "cold thumbnail (decode, encode, write), 1 thread",
        COUNT,
        Box::new(move |p| assert!(engine.thumbnail(p, mtime_of(p), ThumbSize::Large).is_some())),
    );
    let engine = r.engine.clone();
    let warm = avg(
        "warm cache hit",
        COUNT,
        Box::new(move |p| assert!(engine.thumbnail(p, mtime_of(p), ThumbSize::Large).is_some())),
    );
    println!("warm lookup (excluding the caller's own stat): {:.0} us/hit", warm * 1e3);
    let _ = cold;

    let (pool, rx) = Thumbnails::with_engine(r.engine.clone(), 2);
    let t = Instant::now();
    for p in &files {
        pool.request(p.clone(), mtime_of(p), ThumbSize::Normal);
    }
    for _ in 0..COUNT {
        assert!(rx.recv().unwrap().2.is_some());
    }
    let wall = t.elapsed().as_secs_f64();
    println!("cold, 2 threads, Normal: {:.2} s for {COUNT} ({:.1} ms/image wall)", wall, wall * 1e3 / COUNT as f64);
}
