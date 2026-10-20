//! The one duration formatter and the one token formatter (issue
//! #613): every site that used to format a duration or a token count
//! on its own now calls here instead, so rows, the state pane,
//! details, the header, the log's `end` lines, the ETA, the token
//! bar and the treemap labels can never disagree about how either
//! looks.
//!
//! Also the one place a clock time (`HH:MM:SS`) is ever formatted
//! (issue #613's "clock times"): always through an explicitly passed
//! [`ClockZone`], never the machine's own zone read directly, so a
//! test can pin UTC or a fixed offset and still get byte-identical
//! output on any machine. The binary is the only thing that
//! implements one with the real local zone.

/// `0.4s` / `42.0s` under a minute, `6m12s` under an hour, `2h05m`
/// under a day, `3d04h` beyond that. Each level rounds `secs` itself
/// to its own unit once -- never the already-rounded value from the
/// level below -- so a value never rounds twice on its way to its
/// final shape (`61.46s` -> `1m01s`, not `1m02s`; `3629.5s` ->
/// `1h00m`, not `1h01m`).
pub fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0);
    // Rounded to the tenth of a second up front, so a value like
    // `59.96` (which rounds to a whole `60.0s`) is decided by that
    // rounded value, not by the unrounded one still being under 60.
    let tenths = (secs * 10.0).round() as u64;
    if tenths < 600 {
        return format!("{}.{}s", tenths / 10, tenths % 10);
    }
    let total_seconds = secs.round() as u64;
    if total_seconds < 3600 {
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        return format!("{minutes}m{seconds:02}s");
    }
    // Rounded directly from `secs` to whole minutes, not from
    // `total_seconds`: rounding an already-rounded value again is
    // how the hours branch used to round twice.
    let minutes_total = (secs / 60.0).round() as u64;
    if minutes_total < 1440 {
        return format!("{}h{:02}m", minutes_total / 60, minutes_total % 60);
    }
    // Same rule one level up: rounded directly from `secs` to whole
    // hours, not from `minutes_total`.
    let hours_total = (secs / 3600.0).round() as u64;
    format!("{}d{:02}h", hours_total / 24, hours_total % 24)
}

/// One unit's own value, formatted by the "below 100: one decimal,
/// 100 or more: no decimal" rule -- `None` means the rounded value
/// crossed 1000 in this unit and belongs one unit up instead (a
/// carry the caller resolves by re-formatting at the next scale).
fn format_unit_value(value: f64) -> Option<String> {
    if value < 100.0 {
        let tenths = (value * 10.0).round();
        let rounded = tenths / 10.0;
        if rounded >= 100.0 {
            Some(format!("{rounded:.0}"))
        } else {
            Some(format!("{rounded:.1}"))
        }
    } else {
        let rounded = value.round();
        if rounded >= 1000.0 {
            None
        } else {
            Some(format!("{rounded:.0}"))
        }
    }
}

/// `369 t`, `18.0 kt`, `142 kt`, `2.1 Mt`, `412 kt`. Rounding carries
/// into the next unit (999,960 tokens -> `1.0 Mt`, not `1000 kt`).
pub fn format_tokens(n: u64) -> String {
    const UNITS: [(f64, &str); 3] = [(1e9, "Gt"), (1e6, "Mt"), (1e3, "kt")];
    let value = n as f64;
    for (i, &(scale, suffix)) in UNITS.iter().enumerate() {
        if value < scale {
            continue;
        }
        match format_unit_value(value / scale) {
            Some(formatted) => return format!("{formatted} {suffix}"),
            None => {
                // Carried past this unit's own top -- one unit up
                // from here always exists when this branch can be
                // reached (`i == 0`, Gt, never overflows `u64` into
                // a fourth unit at any token count worth showing).
                let (up_scale, up_suffix) = UNITS[i.saturating_sub(1)];
                let up = format_unit_value(value / up_scale)
                    .unwrap_or_else(|| format!("{:.0}", value / up_scale));
                return format!("{up} {up_suffix}");
            }
        }
    }
    format!("{n} t")
}

