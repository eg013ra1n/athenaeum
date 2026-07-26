//! Pure wall-clock scheduling math for [`Mode::Scheduled`](crate::config::Mode).
//!
//! Scheduled mode sends on a **local calendar clock** ("06:00 and 14:30, every
//! day"), not on an interval. That distinction is the whole reason this module
//! exists as pure functions with no clock of its own: everything interesting
//! about a wall-clock schedule happens at the edges — the day boundary, the
//! twice-a-year DST discontinuities, an operator correcting a drifted clock — and
//! none of it is testable if the arithmetic is buried inside the batcher's
//! `select!` loop next to a live `Instant::now()`.
//!
//! The two questions the batcher (T13) asks:
//!
//! - [`next_fire`] — "when do I wake up next?", answered strictly *after* the
//!   instant passed in, so a fire at 06:00 can never re-arm itself for 06:00.
//! - [`missed_point`] — "did the agent sleep through a point?", answered with the
//!   **latest** point in `(last_fire, now]` so a machine that was off for a week
//!   catches up with exactly **one** send, never one per missed point (spec §3).
//!
//! # Time-zone handling
//!
//! The real callers work in [`chrono::Local`], which is process-global and
//! un-mockable. So the arithmetic lives in the generic
//! [`next_fire_in`] / [`missed_point_in`], parameterised over any
//! [`chrono::TimeZone`]; the `Local` functions are one-line wrappers. Tests drive
//! the generic form with `Utc` (plain table arithmetic) and with a real IANA zone
//! from `chrono-tz` (the DST arms) — a hand-rolled fake zone would only prove the
//! fake's own rules.
//!
//! DST resolution rules, both applied inside the module's point resolver:
//!
//! - **Gap** (spring-forward; the local time does not exist): fire at the next
//!   local minute that does exist. `02:30` on a US spring-forward Sunday fires at
//!   `03:00`. The send happens once, slightly late — never skipped for the day.
//! - **Fold** (fall-back; the local time happens twice): take the **earliest** of
//!   the two instants. The second pass is then in the past and is not re-fired,
//!   so an autumn Sunday sends once, like every other day.

use anyhow::{bail, Result};
use chrono::{DateTime, Days, LocalResult, NaiveDate, TimeDelta, TimeZone};

/// How many days *past today* [`next_fire_in`] will look for a valid point: the
/// scan runs `0..=MAX_LOOKAHEAD_DAYS`, i.e. today plus this many following
/// dates. Today plus one day is enough for any sane schedule (every point
/// repeats daily); the rest of the margin only exists so a pathological
/// zone/date arithmetic failure degrades to `None` instead of looping.
const MAX_LOOKAHEAD_DAYS: u64 = 8;

/// How many days *before today* [`missed_point_in`] scans: the scan runs
/// `0..=MAX_LOOKBACK_DAYS`, i.e. today plus this many earlier dates. Today plus
/// yesterday is provably enough: every schedule point repeats daily, so if *any*
/// point falls in `(last_fire, now]` then either today's or yesterday's does —
/// yesterday's points are all earlier than today's midnight and therefore
/// `<= now`, and if a point three days back were inside the window, yesterday's
/// (being later) would be inside it too. The third date is slack for zone-offset
/// edges.
const MAX_LOOKBACK_DAYS: u64 = 2;

/// Minute-by-minute probe budget for walking out of a DST gap. Real gaps are
/// 30–120 minutes; a full day bounds the loop even against a broken tz database.
const MAX_GAP_PROBE_MINUTES: u32 = 24 * 60;

/// Parse one `"HH:MM"` schedule entry into `(hour, minute)`.
///
/// Deliberately strict about the *value* and forgiving about the *padding*:
/// `"6:05"` and `"06:05"` both parse, `"06:05:00"`, `"6h05"`, `"24:00"` and
/// `"06:60"` do not. The error names the offending string because it is shown
/// verbatim to an operator who typed it into `perseus.toml` or the settings page.
pub fn parse_hhmm(s: &str) -> Result<(u8, u8)> {
    let trimmed = s.trim();
    let Some((h, m)) = trimmed.split_once(':') else {
        bail!("schedule time \"{s}\" is not in HH:MM form (e.g. \"06:00\")");
    };
    let bad = |what: &str| {
        anyhow::anyhow!("schedule time \"{s}\" has an invalid {what} — use HH:MM, 00:00–23:59")
    };
    if h.is_empty() || h.len() > 2 || !h.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("hour"));
    }
    if m.len() != 2 || !m.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("minute"));
    }
    let hour: u8 = h.parse().map_err(|_| bad("hour"))?;
    let minute: u8 = m.parse().map_err(|_| bad("minute"))?;
    if hour > 23 {
        return Err(bad("hour"));
    }
    if minute > 59 {
        return Err(bad("minute"));
    }
    Ok((hour, minute))
}

