//! `electricity_value::Value` -> `cel::Value`, Circuitry's `_to_cel`
//! (`core/cel_eval.py`) ported to Rust (runtime-semantics.md §4.2).
//!
//! This is also the sandbox boundary: nothing converted here can carry a
//! live reference back into the caller's data beyond what CEL itself can
//! express, and anything with no CEL counterpart becomes CEL `null` —
//! except a `Value::Int` too large for `i64`, which [`to_cel`] rejects
//! with [`ConvertError::Overflow`] rather than silently mapping to
//! `null`, and a *value* nested deeper than
//! [`electricity_value::MAX_DEPTH`], which [`to_cel`] rejects with
//! [`ConvertError::Depth`] rather than recursing further — reachable
//! with a run-time-built `Value` (`state`/`value`/`meta` are never read
//! through this workspace's own JSON/YAML readers, which enforce that
//! limit themselves, before reaching here), unlike every *other*
//! recursive path in this crate (the `cel`-crate-owned parse tree, and
//! this crate's own `paths`/`equality`/`ordering` walks over it), which
//! is a `cel`-crate-owned structure already bounded by the five caps
//! `nesting::max_nesting_depths` tracks -- not just bracket nesting, but
//! every binary-operator, `&&`/`||`/unary, and member/index/call chain
//! within it too (`nesting.rs`'s own module docs explain why bracket
//! nesting alone was not enough).

use std::fmt;
use std::sync::Arc;

use cel::objects::{Key, Map as CelMap};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use electricity_value::{Dict, IntValue, Value};
use indexmap::IndexMap;

/// Why [`to_cel`] could not convert a `Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertError {
    /// *value* held a `Value::Int` too large for `i64`: CEL's `int` is a
    /// 64-bit signed integer by spec, unlike Python's unbounded `int`.
    /// `celtypes.IntType.__new__` (`celpy`) raises `ValueError("overflow")`
    /// for exactly this case rather than tolerating it — so this, not a
    /// silent `null`, is `_to_cel`'s real behavior for a big int the
    /// expression actually reads (`core/cel_eval.py`).
    Overflow,
    /// *value* nested deeper than [`electricity_value::MAX_DEPTH`] —
    /// never produced by this workspace's own JSON/YAML readers, but
    /// reachable from a run-time-built `Value` (a future state merge or
    /// a loop that wraps a value — not a CEL evaluation result: neither
    /// [`crate::evaluate_condition`] nor [`crate::evaluate_expect`]
    /// produces a `Value` at all, only a `bool`).
    Depth,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::Overflow => write!(f, "overflow"),
            ConvertError::Depth => write!(f, "nesting too deep"),
        }
    }
}

/// Converts *value* into the CEL type system.
///
/// `Value::Date` (a bare `datetime.date`, not a `datetime.datetime`) has no
/// CEL counterpart, matching `_to_cel`, which only special-cases
/// `datetime.datetime`/`datetime.timedelta` and otherwise falls through to
/// `null` — a bare date is never one of those.
///
/// A `Value::DateTime` with no offset (naive, as PyYAML's timestamp
/// resolver produces for a timestamp with no explicit zone) is treated as
/// UTC. This is not a judgment call: `celtypes.TimestampType.__new__`
/// (`celpy`) does exactly this — `tzinfo=source.tzinfo or
/// datetime.timezone.utc` — so a naive and a UTC-aware datetime are
/// already the same CEL value by the time either reaches an expression;
/// there is no naive/aware split left to reproduce inside CEL itself (the
/// split in DESIGN.md §3.2 is `Value`'s own ordering, used outside CEL).
pub fn to_cel(value: &Value) -> Result<cel::Value, ConvertError> {
    to_cel_at_depth(value, 0)
}

