use memchr::memmem::Finder;
use trove_fs::FileKind;

use crate::text::{FOLDER, code_points, decode, extension, fold, has_upper, kind_bit};

pub(crate) const EXACT: u32 = 50;
/// `readme` against `README.md`: the whole name but its extension.
pub(crate) const STEM: u32 = 46;
pub(crate) const PREFIX: u32 = 40;
pub(crate) const BOUNDARY: u32 = 30;
pub(crate) const SUBSTRING: u32 = 20;
/// A word found in a folder on the path rather than in the name.
pub(crate) const ANCESTOR: u32 = 5;
const FOLDER_BONUS: u32 = 3;
/// Subsequence matches score 1 to this: how tightly the letters sit, plus starting on a word and
/// covering the whole name or stem.
const FUZZY_MAX: u32 = 10;
const WILD: u32 = u32::MAX;

pub(crate) enum TermKind {
    Word,
    Phrase,
    Glob(Glob),
}

pub(crate) struct Glob {
    /// The pattern split at `*`; `?` is `WILD`. An empty first or last segment means the pattern
    /// starts or ends with `*`.
    segs: Vec<Vec<u32>>,
}

pub(crate) struct Term {
    pub pat: Vec<u8>,
    cps: Vec<u32>,
    pub sensitive: bool,
    pub kind: TermKind,
    /// The term itself for words and phrases, the longest literal run for a glob.
    pub finder: Finder<'static>,
    pub literal_len: usize,
}

impl Term {
    fn new(text: &str, kind: TermKind) -> Term {
        let sensitive = has_upper(text);
        let pat = if sensitive { text.as_bytes().to_vec() } else { fold(text.as_bytes()) };
        let literal: Vec<u8> = match kind {
            TermKind::Glob(_) => {
                pat.split(|c| *c == b'*' || *c == b'?').max_by_key(|s| s.len()).unwrap_or_default().to_vec()
            }
            _ => pat.clone(),
        };
        let kind = match kind {
            TermKind::Glob(_) => TermKind::Glob(Glob {
                segs: pat
                    .split(|c| *c == b'*')
                    .map(|s| code_points(s).into_iter().map(|c| if c == u32::from(b'?') { WILD } else { c }).collect())
                    .collect(),
            }),
            k => k,
        };
        Term {
            cps: code_points(&pat),
            literal_len: literal.len(),
            finder: Finder::new(&literal).into_owned(),
            pat,
            sensitive,
            kind,
        }
    }

    fn hay<'a>(&self, name: &'a [u8], lower: &'a [u8]) -> &'a [u8] {
        if self.sensitive { name } else { lower }
    }

    pub fn is_word(&self) -> bool {
        matches!(self.kind, TermKind::Word)
    }

    fn fuzzy_ok(&self) -> bool {
        self.is_word() && self.cps.len() >= 2
    }

    /// Quality of the term in the name, and where it matched for words and phrases.
    fn in_name(&self, name: &[u8], lower: &[u8], ranges: Option<&mut Vec<(u32, u32)>>) -> Option<u32> {
        let hay = self.hay(name, lower);
        match &self.kind {
            TermKind::Glob(g) => {
                if self.literal_len > 0 && self.finder.find(hay).is_none() {
                    return None;
                }
                glob_match(g, hay, ranges).then_some(SUBSTRING)
            }
            TermKind::Word | TermKind::Phrase => {
                let (q, at) = self.word(name, hay)?;
                if let Some(r) = ranges {
                    r.push((at as u32, (at + self.pat.len()) as u32));
                }
                Some(q)
            }
        }
    }

    fn word(&self, name: &[u8], hay: &[u8]) -> Option<(u32, usize)> {
        let p = &self.pat;
        if hay.len() < p.len() {
            return None;
        }
        if hay == &p[..] {
            return Some((EXACT, 0));
        }
        if hay.starts_with(p) {
            let stem = hay[p.len()] == b'.' && memchr::memrchr(b'.', hay) == Some(p.len());
            return Some((if stem { STEM } else { PREFIX }, 0));
        }
        let mut first = None;
        for at in self.finder.find_iter(hay) {
            if is_boundary(name, at) {
                return Some((BOUNDARY, at));
            }
            first.get_or_insert(at);
        }
        first.map(|at| (SUBSTRING, at))
    }

