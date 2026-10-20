//! `plugins/clock.py::ClockPlugin`, ported -- `format`/`timezone`/
//! `epoch`, with CPython's own `%z`/`%Z`/`%f` pre-processing in front
//! of the platform's own `strftime` (#448 decision 8).

use crate::fs::{as_bool, get, py_truthy, value_error};
use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, Timelike, Utc};
use electricity_value::{Dict, Value};
use std::ffi::CString;
use std::os::raw::c_char;

/// A zone lookup's own failure -- unknown name, or a name `jiff`
/// accepts but can't place this *instant* in (never actually reached:
/// every zone `jiff` resolves at all can place any `i64` second count).
struct UnknownZone;

/// *name*'s own UTC offset and abbreviation at *instant*, read from the
/// system tz database the way `zoneinfo.ZoneInfo` does: `jiff`, not a
/// zone compiled into a crate (`chrono-tz`) or a reader that only
/// understands a TZif file's explicit transition table (`tzfile`
/// 0.1.3's own docs: it never evaluates the file's trailing POSIX-TZ
/// rule string, so a "slim" tzdata build -- explicit transitions ending
/// years ago, everything past them left to that rule -- would pin the
/// zone at its last explicit transition's offset forever; issue #460's
/// own fix pass). `jiff`'s own `TimeZone::get` searches `TZDIR`/
/// the usual zoneinfo search path first, falling back to its bundled
/// database, matching `zoneinfo.TZPATH`'s own search order closely
/// enough that every name this preview needs to resolve, it does.
fn zone_offset_and_abbreviation(
    name: &str,
    instant: DateTime<Utc>,
) -> Result<(i32, String), UnknownZone> {
    let tz = jiff::tz::TimeZone::get(name).map_err(|_| UnknownZone)?;
    let timestamp = jiff::Timestamp::from_second(instant.timestamp()).map_err(|_| UnknownZone)?;
    let info = tz.to_offset_info(timestamp);
    Ok((info.offset().seconds(), info.abbreviation().to_string()))
}

/// `_format_offset(off, sep="")` -- `%z`'s own replacement text:
/// `+HHMM`, extended to `+HHMMSS` when the offset carries whole
/// seconds (never microseconds -- electricity's offsets are always a
/// whole number of seconds).
fn format_offset_z(total_seconds: i32) -> String {
    let sign = if total_seconds < 0 { '-' } else { '+' };
    let abs = total_seconds.unsigned_abs();
    let (hh, mm, ss) = (abs / 3600, (abs % 3600) / 60, abs % 60);
    let mut s = format!("{sign}{hh:02}{mm:02}");
    if ss != 0 {
        s.push_str(&format!("{ss:02}"));
    }
    s
}

/// `isoformat()`'s own offset suffix: `+HH:MM`, extended to
/// `+HH:MM:SS` when the offset carries whole seconds.
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

/// `now.isoformat()`: `YYYY-MM-DDTHH:MM:SS[.ffffff]+HH:MM[:SS]`,
/// microseconds omitted when zero.
fn isoformat(naive: &NaiveDateTime, offset_seconds: i32) -> String {
    let micro = naive.nanosecond() / 1_000;
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        naive.year(),
        naive.month(),
        naive.day(),
        naive.hour(),
        naive.minute(),
        naive.second()
    );
    if micro != 0 {
        s.push_str(&format!(".{micro:06}"));
    }
    s.push_str(&iso_offset(offset_seconds));
    s
}

