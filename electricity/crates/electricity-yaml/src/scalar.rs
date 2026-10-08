//! YAML 1.1 implicit scalar resolution and tag-directed construction,
//! replicating PyYAML's `resolver.py`/`constructor.py` (runtime-semantics
//! §1.1, DESIGN.md §3.2).
//!
//! Every regex below is PyYAML's own, copied close to verbatim (Rust
//! regex's `(?x)` extended mode matches Python's `re.X`: insignificant
//! whitespace is ignored outside a character class) rather than
//! hand-translated, to minimize the chance of a transcription bug
//! diverging from the reference.

use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use electricity_value::{IntValue, Value};
use num_bigint::BigInt;
use num_traits::Num;
use std::sync::OnceLock;

pub const TAG_STR: &str = "tag:yaml.org,2002:str";
pub const TAG_SEQ: &str = "tag:yaml.org,2002:seq";
pub const TAG_MAP: &str = "tag:yaml.org,2002:map";
pub const TAG_NULL: &str = "tag:yaml.org,2002:null";
pub const TAG_BOOL: &str = "tag:yaml.org,2002:bool";
pub const TAG_INT: &str = "tag:yaml.org,2002:int";
pub const TAG_FLOAT: &str = "tag:yaml.org,2002:float";
pub const TAG_TIMESTAMP: &str = "tag:yaml.org,2002:timestamp";
pub const TAG_MERGE: &str = "tag:yaml.org,2002:merge";
pub const TAG_VALUE: &str = "tag:yaml.org,2002:value";
pub const TAG_BINARY: &str = "tag:yaml.org,2002:binary";

fn bool_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?x)^(?:yes|Yes|YES|no|No|NO
                    |true|True|TRUE|false|False|FALSE
                    |on|On|ON|off|Off|OFF)$",
        )
        .unwrap()
    })
}

fn float_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The trailing `\n?` matches CPython's own un-flagged `$`, which
        // (unlike this crate's regex crate) also matches just before a
        // single trailing `\n` -- needed for a literal/folded block
        // scalar (whose text always ends in one) forced through implicit
        // resolution by a bare `!` (confirmed: `! |\n  12\n` is the int
        // `12` under Circuitry's real loader, not a string).
        regex::Regex::new(
            r"(?x)^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?
                    |\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?
                    |[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
                    |[-+]?\.(?:inf|Inf|INF)
                    |\.(?:nan|NaN|NAN))\n?$",
        )
        .unwrap()
    })
}

fn int_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // See `float_re`'s comment on the trailing `\n?`.
        regex::Regex::new(
            r"(?x)^(?:[-+]?0b[0-1_]+
                    |[-+]?0[0-7_]+
                    |[-+]?(?:0|[1-9][0-9_]*)
                    |[-+]?0x[0-9a-fA-F_]+
                    |[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)\n?$",
        )
        .unwrap()
    })
}

fn null_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    // See `float_re`'s comment on the trailing `\n?`: needed for a bare
    // `!` forcing a literal/folded block scalar (always `\n`-terminated)
    // through implicit resolution, e.g. `! |\n  ~\n` (confirmed: `None`
    // under Circuitry's real loader, not the string `"~\n"`).
    RE.get_or_init(|| regex::Regex::new(r"^(?:~|null|Null|NULL|)\n?$").unwrap())
}

/// The *implicit* timestamp pattern -- distinct from, and stricter than,
/// [`timestamp_re`]: PyYAML's own implicit resolver requires an exact
/// 2-digit month/day for a date with no time part, only relaxing to a
/// single digit when a time component follows. `timestamp_regexp` (used
/// for *construction*, once a scalar is already known to be a
/// timestamp) has no such restriction. Confirmed: a bare `2024-1-1`
/// stays the string `"2024-1-1"` implicitly, but an explicit `!!timestamp
/// 2024-1-1` does construct a date.
fn implicit_timestamp_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // See `float_re`'s comment on the trailing `\n?` (same bare-`!`
        // block-scalar reasoning, confirmed for a date: `! |\n
        // 2024-01-01\n` is `datetime.date(2024, 1, 1)` under Circuitry's
        // real loader, not the string `"2024-01-01\n"`).
        regex::Regex::new(
            r"(?x)^(?:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]
                    |[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?
                     (?:[Tt]|[\x20\t]+)[0-9][0-9]?
                     :[0-9][0-9]:[0-9][0-9](?:\.[0-9]*)?
                     (?:[\x20\t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?)\n?$",
        )
        .unwrap()
    })
}

