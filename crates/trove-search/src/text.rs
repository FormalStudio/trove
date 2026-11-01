use memchr::memrchr;
use trove_fs::FileKind;

/// Kind codes as stored in the index, four bits each. Folder is 0.
pub(crate) const KINDS: [FileKind; 15] = [
    FileKind::Folder,
    FileKind::Image,
    FileKind::Video,
    FileKind::Audio,
    FileKind::Text,
    FileKind::Code,
    FileKind::Pdf,
    FileKind::Document,
    FileKind::Spreadsheet,
    FileKind::Presentation,
    FileKind::Archive,
    FileKind::DiskImage,
    FileKind::Font,
    FileKind::Executable,
    FileKind::Unknown,
];

pub(crate) const FOLDER: u8 = 0;

pub(crate) fn kind_code(name: &[u8]) -> u8 {
    let kind = FileKind::from_name(&String::from_utf8_lossy(name));
    code_of(kind)
}

pub(crate) fn code_of(kind: FileKind) -> u8 {
    KINDS.iter().position(|k| *k == kind).map_or(14, |p| p as u8)
}

pub(crate) fn kind_bit(kind: FileKind) -> u32 {
    1 << code_of(kind)
}

/// Same rule as `trove_fs::Entry::hidden`.
pub(crate) fn is_hidden_name(name: &[u8]) -> bool {
    name.first() == Some(&b'.') || name.last() == Some(&b'~')
}

/// Extension of an already folded name, as `trove_fs::entry::extension_of` reads it.
pub(crate) fn extension(lower: &[u8]) -> &[u8] {
    match memrchr(b'.', lower) {
        Some(0) | None => b"",
        Some(i) => &lower[i + 1..],
    }
}

pub(crate) fn fold_char(c: char) -> char {
    let mut low = c.to_lowercase();
    match (low.next(), low.next()) {
        (Some(l), None) if l.len_utf8() == c.len_utf8() => l,
        _ => c,
    }
}

/// Lowercases while keeping every character's UTF-8 length, so a byte offset into the folded
/// name is the same offset into the real one. The few characters whose lowercase form is longer,
/// shorter or several characters stay as they are; invalid UTF-8 is copied through.
pub(crate) fn fold_into(bytes: &[u8], out: &mut Vec<u8>) {
    if bytes.is_ascii() {
        out.extend(bytes.iter().map(u8::to_ascii_lowercase));
        return;
    }
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            let mut buf = [0; 4];
            out.extend_from_slice(fold_char(c).encode_utf8(&mut buf).as_bytes());
        }
        out.extend_from_slice(chunk.invalid());
    }
}

pub(crate) fn fold(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    fold_into(bytes, &mut out);
    out
}

pub(crate) fn has_upper(s: &str) -> bool {
    s.chars().any(|c| fold_char(c) != c)
}

/// One code point at `i` and its length. A byte that does not start a valid sequence reads as
/// itself, one byte long, so a non-UTF-8 name still walks forward.
pub(crate) fn decode(b: &[u8], i: usize) -> (u32, usize) {
    let c = b[i];
    if c < 0x80 {
        return (u32::from(c), 1);
    }
    let len = if c >> 5 == 0b110 {
        2
    } else if c >> 4 == 0b1110 {
        3
    } else if c >> 3 == 0b1_1110 {
        4
    } else {
        return (u32::from(c), 1);
    };
    if i + len > b.len() {
        return (u32::from(c), 1);
    }
    let mut v = u32::from(c) & (0x7f >> len);
    for &cc in &b[i + 1..i + len] {
        if cc & 0xc0 != 0x80 {
            return (u32::from(c), 1);
        }
        v = (v << 6) | u32::from(cc & 0x3f);
    }
    (v, len)
}

pub(crate) fn code_points(b: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let (c, l) = decode(b, i);
        out.push(c);
        i += l;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_keeps_offsets() {
        for s in ["Ünïcödé_Fïlé.txt", "ẞtraße", "KELVIN \u{212a}", "ÅNGSTRÖM", "İstanbul"] {
            let f = fold(s.as_bytes());
            assert_eq!(f.len(), s.len(), "{s}");
            assert!(std::str::from_utf8(&f).is_ok());
        }
        assert_eq!(fold("Ünïcödé".as_bytes()), "ünïcödé".as_bytes());
        assert_eq!(fold(b"\xffABC"), b"\xffabc");
    }

    #[test]
    fn decodes_and_survives_garbage() {
        assert_eq!(code_points("aé€😀".as_bytes()), vec![0x61, 0xe9, 0x20ac, 0x1f600]);
        assert_eq!(code_points(b"\xe2\x82"), vec![0xe2, 0x82]);
    }
}