/// `_pydatetime._wrap_strftime`'s own `%f`/`%z`/`%Z` pre-pass (the
/// `%:z`/pre-1000-year-`%Y` branches are never reachable here: this
/// milestone's "now" is always a real modern instant, and CPython
/// 3.11's own C-accelerated `datetime` module has no `%:z` handling at
/// all -- #448 decision 8's own pin -- so an un-pre-processed `%:z`
/// falls through to the `else` arm below exactly as it does in 3.11,
/// landing on the platform `strftime` unchanged either way) -- every
/// other directive, known or not, is handed to [`platform_strftime`]
/// verbatim, the same division of labour CPython's own `time.strftime`
/// call gets.
fn wrap_strftime(format: &str, naive: &NaiveDateTime, offset_seconds: i32, tzname: &str) -> String {
    let micros = naive.nanosecond() / 1_000;
    let mut freplace: Option<String> = None;
    let mut zreplace: Option<String> = None;
    let mut zzreplace: Option<String> = None;
    let mut new_format = String::new();
    let chars: Vec<char> = format.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        i += 1;
        if ch != '%' {
            new_format.push(ch);
            continue;
        }
        if i >= n {
            new_format.push('%');
            break;
        }
        let directive = chars[i];
        i += 1;
        match directive {
            'f' => {
                let text = freplace.get_or_insert_with(|| format!("{micros:06}"));
                new_format.push_str(text);
            }
            'z' => {
                let text = zreplace.get_or_insert_with(|| format_offset_z(offset_seconds));
                new_format.push_str(text);
            }
            'Z' => {
                let text = zzreplace.get_or_insert_with(|| tzname.replace('%', "%%"));
                new_format.push_str(text);
            }
            other => {
                new_format.push('%');
                new_format.push(other);
            }
        }
    }
    platform_strftime(&new_format, naive)
}

