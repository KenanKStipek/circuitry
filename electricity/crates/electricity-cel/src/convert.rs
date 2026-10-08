//! `electricity_value::Value` -> `cel::Value`, Circuitry's `_to_cel`
//! (`core/cel_eval.py`) ported to Rust (runtime-semantics.md §4.2).
//!
//! This is also the sandbox boundary: nothing converted here can carry a
//! live reference back into the caller's data beyond what CEL itself can
//! express, and anything with no CEL counterpart becomes CEL `null`.

use std::collections::HashMap;
use std::sync::Arc;

use cel::objects::{Key, Map as CelMap};
use chrono::{FixedOffset, TimeZone};
use electricity_value::{IntValue, Value};

/// Converts *value* into the CEL type system.
///
/// `Value::Date` (a bare `datetime.date`, not a `datetime.datetime`) has no
/// CEL counterpart, matching `_to_cel`, which only special-cases
/// `datetime.datetime`/`datetime.timedelta` and otherwise falls through to
/// `null` — a bare date is never one of those.
///
/// A `Value::Int` too large for `i64` also has no exact CEL counterpart:
/// CEL's `int` is a 64-bit signed integer by spec, unlike Python's
/// unbounded `int` (which is why `cel-python` tolerates it — a host-
/// language artifact, not CEL behavior the differential corpus can commit
/// to). It maps to `null` rather than silently truncating.
///
/// A `Value::DateTime` with no offset (naive, as PyYAML's timestamp
/// resolver produces for a timestamp with no explicit zone) is treated as
/// UTC. This is not a judgment call: `celtypes.TimestampType.__new__`
/// (`celpy`) does exactly this — `tzinfo=source.tzinfo or
/// datetime.timezone.utc` — so a naive and a UTC-aware datetime are
/// already the same CEL value by the time either reaches an expression;
/// there is no naive/aware split left to reproduce inside CEL itself (the
/// split in DESIGN.md §3.2 is `Value`'s own ordering, used outside CEL).
pub fn to_cel(value: &Value) -> cel::Value {
    match value {
        Value::None => cel::Value::Null,
        Value::Bool(b) => cel::Value::Bool(*b),
        Value::Int(i) => int_to_cel(i),
        Value::Float(f) => cel::Value::Float(*f),
        Value::Str(s) => cel::Value::String(Arc::new(s.clone())),
        Value::Bytes(b) => cel::Value::Bytes(Arc::new(b.clone())),
        Value::List(items) => cel::Value::List(Arc::new(items.iter().map(to_cel).collect())),
        Value::Dict(entries) => cel::Value::Map(dict_to_cel(entries)),
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
    }
}

fn int_to_cel(i: &IntValue) -> cel::Value {
    match i {
        IntValue::Small(n) => cel::Value::Int(*n),
        IntValue::Big(_) => cel::Value::Null,
    }
}

/// A `Value` dict key with no CEL [`Key`] counterpart (anything other than
/// `str`/`int`/`bool` — e.g. a `float`, `None`, or a nested `list`/`dict`
/// key) drops that entry rather than mapping it to some placeholder key:
/// CEL map keys must be `bool`/`int`/`uint`/`string`, and there is no
/// value in that set that represents "no counterpart" the way `null` does
/// for a value position. A real orchestration's `state` is keyed by
/// effect/namespace names (always strings) at every level this matters;
/// this only affects a key buried in user data read via `value`/`meta`.
fn dict_to_cel(entries: &electricity_value::Dict) -> CelMap {
    let mut map = HashMap::with_capacity(entries.len());
    for (key, value) in entries {
        if let Some(key) = to_cel_key(key) {
            map.insert(key, to_cel(value));
        }
    }
    CelMap { map: Arc::new(map) }
}

fn to_cel_key(key: &Value) -> Option<Key> {
    match key {
        Value::Bool(b) => Some(Key::Bool(*b)),
        Value::Int(IntValue::Small(n)) => Some(Key::Int(*n)),
        Value::Str(s) => Some(Key::String(Arc::new(s.clone()))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use electricity_value::Dict;

    #[test]
    fn none_maps_to_null() {
        assert_eq!(to_cel(&Value::None), cel::Value::Null);
    }

    #[test]
    fn bare_date_has_no_cel_counterpart() {
        let date = Value::Date(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        assert_eq!(to_cel(&date), cel::Value::Null);
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
            cel::Value::Timestamp(aware)
        );
    }

    #[test]
    fn big_int_has_no_cel_counterpart() {
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        assert_eq!(to_cel(&Value::from(huge)), cel::Value::Null);
    }

    #[test]
    fn dict_drops_keys_with_no_cel_counterpart() {
        let mut d: Dict = Dict::new();
        d.insert(Value::Str("a".into()), Value::from(1_i64));
        d.insert(Value::None, Value::from(2_i64));
        let cel::Value::Map(map) = to_cel(&Value::Dict(d)) else {
            panic!("expected a map");
        };
        assert_eq!(map.map.len(), 1);
    }
}