    pub fn in_ancestor(&self, name: &[u8], lower: &[u8]) -> bool {
        self.finder.find(self.hay(name, lower)).is_some()
    }

    /// Subsequence match: the term's characters in order, the tightest window among the first
    /// few starting points, refused when the letters are spread so far apart it is noise.
    fn fuzzy(&self, name: &[u8], lower: &[u8], ranges: Option<&mut Vec<(u32, u32)>>) -> Option<u32> {
        let hay = self.hay(name, lower);
        let pat = &self.cps;
        let mut k = 0;
        let mut i = 0;
        while i < hay.len() && k < pat.len() {
            let (c, l) = decode(hay, i);
            if c == pat[k] {
                k += 1;
            }
            i += l;
        }
        if k < pat.len() {
            return None;
        }
        let mut best: Option<(usize, usize)> = None;
        let mut i = 0;
        let mut tries = 0;
        while i < hay.len() && tries < 8 {
            let (c, l) = decode(hay, i);
            if c == pat[0] {
                tries += 1;
                let (mut k, mut j, mut span) = (1, i + l, 1);
                while j < hay.len() && k < pat.len() {
                    let (c2, l2) = decode(hay, j);
                    span += 1;
                    if c2 == pat[k] {
                        k += 1;
                    }
                    j += l2;
                }
                if k < pat.len() {
                    break;
                }
                if best.is_none_or(|(s, _)| span < s) {
                    best = Some((span, i));
                }
            }
            i += l;
        }
        let (span, start) = best?;
        if span > pat.len() * 2 + 3 {
            return None;
        }
        let mut end = start;
        let mut k = 0;
        while k < span {
            end += decode(hay, end).1;
            k += 1;
        }
        let whole = end == hay.len() || (hay[end] == b'.' && memchr::memrchr(b'.', hay) == Some(end));
        let score = 1
            + 5 * pat.len() as u32 / span as u32
            + if is_boundary(name, start) { 2 } else { 0 }
            + if whole { 2 } else { 0 };
        if let Some(r) = ranges {
            let (mut k, mut j) = (0, start);
            while j < hay.len() && k < pat.len() {
                let (c, l) = decode(hay, j);
                if c == pat[k] {
                    r.push((j as u32, (j + l) as u32));
                    k += 1;
                }
                j += l;
            }
        }
        Some(score.min(FUZZY_MAX))
    }
}

/// After `_ - . space`, or a camelCase hump.
fn is_boundary(name: &[u8], at: usize) -> bool {
    if at == 0 {
        return true;
    }
    let prev = name[at - 1];
    matches!(prev, b'_' | b'-' | b'.' | b' ')
        || (prev.is_ascii_lowercase() && name.get(at).is_some_and(u8::is_ascii_uppercase))
}

fn seg_at(hay: &[u8], mut i: usize, seg: &[u32]) -> Option<usize> {
    for &want in seg {
        if i >= hay.len() {
            return None;
        }
        let (c, l) = decode(hay, i);
        if want != WILD && want != c {
            return None;
        }
        i += l;
    }
    Some(i)
}

fn glob_match(g: &Glob, hay: &[u8], ranges: Option<&mut Vec<(u32, u32)>>) -> bool {
    let n = g.segs.len();
    let mut found: Vec<(u32, u32)> = Vec::new();
    if n == 1 {
        if seg_at(hay, 0, &g.segs[0]) != Some(hay.len()) {
            return false;
        }
        found.push((0, hay.len() as u32));
    } else {
        let mut pos = 0;
        for (k, seg) in g.segs.iter().enumerate() {
            if seg.is_empty() {
                continue;
            }
            if k == 0 {
                let Some(e) = seg_at(hay, 0, seg) else { return false };
                found.push((0, e as u32));
                pos = e;
            } else if k == n - 1 {
                let mut s = hay.len();
                for _ in 0..seg.len() {
                    if s == 0 {
                        return false;
                    }
                    s -= 1;
                    while s > 0 && hay[s] & 0xc0 == 0x80 {
                        s -= 1;
                    }
                }
                if s < pos || seg_at(hay, s, seg) != Some(hay.len()) {
                    return false;
                }
                found.push((s as u32, hay.len() as u32));
            } else {
                let mut s = pos;
                loop {
                    if let Some(e) = seg_at(hay, s, seg) {
                        found.push((s as u32, e as u32));
                        pos = e;
                        break;
                    }
                    if s >= hay.len() {
                        return false;
                    }
                    s += decode(hay, s).1;
                }
            }
        }
    }
    if let Some(r) = ranges {
        r.extend(found);
    }
    true
}

