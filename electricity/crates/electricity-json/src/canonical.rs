//! `core/saved_state.py::canonical_key_order`/`pretty_key_order`/
//! `_KEY_TYPE_RANK` (issue #778): two key orders, both needed on both
//! engines -- [`canonical_key_order`] for `electricity-vm`'s
//! reflector-plan digest (#810's own numeric-first order, restored
//! after `--pretty`'s own `sorted()`-first order briefly replaced it),
//! and [`pretty_key_order`] for [`crate::dumps`]'s `--pretty` callers
//! (`sorted()`-first, falling back to [`canonical_key_order`] only for
//! a dict mixing key types `sorted()` itself can't compare) -- shared
//! here instead of each engine (or, before #778, each crate) keeping
//! its own copy.
//!
//! `sort_keys=True`'s own native-value comparison (`sorted(dct.items())`,
//! [`crate::WriteError::IncomparableKeys`]) raises for a dict mixing key
//! types Python can't compare -- a YAML 1.1 `on:` key read as the bool
//! `True` beside a string key, say. [`canonical_key_order`] sorts by each
//! key's own JSON text instead (the same text [`crate::stringify_key`]
//! writes), with [`key_type_rank`] as an explicit, total tiebreak for two
//! keys whose text is the same (`"true"` written by both the bool `true`
//! and the string `"true"`) -- never insertion order, so the result is
//! the same whichever key happened to be inserted first.

use crate::{WriteError, stringify_key};
use electricity_value::{Dict, Value};
use std::cmp::Ordering;

/// `core/saved_state.py::_KEY_TYPE_RANK` -- the tiebreak order [`canonical_key_order`]
/// uses when two keys of different types write the same JSON text (e.g. the
/// string `"true"` beside the bool `true`) -- arbitrary but fixed, so the
/// order is total and the same on both engines.
pub fn key_type_rank(key: &Value) -> u8 {
    match key {
        Value::Bool(_) => 0,
        Value::Int(_) => 1,
        Value::Float(_) => 2,
        Value::None => 3,
        Value::Str(_) => 4,
        _ => 5,
    }
}

/// `core/saved_state.py::canonical_key_order` -- *value* with every dict's
/// entries reordered by the JSON text each key would write, then by
/// [`key_type_rank`] as a tiebreak -- so a dict mixing key types `<`
/// can't compare (e.g. a YAML 1.1 `on:` key read as the bool `true`
/// beside a string key) still sorts, deterministically, instead of
/// raising the way [`crate::dumps_default_str`] faithfully does for a
/// plain `sort_keys=true` write (#778; `yaml.dump`, which this
/// replaced for the reflector digest, tolerated it). For a dict whose
/// keys are all strings -- the realistic shape of a reflector's plan,
/// or of most saved state -- the JSON text of a key is the key itself
/// and no two keys ever tie, so this sorts exactly as `sort_keys=true`
/// always did.
///
/// The key's own text is computed *before* its type rank is looked up
/// ([`key_type_rank`] never fails), so a key of a type JSON can't write
/// at all (a `Bytes`/`Date`/`DateTime`/`List`/`Dict` key) still fails
/// with [`crate::stringify_key`]'s own `WriteError`, never a different
/// error from ranking it first.
pub fn canonical_key_order(value: &Value) -> Result<Value, WriteError> {
    match value {
        Value::Dict(dict) => {
            let mut entries: Vec<(Value, Value)> = Vec::with_capacity(dict.len());
            for (k, v) in dict.iter() {
                entries.push((k.clone(), canonical_key_order(v)?));
            }
            let mut out = Dict::new();
            for (k, v) in canonical_sort(entries)? {
                out.insert(k, v);
            }
            Ok(Value::Dict(out))
        }
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(canonical_key_order(item)?);
            }
            Ok(Value::List(out))
        }
        other => Ok(other.clone()),
    }
}

/// `true` iff every key in *entries* is a `Str` -- Python's `sorted()`
/// compares a dict's string keys directly (code-point order), with no
/// `TypeError` possible.
fn all_str_keys(entries: &[(Value, Value)]) -> bool {
    entries.iter().all(|(k, _)| matches!(k, Value::Str(_)))
}

/// `true` iff every key in *entries* is part of Python's numeric tower
/// (`bool`/`int`/`float`) -- `sorted()` compares these directly too,
/// numerically, `bool` counting as `0`/`1`.
fn all_numeric_keys(entries: &[(Value, Value)]) -> bool {
    entries
        .iter()
        .all(|(k, _)| matches!(k, Value::Bool(_) | Value::Int(_) | Value::Float(_)))
}

fn str_entries_sorted(mut entries: Vec<(Value, Value)>) -> Vec<(Value, Value)> {
    entries.sort_by(|(a, _), (b, _)| str_key(a).cmp(str_key(b)));
    entries
}

