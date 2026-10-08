//! Lane B: converts a loaded [`electricity_value::Value`] into the
//! `serde_json::Value` shape `electricity-schema` validates
//! (`structural.rs`'s JSON Schema step), keeping Python's verdicts for
//! dates, bytes, and non-string keys such as `yes:` or `1:`.
//!
//! Python's `jsonschema` validates the document exactly as YAML/JSON
//! loaded it — including values JSON Schema's own type system has no
//! room for (a `datetime.date`/`datetime.datetime` from an unquoted
//! YAML timestamp, `bytes` from an explicit `!!binary` tag) and `dict`
//! keys that aren't strings (YAML's bare `yes:`/`1:` resolve to the
//! key `True`/`1`, not the string `"yes"`/`"1"`). None of those is a
//! valid `serde_json::Value` shape, so each needs a stand-in that
//! preserves the *verdict* `jsonschema`'s own Python `isinstance`
//! checks would reach, not a literal rendering.
//!
//! ## Known divergence: dates/bytes against a `"type": "object"` schema
//!
//! A `Date`/`DateTime`/`Bytes` value is represented as a JSON object
//! carrying [`NON_JSON_SCALAR_MARKER`] (see [`non_json_scalar`]) — this
//! correctly fails every `"type"` keyword Circuitry's bundled schemas
//! actually use a bare scalar position for (`"string"`, `"number"`,
//! `"integer"`, `"boolean"`, `"array"`, `"null"`), matching Python's own
//! `isinstance` verdict, which is false for every one of those against
//! a `datetime.date`/`bytes` object. It does *not* fail `"type":
//! "object"` the same way (Python's `isinstance(value, dict)` is also
//! `False` there, but this representation, itself a JSON object,
//! passes). No field in `schema/orchestration.schema.json` applies
//! `"type": "object"` at a position a bare YAML scalar like a date can
//! reach (every `"object"`-typed field is itself a mapping in the
//! schema's own structure, e.g. `interface`, `retries`, `asset`), so
//! this is a documented, inert gap rather than a live one.
//!
//! ## Non-string `Dict` keys
//!
//! A non-string key is rendered as a string carrying
//! [`NON_STRING_KEY_PREFIX`] (a NUL byte, which cannot appear in a
//! YAML/JSON *text* key any real document writes) followed by the
//! key's own `repr()`-style text — this can never collide with a
//! legitimate schema property name, so a `"properties"`/`"required"`
//! check against the real (string) name behaves exactly as Python's
//! own `dict.get`/`in` would against a key that compares unequal to
//! every string (`True != "the_key"`, `1 != "the_key"`), while the
//! pair itself still counts toward `"additionalProperties"`/
//! `"maxProperties"` the way Python's own dict entry does.

use electricity_value::Value;
use serde_json::{Map, Number};

/// The marker key [`non_json_scalar`] wraps a `Date`/`DateTime`/`Bytes`
/// value's text under — see the module docs' "Known divergence" section.
const NON_JSON_SCALAR_MARKER: &str = "$circuitry_non_json_scalar";

/// Prefixes a non-string `Dict` key's stand-in string — see the module
/// docs' "Non-string `Dict` keys" section. A NUL byte can appear in a
/// Python `str` (and so, in principle, a YAML/JSON string key), but
/// `electricity-yaml`/`electricity-json` both reject a raw control
/// character (`reject_non_printable`/`Invalid control character`)
/// everywhere outside a `\u0000`-style escape — Circuitry's own
/// documents never carry one unescaped, and the one path that does
/// reach this far (an explicit YAML `!!binary`/escaped-NUL text key)
/// is itself exotic enough that a false "looks like a marker" collision
/// is not a realistic document, only a contrived one.
const NON_STRING_KEY_PREFIX: &str = "\u{0}non_string_key:";

