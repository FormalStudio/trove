//! The words and numbers the listing shows: sizes, dates, counts.

use std::time::SystemTime;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Decimal units, as GNOME Files and Finder count: "820 bytes", "1.6 kB", "12 MB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".to_owned() } else { format!("{bytes} bytes") };
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.;
        unit += 1;
    }
    if value < 9.95 { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

/// "4.1 MB (4 100 000 bytes)", the exact count beside the rounded one past a kilobyte.
pub fn size_long(bytes: u64) -> String {
    if bytes < 1000 { size(bytes) } else { format!("{} ({} bytes)", size(bytes), group(bytes as usize)) }
}

pub fn items(n: usize) -> String {
    if n == 1 { "1 item".to_owned() } else { format!("{} items", group(n)) }
}

/// Thousands grouped with a thin space, so a 100k folder's count reads at a glance.
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// The local calendar day a listing's dates are read against, taken once per listing so a row
/// renders without asking the clock.
#[derive(Clone, Debug)]
pub struct Today {
    tz: TimeZone,
    date: Date,
}

impl Today {
    pub fn now() -> Self {
        let tz = TimeZone::system();
        let date = Timestamp::now().to_zoned(tz.clone()).date();
        Self { tz, date }
    }

    fn zoned(&self, t: SystemTime) -> Option<Zoned> {
        Timestamp::try_from(t).ok().map(|ts| ts.to_zoned(self.tz.clone()))
    }

    /// Finder's dates: "Today, 14:02", "Yesterday, 09:10", else "14 Mar 2026".
    pub fn modified(&self, t: SystemTime) -> String {
        let Some(z) = self.zoned(t) else { return String::new() };
        let day = z.date();
        if day == self.date {
            format!("Today, {}", z.strftime("%H:%M"))
        } else if self.date.yesterday().is_ok_and(|y| y == day) {
            format!("Yesterday, {}", z.strftime("%H:%M"))
        } else {
            z.strftime("%-d %b %Y").to_string()
        }
    }

    /// The preview's form, with the time: "Today, 14:02", else "14 Mar 2026, 09:41".
    pub fn long(&self, t: SystemTime) -> String {
        let Some(z) = self.zoned(t) else { return String::new() };
        let day = z.date();
        if day == self.date || self.date.yesterday().is_ok_and(|y| y == day) {
            self.modified(t)
        } else {
            z.strftime("%-d %b %Y, %H:%M").to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_600), "1.6 kB");
        assert_eq!(size(12_400), "12 kB");
        assert_eq!(size(999_999), "1.0 MB");
        assert_eq!(size(4_100_000), "4.1 MB");
        assert_eq!(size(120_000_000_000), "120 GB");
    }

    #[test]
    fn counts() {
        assert_eq!(items(1), "1 item");
        assert_eq!(items(23), "23 items");
        assert_eq!(group(100_000), "100\u{202f}000");
        assert_eq!(group(999), "999");
    }
}