fn str_key(value: &Value) -> &str {
    match value {
        Value::Str(s) => s.as_str(),
        _ => unreachable!("all_str_keys guarantees Str keys"),
    }
}

fn numeric_entries_sorted(mut entries: Vec<(Value, Value)>) -> Vec<(Value, Value)> {
    entries.sort_by(|(a, _), (b, _)| numeric_cmp(a, b));
    entries
}

fn numeric_cmp(a: &Value, b: &Value) -> Ordering {
    a.py_partial_cmp(b)
        .ok()
        .flatten()
        .unwrap_or(Ordering::Equal)
}

/// [`canonical_key_order`]'s own entry sort, factored out so
/// [`pretty_key_order`] can fall back to it without re-deriving a
/// `Value::Dict` first.
fn canonical_sort(entries: Vec<(Value, Value)>) -> Result<Vec<(Value, Value)>, WriteError> {
    let mut keyed = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        let text = stringify_key(&k)?;
        let rank = key_type_rank(&k);
        keyed.push(((text, rank), (k, v)));
    }
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(keyed.into_iter().map(|(_, kv)| kv).collect())
}

/// `core/saved_state.py::pretty_key_order` -- *value* with every dict's
/// keys sorted by the keys themselves, exactly as plain
/// `sorted(d.items())`/`json.dumps(..., sort_keys=True)` already did: an
/// all-string dict sorts by code point, and an all-numeric dict (`bool`
/// counting as `0`/`1`) sorts by value, never by each key's own decimal
/// text (`"1"`, `"10"`, `"2"`).
///
/// Only when the keys are a mix [`canonical_key_order`]'s own types can
/// compare but Python's native `<` can't (e.g. a bool key beside a
/// string key) does this fall back to [`canonical_key_order`]'s order:
/// the JSON text each key would write, then [`key_type_rank`] as a
/// tiebreak. A key of a type JSON can't write at all (`Date`, say) still
/// fails the same way [`canonical_key_order`] does, via the same
/// [`stringify_key`] call.
///
/// Used only by `--pretty`'s own write (`out.rs::pretty_sort_if_needed`)
/// -- never by `electricity-vm`'s reflector-plan digest, which must keep
/// [`canonical_key_order`]'s own order so an existing plan's digest never
/// changes (#810).
pub fn pretty_key_order(value: &Value) -> Result<Value, WriteError> {
    match value {
        Value::Dict(dict) => {
            let mut entries: Vec<(Value, Value)> = Vec::with_capacity(dict.len());
            for (k, v) in dict.iter() {
                entries.push((k.clone(), pretty_key_order(v)?));
            }
            let ordered = pretty_entries_sorted(entries)?;
            let mut out = Dict::new();
            for (k, v) in ordered {
                out.insert(k, v);
            }
            Ok(Value::Dict(out))
        }
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(pretty_key_order(item)?);
            }
            Ok(Value::List(out))
        }
        other => Ok(other.clone()),
    }
}

fn pretty_entries_sorted(entries: Vec<(Value, Value)>) -> Result<Vec<(Value, Value)>, WriteError> {
    if all_str_keys(&entries) {
        return Ok(str_entries_sorted(entries));
    }
    if all_numeric_keys(&entries) {
        return Ok(numeric_entries_sorted(entries));
    }
    canonical_sort(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_a_bool_before_a_string_of_the_same_text_whatever_the_insertion_order() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("true".to_string()),
            Value::Str("string-inserted-first".to_string()),
        );
        dict.insert(
            Value::Bool(true),
            Value::Str("bool-inserted-second".to_string()),
        );
        let ordered = canonical_key_order(&Value::Dict(dict)).unwrap();
        let keys: Vec<&Value> = ordered.as_dict().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![&Value::Bool(true), &Value::Str("true".to_string())]
        );
    }

    #[test]
    fn a_date_key_fails_the_same_way_stringify_key_does_not_a_rank_lookup() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
            Value::Str("v".to_string()),
        );
        dict.insert(Value::Str("other".to_string()), Value::Str("w".to_string()));
        let err = canonical_key_order(&Value::Dict(dict)).unwrap_err();
        assert!(matches!(err, WriteError::KeyNotStrIntFloatBoolNone { .. }));
    }

    #[test]
    fn sorts_by_text_lexicographically_for_a_dict_of_only_string_keys() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("b".to_string()), Value::from(1i64));
        dict.insert(Value::Str("a".to_string()), Value::from(2i64));
        let ordered = canonical_key_order(&Value::Dict(dict)).unwrap();
        let keys: Vec<&Value> = ordered.as_dict().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![&Value::Str("a".to_string()), &Value::Str("b".to_string())]
        );
    }
}