/// *depth* is this call's own nesting level (0 at the root), checked
/// against [`electricity_value::MAX_DEPTH`] before recursing into a
/// `List`/`Dict` child -- not [`Value::depth`] computed once up front,
/// so a value deep enough to matter is rejected as soon as the walk
/// reaches that depth rather than after a full, separate traversal just
/// to measure it. Checks `depth + 1` against the limit as soon as a
/// `List`/`Dict` is matched, not only when recursing into a child, so an
/// *empty* innermost container is counted the same as a non-empty one --
/// matching [`Value::depth`]'s own, same fix for the shape that used to
/// make the two quietly disagree by one (that function's own docs).
fn to_cel_at_depth(value: &Value, depth: usize) -> Result<cel::Value, ConvertError> {
    if depth > electricity_value::MAX_DEPTH {
        return Err(ConvertError::Depth);
    }
    Ok(match value {
        Value::None => cel::Value::Null,
        Value::Bool(b) => cel::Value::Bool(*b),
        Value::Int(i) => int_to_cel(i)?,
        Value::Float(f) => cel::Value::Float(*f),
        Value::Str(s) => cel::Value::String(Arc::new(s.clone())),
        Value::Bytes(b) => cel::Value::Bytes(Arc::new(b.clone())),
        Value::List(items) => {
            if depth + 1 > electricity_value::MAX_DEPTH {
                return Err(ConvertError::Depth);
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(to_cel_at_depth(item, depth + 1)?);
            }
            cel::Value::List(Arc::new(out))
        }
        Value::Dict(entries) => {
            if depth + 1 > electricity_value::MAX_DEPTH {
                return Err(ConvertError::Depth);
            }
            cel::Value::Map(dict_to_cel(entries, depth)?)
        }
        Value::Date(_) => cel::Value::Null,
        Value::DateTime(naive, offset) => {
            let offset = offset.unwrap_or_else(|| FixedOffset::east_opt(0).expect("0 is valid"));
            let aware = match value {
                Value::DateTime(_, Some(_)) => offset
                    .from_local_datetime(naive)
                    .single()
                    .expect("FixedOffset::from_local_datetime is never ambiguous"),
                _ => offset.from_utc_datetime(naive),
            };
            cel::Value::Timestamp(aware)
        }
    })
}

fn int_to_cel(i: &IntValue) -> Result<cel::Value, ConvertError> {
    match i {
        IntValue::Small(n) => Ok(cel::Value::Int(*n)),
        IntValue::Big(_) => Err(ConvertError::Overflow),
    }
}

/// A `Value` dict key with no CEL [`Key`] counterpart drops that entry,
/// with one exception ([`NULL_KEY_SENTINEL`] below): CEL map keys must
/// be `bool`/`int`/`uint`/`string`, and there is no value in that set
/// that represents "no counterpart" the way `null` does for a value
/// position. A key too large for `i64` raises [`ConvertError::Overflow`]
/// instead of being dropped, matching every other big-int read.
fn dict_to_cel(entries: &electricity_value::Dict, depth: usize) -> Result<CelMap, ConvertError> {
    let mut map = IndexMap::with_capacity(entries.len());
    for (key, value) in entries {
        if let Some(key) = to_cel_key(key)? {
            map.insert(key, to_cel_at_depth(value, depth + 1)?);
        }
    }
    Ok(CelMap { map: Arc::new(map) })
}

/// Stands in for every dict key `_to_cel` (`core/cel_eval.py`) converts
/// to Python's own `None` because it is not one of the types that
/// function special-cases: a plain `Value::None` key, and a bare
/// `Value::Date` (never a `datetime.datetime`, the only date/time type
/// `_to_cel` special-cases; a bare `date` falls through to that
/// function's final "no CEL counterpart, exposing it as null" branch,
/// same as any value in that position). A plain Python dict assignment
/// then collapses every such key into the *same* `None` key, last value
/// wins — reachable from an ordinary orchestration, not just data
/// buried in `value`/`meta`: a YAML input file's `2026-01-01: x` is a
/// date key, and PyYAML resolves an unquoted `null`-looking key
/// (`~:`/`null:`) as `None` directly.
///
/// `cel::objects::Key` cannot represent `null` as a map key at all, so
/// this sentinel string stands in for that one collapsed slot instead —
/// a key no real orchestration will ever write, chosen so [`to_cel_key`]
/// can route every `None`/bare-`date` key to the *same* [`Key`], giving
/// [`dict_to_cel`]'s own `map.insert` the same last-value-wins collapse
/// Python's dict assignment gets for free, and so `size()` (which reads
/// the `HashMap`'s own length, needing no help here) counts the
/// collapsed slot once rather than the entries silently vanishing.
/// `equality.rs`'s own `in` arm special-cases this sentinel back into a
/// `null` match; direct indexing (`m[null]`) still raises either way —
/// `cel::Value::Null` has no [`Key`] conversion at all, matching
/// `celtypes.MapType`'s own key-type check, which rejects `None` too.
pub(crate) const NULL_KEY_SENTINEL: &str = "\u{0}electricity-cel: key with no CEL counterpart, collapsed like Circuitry's own null key\u{0}";

fn to_cel_key(key: &Value) -> Result<Option<Key>, ConvertError> {
    match key {
        Value::Bool(b) => Ok(Some(Key::Bool(*b))),
        Value::Int(IntValue::Small(n)) => Ok(Some(Key::Int(*n))),
        Value::Int(IntValue::Big(_)) => Err(ConvertError::Overflow),
        Value::Str(s) => Ok(Some(Key::String(Arc::new(s.clone())))),
        Value::None | Value::Date(_) => {
            Ok(Some(Key::String(Arc::new(NULL_KEY_SENTINEL.to_string()))))
        }
        // A `float`/`datetime`/`bytes`/nested `list`/`dict` key keeps
        // its own, real CEL value in Python (`DoubleType`/
        // `TimestampType`/...), reachable through `in`, never
        // collapsing with a `None` key the way the two cases above do
        // — `cel::objects::Key` has no variant for any of them, so
        // this crate still drops the entry; a known, documented
        // divergence (`lib.rs`'s crate docs).
        _ => Ok(None),
    }
}