/// Resolves a UTC instant to a local calendar time (DESIGN.md's
/// "clock times" section, issue #613). A per-timestamp method, not a
/// single fixed offset held once, so an implementation backed by the
/// system's real zone (`chrono::Local`) resolves DST correctly for
/// each instant rather than for whenever the zone was read.
pub trait ClockZone {
    fn local(&self, utc: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::FixedOffset>;
}

impl ClockZone for chrono::FixedOffset {
    fn local(&self, utc: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::FixedOffset> {
        utc.with_timezone(self)
    }
}

/// `HH:MM:SS`, in `zone`'s own local time, parsed from an RFC-3339
/// timestamp in any source format this crate sees (state's
/// microsecond-plus-offset, or `--events`' millisecond-plus-`Z`).
/// `None` only when `ts` fails to parse at all, which no real source
/// here ever produces.
pub fn format_clock_time(ts: &str, zone: &dyn ClockZone) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let utc = parsed.with_timezone(&chrono::Utc);
    Some(zone.local(utc).format("%H:%M:%S").to_string())
}

/// The Time screen's own clock axis (issue #650): the round tick
/// interval, in seconds, that keeps labels about `target_columns`
/// columns apart over `span_s` spread across `width` columns -- one
/// of 5, 10, 15 or 30 seconds, 1, 2, 5, 10, 15 or 30 minutes, or 1
/// hour, whichever is the smallest that keeps ticks at least that
/// far apart; beyond an hour, the smallest whole number of hours that
/// still does.
pub fn clock_tick_seconds(span_s: f64, width: usize, target_columns: usize) -> f64 {
    const CANDIDATES_S: [f64; 11] = [
        5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ];
    let width = width.max(1) as f64;
    let seconds_per_column = (span_s.max(0.0) / width).max(f64::MIN_POSITIVE);
    let target_columns = target_columns.max(1) as f64;
    for candidate in CANDIDATES_S {
        if candidate / seconds_per_column >= target_columns {
            return candidate;
        }
    }
    let hours = (target_columns * seconds_per_column / 3600.0)
        .ceil()
        .max(1.0);
    hours * 3600.0
}

/// The midpoint between `start_ts` and `end_ts`, formatted the same
/// way [`format_clock_time`] formats either end (the Cost screen's
/// own chart axis, DESIGN.md: "a clock axis from the run's start to
/// now") -- `None` when either timestamp fails to parse.
pub fn midpoint_clock_time(start_ts: &str, end_ts: &str, zone: &dyn ClockZone) -> Option<String> {
    let start = chrono::DateTime::parse_from_rfc3339(start_ts).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(end_ts).ok()?;
    let mid = start + (end - start) / 2;
    let utc = mid.with_timezone(&chrono::Utc);
    Some(zone.local(utc).format("%H:%M:%S").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_under_a_minute() {
        assert_eq!(format_duration(0.4), "0.4s");
        assert_eq!(format_duration(42.0), "42.0s");
    }

    #[test]
    fn duration_under_an_hour() {
        assert_eq!(format_duration(372.0), "6m12s");
    }

    #[test]
    fn duration_under_a_day() {
        assert_eq!(format_duration(7500.0), "2h05m");
    }

    #[test]
    fn duration_a_day_or_longer() {
        assert_eq!(format_duration(273_600.0), "3d04h");
    }

    #[test]
    fn duration_carries_seconds_into_minutes() {
        assert_eq!(format_duration(59.96), "1m00s");
        assert_eq!(format_duration(119.7), "2m00s");
        assert_eq!(format_duration(119.4), "1m59s");
        assert_eq!(format_duration(65.0), "1m05s");
    }

    #[test]
    fn duration_carries_minutes_into_hours() {
        assert_eq!(format_duration(3599.6), "1h00m");
    }

    #[test]
    fn duration_rounds_leftover_seconds_into_the_next_minute() {
        // 7170s is 1h59m30s; truncating the leftover 30s would show
        // `1h59m` instead of carrying it into `2h00m`.
        assert_eq!(format_duration(7170.0), "2h00m");
    }

    #[test]
    fn duration_rounds_leftover_minutes_into_a_day() {
        // 86370s is 23h59m30s; rounding the leftover minutes up to a
        // full 24h carries into the next day rather than showing
        // `24h00m`.
        assert_eq!(format_duration(86_370.0), "1d00h");
    }

    #[test]
    fn duration_rounds_only_once() {
        // 61.46 rounds to 61s directly; rounding the tenths (615)
        // again would give 62s and show `1m02s` instead of `1m01s`.
        assert_eq!(format_duration(61.46), "1m01s");
    }

    #[test]
    fn duration_hours_round_only_once() {
        // 3629.5s is 60.49 minutes; rounding seconds to 3630 first
        // and then that to minutes would carry an extra minute and
        // show `1h01m` instead of `1h00m`.
        assert_eq!(format_duration(3629.5), "1h00m");
    }

    #[test]
    fn duration_days_round_only_once() {
        // 91799.5s is 25.4999 hours; rounding seconds or minutes
        // first and then that to hours would carry an extra hour and
        // show `1d02h` instead of `1d01h`.
        assert_eq!(format_duration(91_799.5), "1d01h");
    }

    #[test]
    fn duration_just_under_a_minute() {
        assert_eq!(format_duration(59.94), "59.9s");
    }

    #[test]
    fn duration_carries_hours_into_days() {
        assert_eq!(format_duration(86_399.6), "1d00h");
    }

    #[test]
    fn tokens_below_a_thousand_are_whole() {
        assert_eq!(format_tokens(0), "0 t");
        assert_eq!(format_tokens(369), "369 t");
        assert_eq!(format_tokens(999), "999 t");
    }

    #[test]
    fn tokens_below_a_hundred_of_a_unit_show_one_decimal() {
        assert_eq!(format_tokens(18_000), "18.0 kt");
        assert_eq!(format_tokens(2_100_000), "2.1 Mt");
    }

    #[test]
    fn tokens_a_hundred_or_more_of_a_unit_show_no_decimal() {
        assert_eq!(format_tokens(142_000), "142 kt");
        assert_eq!(format_tokens(412_000), "412 kt");
    }

    #[test]
    fn tokens_carry_into_the_next_unit() {
        assert_eq!(format_tokens(999_960), "1.0 Mt");
        assert_eq!(format_tokens(999_960_000), "1.0 Gt");
    }

    #[test]
    fn tokens_round_up_to_a_whole_unit() {
        assert_eq!(format_tokens(99_950), "100 kt");
    }

    #[test]
    fn tokens_at_a_unit_boundary() {
        assert_eq!(format_tokens(1_000), "1.0 kt");
        assert_eq!(format_tokens(1_000_000), "1.0 Mt");
        assert_eq!(format_tokens(1_000_000_000), "1.0 Gt");
    }

    #[test]
    fn clock_tick_picks_the_smallest_candidate_that_keeps_ticks_apart() {
        // 80 columns is this crate's own typical timeline width.
        assert_eq!(clock_tick_seconds(30.0, 80, 10), 5.0);
        assert_eq!(clock_tick_seconds(120.0, 80, 10), 15.0);
        assert_eq!(clock_tick_seconds(600.0, 80, 10), 120.0);
        assert_eq!(clock_tick_seconds(1_200.0, 80, 10), 300.0);
        assert_eq!(clock_tick_seconds(2_400.0, 80, 10), 300.0);
        assert_eq!(clock_tick_seconds(3_600.0, 80, 10), 600.0);
        assert_eq!(clock_tick_seconds(7_200.0, 80, 10), 900.0);
        assert_eq!(clock_tick_seconds(14_400.0, 80, 10), 1800.0);
        assert_eq!(clock_tick_seconds(21_600.0, 80, 10), 3600.0);
    }

    #[test]
    fn clock_tick_falls_back_to_whole_hours_past_the_longest_candidate() {
        // A 48h span over just 20 columns needs ticks far enough
        // apart that even hourly ticks aren't enough on their own --
        // the fallback rounds up to the next whole hour that is.
        let tick = clock_tick_seconds(48.0 * 3600.0, 20, 10);
        assert_eq!(tick % 3600.0, 0.0);
        let seconds_per_column = 48.0 * 3600.0 / 20.0;
        assert!(tick / seconds_per_column >= 10.0);
    }

    #[test]
    fn clock_tick_keeps_labels_about_target_columns_apart_at_any_width() {
        for width in [20, 40, 80, 120] {
            for span_s in [30.0, 300.0, 3_600.0, 21_600.0] {
                let tick = clock_tick_seconds(span_s, width, 10);
                let seconds_per_column = span_s / width as f64;
                assert!(
                    tick / seconds_per_column >= 10.0,
                    "tick {tick} too tight at width {width}, span {span_s}"
                );
            }
        }
    }

    #[test]
    fn clock_time_in_utc() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(
            format_clock_time("2026-10-08T11:04:58Z", &utc).unwrap(),
            "11:04:58"
        );
    }

    #[test]
    fn clock_time_at_a_nonzero_offset() {
        let plus_530 = chrono::FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
        assert_eq!(
            format_clock_time("2026-10-08T11:04:58Z", &plus_530).unwrap(),
            "16:34:58"
        );
    }

    #[test]
    fn clock_time_rejects_unparseable_timestamps() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(format_clock_time("not a timestamp", &utc), None);
    }

    #[test]
    fn midpoint_clock_time_halves_the_span() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(
            midpoint_clock_time("2026-10-08T11:00:00Z", "2026-10-08T12:00:00Z", &utc).unwrap(),
            "11:30:00"
        );
    }

    #[test]
    fn midpoint_clock_time_rejects_unparseable_timestamps() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        assert_eq!(
            midpoint_clock_time("not a timestamp", "2026-10-08T12:00:00Z", &utc),
            None
        );
    }
}
