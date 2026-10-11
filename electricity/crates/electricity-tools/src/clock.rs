//! `plugins/clock.py::ClockPlugin`, ported -- `format`/`timezone`/
//! `epoch`, with CPython's own `%z`/`%Z`/`%f` pre-processing in front
//! of the platform's own `strftime` (#448 decision 8).

use crate::fs::{as_bool, get, py_truthy, value_error};
use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDateTime, Offset, TimeZone, Timelike, Utc};
use electricity_value::{Dict, Value};
use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::c_char;

thread_local! {
    static OVERRIDE_NOW: Cell<Option<DateTime<Utc>>> = const { Cell::new(None) };
}

/// The plugin's own wall clock -- overridable by [`with_fixed_now`] so
/// the corpus-replay test can drive `ClockPlugin::execute` from a
/// fixed instant (vm-lanes.md's D1 row: "a module-level function in
/// clock.rs", never a struct field on [`ClockPlugin`] itself, so the
/// plugin stays the same unit struct `registry.rs`'s builder already
/// constructs).
fn now() -> DateTime<Utc> {
    OVERRIDE_NOW
        .with(|cell| cell.get())
        .unwrap_or_else(Utc::now)
}

/// Runs *body* with [`now`] fixed to *instant* on this thread,
/// restoring the real clock afterward even if *body* panics.
pub fn with_fixed_now<T>(instant: DateTime<Utc>, body: impl FnOnce() -> T) -> T {
    let previous = OVERRIDE_NOW.with(|cell| cell.replace(Some(instant)));
    let result = body();
    OVERRIDE_NOW.with(|cell| cell.set(previous));
    result
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

pub struct ClockPlugin;

#[async_trait(?Send)]
impl ToolPlugin for ClockPlugin {
    fn name(&self) -> &str {
        "clock"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let epoch_mode = as_bool(get(&params, "epoch"), false);
        let tz_value = get(&params, "timezone").cloned().unwrap_or(Value::None);

        // One `now()` call for the whole request -- `datetime.now(tz)`
        // is captured once in Python too, so the formatted value, the
        // `raw.iso`/`raw.epoch` fields and the epoch-mode `value` all
        // describe the exact same instant.
        let instant = now();
        let instant_epoch = instant.timestamp();

        let (naive_local, offset_seconds, tz_display, tz_abbr) = if py_truthy(&tz_value) {
            let name = tz_value.py_str();
            let tz = tzfile::Tz::named(&name).map_err(|_| {
                value_error(format!("clock: unknown timezone {}", tz_value.py_repr()))
            })?;
            let utc_naive = instant.naive_utc();
            let offset = (&tz).offset_from_utc_datetime(&utc_naive);
            let fixed = offset.fix();
            let abbr = offset.to_string();
            let local = instant.with_timezone(&fixed).naive_local();
            (local, fixed.local_minus_utc(), name, abbr)
        } else {
            (instant.naive_utc(), 0, "UTC".to_string(), "UTC".to_string())
        };

        let value = if epoch_mode {
            Value::from(instant_epoch)
        } else {
            let format_value = get(&params, "format")
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
    use electricity_value::CancellationToken;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    /// A minimal, dependency-free `block_on`: [`ClockPlugin::execute`]
    /// never actually suspends (no real I/O, no timer), so a single
    /// poll always finishes it -- this exists only so a test can call
    /// [`with_fixed_now`]'s own synchronous closure and run the async
    /// `execute` to completion *inside* it, before the override is
    /// restored (awaiting the returned `Future` after `with_fixed_now`
    /// already returned would run it against the real clock instead).
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        loop {
            if let std::task::Poll::Ready(output) = fut.as_mut().poll(&mut cx) {
                return output;
            }
            std::thread::yield_now();
        }
    }

    fn run_sync(params: Value) -> Result<ToolResult, ToolError> {
        block_on(async {
            let token = CancellationToken::new();
            let call = ToolCall {
                timeout_seconds: 30,
                config: &Value::None,
                token: &token,
                armed: false,
            };
            ClockPlugin.execute(params, &call).await
        })
    }

    fn fixed_instant() -> DateTime<Utc> {
        // `datetime(2024, 7, 4, 12, 30, 45, tzinfo=timezone.utc)`.
        chrono::Utc
            .with_ymd_and_hms(2024, 7, 4, 12, 30, 45)
            .unwrap()
    }

    #[test]
    fn default_format_matches_cpythons_own_strftime() {
        let result =
            with_fixed_now(fixed_instant(), || run_sync(Value::Dict(Dict::new()))).unwrap();
        assert_eq!(result.value, Value::from("2024-07-04T12:30:45+0000"));
    }

    #[test]
    fn isoformat_and_epoch_in_raw_match_cpython() {
        let result =
            with_fixed_now(fixed_instant(), || run_sync(Value::Dict(Dict::new()))).unwrap();
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
        let result = with_fixed_now(fixed_instant(), || {
            run_sync(dict(vec![("epoch", Value::from(true))]))
        })
        .unwrap();
        assert_eq!(result.value, Value::from(1_720_096_245i64));
    }

    #[test]
    fn a_named_timezone_resolves_through_the_system_tzdata_and_applies_dst() {
        let instant = chrono::Utc.with_ymd_and_hms(2024, 1, 15, 8, 0, 0).unwrap();
        let result = with_fixed_now(instant, || {
            run_sync(dict(vec![
                ("timezone", Value::from("America/New_York")),
                ("format", Value::from("%Y-%m-%d %H:%M:%S %Z %z")),
            ]))
        })
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
    fn unknown_timezone_names_itself_by_repr() {
        let err = with_fixed_now(fixed_instant(), || {
            run_sync(dict(vec![("timezone", Value::from("Not/AZone"))]))
        })
        .unwrap_err();
        assert_eq!(err.message, "clock: unknown timezone 'Not/AZone'");
    }

    #[test]
    fn percent_percent_format_is_time_independent() {
        let result = with_fixed_now(fixed_instant(), || {
            run_sync(dict(vec![("format", Value::from("%%"))]))
        })
        .unwrap();
        assert_eq!(result.value, Value::from("%"));
    }

    #[test]
    fn an_explicit_null_format_becomes_the_literal_string_none() {
        let result = with_fixed_now(fixed_instant(), || {
            run_sync(dict(vec![("format", Value::None)]))
        })
        .unwrap();
        // `now.strftime(str(None))` == `now.strftime("None")` -- no `%`
        // directive in it, so the platform strftime returns it verbatim.
        assert_eq!(result.value, Value::from("None"));
    }

    #[test]
    fn with_fixed_now_restores_the_real_clock_afterward() {
        let before = now();
        let fixed = fixed_instant();
        with_fixed_now(fixed, || {
            assert_eq!(now(), fixed);
        });
        assert!(now() >= before);
    }

    #[test]
    fn format_offset_z_matches_pythons_format_offset() {
        assert_eq!(format_offset_z(0), "+0000");
        assert_eq!(format_offset_z(-5 * 3600), "-0500");
        assert_eq!(format_offset_z(5 * 3600 + 30 * 60), "+0530");
    }
}