/// A parsed query: name terms plus filters.
pub(crate) struct Matcher {
    pub terms: Vec<Term>,
    pub ext: Vec<Vec<u8>>,
    pub kinds: u32,
    pub size: Option<(u64, u64)>,
    /// Modification time window, `[from, to)` in seconds.
    pub mtime: Option<(u64, u64)>,
    /// `in:` folder names, folded.
    pub within: Vec<Vec<u8>>,
    pub allow_hidden: bool,
}

impl Matcher {
    pub fn parse(text: &str, show_hidden: bool, now: i64) -> Matcher {
        let mut m = Matcher {
            terms: Vec::new(),
            ext: Vec::new(),
            kinds: 0,
            size: None,
            mtime: None,
            within: Vec::new(),
            allow_hidden: show_hidden || text.trim_start().starts_with('.'),
        };
        for tok in tokenize(text) {
            if tok.quoted {
                if !tok.text.is_empty() {
                    m.terms.push(Term::new(&tok.text, TermKind::Phrase));
                }
                continue;
            }
            if let Some((key, val)) = tok.text.split_once(':')
                && m.filter(key, val, now)
            {
                continue;
            }
            if tok.text.contains(['*', '?']) {
                m.terms.push(Term::new(&tok.text, TermKind::Glob(Glob { segs: Vec::new() })));
            } else {
                m.terms.push(Term::new(&tok.text, TermKind::Word));
            }
        }
        m
    }