/// Converts *value* -- a `cel` tool expression's own result -- into JSON,
/// `plugins/cel.py::cel_to_json` ported to Rust (issue #723). `Err` is the
/// Python `ValueError`'s own message text, for the caller
/// ([`crate::evaluate_value`]) to wrap the way `evaluate_cel_value` does.
///
/// A timestamp becomes an ISO 8601 UTC string
/// (`2026-01-02T03:04:05Z`, with a fraction only when it has one) and a
/// duration an ISO 8601 duration (`PT1H30M`, `PT0.5S`, `-PT1M30S`),
/// matching [`cel_to_json`]'s own `_iso_timestamp`/`_iso_duration`. Bytes,
/// a non-finite double, and a `Struct`/`Opaque`/`Function` value (none of
/// which a `cel` tool expression evaluating against JSON-shaped `state`
/// can actually produce) have no JSON form and are `Err`, matching
/// `cel_to_json` exactly.
///
/// A map's own keys keep the order the expression gave them in -- a map
/// literal keeps its source order, and a map read straight from `state`
/// keeps `state`'s own order, the same way `celtypes.MapType` (a Python
/// `dict`, insertion-ordered) does on `cof run`: the vendored `cel` crate
/// (`vendor/cel`, `vendor/cel/VENDORING.md`) backs `cel::objects::Map` with
/// `indexmap::IndexMap` rather than the upstream crate's plain
/// `std::collections::HashMap`, so iterating `map.map` here is already in
/// that order, needing no sort of its own.
pub fn to_json(value: cel::Value) -> Result<Value, String> {
    Ok(match value {
        cel::Value::Null => Value::None,
        cel::Value::Bool(b) => Value::Bool(b),
        cel::Value::Int(n) => Value::from(n),
        cel::Value::UInt(n) => uint_to_value(n),
        cel::Value::Float(f) => {
            if f.is_finite() {
                Value::Float(f)
            } else {
                return Err(format!("{} is not a finite number", py_float_token(f)));
            }
        }
        cel::Value::String(s) => Value::Str((*s).clone()),
        cel::Value::Bytes(_) => {
            return Err("bytes have no JSON form; convert them with string()".to_string());
        }
        cel::Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                out.push(to_json(item.clone())?);
            }
            Value::List(out)
        }
        cel::Value::Map(map) => {
            let mut dict = Dict::new();
            for (key, item) in map.map.iter() {
                dict.insert(Value::Str(json_key_text(key)), to_json(item.clone())?);
            }
            Value::Dict(dict)
        }
        cel::Value::Timestamp(ts) => Value::Str(iso_timestamp(ts)),
        cel::Value::Duration(d) => Value::Str(iso_duration(d)),
        cel::Value::Struct(_) => return Err("a Struct has no JSON form".to_string()),
        cel::Value::Opaque(_) => return Err("an Opaque has no JSON form".to_string()),
        cel::Value::Function(..) => return Err("a Function has no JSON form".to_string()),
    })
}

/// `_json_key`: `"true"`/`"false"` for a bool key, else `str(int(key))` --
/// `celtypes.IntType`/`UintType` both subclass `int`, so a uint key
/// stringifies the same way an int one does.
fn json_key_text(key: &Key) -> String {
    match key {
        Key::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Key::Int(i) => i.to_string(),
        Key::Uint(u) => u.to_string(),
        Key::String(s) => (**s).clone(),
    }
}

/// `u64` -> JSON int, matching Python's plain `int(value)` on a
/// `celtypes.UintType` (which has no upper bound of its own beyond `u64`
/// here, since `cel::Value::UInt` already is one).
fn uint_to_value(n: u64) -> Value {
    match i64::try_from(n) {
        Ok(small) => Value::from(small),
        Err(_) => Value::from(
            IntValue::parse_decimal(&n.to_string()).expect("decimal digits always parse"),
        ),
    }
}

/// `nan`/`inf`/`-inf` -- Python's own `str(float("nan"))`/`str(float("inf"))`
/// text, used only inside [`to_json`]'s own non-finite error message
/// (`f"{number} is not a finite number"`); never reached for a finite
/// value, which [`to_json`] renders through [`Value::Float`] instead.
fn py_float_token(f: f64) -> String {
    if f.is_nan() {
        "nan".to_string()
    } else if f > 0.0 {
        "inf".to_string()
    } else {
        "-inf".to_string()
    }
}

