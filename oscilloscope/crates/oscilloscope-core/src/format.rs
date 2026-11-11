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

/// Letters for 1,000-token scaling (issue #715), shared by every
/// count that carries a unit (`kt`/`Mt`/`Gt`/`Tt`, [`format_tokens`])
/// and by the arrows' own unit-less form (`k`/`M`/`G`/`T`,
/// [`format_tokens_arrow`]) -- lower `k` for kilo, as SI has it,
/// upper for the rest.
const TOKEN_SCALE: [(f64, &str); 4] = [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k")];

/// The value and scale letter both [`format_tokens`] and
/// [`format_tokens_arrow`] print, so the two can never round or
/// carry differently -- `None` for the letter means `n` is under
/// 1,000 and prints plain.
fn scale_tokens(n: u64) -> (String, &'static str) {
    let value = n as f64;
    for (i, &(scale, letter)) in TOKEN_SCALE.iter().enumerate() {
        if value < scale {
            continue;
        }
        if let Some(formatted) = format_unit_value(value / scale) {
            return (formatted, letter);
        }
        // Carried past this unit's own top -- one unit up from here
        // always exists when this branch can be reached (`i == 0`,
        // `Tt`, which no real token count ever reaches: it would
        // take over a quadrillion tokens).
        let (up_scale, up_letter) = TOKEN_SCALE[i.saturating_sub(1)];
        let up = format_unit_value(value / up_scale)
            .unwrap_or_else(|| format!("{:.0}", value / up_scale));
        return (up, up_letter);
    }
    (n.to_string(), "")
}

/// `369 t`, `18.0 kt`, `142 kt`, `2.1 Mt`, `412 kt`, `1.0 Tt`.
/// Rounding carries into the next unit (999,960 tokens -> `1.0 Mt`,
/// not `1000 kt`).
pub fn format_tokens(n: u64) -> String {
    let (value, letter) = scale_tokens(n);
    if letter.is_empty() {
        format!("{value} t")
    } else {
        format!("{value} {letter}t")
    }
}

/// The arrows' own unit-less form (issue #715, `design/tui-design.md`
/// §3.5's "Arrows carry no unit"): `212`, `1.2k`, `35.4M`, `1.2G` --
/// the same scale and rounding as [`format_tokens`], just without
/// its trailing ` t`/` kt`/... unit.
pub fn format_tokens_arrow(n: u64) -> String {
    let (value, letter) = scale_tokens(n);
    format!("{value}{letter}")
}

/// `41k`, `200k`, `900` -- the agent block's own compact context
/// figure (issue #703: `ctx 41k`, `/200k`), rounded to the nearest
/// thousand once `n` reaches one; below that, the plain number with
/// no suffix at all, since a context window never runs that small
/// in practice but a count of zero or a few tokens must still read
/// as itself, not as `0k`.
pub fn format_tokens_k(n: u64) -> String {
    if n >= 1000 {
        format!("{}k", (n as f64 / 1000.0).round() as u64)
    } else {
        n.to_string()
    }
}

/// A store's own share of a total, as a percentage (issue #641):
/// `{share:.0}%` alone reads a share under 0.5% back as a plain
/// `0%`, indistinguishable from one that is actually zero -- any
/// share that rounds to zero but is not itself zero reads `<1%`
/// instead.
pub fn format_share_pct(share_pct: f64) -> String {
    if share_pct > 0.0 && share_pct < 1.0 {
        "<1%".to_string()
    } else {
        format!("{share_pct:.0}%")
    }
}

/// A real dotted path, relative to `ancestor_path`, as a " › "-joined
/// breadcrumb (`reviews › iter_1 › review`) -- the Store screen's own
/// table row label (relative to the whole run, issue #695) and the
/// Signal's own step breadcrumb (`screens::signal_breadcrumb`), both
/// built by calling this with a different `ancestor_path` rather than
/// a second, possibly-diverging copy of the stripping rule. `path`
/// unchanged if it doesn't actually start with `ancestor_path` (never
/// expected, since every path this is called with always descends
/// from the tree it was read out of).
pub fn path_breadcrumb_relative_to(path: &str, ancestor_path: &str) -> String {
    let prefix = format!("{ancestor_path}.");
    let relative = path.strip_prefix(prefix.as_str()).unwrap_or(path);
    relative.replace('.', " \u{203a} ")
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

    /// Issue #641: a share that rounds to zero but isn't reads
    /// `<1%`, never a plain `0%` indistinguishable from an actual
    /// zero.
    #[test]
    fn share_pct_marks_a_small_nonzero_share_distinct_from_zero() {
        assert_eq!(format_share_pct(0.4), "<1%");
        assert_eq!(format_share_pct(0.0), "0%");
        assert_eq!(format_share_pct(54.0), "54%");
        assert_eq!(format_share_pct(99.6), "100%");
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
        assert_eq!(format_tokens(1_000_000_000_000), "1.0 Tt");
    }

    #[test]
    fn tokens_scale_into_tt() {
        assert_eq!(format_tokens(2_100_000_000_000), "2.1 Tt");
        assert_eq!(format_tokens(142_000_000_000_000), "142 Tt");
    }

    #[test]
    fn tokens_tt_carries_from_gt() {
        assert_eq!(format_tokens(999_960_000_000), "1.0 Tt");
    }

    /// Issue #715's own arrow examples: no unit at all below 1,000,
    /// then the same scale and rounding as `format_tokens` with a
    /// one-letter suffix and no space.
    #[test]
    fn arrow_tokens_carry_no_unit() {
        assert_eq!(format_tokens_arrow(212), "212");
        assert_eq!(format_tokens_arrow(3), "3");
        assert_eq!(format_tokens_arrow(1_200), "1.2k");
        assert_eq!(format_tokens_arrow(56), "56");
        assert_eq!(format_tokens_arrow(35_400_000), "35.4M");
        assert_eq!(format_tokens_arrow(203_000), "203k");
        assert_eq!(format_tokens_arrow(1_200_000_000), "1.2G");
        assert_eq!(format_tokens_arrow(98_800), "98.8k");
    }

    #[test]
    fn arrow_tokens_carry_into_the_next_unit() {
        assert_eq!(format_tokens_arrow(999_960), "1.0M");
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
}