fn timestamp_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // See `float_re`'s comment on the trailing `\n?` (same bare-`!`
        // block-scalar reasoning as `implicit_timestamp_re` above -- this
        // is the separate regex `construct_timestamp` itself matches
        // against, so it needs the same tolerance).
        regex::Regex::new(
            r"(?x)^(?P<year>[0-9][0-9][0-9][0-9])
                -(?P<month>[0-9][0-9]?)
                -(?P<day>[0-9][0-9]?)
                (?:(?:[Tt]|[\x20\t]+)
                (?P<hour>[0-9][0-9]?)
                :(?P<minute>[0-9][0-9])
                :(?P<second>[0-9][0-9])
                (?:\.(?P<fraction>[0-9]*))?
                (?:[\x20\t]*(?P<tz>Z|(?P<tz_sign>[-+])(?P<tz_hour>[0-9][0-9]?)
                (?::(?P<tz_minute>[0-9][0-9]))?))?)?\n?$",
        )
        .unwrap()
    })
}

/// The tag a *plain* scalar implicitly resolves to, checked in PyYAML's
/// own registration order (bool, float, int, merge, null, timestamp,
/// value), defaulting to `str`. Only plain-style scalars go through this;
/// quoted and block scalars are always `str` (runtime-semantics §1.1,
/// confirmed: `"on"`/`'on'`/a literal block containing `on` all stay the
/// string `"on"`).
pub fn implicit_tag(text: &str) -> &'static str {
    if bool_re().is_match(text) {
        TAG_BOOL
    } else if float_re().is_match(text) {
        TAG_FLOAT
    } else if int_re().is_match(text) {
        TAG_INT
    } else if text == "<<" {
        TAG_MERGE
    } else if null_re().is_match(text) {
        TAG_NULL
    } else if implicit_timestamp_re().is_match(text) {
        TAG_TIMESTAMP
    } else if text == "=" {
        TAG_VALUE
    } else {
        TAG_STR
    }
}

