//! Lane B: converts a loaded [`electricity_value::Value`] into the
//! `serde_json::Value` shape `electricity-schema` validates
//! (`structural.rs`'s JSON Schema step), keeping Python's verdicts for
//! dates, bytes, NaN/infinity, and non-string keys such as `yes:` or
//! `1:`.
//!
//! Python's `jsonschema` validates the document exactly as YAML/JSON
//! loaded it — including values JSON Schema's own type system has no
//! room for (a `datetime.date`/`datetime.datetime` from an unquoted
//! YAML timestamp, `bytes` from an explicit `!!binary` tag, a `NaN`/
//! `Infinity` float literal) and `dict` keys that aren't strings
//! (YAML's bare `yes:`/`1:` resolve to the key `True`/`1`, not the
//! string `"yes"`/`"1"`). None of those is a valid `serde_json::Value`
//! shape, so each needs a stand-in that preserves the *verdict*
//! `jsonschema`'s own Python `isinstance` checks (and, for `NaN`/
//! `Infinity`, its numeric comparisons) would reach, not a literal
//! rendering.
//!
//! ## Dates/bytes/NaN/infinity against a `"type": "object"` schema
//!
//! A `Date`/`DateTime`/`Bytes`/`NaN`/`Infinity` value is represented by
//! [`electricity_schema::non_json_scalar`] — a JSON object marker that,
//! on its own, would incorrectly *pass* a bare `"type": "object"`
//! position (`params`, a tool's `inputs`, a prompt's `schema`,
//! `interface`, `interface.inputs.<k>` all use it, and a bare YAML
//! scalar *can* reach every one of them) the same way Python's own
//! `isinstance(value, dict)` does *not*. `electricity-schema` fixes
//! this at validation time, not here: it overrides the `"type"`
//! keyword itself (`electricity-schema`'s own `type_keyword_factory`)
//! so it fails a marker against every declared type except — for a
//! number-like marker (`NaN`/`Infinity`/`-Infinity`, all three:
//! Python's own `isinstance(x, float)` is `True` for every one of
//! them) — `"number"` itself. The same crate also drops any
//! `"required"`/`"additionalProperties"`/other object-shape keyword
//! error a marker's own JSON-object encoding would otherwise spuriously
//! trigger (`orchestration_errors`'s own marker filter), matching
//! Python's `jsonschema`, which never runs those validators against a
//! non-`dict` instance in the first place.
//!
//! **Known divergence:** `minimum`/`maximum` against an `Infinity`/
//! `-Infinity` value still only vacuously pass (the marker isn't a
//! `serde_json::Number` at all, and `minimum`/`maximum` are no-ops on a
//! non-numeric instance per the JSON Schema spec), where Python's own
//! `float('inf')` comparisons would actually enforce a finite bound --
//! e.g. `threshold: .inf` (`minimum: 0`, `maximum: 1`, a bare `number`
//! position a YAML `Infinity` literal reaches directly) is a `maximum`
//! error in Python and passes here. This is a live, reachable gap, not
//! an inert one: representing `Infinity` as a real, arbitrary-precision
//! JSON number (`1e400`, which Rust's own `f64` parser overflows back
//! to `inf`) was tried and reverted, because the `jsonschema` crate's
//! own `minimum`/`maximum`/`type: integer` keywords call
//! `Number::as_f64().expect("Always valid")` on exactly this shape, and
//! `serde_json`'s `arbitrary_precision` feature makes `as_f64()` return
//! `None` -- not the overflowed `inf` -- for a number whose parsed
//! value isn't finite, so that representation panics inside the
//! third-party crate the moment such a document reaches
//! `minimum`/`maximum`/`type: integer` (the same panic
//! [`int_to_json_number`] now guards against for an out-of-range
//! integer literal), rather than producing a wrong-but-safe verdict.
//! Closing this gap for `Infinity` the same way would need
//! `minimum`/`maximum` overridden alongside `"type"`; left as a
//! documented divergence rather than done here.
//!
//! ## Non-string `Dict` keys
//!
//! A non-string key is rendered as a string carrying
//! [`electricity_schema::NON_STRING_KEY_PREFIX`] (a NUL byte, which
//! cannot appear in a YAML/JSON *text* key any real document writes)
//! followed by the key's own `repr()`-style text — this can never
//! collide with a legitimate schema property name, so a
//! `"properties"`/`"required"` check against the real (string) name
//! behaves exactly as Python's own `dict.get`/`in` would against a key
//! that compares unequal to every string (`True != "the_key"`, `1 !=
//! "the_key"`), while the pair itself still counts toward
//! `"additionalProperties"`/`"maxProperties"` the way Python's own dict
//! entry does. `electricity-schema`'s own `json_path_from_pointer`
//! decodes the marker back into Python's own `json_path` rendering for
//! an `int`/`bool` key (`1:` -> `[1]`, `yes:` -> `[True]`); for any
//! other hashable non-string key (a float, `None`, a date/datetime,
//! bytes), Python's own `json_path` raises `TypeError` instead, which
//! collapses the whole structural check into one error rather than
//! this location's own — not reproduced here (a narrow, documented
//! divergence); the location instead falls back to a non-empty,
//! best-effort quoted rendering of the key's own `repr()` text.

