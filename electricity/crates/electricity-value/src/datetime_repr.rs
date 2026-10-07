//! `str()`/`repr()` parity with CPython 3.11's `datetime.date` and
//! `datetime.datetime` (`Lib/datetime.py`'s pure-Python implementation,
//! which the C accelerator module matches byte for byte).
//!
//! electricity's `Value::DateTime` carries only a UTC-offset number of
//! seconds, not a `tzinfo` object identity. CPython's `timezone.__new__`
//! itself collapses any zero-second offset to the `timezone.utc` singleton
//! (verified directly: `repr(timezone(timedelta(0))) ==
//! 'datetime.timezone.utc'`), so an offset of exactly zero always reprs as
//! `datetime.timezone.utc` here too — this is not a `Value`-level
//! approximation, it is what CPython itself does for *any* zero offset
//! regardless of how it was constructed.

use chrono::{FixedOffset, NaiveDate, NaiveDateTime, Timelike};

/// `str(datetime.date(...))`: `YYYY-MM-DD`, year zero-padded to at least 4 digits.
pub fn date_str(date: &NaiveDate) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year_ce_signed(),
        date.month_py(),
        date.day_py()
    )
}

/// `repr(datetime.date(...))`: `datetime.date(Y, M, D)`, decimal, unpadded.
pub fn date_repr(date: &NaiveDate) -> String {
    format!(
        "datetime.date({}, {}, {})",
        date.year_ce_signed(),
        date.month_py(),
        date.day_py()
    )
}

/// `str(datetime.datetime(...))`: `datetime.__str__` is `isoformat(sep=' ')`.
pub fn datetime_str(naive: &NaiveDateTime, offset: Option<&FixedOffset>) -> String {
    let mut s = format!("{} {}", date_str(&naive.date()), time_component(naive));
    if let Some(offset) = offset {
        s.push_str(&iso_offset(offset.local_minus_utc()));
    }
    s
}

/// `repr(datetime.datetime(...))`: trailing zero `microsecond` dropped,
/// then trailing zero `second` dropped (never `hour`/`minute`), plus
/// `tzinfo=...` when aware.
pub fn datetime_repr(naive: &NaiveDateTime, offset: Option<&FixedOffset>) -> String {
    let date = naive.date();
    let micro = naive.nanosecond() / 1_000;
    let mut fields = vec![
        date.year_ce_signed(),
        date.month_py() as i64,
        date.day_py() as i64,
        naive.hour() as i64,
        naive.minute() as i64,
        naive.second() as i64,
        micro as i64,
    ];
    if *fields.last().unwrap() == 0 {
        fields.pop();
        if *fields.last().unwrap() == 0 {
            fields.pop();
        }
    }
    let joined = fields
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = format!("datetime.datetime({joined})");
    if let Some(offset) = offset {
        s.pop(); // drop the closing ')'
        s.push_str(&format!(
            ", tzinfo={})",
            timezone_repr(offset.local_minus_utc())
        ));
    }
    s
}

fn time_component(naive: &NaiveDateTime) -> String {
    let micro = naive.nanosecond() / 1_000;
    let base = format!(
        "{:02}:{:02}:{:02}",
        naive.hour(),
        naive.minute(),
        naive.second()
    );
    if micro == 0 {
        base
    } else {
        format!("{base}.{micro:06}")
    }
}

/// `_format_offset`: `+HH:MM`, extended to `+HH:MM:SS` when the offset
/// carries whole seconds.
fn iso_offset(total_seconds: i32) -> String {
    let sign = if total_seconds < 0 { '-' } else { '+' };
    let abs = total_seconds.unsigned_abs();
    let (hh, mm, ss) = (abs / 3600, (abs % 3600) / 60, abs % 60);
    if ss == 0 {
        format!("{sign}{hh:02}:{mm:02}")
    } else {
        format!("{sign}{hh:02}:{mm:02}:{ss:02}")
    }
}

/// `repr(tzinfo)` for a fixed-offset `timezone`: `datetime.timezone.utc`
/// when the offset is zero, else `datetime.timezone(datetime.timedelta(...))`.
fn timezone_repr(total_seconds: i32) -> String {
    if total_seconds == 0 {
        return "datetime.timezone.utc".to_string();
    }
    format!(
        "datetime.timezone({})",
        timedelta_repr(total_seconds as i64)
    )
}