/// The platform's own C `strftime(3)` -- every directive `wrap_strftime`
/// didn't already substitute (`%Y`, `%m`, `%d`, `%A`, `%p`, locale-
/// dependent names, GNU/BSD extensions, ...), grown until the buffer is
/// big enough or a sane upper bound is hit.
fn platform_strftime(format: &str, naive: &NaiveDateTime) -> String {
    if format.is_empty() {
        return String::new();
    }
    let Ok(c_format) = CString::new(format) else {
        return String::new();
    };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = naive.year() - 1900;
    tm.tm_mon = naive.month() as i32 - 1;
    tm.tm_mday = naive.day() as i32;
    tm.tm_hour = naive.hour() as i32;
    tm.tm_min = naive.minute() as i32;
    tm.tm_sec = naive.second() as i32;
    tm.tm_wday = naive.weekday().num_days_from_sunday() as i32;
    tm.tm_yday = naive.ordinal() as i32 - 1;
    tm.tm_isdst = -1;

    let mut buf_size: usize = 256;
    loop {
        let mut buf: Vec<c_char> = vec![0; buf_size];
        let written = unsafe { libc::strftime(buf.as_mut_ptr(), buf_size, c_format.as_ptr(), &tm) };
        if written > 0 || buf_size > (1 << 20) {
            let bytes: Vec<u8> = buf[..written].iter().map(|&c| c as u8).collect();
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        buf_size *= 2;
    }
}

/// `ClockPlugin::execute`'s own body, taking *now* as a parameter
/// instead of reading a live clock -- the injection point #460's lane
/// D1 ruling asks for ("a module-level function..., never a global
/// mutable"), so a test (or the corpus-replay test) calls this
/// directly with a fixed instant rather than overriding a thread-local
/// or a process-wide global.
pub fn execute_at(params: &Value, now: DateTime<Utc>) -> Result<ToolResult, ToolError> {
    let epoch_mode = as_bool(get(params, "epoch"), false);
    let tz_value = get(params, "timezone").cloned().unwrap_or(Value::None);
    let instant_epoch = now.timestamp();

    let (naive_local, offset_seconds, tz_display, tz_abbr) = if py_truthy(&tz_value) {
        let name = tz_value.py_str();
        let (offset_seconds, abbr) = zone_offset_and_abbreviation(&name, now)
            .map_err(|_| value_error(format!("clock: unknown timezone {}", tz_value.py_repr())))?;
        let local = now.naive_utc() + Duration::seconds(i64::from(offset_seconds));
        (local, offset_seconds, name, abbr)
    } else {
        (now.naive_utc(), 0, "UTC".to_string(), "UTC".to_string())
    };

    let value = if epoch_mode {
        Value::from(instant_epoch)
    } else {
        let format_value = get(params, "format")
            .cloned()
            .unwrap_or_else(|| Value::Str("%Y-%m-%dT%H:%M:%S%z".to_string()));
        let format = format_value.py_str();
        Value::Str(wrap_strftime(
            &format,
            &naive_local,
            offset_seconds,
            &tz_abbr,
        ))
    };

    let mut raw = Dict::new();
    raw.insert(
        Value::Str("iso".to_string()),
        Value::Str(isoformat(&naive_local, offset_seconds)),
    );
    raw.insert(Value::Str("epoch".to_string()), Value::from(instant_epoch));
    raw.insert(Value::Str("timezone".to_string()), Value::Str(tz_display));

    Ok(ToolResult::new(value, Value::Dict(raw)))
}

pub struct ClockPlugin;

#[async_trait(?Send)]
impl ToolPlugin for ClockPlugin {
    fn name(&self) -> &str {
        "clock"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        execute_at(&params, Utc::now())
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    fn run_at(now: DateTime<Utc>, params: Value) -> Result<ToolResult, ToolError> {
        execute_at(&params, now)
    }

    fn fixed_instant() -> DateTime<Utc> {
        // `datetime(2024, 7, 4, 12, 30, 45, tzinfo=timezone.utc)`.
        chrono::Utc
            .with_ymd_and_hms(2024, 7, 4, 12, 30, 45)
            .unwrap()
    }

    #[test]
    fn default_format_matches_cpythons_own_strftime() {
        let result = run_at(fixed_instant(), Value::Dict(Dict::new())).unwrap();
        assert_eq!(result.value, Value::from("2024-07-04T12:30:45+0000"));
    }

    #[test]
    fn isoformat_and_epoch_in_raw_match_cpython() {
        let result = run_at(fixed_instant(), Value::Dict(Dict::new())).unwrap();
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::Str("iso".to_string())),
            Some(&Value::from("2024-07-04T12:30:45+00:00"))
        );
        assert_eq!(
            raw.get(&Value::Str("epoch".to_string())),
            Some(&Value::from(1_720_096_245i64))
        );
        assert_eq!(
            raw.get(&Value::Str("timezone".to_string())),
            Some(&Value::from("UTC"))
        );
    }

    #[test]
    fn epoch_mode_returns_the_integer_epoch() {
        let result = run_at(fixed_instant(), dict(vec![("epoch", Value::from(true))])).unwrap();
        assert_eq!(result.value, Value::from(1_720_096_245i64));
    }

    #[test]
    fn a_named_timezone_resolves_through_the_system_tzdata_and_applies_dst() {
        let instant = chrono::Utc.with_ymd_and_hms(2024, 1, 15, 8, 0, 0).unwrap();
        let result = run_at(
            instant,
            dict(vec![
                ("timezone", Value::from("America/New_York")),
                ("format", Value::from("%Y-%m-%d %H:%M:%S %Z %z")),
            ]),
        )
        .unwrap();
        assert_eq!(result.value, Value::from("2024-01-15 03:00:00 EST -0500"));
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(
            raw.get(&Value::Str("iso".to_string())),
            Some(&Value::from("2024-01-15T03:00:00-05:00"))
        );
        assert_eq!(
            raw.get(&Value::Str("timezone".to_string())),
            Some(&Value::from("America/New_York"))
        );
    }

    #[test]
    fn a_named_timezone_applies_dst_in_summer_too() {
        let result = run_at(
            fixed_instant(),
            dict(vec![
                ("timezone", Value::from("America/New_York")),
                ("format", Value::from("%Z %z")),
            ]),
        )
        .unwrap();
        assert_eq!(result.value, Value::from("EDT -0400"));
    }

    #[test]
    fn unknown_timezone_names_itself_by_repr() {
        let err = run_at(
            fixed_instant(),
            dict(vec![("timezone", Value::from("Not/AZone"))]),
        )
        .unwrap_err();
        assert_eq!(err.message, "clock: unknown timezone 'Not/AZone'");
    }

    #[test]
    fn unknown_timezone_reprs_a_non_string_value_without_quotes() {
        let err = run_at(fixed_instant(), dict(vec![("timezone", Value::from(5i64))])).unwrap_err();
        assert_eq!(err.message, "clock: unknown timezone 5");
    }

    #[test]
    fn percent_percent_format_is_time_independent() {
        let result = run_at(fixed_instant(), dict(vec![("format", Value::from("%%"))])).unwrap();
        assert_eq!(result.value, Value::from("%"));
    }

    #[test]
    fn literal_percent_followed_by_a_directive_does_not_swallow_it() {
        // `"%%z"` is a literal `%` (from `%%`) followed by the literal
        // character `z` -- not `%z` re-interpreted as a directive.
        let result = run_at(fixed_instant(), dict(vec![("format", Value::from("%%z"))])).unwrap();
        assert_eq!(result.value, Value::from("%z"));
    }

    #[test]
    fn an_explicit_null_format_becomes_the_literal_string_none() {
        let result = run_at(fixed_instant(), dict(vec![("format", Value::None)])).unwrap();
        // `now.strftime(str(None))` == `now.strftime("None")` -- no `%`
        // directive in it, so the platform strftime returns it verbatim.
        assert_eq!(result.value, Value::from("None"));
    }

    #[test]
    fn format_offset_z_matches_pythons_format_offset() {
        assert_eq!(format_offset_z(0), "+0000");
        assert_eq!(format_offset_z(-5 * 3600), "-0500");
        assert_eq!(format_offset_z(5 * 3600 + 30 * 60), "+0530");
    }

    // ---------------------------------------------------------------
    // Finding 5: the tz reader must evaluate a TZif file's own
    // trailing POSIX-TZ rule (its "footer"), not just its explicit
    // transition table -- a "slim" tzdata build relies on exactly that
    // for every instant past its last explicit transition.
    // ---------------------------------------------------------------

    fn be32(v: i32) -> [u8; 4] {
        v.to_be_bytes()
    }

    fn be64(v: i64) -> [u8; 8] {
        v.to_be_bytes()
    }

    /// A hand-built, minimal TZif v2 file (RFC 8536) whose only
    /// *explicit* transition is at the Unix epoch, into a fixed
    /// `EST` (UTC-5) type -- with no further transitions at all, a tz
    /// reader that ignores the trailing POSIX-TZ rule string would
    /// report `EST`/`-0500` forever, including in July. Its footer,
    /// `EST5EDT,M3.2.0,M11.1.0`, is the real post-2007 US daylight-
    /// saving rule (2nd Sunday in March to 1st Sunday in November).
    fn slim_tzif_with_us_dst_footer() -> Vec<u8> {
        let footer = b"\nEST5EDT,M3.2.0,M11.1.0\n";
        let mut out = Vec::new();
        for time_width_is_64 in [false, true] {
            out.extend_from_slice(b"TZif");
            out.push(b'2');
            out.extend_from_slice(&[0u8; 15]);
            out.extend_from_slice(&be32(0)); // isutcnt
            out.extend_from_slice(&be32(0)); // isstdcnt
            out.extend_from_slice(&be32(0)); // leapcnt
            out.extend_from_slice(&be32(1)); // timecnt
            out.extend_from_slice(&be32(1)); // typecnt
            out.extend_from_slice(&be32(4)); // charcnt: b"EST\0"
            if time_width_is_64 {
                out.extend_from_slice(&be64(0)); // transition at the epoch
            } else {
                out.extend_from_slice(&be32(0));
            }
            out.push(0); // transition type index
            out.extend_from_slice(&be32(-5 * 3600)); // utoff: EST, UTC-5
            out.push(0); // isdst
            out.push(0); // desigidx
            out.extend_from_slice(b"EST\0");
        }
        out.extend_from_slice(footer);
        out
    }

    #[test]
    fn a_slim_tzif_evaluates_its_own_posix_footer_past_the_last_transition() {
        let data = slim_tzif_with_us_dst_footer();
        let tz = jiff::tz::TimeZone::tzif("Synthetic/SlimEastern", &data).unwrap();
        // 2024-07-04T12:30:45Z -- the same instant `fixed_instant()`
        // names -- is high summer, long after this file's only
        // explicit transition (the 1970 epoch): only the footer's own
        // `M3.2.0,M11.1.0` rule can place it in EDT.
        let timestamp = jiff::Timestamp::from_second(1_720_096_245).unwrap();
        let info = tz.to_offset_info(timestamp);
        assert_eq!(info.offset().seconds(), -4 * 3600);
        assert_eq!(info.abbreviation(), "EDT");
    }
}