/// Render a `(hour, minute)` point back to the canonical zero-padded `"HH:MM"`.
/// The single renderer, so the config file, the web page and the log lines all
/// spell a point the same way.
pub fn format_hhmm(point: (u8, u8)) -> String {
    format!("{:02}:{:02}", point.0, point.1)
}

/// Parse, validate, sort and de-duplicate a list of `"HH:MM"` strings — the
/// normalisation every consumer of `schedule_times` shares. The first invalid
/// entry aborts with its own message (see [`parse_hhmm`]).
pub fn parse_points<S: AsRef<str>>(times: &[S]) -> Result<Vec<(u8, u8)>> {
    let mut points = times
        .iter()
        .map(|t| parse_hhmm(t.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    points.sort_unstable();
    points.dedup();
    Ok(points)
}

/// Resolve one local `(date, hour, minute)` to a real instant in `tz`, applying
/// the module's DST rules: a fold takes the earliest instant, a gap walks forward
/// minute by minute to the first local time that exists.
///
/// `None` only for genuinely unresolvable input (an out-of-range date, or a zone
/// whose lookup fails for a whole day) — the caller skips that candidate rather
/// than inventing one.
fn resolve_point<Tz: TimeZone>(
    tz: &Tz,
    date: NaiveDate,
    hour: u8,
    minute: u8,
) -> Option<DateTime<Tz>> {
    let mut naive = date.and_hms_opt(u32::from(hour), u32::from(minute), 0)?;
    for _ in 0..=MAX_GAP_PROBE_MINUTES {
        match tz.from_local_datetime(&naive) {
            // Unique mapping — the ordinary case, every day of the year but two.
            LocalResult::Single(dt) => return Some(dt),
            // Fold: the wall clock passes this time twice. Take the first pass;
            // the second is then already in the past for `next_fire`.
            LocalResult::Ambiguous(earliest, _latest) => return Some(earliest),
            // Gap (or a tz lookup failure, which chrono reports the same way):
            // step to the next local minute and ask again. Crossing midnight in
            // the process is fine — that is still "the next valid instant".
            LocalResult::None => naive = naive.checked_add_signed(TimeDelta::minutes(1))?,
        }
    }
    None
}

/// The next schedule point strictly after `now`, in `now`'s own time zone.
///
/// `None` when `times` is empty (or nothing resolvable was found in the
/// `MAX_LOOKAHEAD_DAYS` scan window). Returning an `Option` rather than a fabricated
/// timestamp is deliberate: "no schedule points" must **disarm** the batcher's
/// timer, and a caller that has to unwrap a value cannot express that. It also
/// maps 1:1 onto the batcher's existing `deadline: Option<Instant>`.
pub fn next_fire_in<Tz: TimeZone>(times: &[(u8, u8)], now: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    if times.is_empty() {
        return None;
    }
    let tz = now.timezone();
    let today = now.naive_local().date();
    for day in 0..=MAX_LOOKAHEAD_DAYS {
        let date = today.checked_add_days(Days::new(day))?;
        let mut best: Option<DateTime<Tz>> = None;
        for &(hour, minute) in times {
            let Some(candidate) = resolve_point(&tz, date, hour, minute) else {
                continue;
            };
            // Strictly after `now`: a fire at exactly 06:00 must not re-arm for
            // the same 06:00 — that would spin the loop, not schedule a day.
            if candidate > *now && best.as_ref().is_none_or(|b| candidate < *b) {
                best = Some(candidate);
            }
        }
        // The earliest qualifying point on the earliest day that has one. Days are
        // scanned in order and a later day cannot start before this one ends, so
        // the first day that yields a candidate holds the answer.
        if best.is_some() {
            return best;
        }
    }
    None
}

/// The **latest** schedule point in `(last_fire, now]`, if the agent slept
/// through one or more.
///
/// Latest-only is the contract (spec §3): an agent that was off for a week owes
/// exactly one catch-up send, not one per missed point — the pending set is
/// drained whole either way, so N fires would just be N-1 empty no-ops with N-1
/// misleading history rows. The window is half-open at the bottom (a point at
/// exactly `last_fire` already fired) and closed at the top (a point at exactly
/// `now` is due now).
pub fn missed_point_in<Tz: TimeZone>(
    times: &[(u8, u8)],
    last_fire: &DateTime<Tz>,
    now: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    if times.is_empty() || now <= last_fire {
        return None;
    }
    let tz = now.timezone();
    let today = now.naive_local().date();
    for day in 0..=MAX_LOOKBACK_DAYS {
        let date = today.checked_sub_days(Days::new(day))?;
        let mut best: Option<DateTime<Tz>> = None;
        for &(hour, minute) in times {
            let Some(candidate) = resolve_point(&tz, date, hour, minute) else {
                continue;
            };
            if candidate > *last_fire
                && candidate <= *now
                && best.as_ref().is_none_or(|b| candidate > *b)
            {
                best = Some(candidate);
            }
        }
        // Days are scanned newest-first, so the first day with a hit holds the
        // latest missed point.
        if best.is_some() {
            return best;
        }
    }
    None
}

/// [`next_fire_in`] in the process-local zone — what the batcher calls.
pub fn next_fire(
    times: &[(u8, u8)],
    now: DateTime<chrono::Local>,
) -> Option<DateTime<chrono::Local>> {
    next_fire_in(times, &now)
}

/// [`missed_point_in`] in the process-local zone — what the startup catch-up
/// check calls.
pub fn missed_point(
    times: &[(u8, u8)],
    last_fire: DateTime<chrono::Local>,
    now: DateTime<chrono::Local>,
) -> Option<DateTime<chrono::Local>> {
    missed_point_in(times, &last_fire, &now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// `Utc` stands in for "a zone with no discontinuities" — the plain
    /// day-boundary arithmetic. The DST arms below use a real IANA zone.
    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn next_fire_single_time_later_today() {
        let now = utc(2026, 7, 26, 5, 0);
        assert_eq!(
            next_fire_in(&[(6, 0)], &now),
            Some(utc(2026, 7, 26, 6, 0)),
            "a point still ahead today is today's"
        );
    }

    #[test]
    fn next_fire_single_time_already_passed_rolls_to_tomorrow() {
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(next_fire_in(&[(6, 0)], &now), Some(utc(2026, 7, 27, 6, 0)));
    }

    #[test]
    fn next_fire_at_exactly_the_point_is_tomorrow() {
        // Strictly-after: firing AT 06:00 must not re-arm for the same 06:00.
        let now = utc(2026, 7, 26, 6, 0);
        assert_eq!(next_fire_in(&[(6, 0)], &now), Some(utc(2026, 7, 27, 6, 0)));
    }

    #[test]
    fn next_fire_wraps_over_midnight() {
        let now = utc(2026, 7, 26, 23, 30);
        assert_eq!(next_fire_in(&[(0, 0)], &now), Some(utc(2026, 7, 27, 0, 0)));
    }

    #[test]
    fn next_fire_two_times_picks_the_nearer() {
        let times = [(6, 0), (14, 30)];
        assert_eq!(
            next_fire_in(&times, &utc(2026, 7, 26, 7, 0)),
            Some(utc(2026, 7, 26, 14, 30)),
            "later-today point wins over tomorrow's earlier one"
        );
        assert_eq!(
            next_fire_in(&times, &utc(2026, 7, 26, 15, 0)),
            Some(utc(2026, 7, 27, 6, 0)),
            "both of today's are gone → tomorrow's first"
        );
        assert_eq!(
            next_fire_in(&times, &utc(2026, 7, 26, 3, 0)),
            Some(utc(2026, 7, 26, 6, 0)),
            "unsorted-in-time input still yields the earliest future point"
        );
    }

    #[test]
    fn next_fire_unsorted_input_is_order_independent() {
        // The config normalises, but the math must not depend on it.
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(
            next_fire_in(&[(14, 30), (6, 0)], &now),
            next_fire_in(&[(6, 0), (14, 30)], &now)
        );
    }

    #[test]
    fn next_fire_backward_clock_jump_re_arms_for_the_same_day() {
        // Spec §3: a backward clock jump merely re-arms (worst case one extra
        // no-op flush). After firing at 06:00 the timer is set for tomorrow; if
        // the clock is then corrected back to 05:30, the next arm is today's
        // 06:00 again — never a negative or skipped deadline.
        assert_eq!(
            next_fire_in(&[(6, 0)], &utc(2026, 7, 26, 6, 0)),
            Some(utc(2026, 7, 27, 6, 0))
        );
        assert_eq!(
            next_fire_in(&[(6, 0)], &utc(2026, 7, 26, 5, 30)),
            Some(utc(2026, 7, 26, 6, 0))
        );
    }

    #[test]
    fn next_fire_without_times_is_none() {
        // No points → the caller disarms. Never a fabricated deadline.
        assert_eq!(next_fire_in(&[], &utc(2026, 7, 26, 5, 0)), None);
    }

    // ---- DST, against the real IANA database (dev-dependency `chrono-tz`) ----

    /// America/New_York, 2026: spring forward 2026-03-08 (02:00 → 03:00 local,
    /// so 02:00–02:59 does not exist); fall back 2026-11-01 (02:00 → 01:00, so
    /// 01:00–01:59 happens twice).
    const NY: chrono_tz::Tz = chrono_tz::America::New_York;

    #[test]
    fn next_fire_spring_forward_gap_fires_at_the_next_valid_minute() {
        // 02:30 does not exist on 2026-03-08. Spec §3: fire at the next valid
        // minute — 03:00 EDT — rather than skipping the day entirely.
        let now = NY.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).unwrap();
        let fire = next_fire_in(&[(2, 30)], &now).expect("a fire must exist");
        assert_eq!(
            fire.naive_local(),
            NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(3, 0, 0)
                .unwrap()
        );
        // 03:00 EDT = 07:00 UTC.
        assert_eq!(fire.to_utc(), utc(2026, 3, 8, 7, 0));
    }

    #[test]
    fn next_fire_outside_the_gap_is_unaffected_on_a_spring_forward_day() {
        let now = NY.with_ymd_and_hms(2026, 3, 8, 0, 0, 0).unwrap();
        let fire = next_fire_in(&[(6, 0)], &now).expect("a fire must exist");
        // 06:00 EDT = 10:00 UTC (already on the summer offset).
        assert_eq!(fire.to_utc(), utc(2026, 3, 8, 10, 0));
    }

    #[test]
    fn next_fire_fall_back_fold_takes_the_earliest_instant() {
        // 01:30 happens twice on 2026-11-01. The earliest is 01:30 EDT = 05:30
        // UTC; the later pass (01:30 EST = 06:30 UTC) is then in the past and is
        // not re-fired, so the day still sends exactly once.
        let now = NY.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let fire = next_fire_in(&[(1, 30)], &now).expect("a fire must exist");
        assert_eq!(fire.to_utc(), utc(2026, 11, 1, 5, 30));

        // Standing just after the first pass, the next fire is TOMORROW's 01:30,
        // not the second pass of the fold.
        let after_first = NY
            .with_ymd_and_hms(2026, 11, 1, 1, 30, 0)
            .earliest()
            .unwrap();
        let next = next_fire_in(&[(1, 30)], &after_first).expect("a fire must exist");
        assert_eq!(next.to_utc(), utc(2026, 11, 2, 6, 30), "01:30 EST next day");
    }

    #[test]
    fn next_fire_across_a_dst_boundary_keeps_the_wall_clock() {
        // The point of wall-clock scheduling: the day after the switch still
        // fires at 06:00 local, even though the UTC instant moved by an hour.
        let before = NY.with_ymd_and_hms(2026, 3, 7, 7, 0, 0).unwrap();
        let fire = next_fire_in(&[(6, 0)], &before).expect("a fire must exist");
        assert_eq!(fire.naive_local().time().to_string(), "06:00:00");
        assert_eq!(fire.to_utc(), utc(2026, 3, 8, 10, 0), "06:00 EDT");
    }

    // ---- missed_point ----

    #[test]
    fn missed_point_none_when_nothing_in_the_window() {
        // Fired at 06:00, now 07:00, next point is 14:30 → nothing missed.
        let last = utc(2026, 7, 26, 6, 0);
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(missed_point_in(&[(6, 0), (14, 30)], &last, &now), None);
    }

    #[test]
    fn missed_point_one_missed() {
        // Agent was down from 05:00 to 07:00; 06:00 passed unattended.
        let last = utc(2026, 7, 26, 5, 0);
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(
            missed_point_in(&[(6, 0), (14, 30)], &last, &now),
            Some(utc(2026, 7, 26, 6, 0))
        );
    }

    #[test]
    fn missed_point_several_missed_returns_only_the_latest() {
        // Off for three days with two points a day = six missed points; the
        // catch-up owes exactly ONE send, the most recent point (spec §3).
        let last = utc(2026, 7, 23, 5, 0);
        let now = utc(2026, 7, 26, 15, 0);
        assert_eq!(
            missed_point_in(&[(6, 0), (14, 30)], &last, &now),
            Some(utc(2026, 7, 26, 14, 30))
        );
    }

    #[test]
    fn missed_point_off_for_a_week_still_returns_the_latest() {
        let last = utc(2026, 7, 19, 15, 0);
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(
            missed_point_in(&[(6, 0), (14, 30)], &last, &now),
            Some(utc(2026, 7, 26, 6, 0))
        );
    }

    #[test]
    fn missed_point_window_is_open_at_last_fire_and_closed_at_now() {
        let point = utc(2026, 7, 26, 6, 0);
        // Exactly at last_fire → already fired, not missed.
        assert_eq!(
            missed_point_in(&[(6, 0)], &point, &utc(2026, 7, 26, 6, 30)),
            None
        );
        // Exactly at now → due.
        assert_eq!(
            missed_point_in(&[(6, 0)], &utc(2026, 7, 26, 5, 0), &point),
            Some(point)
        );
    }

    #[test]
    fn missed_point_none_when_clock_went_backwards_or_no_times() {
        let last = utc(2026, 7, 26, 8, 0);
        let now = utc(2026, 7, 26, 7, 0);
        assert_eq!(missed_point_in(&[(6, 0)], &last, &now), None);
        assert_eq!(missed_point_in(&[], &utc(2026, 7, 26, 5, 0), &now), None);
    }

    #[test]
    fn missed_point_crosses_a_dst_boundary() {
        // Down over the spring-forward weekend; the latest missed point is the
        // 06:00 on the switch day itself (06:00 EDT = 10:00 UTC).
        let last = NY.with_ymd_and_hms(2026, 3, 6, 7, 0, 0).unwrap();
        let now = NY.with_ymd_and_hms(2026, 3, 8, 12, 0, 0).unwrap();
        let missed = missed_point_in(&[(6, 0)], &last, &now).expect("a missed point");
        assert_eq!(missed.to_utc(), utc(2026, 3, 8, 10, 0));
    }

    // ---- HH:MM parsing ----

    #[test]
    fn parse_hhmm_accepts_valid_forms() {
        assert_eq!(parse_hhmm("06:00").unwrap(), (6, 0));
        assert_eq!(parse_hhmm("6:05").unwrap(), (6, 5));
        assert_eq!(parse_hhmm(" 14:30 ").unwrap(), (14, 30));
        assert_eq!(parse_hhmm("00:00").unwrap(), (0, 0));
        assert_eq!(parse_hhmm("23:59").unwrap(), (23, 59));
    }

    #[test]
    fn parse_hhmm_rejects_bad_values_naming_the_input() {
        for bad in [
            "", "6", "24:00", "06:60", "6h05", "06:5", "-1:00", "06:00:00", "aa:bb",
        ] {
            let err = parse_hhmm(bad).unwrap_err().to_string();
            // Match the QUOTED input token, not a bare substring: every message
            // ends with the hint `(e.g. "06:00")`, so a digit-only input like
            // "6" would satisfy `err.contains(bad)` without the message ever
            // naming it. `"6"` (with the quotes) appears only where the parser
            // echoes the operator's own string back.
            let quoted = format!("\"{bad}\"");
            assert!(
                err.contains(&quoted),
                "error for {bad:?} must name the offending value, got: {err}"
            );
        }
    }

    #[test]
    fn parse_points_sorts_and_dedups() {
        let points = parse_points(&["14:30", "06:00", "6:00", "00:15"]).unwrap();
        assert_eq!(points, vec![(0, 15), (6, 0), (14, 30)]);
    }

    #[test]
    fn parse_points_propagates_the_first_bad_entry() {
        let err = parse_points(&["06:00", "25:00"]).unwrap_err().to_string();
        assert!(err.contains("25:00"), "got: {err}");
    }

    #[test]
    fn format_hhmm_pads() {
        assert_eq!(format_hhmm((6, 0)), "06:00");
        assert_eq!(format_hhmm((14, 30)), "14:30");
    }

    #[test]
    fn local_wrappers_agree_with_the_generic_form() {
        // The `Local` wrappers are one-liners; this pins that they delegate
        // rather than re-implement (the process zone is whatever it is).
        let now = chrono::Local::now();
        assert_eq!(next_fire(&[(6, 0)], now), next_fire_in(&[(6, 0)], &now));
        let last = now - TimeDelta::hours(30);
        assert_eq!(
            missed_point(&[(6, 0)], last, now),
            missed_point_in(&[(6, 0)], &last, &now)
        );
    }
}
