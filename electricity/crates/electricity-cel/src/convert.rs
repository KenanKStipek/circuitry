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
//! is a `cel`-crate-owned structure already bounded by
//! `nesting::MAX_NESTING_DEPTH` -- not just its *bracket* nesting, but
//! every binary-operator and member/index/call chain within it too
//! (`nesting.rs`'s own module docs explain why bracket nesting alone
//! was not enough).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use cel::objects::{Key, Map as CelMap};
use chrono::{FixedOffset, TimeZone};
use electricity_value::{IntValue, Value};

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
    /// reachable from a run-time-built `Value` (a CEL evaluation result,
    /// a future state merge or loop that wraps a value).
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
/// to measure it.
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
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(to_cel_at_depth(item, depth + 1)?);
            }
            cel::Value::List(Arc::new(out))
        }
        Value::Dict(entries) => cel::Value::Map(dict_to_cel(entries, depth)?),
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

/// A `Value` dict key with no CEL [`Key`] counterpart (anything other than
/// `str`/`int`/`bool` — e.g. a `float`, `None`, or a nested `list`/`dict`
/// key) drops that entry rather than mapping it to some placeholder key:
/// CEL map keys must be `bool`/`int`/`uint`/`string`, and there is no
/// value in that set that represents "no counterpart" the way `null` does
/// for a value position. This is a known divergence from Circuitry's own
/// `_to_cel`, documented (with its reachability — a plain YAML date key
/// under `state.input` triggers it, not just data buried in `value`/
/// `meta`) in `lib.rs`'s crate docs. A key too large for `i64` raises
/// [`ConvertError::Overflow`] instead of being dropped, matching every
/// other big-int read.
fn dict_to_cel(entries: &electricity_value::Dict, depth: usize) -> Result<CelMap, ConvertError> {
    let mut map = HashMap::with_capacity(entries.len());
    for (key, value) in entries {
        if let Some(key) = to_cel_key(key)? {
            map.insert(key, to_cel_at_depth(value, depth + 1)?);
        }
    }
    Ok(CelMap { map: Arc::new(map) })
}

fn to_cel_key(key: &Value) -> Result<Option<Key>, ConvertError> {
    match key {
        Value::Bool(b) => Ok(Some(Key::Bool(*b))),
        Value::Int(IntValue::Small(n)) => Ok(Some(Key::Int(*n))),
        Value::Int(IntValue::Big(_)) => Err(ConvertError::Overflow),
        Value::Str(s) => Ok(Some(Key::String(Arc::new(s.clone())))),
        _ => Ok(None),
    }
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
    fn dict_drops_keys_with_no_cel_counterpart() {
        let mut d: Dict = Dict::new();
        d.insert(Value::Str("a".into()), Value::from(1_i64));
        d.insert(Value::None, Value::from(2_i64));
        let cel::Value::Map(map) = to_cel(&Value::Dict(d)).unwrap() else {
            panic!("expected a map");
        };
        assert_eq!(map.map.len(), 1);
    }

    #[test]
    fn dict_key_overflow_raises_instead_of_dropping() {
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        let mut d: Dict = Dict::new();
        d.insert(Value::from(huge), Value::from(1_i64));
        assert_eq!(to_cel(&Value::Dict(d)), Err(ConvertError::Overflow));
    }
}
