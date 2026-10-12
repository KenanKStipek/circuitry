//! `validate_prompt_schema`'s own Python-`jsonschema`-exact message text
//! (issue #498 ruling 6, tightened by PR #502's review finding 2/orchestrator
//! ruling 2) -- `jsonschema.validate` raises `jsonschema.exceptions.best_match(
//! validator.iter_errors(instance))`, so matching it exactly needs both the
//! *selection* (`best_match`'s own relevance key) and the *text* (each
//! keyword's own `_keywords.py` message template, Python's `%r`/`str()`
//! formatting and all) to agree with Python's `jsonschema` 4.x.
//!
//! Only the keyword families [`python_message`] actually covers get this
//! treatment; everything else still falls back to the `jsonschema`-crate's
//! own `Display` text (`validate_prompt_schema`'s own fallback arm) -- see
//! this crate's own top-level docs and `DESIGN.md` §13 for the narrowed
//! known-divergence list that leaves.

use jsonschema::ValidationError;
use jsonschema::error::ValidationErrorKind;
use serde_json::Value as JsonValue;

use electricity_value::Value;

/// `jsonschema.exceptions.best_match`: Python's own relevance key is
/// `(-len(error.path), validator not in WEAK_MATCHES, validator in
/// STRONG_MATCHES)` (`STRONG_MATCHES` is always empty), picked with `max`,
/// which keeps the *first* maximal element when several tie -- unlike
/// Rust's own `Iterator::max_by_key`, which keeps the *last*. `path` here is
/// the error's own *instance* path (`error.path`, not `error.schema_path`),
/// matching Python.
///
/// The one piece of `best_match` this never replicates: once the winner is
/// an `anyOf`/`oneOf` failure, Python descends into `best.context` (the
/// losing branches' own sub-errors) with a second, `min`-based pass. The
/// `jsonschema` Rust crate's own `ValidationErrorKind::AnyOf`/`OneOfNotValid`/
/// `OneOfMultipleValid` carry no such sub-error list at all (`error.rs` in
/// that crate), so there is nothing to descend into -- an accepted,
/// documented gap (`DESIGN.md` §13), never reached by any of the keyword
/// families [`python_message`] covers.
pub fn best_match<'a>(
    errors: impl Iterator<Item = ValidationError<'a>>,
) -> Option<ValidationError<'a>> {
    let mut best: Option<ValidationError<'a>> = None;
    let mut best_key: Option<(i64, bool)> = None;
    for error in errors {
        let key = relevance(&error);
        let better = match best_key {
            None => true,
            Some(current) => key > current,
        };
        if better {
            best_key = Some(key);
            best = Some(error);
        }
    }
    best
}

fn relevance(error: &ValidationError<'_>) -> (i64, bool) {
    let depth = error.instance_path.as_str().split('/').skip(1).count();
    let weak = is_weak_keyword(&error.kind);
    (-(depth as i64), !weak)
}

fn is_weak_keyword(kind: &ValidationErrorKind) -> bool {
    matches!(
        kind,
        ValidationErrorKind::AnyOf
            | ValidationErrorKind::OneOfNotValid
            | ValidationErrorKind::OneOfMultipleValid
    )
}

