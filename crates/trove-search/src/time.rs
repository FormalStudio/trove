/// Local midnight of the day `offset` days from the one holding `t`.
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
pub(crate) fn date_start(y: i32, m: i32, d: i32, offset: i32) -> i64 {
    // SAFETY: as above; mktime only reads and normalises the struct it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d + offset;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as i64 }
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
