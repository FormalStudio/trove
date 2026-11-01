use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub(crate) struct TextRead {
    pub text: String,
    pub truncated: bool,
    pub lines: usize,
}

/// Up to `max` bytes of `path` as text. `None` when the content is binary.
pub(crate) fn read_text(path: &Path, max: usize) -> io::Result<Option<TextRead>> {
    let mut buf = Vec::new();
    File::open(path)?.take(max as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > max;
    buf.truncate(max);
    let Some(text) = decode(&buf, truncated) else { return Ok(None) };
    let mut lines = text.bytes().filter(|&b| b == b'\n').count();
    if !text.is_empty() && !text.ends_with('\n') {
        lines += 1;
    }
    Ok(Some(TextRead { text, truncated, lines }))
}

/// UTF-8, or Latin-1 when the bytes are not UTF-8 but look like text. `cut` says the buffer
/// ends where a read stopped, so a trailing partial character is not an error.
pub(crate) fn decode(buf: &[u8], cut: bool) -> Option<String> {
    if buf[..buf.len().min(8192)].contains(&0) {
        return None;
    }
    let buf = buf.strip_prefix(b"\xef\xbb\xbf").unwrap_or(buf);
    match std::str::from_utf8(buf) {
        Ok(s) => Some(s.to_owned()),
        Err(e) if cut && e.error_len().is_none() => Some(String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()),
        Err(_) => buf.iter().all(|&b| latin1_text_byte(b)).then(|| buf.iter().map(|&b| char::from(b)).collect()),
    }
}

fn latin1_text_byte(b: u8) -> bool {
    match b {
        b'\t' | b'\n' | b'\r' | 0x0c | 0x1b => true,
        0x00..=0x1f | 0x7f..=0x9f => false,
        _ => true,
    }
}

/// The highlighter's language name for a file name, from a static table.
pub(crate) fn language_of(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "makefile" | "gnumakefile" | "justfile" => Some("make"),
        "dockerfile" | "containerfile" => Some("dockerfile"),
        "cmakelists.txt" => Some("cmake"),
        "cargo.lock" | "flake.lock" => Some("toml"),
        "pkgbuild" | ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" => Some("shell"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    Some(match trove_fs::entry::extension_of(&lower) {
        "rs" => "rust",
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "go" => "go",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "nix" => "nix",
        "lua" => "lua",
        "sql" => "sql",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "xml" | "svg" | "plist" => "xml",
        "ini" | "conf" | "cfg" | "desktop" | "service" => "ini",
        "diff" | "patch" => "diff",
        "zig" => "zig",
        "hs" => "haskell",
        "ml" => "ocaml",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "cs" => "csharp",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "mk" => "make",
        "cmake" => "cmake",
        "tf" => "hcl",
        "proto" => "protobuf",
        "graphql" => "graphql",
        "el" => "lisp",
        "vim" => "vim",
        "ps1" => "powershell",
        "bat" => "batch",
        "r" => "r",
        "jl" => "julia",
        "scala" => "scala",
        "clj" => "clojure",
        "star" => "python",
        "tex" => "latex",
        "csv" | "tsv" => "csv",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn counts_lines_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "a.txt", b"one\ntwo\nthree");
        let r = read_text(&p, 1000).unwrap().unwrap();
        assert_eq!((r.text.as_str(), r.truncated, r.lines), ("one\ntwo\nthree", false, 3));
        let r = read_text(&p, 8).unwrap().unwrap();
        assert_eq!((r.text.as_str(), r.truncated, r.lines), ("one\ntwo\n", true, 2));
        let r = read_text(&write(&dir, "exact", b"abcd"), 4).unwrap().unwrap();
        assert!(!r.truncated, "a file of exactly max bytes is whole");
        let r = read_text(&write(&dir, "empty", b""), 10).unwrap().unwrap();
        assert_eq!((r.text.as_str(), r.lines), ("", 0));
    }

    #[test]
    fn truncation_inside_a_character_keeps_the_valid_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "u.txt", "aé".as_bytes());
        let r = read_text(&p, 2).unwrap().unwrap();
        assert_eq!(r.text, "a");
        assert!(r.truncated);
    }

    #[test]
    fn binary_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_text(&write(&dir, "nul", b"abc\0def"), 100).unwrap().is_none());
        assert!(read_text(&write(&dir, "ctl", b"abc\x01\x02\xff"), 100).unwrap().is_none());
    }

    #[test]
    fn latin1_fallback_only_for_text() {
        let dir = tempfile::tempdir().unwrap();
        let r = read_text(&write(&dir, "l1", b"caf\xe9\n"), 100).unwrap().unwrap();
        assert_eq!(r.text, "café\n");
    }

    #[test]
    fn bom_is_dropped_and_languages_resolve() {
        assert_eq!(decode(b"\xef\xbb\xbfhi", false).as_deref(), Some("hi"));
        assert_eq!(language_of("main.rs"), Some("rust"));
        assert_eq!(language_of("Makefile"), Some("make"));
        assert_eq!(language_of("app.TS"), Some("typescript"));
        assert_eq!(language_of("notes.txt"), None);
    }
}
