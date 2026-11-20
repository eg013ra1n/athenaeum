//! The night rule (MCP plan R1): a frame belongs to the night of the UTC date of `date_obs - 12 h`.
//!
//! Two twins that must agree: [`night_sql`] groups in SQL, [`night_of_date_obs`] groups in Rust. The Rust twin
//! parses exactly what SQLite's `DATE()` parses (a hand-written copy of its grammar, pinned against the real
//! function by `scope::tests::night_sql_equals_night_of_date_obs`), so a scope that filters by night in SQL and a
//! list that groups by night in Rust never disagree on a frame.
//!
//! Limit, by ruling: the boundary is a fixed 12:00 UTC, not local noon; an observatory far from Greenwich can see
//! a night cut in two. Recorded in `docs/mcp/README.md`, not fixed here.
//!
//! Not parsed (SQLite gives a date for them, this gives `None`): a bare time (`22:15:03`, today's date), a
//! number (a Julian day), `now`. A `date_obs` the scanner writes is never one of them.

use chrono::{Datelike, Duration, NaiveDate};

/// The SQL twin of [`night_of_date_obs`] for a column or expression: `DATE(<column>, '-12 hours')`.
pub fn night_sql(column: &str) -> String {
    format!("DATE({column}, '-12 hours')")
}

/// SQLite's `sqlite3Isspace`.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `n` ASCII digits at `pos`, their value in `min..=max` (SQLite's `getDigits`).
fn digits(b: &[u8], pos: usize, n: usize, min: i64, max: i64) -> Option<i64> {
    let chunk = b.get(pos..pos + n)?;
    if !chunk.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = chunk.iter().fold(0i64, |a, d| a * 10 + i64::from(d - b'0'));
    (min..=max).contains(&v).then_some(v)
}

/// The time of day and zone after the date: `HH:MM[:SS[.fff]][Z|±HH:MM]`, whitespace around the zone allowed.
/// Returns `(milliseconds since midnight, zone offset in minutes)`; `None` where SQLite rejects the text.
fn parse_time(b: &[u8], mut i: usize) -> Option<(i64, i64)> {
    let h = digits(b, i, 2, 0, 24)?;
    if b.get(i + 2) != Some(&b':') {
        return None;
    }
    let m = digits(b, i + 3, 2, 0, 59)?;
    i += 5;
    let mut sec_ms = 0_i64;
    if b.get(i) == Some(&b':') {
        i += 1;
        sec_ms = digits(b, i, 2, 0, 59)? * 1000;
        i += 2;
        if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
            // Whole milliseconds, the rest of the fraction cut off (not rounded).
            let mut place = 100;
            while let Some(d) = b.get(i).filter(|d| d.is_ascii_digit()) {
                sec_ms += place * i64::from(d - b'0');
                place /= 10;
                i += 1;
            }
        }
    }
    while b.get(i).copied().is_some_and(is_space) {
        i += 1;
    }
    let mut tz = 0_i64;
    match b.get(i) {
        Some(b'Z' | b'z') => i += 1,
        Some(&c @ (b'-' | b'+')) => {
            let hh = digits(b, i + 1, 2, 0, 14)?;
            if b.get(i + 3) != Some(&b':') {
                return None;
            }
            let mm = digits(b, i + 4, 2, 0, 59)?;
            tz = if c == b'-' { -(hh * 60 + mm) } else { hh * 60 + mm };
            i += 6;
        }
        _ => {}
    }
    while b.get(i).copied().is_some_and(is_space) {
        i += 1;
    }
    if i != b.len() {
        return None;
    }
    let ms = h * 3_600_000 + m * 60_000 + sec_ms;
    Some((ms, tz))
}

/// The night a frame belongs to, from its `DATE-OBS` text alone: the UTC date of `date_obs - 12 h`, as
/// `YYYY-MM-DD`. Accepts the forms SQLite's `DATE()` accepts: `T` or a space (any run of whitespace and `T`s)
/// between date and time, a date alone, fractional seconds, `Z` or `±HH:MM`; `None` where `DATE()` is NULL.
///
/// ```
/// use athenaeum_core::scope::night::night_of_date_obs;
/// assert_eq!(night_of_date_obs("2026-09-27T03:10:00Z").as_deref(), Some("2026-09-26"));
/// assert_eq!(night_of_date_obs("garbage"), None);
/// ```
pub fn night_of_date_obs(date_obs: &str) -> Option<String> {
    let b = date_obs.as_bytes();
    let (neg, mut i) = if b.first() == Some(&b'-') { (true, 1) } else { (false, 0) };
    let y = digits(b, i, 4, 0, 9999)?;
    if b.get(i + 4) != Some(&b'-') {
        return None;
    }
    let mo = digits(b, i + 5, 2, 1, 12)?;
    if b.get(i + 7) != Some(&b'-') {
        return None;
    }
    let d = digits(b, i + 8, 2, 1, 31)?;
    i += 10;
    while b.get(i).is_some_and(|&c| is_space(c) || c == b'T') {
        i += 1;
    }
    let (ms, tz) = if i == b.len() { (0, 0) } else { parse_time(b, i)? };
    // Day 29-31 of a short month rolls into the next month, as SQLite's Julian-day arithmetic does.
    let first = NaiveDate::from_ymd_opt(if neg { -(y as i32) } else { y as i32 }, mo as u32, 1)?;
    let total = ms - tz * 60_000 - 12 * 3_600_000;
    let date = first.checked_add_signed(Duration::days(d - 1 + total.div_euclid(86_400_000)))?;
    let sign = if date.year() < 0 { "-" } else { "" };
    Some(format!("{sign}{:04}-{:02}-{:02}", date.year().abs(), date.month(), date.day()))
}