    /// A filter key with a value that does not parse (yet: the user is still typing) is
    /// dropped rather than read as a word that matches nothing.
    fn filter(&mut self, key: &str, val: &str, now: i64) -> bool {
        match key.to_ascii_lowercase().as_str() {
            "ext" => {
                for v in val.split(',') {
                    let v = v.trim_start_matches('.');
                    if !v.is_empty() {
                        self.ext.push(fold(v.as_bytes()));
                    }
                }
            }
            "kind" => {
                for v in val.split([',', '|']) {
                    self.kinds |= kind_bits(v);
                }
            }
            "size" => {
                if let Some(r) = parse_size(val) {
                    self.size = Some(intersect(self.size, r));
                }
            }
            "modified" => {
                if let Some(r) = parse_modified(val, now) {
                    self.mtime = Some(intersect(self.mtime, r));
                }
            }
            "in" => {
                if !val.is_empty() {
                    self.within.push(fold(val.as_bytes()));
                }
            }
            _ => return false,
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.ext.is_empty()
            && self.kinds == 0
            && self.size.is_none()
            && self.mtime.is_none()
            && self.within.is_empty()
    }

    pub fn needs_meta(&self) -> bool {
        self.size.is_some() || self.mtime.is_some()
    }

    pub fn has_fuzzy(&self) -> bool {
        self.terms.iter().any(Term::fuzzy_ok)
    }

    /// Checks needing only the name and the record's flags.
    pub fn cheap_ok(&self, lower: &[u8], is_dir: bool, kind: u8) -> bool {
        if self.kinds != 0 {
            let k = if is_dir { FOLDER } else { kind };
            if self.kinds & (1 << k) == 0 {
                return false;
            }
        }
        if !self.ext.is_empty() && (is_dir || !self.ext.iter().any(|e| extension(lower) == &e[..])) {
            return false;
        }
        !(is_dir && self.size.is_some())
    }

    pub fn meta_ok(&self, size: u64, mtime: u64) -> bool {
        self.size.is_none_or(|(lo, hi)| (lo..=hi).contains(&size))
            && self.mtime.is_none_or(|(lo, hi)| (lo..hi).contains(&mtime))
    }

    /// `in:` names the scope itself already satisfies need no check per entry.
    pub fn within_left(&self, scope_lower: &[u8]) -> Vec<Vec<u8>> {
        self.within.iter().filter(|w| w[..] != *scope_lower).cloned().collect()
    }

    /// The name's quality, or None. With `fuzzy`, only matches that needed a subsequence count:
    /// everything else was already found by the plain pass. A word not in the name may be in a
    /// folder on the path (`in_ancestor`), as long as some term matched the name itself.
    pub fn eval(
        &self,
        name: &[u8],
        lower: &[u8],
        is_dir: bool,
        fuzzy: bool,
        mut in_ancestor: impl FnMut(&Term) -> bool,
    ) -> Option<u32> {
        let mut q = 0;
        let mut in_name = false;
        let mut used_fuzzy = false;
        for t in &self.terms {
            if let Some(s) = t.in_name(name, lower, None) {
                q += s;
                in_name = true;
            } else if !t.is_word() {
                return None;
            } else if in_ancestor(t) {
                q += ANCESTOR;
            } else if fuzzy
                && t.fuzzy_ok()
                && let Some(s) = t.fuzzy(name, lower, None)
            {
                q += s;
                in_name = true;
                used_fuzzy = true;
            } else {
                return None;
            }
        }
        if (!self.terms.is_empty() && !in_name) || (fuzzy && !used_fuzzy) {
            return None;
        }
        if is_dir && self.terms.len() == 1 && q == EXACT {
            q += FOLDER_BONUS;
        }
        Some(q)
    }

    /// Byte ranges of the name to highlight, sorted and merged.
    pub fn highlights(&self, name: &[u8], lower: &[u8], fuzzy: bool) -> Vec<(u32, u32)> {
        let mut r = Vec::new();
        for t in &self.terms {
            if t.in_name(name, lower, Some(&mut r)).is_none() && fuzzy && t.fuzzy_ok() {
                t.fuzzy(name, lower, Some(&mut r));
            }
        }
        r.retain(|(a, b)| a < b);
        r.sort_unstable();
        let mut out: Vec<(u32, u32)> = Vec::with_capacity(r.len());
        for (a, b) in r {
            match out.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }

    /// Substring needles for the index prefilter, with whether each searches the case-sensitive
    /// blob. One needle when some literal must be in every matching name (the longest such);
    /// otherwise every word, since each name must hold at least one of them. Empty when nothing
    /// narrows the scan.
    pub fn needles(&self) -> Vec<(&Finder<'static>, bool)> {
        let words: Vec<&Term> = self.terms.iter().filter(|t| t.is_word()).collect();
        let mut required: Vec<&Term> = self.terms.iter().filter(|t| !t.is_word() && t.literal_len > 0).collect();
        if words.len() == 1 && self.terms.len() == 1 {
            required.push(words[0]);
        }
        if let Some(t) = required.iter().max_by_key(|t| t.literal_len) {
            return vec![(&t.finder, t.sensitive)];
        }
        if !words.is_empty() && self.terms.iter().all(Term::is_word) {
            return words.iter().map(|t| (&t.finder, t.sensitive)).collect();
        }
        Vec::new()
    }

    /// The `.ext` needle when an `ext:` filter is the only thing that can narrow the scan.
    pub fn ext_needle(&self) -> Option<Finder<'static>> {
        if self.ext.len() != 1 || !self.terms.is_empty() {
            return None;
        }
        let mut n = vec![b'.'];
        n.extend_from_slice(&self.ext[0]);
        n.push(b'/');
        Some(Finder::new(&n).into_owned())
    }
}

struct Token {
    text: String,
    quoted: bool,
}

/// Splits on whitespace; `"..."` is one phrase, and `key:"a b"` keeps its quoted value whole.
fn tokenize(s: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let text: String = chars.by_ref().take_while(|&c| c != '"').collect();
            out.push(Token { text, quoted: true });
            continue;
        }
        let mut text = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            if c == '"' && text.ends_with(':') {
                text.extend(chars.by_ref().take_while(|&c| c != '"'));
            } else {
                text.push(c);
            }
        }
        out.push(Token { text, quoted: false });
    }
    out
}

fn kind_bits(v: &str) -> u32 {
    let v = v.trim().to_ascii_lowercase();
    if v.is_empty() {
        return 0;
    }
    let table: [(&str, u32); 12] = [
        ("image", kind_bit(FileKind::Image)),
        ("video", kind_bit(FileKind::Video)),
        ("audio", kind_bit(FileKind::Audio)),
        (
            "doc",
            kind_bit(FileKind::Text)
                | kind_bit(FileKind::Pdf)
                | kind_bit(FileKind::Document)
                | kind_bit(FileKind::Spreadsheet)
                | kind_bit(FileKind::Presentation),
        ),
        ("code", kind_bit(FileKind::Code)),
        ("folder", kind_bit(FileKind::Folder)),
        ("archive", kind_bit(FileKind::Archive) | kind_bit(FileKind::DiskImage)),
        ("text", kind_bit(FileKind::Text)),
        ("pdf", kind_bit(FileKind::Pdf)),
        ("font", kind_bit(FileKind::Font)),
        ("app", kind_bit(FileKind::Executable)),
        ("music", kind_bit(FileKind::Audio)),
    ];
    table.iter().find(|(name, _)| name.starts_with(&v)).map_or(0, |(_, b)| *b)
}