/// `SafeConstructor.construct_yaml_bool`: lowercases, then looks up a
/// fixed 6-entry table. Used for both implicit (already regex-validated,
/// so always found) and explicit `!!bool` (arbitrary casing, e.g. `!!bool
/// YeS`, something the implicit regex would never match) scalars.
pub fn construct_bool(text: &str) -> Option<bool> {
    match text.to_lowercase().as_str() {
        "yes" | "true" | "on" => Some(true),
        "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// `SafeConstructor.construct_yaml_int`: strips underscores, then a
/// `-`/`+` sign, then dispatches on the (sign-stripped) prefix — `0`
/// alone, `0b`/`0x`, a legacy `0`-leading octal, `H:MM:SS` sexagesimal,
/// or plain decimal. Arbitrary-precision throughout (Python ints are
/// unbounded).
///
/// Deliberately *not* trimmed up front, unlike an earlier version of
/// this function: PyYAML's own `construct_yaml_int` never calls `.strip()`
/// either -- only Python's `int(s, base)`, at the one spot each branch
/// below finally calls it, tolerates surrounding whitespace on its own.
/// Trimming before the sign/prefix checks instead of at that point changes
/// which branch a value with leading whitespace takes: PyYAML's sign
/// check runs on the *raw* first character, so `"- 42"` strips a sign
/// and falls through every prefix check (none match `" 42"`) to the
/// final decimal branch, whose `int(" 42")` call is what actually
/// tolerates the inner space -- while `" 0x1A"` (whitespace *before* the
/// sign/prefix) never matches the `0x` branch at all (`" 0x1A"` doesn't
/// start with `"0x"`) and falls all the way to `int(" 0x1A")`, which
/// raises, not to a hex parse.
pub fn construct_int(text: &str) -> Option<IntValue> {
    let value = text.replace('_', "");
    let (negative, rest) = split_sign(&value)?;
    let magnitude = if rest == "0" {
        BigInt::from(0)
    } else if let Some(digits) = rest.strip_prefix("0b") {
        parse_radix(digits, 2)?
    } else if let Some(digits) = rest.strip_prefix("0x") {
        parse_radix(digits, 16)?
    } else if rest.starts_with('0') {
        parse_radix(rest, 8)?
    } else if rest.contains(':') {
        sexagesimal_bigint(rest)?
    } else {
        parse_radix(rest, 10)?
    };
    Some(IntValue::from_bigint(if negative {
        -magnitude
    } else {
        magnitude
    }))
}

/// `s`, trimmed and parsed as `radix` -- the one point at which this
/// crate's `construct_int` tolerates surrounding whitespace, mirroring
/// Python's `int(s, radix)` doing the same at the one point *it*'s
/// called (`construct_int`'s own doc comment).
fn parse_radix(s: &str, radix: u32) -> Option<BigInt> {
    let trimmed = s.trim();
    non_empty(trimmed)?;
    BigInt::from_str_radix(trimmed, radix).ok()
}

/// `SafeConstructor.construct_yaml_float`: strips underscores and
/// lowercases, then a sign, then `.inf`/`.nan`, `H:MM:SS[.ff]`
/// sexagesimal, or a plain `f64` parse -- not trimmed up front, for the
/// same reason as [`construct_int`] (its doc comment): `" .inf"` never
/// matches the exact `".inf"` check and falls through to a plain `f64`
/// parse of `" .inf"` (trimmed to `".inf"`, which -- unlike Python's
/// `float()` -- Rust's own `f64` parser rejects outright, matching
/// PyYAML's `float(".inf")` likewise raising: neither accepts the
/// leading-dot spelling outside this function's own explicit check).
pub fn construct_float(text: &str) -> Option<f64> {
    let value = text.replace('_', "").to_lowercase();
    let (negative, rest) = split_sign(&value)?;
    let sign = if negative { -1.0 } else { 1.0 };
    if rest == ".inf" {
        return Some(sign * f64::INFINITY);
    }
    if rest == ".nan" {
        // PyYAML returns its single shared `nan_value` unmultiplied by
        // sign here — a leading `-` on `.nan` doesn't flip anything a
        // NaN's sign bit would observably change anyway.
        return Some(f64::NAN);
    }
    if rest.contains(':') {
        // `constructor.py:283-289` adds from the *last* digit group to the
        // first (`value += digit*base; base *= 60`), which rounds once per
        // group starting from the smallest; summing left-to-right instead
        // (`total*60 + part`) rounds in a different order and can land on
        // a different double for a value exactly halfway between two of
        // them (e.g. `1:30:00.06`) -- copy PyYAML's own order exactly.
        let mut value = 0.0_f64;
        let mut base = 1.0_f64;
        for part in rest.rsplit(':') {
            let part = part.trim();
            non_empty(part)?;
            value += part.parse::<f64>().ok()? * base;
            base *= 60.0;
        }
        return Some(sign * value);
    }
    let trimmed = rest.trim();
    non_empty(trimmed)?;
    trimmed.parse::<f64>().ok().map(|v| sign * v)
}

/// `SafeConstructor.construct_yaml_timestamp`: a date, or a naive/offset
/// date-time, from PyYAML's `timestamp_regexp`. Returns `Value::Date` for
/// a date-only match, `Value::DateTime` otherwise — `None` (`tzinfo`)
/// when the scalar carries no explicit offset, matching PyYAML's naive
/// `datetime.datetime` (DESIGN.md §3.2).
pub fn construct_timestamp(text: &str) -> Option<Value> {
    let caps = timestamp_re().captures(text)?;
    let year: i32 = caps.name("year")?.as_str().parse().ok()?;
    // Python's `datetime.date`/`datetime.datetime` reject year 0 (`MINYEAR
    // == 1`); `chrono::NaiveDate` has no such floor, so without this check
    // `0000-01-01` would load here where Circuitry's real loader raises.
    if year < 1 {
        return None;
    }
    let month: u32 = caps.name("month")?.as_str().parse().ok()?;
    let day: u32 = caps.name("day")?.as_str().parse().ok()?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let Some(hour_m) = caps.name("hour") else {
        return Some(Value::Date(date));
    };
    let hour: u32 = hour_m.as_str().parse().ok()?;
    let minute: u32 = caps.name("minute")?.as_str().parse().ok()?;
    let second: u32 = caps.name("second")?.as_str().parse().ok()?;
    let micros: u32 = match caps.name("fraction") {
        Some(m) => {
            let mut digits = m.as_str().to_string();
            digits.truncate(6);
            while digits.len() < 6 {
                digits.push('0');
            }
            digits.parse().ok()?
        }
        None => 0,
    };
    let time = NaiveTime::from_hms_micro_opt(hour, minute, second, micros)?;
    let naive = NaiveDateTime::new(date, time);
    let offset = if caps.name("tz_sign").is_some() {
        let tz_hour: i32 = caps.name("tz_hour")?.as_str().parse().ok()?;
        let tz_minute: i32 = caps
            .name("tz_minute")
            .map(|m| m.as_str().parse().ok())
            .unwrap_or(Some(0))?;
        let total_seconds = (tz_hour * 3600 + tz_minute * 60)
            * if caps.name("tz_sign").unwrap().as_str() == "-" {
                -1
            } else {
                1
            };
        Some(FixedOffset::east_opt(total_seconds)?)
    } else if caps.name("tz").is_some() {
        Some(FixedOffset::east_opt(0)?)
    } else {
        None
    };
    Some(Value::DateTime(naive, offset))
}

/// `SafeConstructor.construct_yaml_binary`: base64-decodes the scalar
/// text. PyYAML first encodes to ASCII bytes (failing on any non-ASCII
/// character) and then runs the base64 decode proper; both failure modes
/// collapse to `None` here since neither needs an exact message (§1, §12).
pub fn construct_binary(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None;
    }
    base64_decode(text.as_bytes())
}

fn split_sign(s: &str) -> Option<(bool, &str)> {
    match s.as_bytes().first()? {
        b'-' => Some((true, &s[1..])),
        b'+' => Some((false, &s[1..])),
        _ => Some((false, s)),
    }
}

fn non_empty(s: &str) -> Option<()> {
    if s.is_empty() { None } else { Some(()) }
}

fn sexagesimal_bigint(rest: &str) -> Option<BigInt> {
    let mut total = BigInt::from(0);
    for part in rest.split(':') {
        let part = part.trim();
        non_empty(part)?;
        let digit = BigInt::from_str_radix(part, 10).ok()?;
        total = total * 60 + digit;
    }
    Some(total)
}

/// A minimal, dependency-free base64 (standard alphabet, `=`-padded)
/// decoder — PyYAML's `!!binary` is rare enough in orchestration
/// documents that pulling in a whole crate for it isn't worth it.
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    // PyYAML's `construct_yaml_binary` (`base64.decodebytes`, over
    // `binascii.a2b_base64`) silently discards *any* byte outside the
    // alphabet and its `=` padding before decoding -- not just
    // whitespace (confirmed: `"aGVs!bG8="` decodes to `b"hello"`).
    let filtered: Vec<u8> = input
        .iter()
        .copied()
        .filter(|&b| value(b).is_some() || b == b'=')
        .collect();
    let core = filtered
        .iter()
        .position(|&b| b == b'=')
        .map_or(filtered.as_slice(), |i| &filtered[..i]);
    let pad = filtered.len() - core.len();
    if filtered.len() % 4 != 0 || pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(filtered.len() / 4 * 3);
    let digits: Option<Vec<u8>> = core.iter().map(|&b| value(b)).collect();
    let digits = digits?;
    for chunk in digits.chunks(4) {
        let n = chunk.len();
        let mut buf = [0u8; 4];
        buf[..n].copy_from_slice(chunk);
        let combined =
            (buf[0] as u32) << 18 | (buf[1] as u32) << 12 | (buf[2] as u32) << 6 | (buf[3] as u32);
        out.push((combined >> 16) as u8);
        if n > 2 {
            out.push((combined >> 8) as u8);
        }
        if n > 3 {
            out.push(combined as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_table() {
        assert_eq!(implicit_tag("on"), TAG_BOOL);
        assert_eq!(implicit_tag("Off"), TAG_BOOL);
        assert_eq!(implicit_tag("y"), TAG_STR);
        assert_eq!(implicit_tag("n"), TAG_STR);
        assert_eq!(construct_bool("YeS"), Some(true));
    }

    #[test]
    fn int_forms() {
        assert_eq!(implicit_tag("017"), TAG_INT);
        assert_eq!(construct_int("017").unwrap().to_string(), "15");
        assert_eq!(implicit_tag("0o17"), TAG_STR);
        assert_eq!(construct_int("0x1A").unwrap().to_string(), "26");
        assert_eq!(construct_int("1:30:00").unwrap().to_string(), "5400");
        assert_eq!(construct_int("-0b101").unwrap().to_string(), "-5");
    }

    #[test]
    fn float_forms() {
        assert_eq!(implicit_tag("1e3"), TAG_STR);
        assert_eq!(implicit_tag("1.0e+3"), TAG_FLOAT);
        assert!(construct_float(".inf").unwrap().is_infinite());
        assert!(construct_float(".nan").unwrap().is_nan());
    }

    #[test]
    fn null_forms() {
        assert_eq!(implicit_tag(""), TAG_NULL);
        assert_eq!(implicit_tag("~"), TAG_NULL);
        assert_eq!(implicit_tag("Null"), TAG_NULL);
    }

    #[test]
    fn timestamp_forms() {
        assert_eq!(implicit_tag("2024-01-01"), TAG_TIMESTAMP);
        let v = construct_timestamp("2024-01-01").unwrap();
        assert!(matches!(v, Value::Date(_)));
        let v = construct_timestamp("2024-01-02T03:04:05Z").unwrap();
        assert!(matches!(v, Value::DateTime(_, Some(_))));
        let v = construct_timestamp("2024-01-02 03:04:05").unwrap();
        assert!(matches!(v, Value::DateTime(_, None)));
    }

    #[test]
    fn binary_round_trip() {
        assert_eq!(construct_binary("aGVsbG8=").unwrap(), b"hello".to_vec());
    }
}