/// `repr(datetime.timedelta(...))` for a whole-second duration: Python
/// normalizes to `days` (any sign) plus `seconds` in `[0, 86400)` plus
/// `microseconds` in `[0, 1_000_000)`, omitting any field that is zero
/// (electricity's offsets never carry microseconds).
fn timedelta_repr(total_seconds: i64) -> String {
    let days = total_seconds.div_euclid(86_400);
    let seconds = total_seconds.rem_euclid(86_400);
    let mut args = Vec::new();
    if days != 0 {
        args.push(format!("days={days}"));
    }
    if seconds != 0 {
        args.push(format!("seconds={seconds}"));
    }
    if args.is_empty() {
        args.push("0".to_string());
    }
    format!("datetime.timedelta({})", args.join(", "))
}

/// Extension helpers so `date_str`/`date_repr`/`datetime_repr` read as plain
/// field access, matching how CPython exposes `date.year`/`.month`/`.day`.
trait NaiveDateFields {
    fn year_ce_signed(&self) -> i64;
    fn month_py(&self) -> u32;
    fn day_py(&self) -> u32;
}

impl NaiveDateFields for NaiveDate {
    fn year_ce_signed(&self) -> i64 {
        use chrono::Datelike;
        self.year() as i64
    }
    fn month_py(&self) -> u32 {
        use chrono::Datelike;
        self.month()
    }
    fn day_py(&self) -> u32 {
        use chrono::Datelike;
        self.day()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    fn ndt(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32, micro: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_time(NaiveTime::from_hms_micro_opt(hh, mm, ss, micro).unwrap())
    }

    #[test]
    fn date_forms() {
        let d = NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
        assert_eq!(date_str(&d), "2020-01-02");
        assert_eq!(date_repr(&d), "datetime.date(2020, 1, 2)");
    }

    #[test]
    fn naive_datetime_forms() {
        let dt = ndt(2020, 1, 2, 3, 4, 5, 0);
        assert_eq!(datetime_str(&dt, None), "2020-01-02 03:04:05");
        assert_eq!(
            datetime_repr(&dt, None),
            "datetime.datetime(2020, 1, 2, 3, 4, 5)"
        );

        let dt0 = ndt(2020, 1, 2, 3, 4, 0, 0);
        assert_eq!(
            datetime_repr(&dt0, None),
            "datetime.datetime(2020, 1, 2, 3, 4)"
        );

        let dtm = ndt(2020, 1, 2, 3, 4, 5, 123456);
        assert_eq!(datetime_str(&dtm, None), "2020-01-02 03:04:05.123456");
        assert_eq!(
            datetime_repr(&dtm, None),
            "datetime.datetime(2020, 1, 2, 3, 4, 5, 123456)"
        );
    }

    #[test]
    fn utc_offset_forms() {
        let dt = ndt(2020, 1, 2, 3, 4, 5, 0);
        let utc = FixedOffset::east_opt(0).unwrap();
        assert_eq!(datetime_str(&dt, Some(&utc)), "2020-01-02 03:04:05+00:00");
        assert_eq!(
            datetime_repr(&dt, Some(&utc)),
            "datetime.datetime(2020, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)"
        );
    }

    #[test]
    fn positive_offset_forms() {
        let dt = ndt(2020, 1, 2, 3, 4, 5, 123456);
        let off = FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
        assert_eq!(
            datetime_str(&dt, Some(&off)),
            "2020-01-02 03:04:05.123456+05:30"
        );
        assert_eq!(
            datetime_repr(&dt, Some(&off)),
            "datetime.datetime(2020, 1, 2, 3, 4, 5, 123456, \
             tzinfo=datetime.timezone(datetime.timedelta(seconds=19800)))"
        );
    }

    #[test]
    fn negative_offset_with_seconds_forms() {
        let dt = ndt(2020, 1, 2, 3, 4, 5, 0);
        let off = FixedOffset::east_opt(-(5 * 3600 + 30 * 60 + 15)).unwrap();
        assert_eq!(
            datetime_str(&dt, Some(&off)),
            "2020-01-02 03:04:05-05:30:15"
        );
        assert_eq!(
            datetime_repr(&dt, Some(&off)),
            "datetime.datetime(2020, 1, 2, 3, 4, 5, \
             tzinfo=datetime.timezone(datetime.timedelta(days=-1, seconds=66585)))"
        );
    }
}
