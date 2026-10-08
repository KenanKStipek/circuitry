//! Lane C: the Python coercions `core/compiler.py` applies to a raw
//! document value -- `int(...)`, `float(...)`, `str()`, truthiness, and
//! the `a or b or default` truthy-chaining idiom it uses throughout for
//! a field with a fallback -- plus `on_error:` normalization (an
//! invalid value becomes `"fail"`/`"break"`/etc., never a compile
//! error).

use crate::state_ns::is_truthy;
use electricity_value::{Dict, Value};
use num_traits::ToPrimitive;

/// *dict*'s raw value at *key*, if present *and* Python-truthy --
/// `effect.get(key)` under an `or` test.
pub(crate) fn get_truthy<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
        .filter(|v| is_truthy(v))
}

/// The first of *candidates* that is present and Python-truthy --
/// `core/compiler.py`'s `effect.get("a") or effect.get("b")` idiom,
/// generalized to any number of fallbacks.
pub(crate) fn first_truthy<'a>(candidates: &[Option<&'a Value>]) -> Option<&'a Value> {
    candidates.iter().flatten().find(|v| is_truthy(v)).copied()
}

/// *dict*'s raw value at *key* if present and not `None` --
/// `effect.get(key) is not None`, never filtered by truthiness the
/// way [`get_truthy`] is. Ports the handful of fields `core/
/// compiler.py` checks with `is not None` rather than its usual
/// `or`-chained-default idiom: `0`/`0.0`/an empty value is a real,
/// meaningful setting for these, not "absent" (`timeout_ms: 0`, a
/// loop's `max_iterations: 0`, `max_concurrency: 0`).
pub(crate) fn get_present<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    match dict.get(&Value::Str(key.to_string())) {
        Some(Value::None) | None => None,
        Some(value) => Some(value),
    }
}

/// Python's `bool(effect.get(key, default))` truthiness, for a flag
/// whose Python default is itself a literal `True`/`False`
/// (`effect.get("stop_on_error", False)`, `effect.get("validate",
/// True)`) rather than the `or`-chained-default idiom above.
pub(crate) fn get_bool_default(dict: &Dict, key: &str, default: bool) -> bool {
    match dict.get(&Value::Str(key.to_string())) {
        Some(value) => is_truthy(value),
        None => default,
    }
}

/// Python's `int(value)`, best-effort: a `bool`/`int`/`float` converts
/// the way CPython's `int()` does; a `str` parses as a base-10 integer
/// (CPython's leniency around whitespace/underscores/arbitrary
/// precision is not reproduced, matching `electricity-template`'s own
/// documented `int()` divergence); anything else, or a string that
/// doesn't parse, is `0` -- schema validation (lane B) has already
/// rejected a document whose own schema requires an integer here; a
/// value that reaches this coercion with the wrong Python type at all
/// is already outside what `cof check` would have accepted, so `0` is
/// a safe, never-panicking fallback rather than Python's own (uncaught,
/// crashing) `TypeError`.
pub(crate) fn py_int(value: &Value) -> i64 {
    match value {
        Value::Bool(b) => i64::from(*b),
        Value::Int(i) => i.to_bigint().to_i64().unwrap_or(i64::MAX),
        Value::Float(f) => *f as i64,
        Value::Str(s) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// Python's `float(value)`, best-effort -- see [`py_int`]'s own notes
/// on why a non-parsing/wrong-type input safely falls back to `0.0`
/// rather than reproducing Python's crashing `TypeError`.
pub(crate) fn py_float(value: &Value) -> f64 {
    match value {
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Int(i) => i.to_f64(),
        Value::Float(f) => *f,
        Value::Str(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `str(effect.get(key) or default).strip().lower()`, narrowed to
/// *allowed*: an invalid/unrecognized value becomes *fallback* --
/// Python's own `on_error`/`flow`-shaped coercions, each of which picks
/// a safe default rather than raising on a bad value (`core/
/// compiler.py`'s `on_error_raw = str(effect.get("on_error") or
/// "fail").strip().lower()` pattern, repeated per effect type with a
/// different *allowed* set).
pub(crate) fn normalize_choice(
    dict: &Dict,
    key: &str,
    default: &str,
    allowed: &[&str],
    fallback: &str,
) -> String {
    let raw = get_truthy(dict, key)
        .map(Value::py_str)
        .unwrap_or_else(|| default.to_string());
    let normalized = raw.trim().to_lowercase();
    if allowed.contains(&normalized.as_str()) {
        normalized
    } else {
        fallback.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_with(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    #[test]
    fn get_truthy_skips_falsy_values() {
        let dict = dict_with(vec![
            ("a", Value::Int(0.into())),
            ("b", Value::Str("x".into())),
        ]);
        assert_eq!(get_truthy(&dict, "a"), None);
        assert_eq!(get_truthy(&dict, "b"), Some(&Value::Str("x".to_string())));
        assert_eq!(get_truthy(&dict, "missing"), None);
    }

    #[test]
    fn normalize_choice_falls_back_on_invalid_value() {
        let dict = dict_with(vec![("on_error", Value::Str("bogus".into()))]);
        assert_eq!(
            normalize_choice(
                &dict,
                "on_error",
                "fail",
                &["fail", "skip", "continue"],
                "fail"
            ),
            "fail"
        );
    }

    #[test]
    fn normalize_choice_accepts_a_valid_value_case_insensitively() {
        let dict = dict_with(vec![("on_error", Value::Str(" Skip ".into()))]);
        assert_eq!(
            normalize_choice(
                &dict,
                "on_error",
                "fail",
                &["fail", "skip", "continue"],
                "fail"
            ),
            "skip"
        );
    }

    #[test]
    fn normalize_choice_uses_default_when_absent_or_falsy() {
        let dict = dict_with(vec![("on_error", Value::Str(String::new()))]);
        assert_eq!(
            normalize_choice(
                &dict,
                "on_error",
                "fail",
                &["fail", "skip", "continue"],
                "fail"
            ),
            "fail"
        );
    }

    #[test]
    fn py_int_truncates_a_float_toward_zero() {
        assert_eq!(py_int(&Value::Float(4.9)), 4);
        assert_eq!(py_int(&Value::Float(-4.9)), -4);
    }

    #[test]
    fn py_float_converts_bool_and_int() {
        assert_eq!(py_float(&Value::Bool(true)), 1.0);
        assert_eq!(py_float(&Value::Int(3.into())), 3.0);
    }
}