fn intersect(a: Option<(u64, u64)>, b: (u64, u64)) -> (u64, u64) {
    match a {
        Some((lo, hi)) => (lo.max(b.0), hi.min(b.1)),
        None => b,
    }
}

fn split_op(v: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = v.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", v)
}

/// `>10mb`, `<=1.5g`, `500k`; decimal units as GNOME shows sizes, `kib`/`mib`/`gib` binary. A bare
/// size means at least that much.
fn parse_size(v: &str) -> Option<(u64, u64)> {
    let (op, rest) = split_op(v.trim());
    let split = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let num: f64 = rest[..split].parse().ok()?;
    let mul = match rest[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let x = (num * mul) as u64;
    Some(match op {
        ">" => (x.saturating_add(1), u64::MAX),
        "<" => (0, x.saturating_sub(1)),
        "<=" => (0, x),
        "=" => (x, x),
        _ => (x, u64::MAX),
    })
}

fn parse_modified(v: &str, now: i64) -> Option<(u64, u64)> {
    const DAY: i64 = 86_400;
    let clamp = |t: i64| t.max(0) as u64;
    let today = crate::time::day_start(now, 0);
    let window = |from: i64| Some((clamp(from), u64::MAX));
    match v.to_ascii_lowercase().as_str() {
        "today" => return window(today),
        "yesterday" => return Some((clamp(crate::time::day_start(now, -1)), clamp(today))),
        "week" => return window(now - 7 * DAY),
        "month" => return window(now - 30 * DAY),
        "year" => return window(now - 365 * DAY),
        _ => {}
    }
    let (op, rest) = split_op(v.trim());
    let mut parts = rest.splitn(3, '-').map(|p| p.parse::<i32>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let start = clamp(crate::time::date_start(y, m, d, 0));
    let next = clamp(crate::time::date_start(y, m, d, 1));
    Some(match op {
        "<" => (0, start),
        "<=" => (0, next),
        ">" => (next, u64::MAX),
        ">=" => (start, u64::MAX),
        _ => (start, next),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str) -> Matcher {
        Matcher::parse(q, false, 1_780_000_000)
    }

    fn q(m: &Matcher, name: &str, is_dir: bool) -> Option<u32> {
        let lower = fold(name.as_bytes());
        m.eval(name.as_bytes(), &lower, is_dir, false, |_| false)
            .or_else(|| m.eval(name.as_bytes(), &lower, is_dir, true, |_| false))
    }

    #[test]
    fn parses_tokens_and_filters() {
        let x = m(r#"foo "two words" *.rs ext:png,.JPG kind:im size:>10mb modified:today in:"My Games" what:ever"#);
        let kinds: Vec<&str> = x
            .terms
            .iter()
            .map(|t| match t.kind {
                TermKind::Word => "word",
                TermKind::Phrase => "phrase",
                TermKind::Glob(_) => "glob",
            })
            .collect();
        assert_eq!(kinds, ["word", "phrase", "glob", "word"]);
        assert_eq!(x.terms[1].pat, b"two words");
        assert_eq!(x.terms[3].pat, b"what:ever");
        assert_eq!(x.ext, [b"png".to_vec(), b"jpg".to_vec()]);
        assert_eq!(x.kinds, kind_bit(FileKind::Image));
        assert_eq!(x.size, Some((10_000_001, u64::MAX)));
        assert!(x.mtime.is_some());
        assert_eq!(x.within, [b"my games".to_vec()]);
        assert!(!x.allow_hidden);
        assert!(m(".config hypr").allow_hidden);
        assert!(m("size: kind:zzz modified:soon").is_empty());
    }

    #[test]
    fn smart_case() {
        let lower = m("readme");
        assert!(!lower.terms[0].sensitive);
        assert!(q(&lower, "README.md", false).is_some());
        let upper = m("ReadMe");
        assert!(upper.terms[0].sensitive);
        assert!(q(&upper, "README.md", false).is_none());
        assert!(q(&upper, "ReadMe.txt", false).is_some());
    }

    #[test]
    fn tiers() {
        let x = m("cargo");
        assert_eq!(q(&x, "cargo", false), Some(EXACT));
        assert_eq!(q(&x, "cargo", true), Some(EXACT + FOLDER_BONUS));
        assert_eq!(q(&x, "Cargo.toml", false), Some(STEM));
        assert_eq!(q(&x, "cargo-watch", false), Some(PREFIX));
        assert_eq!(q(&x, "my_cargo", false), Some(BOUNDARY));
        assert_eq!(q(&x, "myCargoThing", false), Some(BOUNDARY));
        assert_eq!(q(&x, "supercargo", false), Some(SUBSTRING));
        let f = q(&m("crgo"), "Cargo.toml", false).unwrap_or(0);
        assert!(f > 0 && f <= FUZZY_MAX);
        assert_eq!(q(&x, "c____a____r____g____o", false), None);
        assert_eq!(q(&m("zz"), "cargo", false), None);
    }

    #[test]
    fn globs() {
        let x = m("*.png");
        assert!(q(&x, "beach.PNG", false).is_some());
        assert!(q(&x, "beach.png.txt", false).is_none());
        assert!(q(&m("IMG_??.jpg"), "IMG_12.jpg", false).is_some());
        assert!(q(&m("IMG_??.jpg"), "IMG_123.jpg", false).is_none());
        assert!(q(&m("a*b*c"), "aXbYc", false).is_some());
        assert!(q(&m("a*b*c"), "aXcYb", false).is_none());
        assert!(q(&m("é?*"), "éa", false).is_some());
        assert!(q(&m("a*a"), "a", false).is_none());
        let g = m("*bea*");
        let name = "Dog beach.jpg";
        assert_eq!(g.highlights(name.as_bytes(), &fold(name.as_bytes()), false), [(4, 7)]);
    }

    #[test]
    fn filters() {
        let x = m("ext:png");
        assert!(x.cheap_ok(b"a.png", false, 0));
        assert!(!x.cheap_ok(b"a.png", true, 0));
        assert!(!x.cheap_ok(b"a.jpg", false, 0));
        let k = m("kind:folder");
        assert!(k.cheap_ok(b"x", true, 5));
        assert!(!k.cheap_ok(b"x", false, 5));
        let s = m("size:<1k");
        assert!(s.meta_ok(999, 0) && !s.meta_ok(1000, 0));
        assert!(!s.cheap_ok(b"d", true, 0));
        let d = m("modified:<2026-01-01");
        let jan1 = crate::time::date_start(2026, 1, 1, 0) as u64;
        assert!(d.meta_ok(0, jan1 - 1) && !d.meta_ok(0, jan1));
        let day = m("modified:2026-01-01");
        assert!(day.meta_ok(0, jan1 + 3600) && !day.meta_ok(0, jan1 + 90_000));
    }

    #[test]
    fn unicode_highlights() {
        let name = "Ünïcödé_Fïlé.txt";
        let lower = fold(name.as_bytes());
        let x = m("fïlé");
        let h = x.highlights(name.as_bytes(), &lower, false);
        assert_eq!(h.len(), 1);
        assert_eq!(&name[h[0].0 as usize..h[0].1 as usize], "Fïlé");
        let f = m("nïcf");
        let h = f.highlights(name.as_bytes(), &lower, true);
        let picked: String = h.iter().map(|&(a, b)| &name[a as usize..b as usize]).collect();
        assert_eq!(picked, "nïcF");
    }

    #[test]
    fn ancestors_and_needles() {
        let x = m(".config hypr");
        let lower = fold(b"hyprland.conf");
        assert!(x.eval(b"hyprland.conf", &lower, false, false, |t| t.in_ancestor(b".config", b".config")).is_some());
        assert!(x.eval(b"hyprland.conf", &lower, false, false, |_| false).is_none());
        assert!(x.eval(b".config", b".config", true, false, |_| false).is_none());
        assert_eq!(x.needles().len(), 2);
        assert_eq!(m("\"a b\" cat").needles().len(), 1);
        assert_eq!(m("kind:image").needles().len(), 0);
        assert!(m("ext:png").ext_needle().is_some());
    }
}