use electricity_schema::non_json_scalar;
use electricity_value::Value;
use serde_json::{Map, Number};

/// Converts *value* into the `serde_json::Value` shape
/// `electricity_schema::orchestration_errors`/`profile_errors` validate.
pub fn to_schema_instance(value: &Value) -> serde_json::Value {
    match value {
        Value::None => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => int_to_json_number(i),
        Value::Float(f) => float_to_json_number(*f),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(bytes) => {
            non_json_scalar(&format!("b{:?}", String::from_utf8_lossy(bytes)), false)
        }
        Value::Date(date) => non_json_scalar(&date.to_string(), false),
        Value::DateTime(..) => non_json_scalar(&value.py_str(), false),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(to_schema_instance).collect())
        }
        Value::Dict(dict) => {
            let mut map = Map::with_capacity(dict.len());
            for (key, child) in dict {
                let key_string = match key {
                    Value::Str(s) => s.clone(),
                    other => electricity_schema::non_string_key_marker(&other.py_repr()),
                };
                map.insert(key_string, to_schema_instance(child));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn int_to_json_number(i: &electricity_value::IntValue) -> serde_json::Value {
    use electricity_value::IntValue;
    match i {
        IntValue::Small(n) => serde_json::Value::Number(Number::from(*n)),
        IntValue::Big(big) => {
            let digits = big.to_string();
            match parse_arbitrary_precision_number(&digits) {
                // `serde_json`'s own `arbitrary_precision` `as_f64` (see
                // this module's docs) returns `None` for a parsed value
                // whose magnitude overflows `f64` to infinity -- and the
                // `jsonschema` crate's `minimum`/`maximum`/`type: integer`
                // keywords then call `.expect("Always valid")` on exactly
                // that `None` and panic. A number this large can't reach
                // `electricity-schema` any other way, so clamp instead of
                // handing it a shape that crate cannot represent: every
                // numeric bound Circuitry's bundled schema pairs with an
                // integer field is `0` or `1`, so a positive magnitude this
                // large is always past every such bound at `u64::MAX`, and a
                // negative one past every such bound at `i64::MIN` -- the
                // same side of the bound the real value is on, and still a
                // real JSON number (so `type: integer` still accepts it, as
                // Python's own `isinstance(x, int)` does regardless of
                // magnitude).
                Some(number) if number.as_f64().is_some() => serde_json::Value::Number(number),
                _ => serde_json::Value::Number(if digits.starts_with('-') {
                    Number::from(i64::MIN)
                } else {
                    Number::from(u64::MAX)
                }),
            }
        }
    }
}

/// A tiny shim around `serde_json::Number`'s own `FromStr` (via
/// `from_str`/`arbitrary_precision`, gated behind that crate feature
/// this crate enables) so an integer literal too large for `i64` still
/// becomes a real JSON number instance rather than always falling back
/// to [`non_json_scalar`] for it. Never used for `NaN`/`Infinity` --
/// see [`float_to_json_number`]'s own doc comment on why those stay
/// markers instead.
fn parse_arbitrary_precision_number(s: &str) -> Option<Number> {
    s.parse::<Number>().ok()
}

fn float_to_json_number(f: f64) -> serde_json::Value {
    if f.is_finite() {
        return serde_json::Value::Number(
            Number::from_f64(f).expect("a finite f64 is always a valid Number"),
        );
    }
    // `NaN`/`Infinity`/`-Infinity`: Python's own `isinstance(x, float)` is
    // `True` for every one of them, but no `serde_json::Number` can hold
    // any of the three without risking a panic inside the `jsonschema`
    // crate itself -- see the module docs' own "Known divergence"
    // paragraph for why an arbitrary-precision `1e400` literal was tried
    // for `Infinity` and reverted. All three get the same number-like
    // marker instead, which `electricity-schema`'s sibling keyword lets
    // back in only against `"type": "number"`.
    non_json_scalar(&f.to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use electricity_value::Dict;

    #[test]
    fn plain_scalars_round_trip() {
        assert_eq!(to_schema_instance(&Value::None), serde_json::Value::Null);
        assert_eq!(
            to_schema_instance(&Value::Bool(true)),
            serde_json::Value::Bool(true)
        );
        assert_eq!(to_schema_instance(&Value::from(5i64)), serde_json::json!(5));
        assert_eq!(
            to_schema_instance(&Value::Str("hi".to_string())),
            serde_json::json!("hi")
        );
    }

    #[test]
    fn a_date_is_not_a_json_string() {
        let date = Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let instance = to_schema_instance(&date);
        assert!(!instance.is_string());
        assert!(instance.is_object());
    }

    #[test]
    fn bytes_is_not_a_json_string() {
        let instance = to_schema_instance(&Value::Bytes(b"hi".to_vec()));
        assert!(!instance.is_string());
        assert!(instance.is_object());
    }

    #[test]
    fn non_string_dict_key_does_not_collide_with_a_real_property_name() {
        let mut dict = Dict::new();
        dict.insert(Value::Bool(true), Value::Str("x".to_string()));
        dict.insert(Value::Str("on".to_string()), Value::Str("y".to_string()));
        let instance = to_schema_instance(&Value::Dict(dict));
        let map = instance.as_object().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("on"), Some(&serde_json::json!("y")));
        assert!(
            map.keys()
                .any(|k| electricity_schema::is_non_string_key_marker(k) && k.ends_with("True"))
        );
    }

    #[test]
    fn big_integer_stays_a_json_number() {
        let big =
            electricity_value::IntValue::parse_decimal("123456789012345678901234567890").unwrap();
        let instance = to_schema_instance(&Value::Int(big));
        assert!(instance.is_number());
    }

    #[test]
    fn nan_is_not_a_json_number_but_the_marker_is_flagged_number_like() {
        let instance = to_schema_instance(&Value::Float(f64::NAN));
        assert!(!instance.is_number());
        let map = instance.as_object().unwrap();
        assert_eq!(
            map.get(electricity_schema::NON_JSON_SCALAR_NUMBER_LIKE),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn infinity_is_not_a_json_number_but_the_marker_is_flagged_number_like() {
        for value in [Value::Float(f64::INFINITY), Value::Float(f64::NEG_INFINITY)] {
            let instance = to_schema_instance(&value);
            assert!(!instance.is_number());
            let map = instance.as_object().unwrap();
            assert_eq!(
                map.get(electricity_schema::NON_JSON_SCALAR_NUMBER_LIKE),
                Some(&serde_json::json!(true))
            );
        }
    }
}