/// Converts *value* into the `serde_json::Value` shape
/// `electricity_schema::orchestration_errors`/`profile_errors` validate.
pub fn to_schema_instance(value: &Value) -> serde_json::Value {
    match value {
        Value::None => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => int_to_json_number(i),
        Value::Float(f) => float_to_json_number(*f),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(bytes) => non_json_scalar(&format!("b{:?}", String::from_utf8_lossy(bytes))),
        Value::Date(date) => non_json_scalar(&date.to_string()),
        Value::DateTime(..) => non_json_scalar(&value.py_str()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(to_schema_instance).collect())
        }
        Value::Dict(dict) => {
            let mut map = Map::with_capacity(dict.len());
            for (key, child) in dict {
                let key_string = match key {
                    Value::Str(s) => s.clone(),
                    other => format!("{NON_STRING_KEY_PREFIX}{}", other.py_repr()),
                };
                map.insert(key_string, to_schema_instance(child));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// `true` iff *key* is the stand-in for a `Dict` key that wasn't a
/// `Value::Str` — i.e. it can never equal a legitimate schema property
/// name (see [`NON_STRING_KEY_PREFIX`]'s doc comment). Exposed so
/// `structural.rs`'s own unknown-key walk (which reads the schema
/// instance's own `Map` back as a lookup table) can tell a non-string
/// key's slot apart from a string key spelled unusually.
pub fn is_non_string_key_marker(key: &str) -> bool {
    key.starts_with(NON_STRING_KEY_PREFIX)
}

fn non_json_scalar(label: &str) -> serde_json::Value {
    let mut map = Map::with_capacity(1);
    map.insert(
        NON_JSON_SCALAR_MARKER.to_string(),
        serde_json::Value::String(label.to_string()),
    );
    serde_json::Value::Object(map)
}

fn int_to_json_number(i: &electricity_value::IntValue) -> serde_json::Value {
    use electricity_value::IntValue;
    match i {
        IntValue::Small(n) => serde_json::Value::Number(Number::from(*n)),
        IntValue::Big(big) => match Number::from_str_f64_safe(&big.to_string()) {
            Some(number) => serde_json::Value::Number(number),
            None => non_json_scalar(&big.to_string()),
        },
    }
}

/// `Number::from_str_f64_safe`: a tiny local shim around
/// `serde_json::Number`'s own `FromStr` (via `from_str`/`arbitrary_
/// precision`, gated behind that crate feature this crate enables) so
/// an integer literal too large for `i64` still becomes a real JSON
/// number instance (preserving Python's `isinstance(x, int)` ==
/// `"type": "integer"`/`"number"` verdict) rather than always falling
/// back to [`non_json_scalar`] for it.
trait NumberExt {
    fn from_str_f64_safe(s: &str) -> Option<Number>;
}

impl NumberExt for Number {
    fn from_str_f64_safe(s: &str) -> Option<Number> {
        s.parse::<Number>().ok()
    }
}

fn float_to_json_number(f: f64) -> serde_json::Value {
    // NaN/+-Infinity have no JSON Schema "number" representation either
    // (Python's own `json.dumps` already can't round-trip them without
    // its own `allow_nan` extension) -- `jsonschema`'s `isinstance(x,
    // float)` is still `True` for them in Python, but there is no
    // `serde_json::Number` for a non-finite value, so this is the one
    // place a genuinely numeric value still falls back to the
    // non-JSON-scalar marker. No field in Circuitry's bundled schemas
    // distinguishes "number" from "no type constraint" at a position a
    // `NaN`/`Infinity` literal could reach, so -- like the "Known
    // divergence" above -- this is inert in practice, not a live gap.
    match Number::from_f64(f) {
        Some(number) => serde_json::Value::Number(number),
        None => non_json_scalar(&f.to_string()),
    }
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
                .any(|k| is_non_string_key_marker(k) && k.ends_with("True"))
        );
    }

    #[test]
    fn big_integer_stays_a_json_number() {
        let big =
            electricity_value::IntValue::parse_decimal("123456789012345678901234567890").unwrap();
        let instance = to_schema_instance(&Value::Int(big));
        assert!(instance.is_number());
    }
}