/// `_keywords.py`'s own message template for *error*'s own keyword, with
/// Python's exact `%r`/`str()` formatting -- `None` for a keyword this
/// module doesn't cover (the caller falls back to the `jsonschema` crate's
/// own `Display` text for those).
pub fn python_message(error: &ValidationError<'_>, schema: &JsonValue) -> Option<String> {
    let instance = json_to_value(&error.instance);
    match &error.kind {
        ValidationErrorKind::Required { property } => Some(format!(
            "{} is a required property",
            json_to_value(property).py_repr()
        )),
        ValidationErrorKind::Type { kind } => {
            let reprs = type_reprs(kind);
            Some(format!("{} is not of type {reprs}", instance.py_repr()))
        }
        ValidationErrorKind::Enum { options } => Some(format!(
            "{} is not one of {}",
            instance.py_repr(),
            json_to_value(options).py_repr()
        )),
        ValidationErrorKind::Constant { expected_value } => Some(format!(
            "{} was expected",
            json_to_value(expected_value).py_repr()
        )),
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            let mut sorted = unexpected.clone();
            sorted.sort();
            let joined = sorted
                .iter()
                .map(|s| Value::Str(s.clone()).py_repr())
                .collect::<Vec<_>>()
                .join(", ");
            let verb = if sorted.len() == 1 { "was" } else { "were" };
            Some(format!(
                "Additional properties are not allowed ({joined} {verb} unexpected)"
            ))
        }
        ValidationErrorKind::MinItems { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "is too short"
            };
            Some(format!("{} {message}", instance.py_repr()))
        }
        ValidationErrorKind::MaxItems { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "is too long"
            };
            Some(format!("{} {message}", instance.py_repr()))
        }
        ValidationErrorKind::MinLength { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "is too short"
            };
            Some(format!("{} {message}", instance.py_repr()))
        }
        ValidationErrorKind::MaxLength { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "is too long"
            };
            Some(format!("{} {message}", instance.py_repr()))
        }
        ValidationErrorKind::Minimum { limit } => Some(format!(
            "{} is less than the minimum of {}",
            instance.py_repr(),
            json_to_value(limit).py_repr()
        )),
        ValidationErrorKind::Maximum { limit } => Some(format!(
            "{} is greater than the maximum of {}",
            instance.py_repr(),
            json_to_value(limit).py_repr()
        )),
        ValidationErrorKind::ExclusiveMinimum { limit } => Some(format!(
            "{} is less than or equal to the minimum of {}",
            instance.py_repr(),
            json_to_value(limit).py_repr()
        )),
        ValidationErrorKind::ExclusiveMaximum { limit } => Some(format!(
            "{} is greater than or equal to the maximum of {}",
            instance.py_repr(),
            json_to_value(limit).py_repr()
        )),
        ValidationErrorKind::MultipleOf { multiple_of } => {
            // `f"{instance!r} is not a multiple of {dB}"` uses Python's
            // `str(dB)`, not `repr`, but the two are identical for a
            // number -- except `multiple_of` here is always `f64`
            // (the Rust crate's own `ValidationErrorKind` shape), which
            // would render an int divisor with a spurious `.0` Python
            // never has (`multipleOf: 2` is `2`, not `2.0`). Reading the
            // original schema value back by its own `schema_path`
            // keeps the declared type.
            let divisor = schema_value_at(schema, error.schema_path.as_str())
                .map(json_to_value)
                .unwrap_or(Value::Float(*multiple_of));
            Some(format!(
                "{} is not a multiple of {}",
                instance.py_repr(),
                divisor.py_repr()
            ))
        }
        ValidationErrorKind::UniqueItems => {
            Some(format!("{} has non-unique elements", instance.py_repr()))
        }
        ValidationErrorKind::Pattern { pattern } => Some(format!(
            "{} does not match {}",
            instance.py_repr(),
            Value::Str(pattern.clone()).py_repr()
        )),
        _ => None,
    }
}

/// `type(validator, types, instance, schema)`'s own `", ".join(repr(type)
/// for type in types)` -- `ensure_list`'s own declared order for a single
/// type is trivial; for `type: [...]` (several types), the `jsonschema`
/// Rust crate's own bitmap has no declared-order memory (a known,
/// documented divergence for that one case -- `DESIGN.md` §13), so this
/// renders the bitmap's own fixed iteration order instead.
fn type_reprs(kind: &jsonschema::error::TypeKind) -> String {
    use jsonschema::error::TypeKind;
    match kind {
        TypeKind::Single(single) => format!("'{single}'"),
        TypeKind::Multiple(multiple) => (*multiple)
            .into_iter()
            .map(|t| format!("'{t}'"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Walks *schema* by a `jsonschema`-crate `Location`'s own JSON-Pointer
/// text (`/multipleOf`, `/properties/a/multipleOf`, ...), returning the
/// raw schema value at that point -- [`python_message`]'s own `MultipleOf`
/// arm uses this to recover a divisor's original declared type (see its
/// own doc comment).
fn schema_value_at<'a>(schema: &'a JsonValue, pointer: &str) -> Option<&'a JsonValue> {
    let mut node = schema;
    for segment in pointer.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        node = match node {
            JsonValue::Object(map) => map.get(segment.as_str())?,
            JsonValue::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(node)
}

/// *value* (a `serde_json::Value`, as every `ValidationErrorKind` field and
/// `ValidationError::instance` carry) rendered the way `py_repr` renders
/// the same data read back out of an `electricity_value::Dict`/`Value`
/// tree -- the inverse of `electricity-compiler::schema_instance::
/// to_schema_instance`. An object's own key order follows whatever this
/// workspace's `serde_json::Map` iterates in (a `BTreeMap`, alphabetical --
/// `electricity-vm/Cargo.toml`'s own dev-dependency comment explains why
/// `preserve_order` is never turned on workspace-wide), which can disagree
/// with Python's own insertion-order `dict` repr for a multi-key object;
/// every keyword family [`python_message`] covers only ever renders a
/// scalar, array or single-key object as `{instance!r}`, so this never
/// actually diverges for those, and is not claimed to for anything else.
fn json_to_value(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::None,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u as i64)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => Value::Str(s.clone()),
        JsonValue::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
        JsonValue::Object(map) => {
            let mut dict = electricity_value::Dict::new();
            for (k, v) in map {
                dict.insert(Value::Str(k.clone()), json_to_value(v));
            }
            Value::Dict(dict)
        }
    }
}
