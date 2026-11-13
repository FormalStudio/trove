/// Local midnight of the day `offset` days from the one holding `t`.
#[cfg(unix)]
pub(crate) fn day_start(t: i64, offset: i32) -> i64 {
    let tt = t as libc::time_t;
    // SAFETY: tm is plain data; localtime_r writes it and reads only tt.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return t - t.rem_euclid(86_400) + i64::from(offset) * 86_400;
    }
    date_start(tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, offset)
}

/// Local midnight starting `y-m-d` plus `offset` days; mktime carries a day past the month's end
/// into the next month, and finds the right offset across a DST change.
#[cfg(unix)]
pub(crate) fn date_start(y: i32, m: i32, d: i32, offset: i32) -> i64 {
    // SAFETY: as above; mktime only reads and normalises the struct it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d + offset;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as i64 }
}

#[cfg(windows)]
pub(crate) fn day_start(t: i64, offset: i32) -> i64 {
    let Ok(ts) = jiff::Timestamp::from_second(t) else { return t - t.rem_euclid(86_400) + i64::from(offset) * 86_400 };
    let date = ts.to_zoned(jiff::tz::TimeZone::system()).date();
    date_start(i32::from(date.year()), i32::from(date.month()), i32::from(date.day()), offset)
}

/// Local midnight starting `y-m-d` plus `offset` days; a day past the month's end carries into
/// the next month.
#[cfg(windows)]
pub(crate) fn date_start(y: i32, m: i32, d: i32, offset: i32) -> i64 {
    use jiff::ToSpan;
    let Ok(first) = jiff::civil::DateTime::new(y as i16, m as i8, 1, 0, 0, 0, 0) else { return 0 };
    let day = first.checked_add(i64::from(d + offset - 1).days());
    day.and_then(|d| d.to_zoned(jiff::tz::TimeZone::system())).map_or(0, |z| z.timestamp().as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days() {
        let jan1 = date_start(2026, 1, 1, 0);
        assert_eq!(date_start(2025, 12, 31, 1), jan1);
        assert_eq!(day_start(jan1 + 5000, 0), jan1);
        assert_eq!(day_start(jan1 + 5000, -1), date_start(2025, 12, 31, 0));
    }
}