/// `_iso_timestamp`: always converted to UTC first -- a `cel::Value::
/// Timestamp` is never naive (unlike `Value::DateTime`, which can be),
/// matching that every `celtypes.TimestampType` a CEL evaluation can
/// produce already carries `tzinfo` (`to_cel`'s own doc comment), so
/// Python's own `if value.tzinfo is not None` is always true by the time
/// `cel_to_json` reaches one.
fn iso_timestamp(ts: DateTime<FixedOffset>) -> String {
    use chrono::Timelike;
    let utc = ts.with_timezone(&Utc);
    let mut text = utc.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micro = utc.nanosecond() / 1_000;
    if micro != 0 {
        let frac = format!(".{micro:06}");
        text.push_str(frac.trim_end_matches('0'));
    }
    text.push('Z');
    text
}

/// `_iso_duration`.
fn iso_duration(d: chrono::Duration) -> String {
    let total_micros = d.num_microseconds().unwrap_or(i64::MAX);
    let sign = if total_micros < 0 { "-" } else { "" };
    let mut micros = total_micros.unsigned_abs();
    let hours = micros / (3_600 * 1_000_000);
    micros %= 3_600 * 1_000_000;
    let minutes = micros / (60 * 1_000_000);
    micros %= 60 * 1_000_000;
    let seconds = micros / 1_000_000;
    let fraction = micros % 1_000_000;
    let mut parts: Vec<String> = Vec::new();
    if hours != 0 {
        parts.push(format!("{hours}H"));
    }
    if minutes != 0 {
        parts.push(format!("{minutes}M"));
    }
    if seconds != 0 || fraction != 0 || parts.is_empty() {
        let mut second_text = seconds.to_string();
        if fraction != 0 {
            let frac = format!(".{fraction:06}");
            second_text.push_str(frac.trim_end_matches('0'));
        }
        parts.push(format!("{second_text}S"));
    }
    format!("{sign}PT{}", parts.join(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use electricity_value::Dict;

    #[test]
    fn none_maps_to_null() {
        assert_eq!(to_cel(&Value::None), Ok(cel::Value::Null));
    }

    #[test]
    fn bare_date_has_no_cel_counterpart() {
        let date = Value::Date(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        assert_eq!(to_cel(&date), Ok(cel::Value::Null));
    }

    #[test]
    fn naive_datetime_is_treated_as_utc() {
        let naive = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let aware = FixedOffset::east_opt(0).unwrap().from_utc_datetime(&naive);
        assert_eq!(
            to_cel(&Value::DateTime(naive, None)),
            Ok(cel::Value::Timestamp(aware))
        );
    }

    #[test]
    fn big_int_overflow_is_rejected_not_nulled() {
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        assert_eq!(to_cel(&Value::from(huge)), Err(ConvertError::Overflow));
    }

    #[test]
    fn dict_drops_a_float_key_with_no_cel_counterpart() {
        // Unlike `None`/a bare `date` key (collapsed into the null
        // sentinel below), a `float` key keeps its own real CEL value
        // in Python, which `cel::objects::Key` still has no variant
        // for -- a known, documented divergence this crate doesn't fix.
        let mut d: Dict = Dict::new();
        d.insert(Value::Str("a".into()), Value::from(1_i64));
        d.insert(Value::Float(2.5), Value::from(2_i64));
        let cel::Value::Map(map) = to_cel(&Value::Dict(d)).unwrap() else {
            panic!("expected a map");
        };
        assert_eq!(map.map.len(), 1);
    }

    #[test]
    fn none_and_date_keys_collapse_into_the_same_null_sentinel() {
        // Matches `_to_cel`'s own collapse: a plain Python dict
        // assignment with two keys that both convert to `None`
        // overwrites, leaving one entry holding the last value.
        let mut d: Dict = Dict::new();
        d.insert(Value::Str("a".into()), Value::from(1_i64));
        d.insert(Value::None, Value::from(2_i64));
        d.insert(
            Value::Date(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()),
            Value::from(3_i64),
        );
        let cel::Value::Map(map) = to_cel(&Value::Dict(d)).unwrap() else {
            panic!("expected a map");
        };
        assert_eq!(map.map.len(), 2);
        let sentinel = map
            .map
            .keys()
            .find(|k| matches!(k, Key::String(s) if s.as_str() == NULL_KEY_SENTINEL))
            .unwrap();
        assert_eq!(map.map.get(sentinel), Some(&cel::Value::Int(3)));
    }

    #[test]
    fn dict_key_overflow_raises_instead_of_dropping() {
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        let mut d: Dict = Dict::new();
        d.insert(Value::from(huge), Value::from(1_i64));
        assert_eq!(to_cel(&Value::Dict(d)), Err(ConvertError::Overflow));
    }
}
