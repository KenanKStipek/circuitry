//! Lane B: `structural_errors`/`unknown_key_warnings`, porting
//! `core/document_check.py`'s structural pass: near-miss unknown keys
//! ([`crate::difflib`]), JSON Schema through [`crate::schema_instance`],
//! `group:` placement, and `interface.inputs` checks.
//!
//! ## Known divergence: multi-error schema ordering
//!
//! Circuitry's own `schema_errors` sorts multiple simultaneous schema
//! violations by `str(err)` on the raw Python `jsonschema.
//! ValidationError` -- third-party text from a different JSON Schema
//! implementation than the Rust `jsonschema` crate this workspace uses,
//! so that exact order cannot be reproduced here (`electricity-schema`'s
//! own crate docs note the same gap for `cli.profiles`' error list, for
//! the identical reason). [`schema_errors`] instead sorts by
//! `(location, message)`, a stable, deterministic order of its own --
//! every individual error's own text and location still match
//! Circuitry's own `_describe_schema_error` output exactly (that part is
//! Circuitry's own format, not third-party), only the *relative order*
//! of two or more simultaneous violations can differ.

use crate::difflib;
use crate::schema_instance::to_schema_instance;
use electricity_value::Value;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// `core/document_check.py::_LEAF_EFFECT_TYPES`.
const LEAF_EFFECT_TYPES: [&str; 2] = ["tool", "prompt"];

/// `core/document_check.py::_EFFECT_DEFS`: effect type -> the schema
/// definition listing its keys.
const EFFECT_DEFS: [(&str, &str); 9] = [
    ("prompt", "PromptEffect"),
    ("dynamic", "DynamicEffect"),
    ("if", "ConditionalEffect"),
    ("conditional", "ConditionalEffect"),
    ("loop", "LoopEffect"),
    ("reflector", "ReflectorEffect"),
    ("tool", "ToolEffect"),
    ("use", "UseEffect"),
    ("yield", "YieldEffect"),
];

/// `core/document_check.py::_LEGACY_CONTAINER_KEYS`.
const LEGACY_CONTAINER_KEYS: [&str; 2] = ["steps", "strategy"];

/// `core/document_check.py::_EXTRA_TOP_LEVEL_KEYS`.
const EXTRA_TOP_LEVEL_KEYS: [&str; 5] = ["description", "runtime", "plugins", "steps", "strategy"];

/// `core/document_check.py::_MISTAKEN_FOR`.
const MISTAKEN_FOR: [(&str, &str); 1] = [("adapter", "provider")];

/// `core/document_check.py::_CHILD_KEYS`.
const CHILD_KEYS: [&str; 6] = ["effects", "steps", "then", "else", "body", "finally"];

/// `core/interface_inputs.py::_TYPE_NAMES`.
pub(crate) const INTERFACE_TYPE_NAMES: [&str; 6] =
    ["string", "number", "integer", "boolean", "array", "object"];

fn known_effect_keys(effect_type: &str) -> BTreeSet<String> {
    let def_name = EFFECT_DEFS
        .iter()
        .find(|(name, _)| *name == effect_type)
        .map(|(_, def)| *def)
        .expect("caller already checked effect_type is in EFFECT_DEFS");
    let mut keys = electricity_schema::orchestration_def_properties(def_name)
        .unwrap_or_else(|| panic!("bundled schema has no $defs.{def_name}"));
    if effect_type == "dynamic" || effect_type == "reflector" {
        keys.extend(LEGACY_CONTAINER_KEYS.iter().map(|s| s.to_string()));
    }
    keys
}

static TOP_LEVEL_KNOWN_KEYS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    let mut keys = electricity_schema::orchestration_top_level_properties();
    keys.extend(EXTRA_TOP_LEVEL_KEYS.iter().map(|s| s.to_string()));
    keys
});

/// `core/document_check.py::_near_miss`: the known key *key* was
/// probably meant to be, or `None`.
fn near_miss(key: &str, known: &BTreeSet<String>) -> Option<String> {
    let normalized = key.trim().to_lowercase().replace('-', "_");
    if known.contains(&normalized) {
        return Some(normalized);
    }
    if let Some((_, mistaken_for)) = MISTAKEN_FOR.iter().find(|(k, _)| *k == normalized) {
        if known.contains(*mistaken_for) {
            return Some(mistaken_for.to_string());
        }
    }
    let sorted: Vec<String> = known.iter().cloned().collect(); // BTreeSet already sorts ascending.
    difflib::get_close_match(&normalized, &sorted, 0.8).map(|s| s.to_string())
}

/// `type(x).__name__` -- the *unqualified* class name Python's own
/// `__name__` attribute gives, unlike [`Value::type_name`] (shared
/// across crates for other purposes, e.g. a comparison-type error),
/// which spells a date/datetime's own name as `"datetime.date"`/
/// `"datetime.datetime"` -- the module-qualified form `__name__` never
/// produces.
pub(crate) fn py_class_name(value: &Value) -> &'static str {
    match value {
        Value::Date(_) => "date",
        Value::DateTime(..) => "datetime",
        other => other.type_name(),
    }
}

/// `str(key)` as `document_check.py`'s `report()` renders it: `repr(key)`
/// for a string key, or `f"{key!r} (YAML read the unquoted key as a
/// {type(key).__name__})"` for anything else.
fn key_label(key: &Value) -> String {
    match key {
        Value::Str(_) => key.py_repr(),
        other => format!(
            "{} (YAML read the unquoted key as a {})",
            other.py_repr(),
            py_class_name(other)
        ),
    }
}

/// One entry of `_unknown_keys`'s `report()`: appends to *errors* when
/// *key* is a near miss of a known key, to *warnings* otherwise.
fn report_unknown_key(
    key: &Value,
    known: &BTreeSet<String>,
    where_: &str,
    owner: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let label = key_label(key);
    let near = match key.as_str() {
        Some(s) => near_miss(s, known),
        None => None,
    };
    match near {
        Some(matched) => errors.push(format!(
            "{where_}: unknown key {label} on {owner} — did you mean '{matched}'? \
             As written it is ignored."
        )),
        None => {
            let known_sorted: Vec<&String> = known.iter().collect();
            let known_list = known_sorted
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "{where_}: unknown key {label} on {owner} is ignored. Known keys: {known_list}."
            ));
        }
    }
}

/// `effect.get("type")`, normalized the way `_unknown_keys`'s/`group_
/// field_errors`'s walk both normalize it: a `str`, stripped and
/// lowercased, or `""` for anything else (including a missing `type`).
fn effect_type_of(effect: &Value) -> String {
    effect
        .as_dict()
        .and_then(|d| d.get(&Value::Str("type".to_string())))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default()
}

fn is_known_effect_type(effect_type: &str) -> bool {
    EFFECT_DEFS.iter().any(|(name, _)| *name == effect_type)
}

/// `orch.get("effects")`, falling back to `orch.get("steps")` only when
/// `effects` is *absent or `None`* (`document_check.py`'s `effects if
/// effects is not None else orch.get("steps")`) -- an explicit
/// `effects:` (YAML's bare key with nothing after it, or a JSON `null`)
/// is `None`, not merely absent, and must fall back the same way;
/// `effects: []` must not, since `[]` is not `None`.
fn effects_or_steps(dict: &electricity_value::Dict) -> Option<&Value> {
    match dict.get(&Value::Str("effects".to_string())) {
        None | Some(Value::None) => dict.get(&Value::Str("steps".to_string())),
        some => some,
    }
}

fn walk_unknown_keys(
    effects: Option<&Value>,
    path: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let Some(Value::List(items)) = effects else {
        return;
    };
    for (index, effect) in items.iter().enumerate() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        let here = format!("{path}[{index}]");
        let effect_type = effect_type_of(effect);
        if is_known_effect_type(&effect_type) {
            let known = known_effect_keys(&effect_type);
            for key in dict.keys() {
                let key_str = key.as_str();
                let is_known = key_str.is_some_and(|s| known.contains(s));
                if !is_known {
                    report_unknown_key(
                        key,
                        &known,
                        &here,
                        &format!("a '{effect_type}' effect"),
                        errors,
                        warnings,
                    );
                }
            }
        }
        for child_key in CHILD_KEYS {
            walk_unknown_keys(
                dict.get(&Value::Str(child_key.to_string())),
                &format!("{here}.{child_key}"),
                errors,
                warnings,
            );
        }
    }
}

fn unknown_keys(document: &Value) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(dict) = document.as_dict() else {
        return (errors, warnings);
    };

    for key in dict.keys() {
        let key_str = key.as_str();
        let is_known = key_str.is_some_and(|s| TOP_LEVEL_KNOWN_KEYS.contains(s));
        if !is_known {
            report_unknown_key(
                key,
                &TOP_LEVEL_KNOWN_KEYS,
                "top level",
                "the document",
                &mut errors,
                &mut warnings,
            );
        }
    }

    let effects = effects_or_steps(dict);
    walk_unknown_keys(effects, "effects", &mut errors, &mut warnings);
    walk_unknown_keys(
        dict.get(&Value::Str("finally".to_string())),
        "finally",
        &mut errors,
        &mut warnings,
    );

    (errors, warnings)
}

/// `core/document_check.py::unknown_key_errors`.
pub fn unknown_key_errors(document: &Value) -> Vec<String> {
    unknown_keys(document).0
}

/// `core/document_check.py::unknown_key_warnings`.
pub fn unknown_key_warnings(document: &Value) -> Vec<String> {
    unknown_keys(document).1
}

/// `core/document_check.py::schema_errors` -- see this module's own
/// "Known divergence" doc comment on multi-error ordering.
pub fn schema_errors(document: &Value) -> Vec<String> {
    let instance = to_schema_instance(document);
    let mut errors = electricity_schema::orchestration_errors(&instance);
    errors.sort_by(|a, b| (&a.location, &a.message).cmp(&(&b.location, &b.message)));
    errors.into_iter().map(|e| e.describe()).collect()
}

/// `core/document_check.py::group_field_errors`.
pub fn group_field_errors(document: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(dict) = document.as_dict() else {
        return errors;
    };
    let effects = effects_or_steps(dict);
    walk_group_fields(effects, "effects", &mut errors);
    walk_group_fields(
        dict.get(&Value::Str("finally".to_string())),
        "finally",
        &mut errors,
    );
    errors
}

fn walk_group_fields(effects: Option<&Value>, path: &str, errors: &mut Vec<String>) {
    let Some(Value::List(items)) = effects else {
        return;
    };
    for (index, effect) in items.iter().enumerate() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        let here = format!("{path}[{index}]");
        let effect_type = effect_type_of(effect);
        if dict.contains_key(&Value::Str("group".to_string()))
            && is_known_effect_type(&effect_type)
            && !LEAF_EFFECT_TYPES.contains(&effect_type.as_str())
        {
            errors.push(format!(
                "{here}: 'group' is only allowed on tool/prompt effects (it names a \
                 concurrency-group slot a leaf effect holds at dispatch) — found on a \
                 '{effect_type}' effect."
            ));
        }
        for child_key in CHILD_KEYS {
            walk_group_fields(
                dict.get(&Value::Str(child_key.to_string())),
                &format!("{here}.{child_key}"),
                errors,
            );
        }
    }
}

/// `core/interface_inputs.py::_matches_type`.
pub(crate) fn matches_type(value: &Value, declared_type: &str) -> bool {
    match declared_type {
        "string" => matches!(value, Value::Str(_)),
        "number" => matches!(value, Value::Int(_) | Value::Float(_)),
        "integer" => matches!(value, Value::Int(_)),
        "boolean" => matches!(value, Value::Bool(_)),
        "array" => matches!(value, Value::List(_)),
        "object" => matches!(value, Value::Dict(_)),
        _ => true,
    }
}

fn iface_inputs(document: &Value) -> Option<&electricity_value::Dict> {
    document
        .as_dict()?
        .get(&Value::Str("interface".to_string()))?
        .as_dict()?
        .get(&Value::Str("inputs".to_string()))?
        .as_dict()
}

/// `core/document_check.py::interface_unknown_type_errors`.
pub fn interface_unknown_type_errors(document: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(inputs) = iface_inputs(document) else {
        return errors;
    };
    let allowed = INTERFACE_TYPE_NAMES.join(", ");
    for (key, spec) in inputs {
        let Some(spec_dict) = spec.as_dict() else {
            continue;
        };
        let Some(declared_type) = spec_dict.get(&Value::Str("type".to_string())) else {
            continue;
        };
        if let Value::Str(type_name) = declared_type {
            if INTERFACE_TYPE_NAMES.contains(&type_name.as_str()) {
                continue;
            }
        }
        errors.push(format!(
            "interface.inputs.{}.type: {} is not a recognized type — expected one of {allowed}.",
            key.py_str(),
            declared_type.py_repr(),
        ));
    }
    errors
}

/// `core/document_check.py::_unquote_hint`.
fn unquote_hint(value: &str, declared_type: &str) -> String {
    let coerced = match electricity_yaml::load_yaml(value) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    if !matches_type(&coerced, declared_type) {
        return String::new();
    }
    format!(" — quoting it makes it a string; remove the quotes to declare it as {declared_type}")
}

/// `core/document_check.py::interface_default_type_errors`.
pub fn interface_default_type_errors(document: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let Some(inputs) = iface_inputs(document) else {
        return errors;
    };
    for (key, spec) in inputs {
        let Some(spec_dict) = spec.as_dict() else {
            continue;
        };
        let Some(default_value) = spec_dict.get(&Value::Str("default".to_string())) else {
            continue;
        };
        let declared_type = match spec_dict.get(&Value::Str("type".to_string())) {
            Some(Value::Str(s)) if INTERFACE_TYPE_NAMES.contains(&s.as_str()) => s.as_str(),
            _ => continue,
        };
        if matches_type(default_value, declared_type) {
            continue;
        }
        let hint = match default_value {
            Value::Str(s) => unquote_hint(s, declared_type),
            _ => String::new(),
        };
        errors.push(format!(
            "interface.inputs.{}.default: declared type '{}' but {} is {}{hint}.",
            key.py_str(),
            declared_type,
            default_value.py_repr(),
            py_class_name(default_value),
        ));
    }
    errors
}

/// `core/document_check.py::structural_errors`: near-miss unknown keys
/// first (the likelier cause), then schema errors, then `group:`
/// placement, then an `interface.inputs` unrecognized type, then a
/// default that doesn't match its (recognized) type.
pub fn structural_errors(document: &Value) -> Vec<String> {
    let mut errors = unknown_key_errors(document);
    errors.extend(schema_errors(document));
    errors.extend(group_field_errors(document));
    errors.extend(interface_unknown_type_errors(document));
    errors.extend(interface_default_type_errors(document));
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn doc_with_effects(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn effect(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn misspelled_while_is_an_error_naming_the_key() {
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("loop".to_string())),
            ("name", Value::Str("poll".to_string())),
            ("whlie", Value::Dict(Dict::new())),
            ("body", Value::List(vec![])),
        ])]);
        let errors = unknown_key_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains(
                "effects[0]: unknown key 'whlie' on a 'loop' effect — did you mean 'while'?"
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn unrelated_unknown_key_is_a_warning_not_an_error() {
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("tool".to_string())),
            ("name", Value::Str("t".to_string())),
            ("provider", Value::Str("json".to_string())),
            ("params", Value::Dict(Dict::new())),
            ("owner", Value::Str("ops".to_string())),
        ])]);
        assert_eq!(unknown_key_errors(&doc), Vec::<String>::new());
        let warnings = unknown_key_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("effects[0]: unknown key 'owner' on a 'tool' effect is ignored")
        );
    }

    #[test]
    fn unquoted_yaml_boolean_key_is_reported_as_such() {
        let mut inner = Dict::new();
        inner.insert(
            Value::Str("type".to_string()),
            Value::Str("tool".to_string()),
        );
        inner.insert(Value::Str("name".to_string()), Value::Str("t".to_string()));
        inner.insert(
            Value::Str("provider".to_string()),
            Value::Str("json".to_string()),
        );
        inner.insert(Value::Str("params".to_string()), Value::Dict(Dict::new()));
        inner.insert(Value::Bool(true), Value::Str("x".to_string()));
        let doc = doc_with_effects(vec![Value::Dict(inner)]);
        let warnings = unknown_key_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("unknown key True (YAML read the unquoted key as a bool)"),
            "{warnings:?}"
        );
    }

    #[test]
    fn near_miss_key_inside_a_dynamics_finally_is_an_error() {
        let tool = effect(vec![
            ("type", Value::Str("tool".to_string())),
            ("name", Value::Str("cleanup".to_string())),
            ("provider", Value::Str("json".to_string())),
            ("params", Value::Dict(Dict::new())),
            ("on_eror", Value::Str("continue".to_string())),
        ]);
        let mut dynamic = Dict::new();
        dynamic.insert(
            Value::Str("type".to_string()),
            Value::Str("dynamic".to_string()),
        );
        dynamic.insert(Value::Str("name".to_string()), Value::Str("d".to_string()));
        dynamic.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        dynamic.insert(Value::Str("finally".to_string()), Value::List(vec![tool]));
        let doc = doc_with_effects(vec![Value::Dict(dynamic)]);
        let errors = unknown_key_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("finally"));
        assert!(errors[0].contains("unknown key 'on_eror'"));
        assert!(errors[0].contains("did you mean 'on_error'?"));
    }

    #[test]
    fn top_level_near_miss_is_an_error_and_other_keys_warn() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("modle".to_string()),
            Value::Str("llama3".to_string()),
        );
        dict.insert(
            Value::Str("name".to_string()),
            Value::Str("mine".to_string()),
        );
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let doc = Value::Dict(dict);
        let errors = unknown_key_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .contains("top level: unknown key 'modle' on the document — did you mean 'model'?")
        );
        let warnings = unknown_key_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown key 'name' on the document is ignored"));
    }

    #[test]
    fn schema_errors_name_the_offending_node() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), Value::Str("x".to_string()));
        let loop_effect = effect(vec![
            ("type", Value::Str("loop".to_string())),
            ("each", Value::Dict(each)),
            ("body", Value::List(vec![])),
        ]);
        let doc = doc_with_effects(vec![loop_effect]);
        // Circuitry's own (Python `jsonschema`) message is "effects[0].body:
        // value should be non-empty" -- the location (Circuitry's own
        // format) matches exactly; the message past the ": " is the Rust
        // `jsonschema` crate's own third-party text, not required to match
        // word for word (DESIGN.md §1/§12, `electricity-schema`'s own crate
        // docs).
        let errors = schema_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("effects[0].body: "), "{errors:?}");
    }

    #[test]
    fn a_date_at_an_object_typed_position_is_exactly_one_error() {
        // F2 (second-round review): `retries` is `{"type": "object",
        // "additionalProperties": false}` -- a marker object there used to
        // also trip "additionalProperties" (leaking the internal marker
        // key name) on top of the "type" error. Python's own `jsonschema`
        // never runs an object-shape keyword against a non-dict instance,
        // so there is exactly one error here.
        let date = Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("prompt".to_string())),
            ("name", Value::Str("x".to_string())),
            ("template", Value::Str("hi".to_string())),
            ("retries", date),
        ])]);
        let errors = schema_errors(&doc);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("effects[0].retries: "), "{errors:?}");
        assert!(
            !errors[0].contains("circuitry_non_json_scalar"),
            "leaked the internal marker key name: {errors:?}"
        );
    }

    #[test]
    fn an_integer_beyond_f64_range_is_a_maximum_error_not_a_panic() {
        // F1/F4 (second-round review): `threshold` is `number`, `minimum:
        // 0`, `maximum: 1` -- a bare YAML integer this large used to panic
        // inside the `jsonschema` crate (`arbitrary_precision`'s `as_f64`
        // returns `None` for a value that overflows to infinity, and
        // `maximum`'s own validator calls `.expect("Always valid")` on
        // it). Python: a `maximum` error at `effects[0].threshold`.
        let huge =
            electricity_value::IntValue::parse_decimal(&format!("1{}", "0".repeat(400))).unwrap();
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("if".to_string())),
            ("if", {
                let mut cond = Dict::new();
                cond.insert(
                    Value::Str("mode".to_string()),
                    Value::Str("cel".to_string()),
                );
                cond.insert(
                    Value::Str("expr".to_string()),
                    Value::Str("true".to_string()),
                );
                Value::Dict(cond)
            }),
            ("then", Value::List(vec![])),
            ("threshold", Value::Int(huge)),
        ])]);
        let errors = schema_errors(&doc);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("effects[0].threshold: "),
            "{errors:?}"
        );
    }

    #[test]
    fn a_negative_integer_beyond_f64_range_is_a_minimum_error_not_a_panic() {
        // Same guard, the negative/`minimum` side: a loop's `max_concurrency`
        // is `integer`, `minimum: 1`. Python: a `minimum` error at
        // `effects[0].max_concurrency`.
        let huge_negative =
            electricity_value::IntValue::parse_decimal(&format!("-1{}", "0".repeat(400))).unwrap();
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), Value::Str("x".to_string()));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("loop".to_string())),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![effect(vec![
                    ("type", Value::Str("tool".to_string())),
                    ("name", Value::Str("t".to_string())),
                    ("provider", Value::Str("json".to_string())),
                ])]),
            ),
            ("max_concurrency", Value::Int(huge_negative)),
        ])]);
        let errors = schema_errors(&doc);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("effects[0].max_concurrency: "),
            "{errors:?}"
        );
    }

    #[test]
    fn an_integer_beyond_f64_range_within_an_unbounded_above_minimum_is_valid() {
        // `timeout_ms` is `integer`, `minimum: 0`, no `maximum` -- Python:
        // valid regardless of magnitude (`isinstance(x, int)` has no size
        // limit).
        let huge =
            electricity_value::IntValue::parse_decimal(&format!("1{}", "0".repeat(400))).unwrap();
        let doc = doc_with_effects(vec![effect(vec![
            ("type", Value::Str("tool".to_string())),
            ("name", Value::Str("t".to_string())),
            ("provider", Value::Str("json".to_string())),
            ("timeout_ms", Value::Int(huge)),
        ])]);
        assert_eq!(schema_errors(&doc), Vec::<String>::new());
    }

    #[test]
    fn group_on_a_container_effect_is_an_error() {
        let dynamic = effect(vec![
            ("type", Value::Str("dynamic".to_string())),
            ("name", Value::Str("d".to_string())),
            ("effects", Value::List(vec![])),
            ("group", Value::Str("g".to_string())),
        ]);
        let doc = doc_with_effects(vec![dynamic]);
        let errors = group_field_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("'group' is only allowed on tool/prompt effects"));
    }

    #[test]
    fn interface_unknown_type_is_reported() {
        let mut spec = Dict::new();
        spec.insert(
            Value::Str("type".to_string()),
            Value::Str("stringg".to_string()),
        );
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("x".to_string()), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let errors = interface_unknown_type_errors(&Value::Dict(dict));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("interface.inputs.x.type"));
    }

    #[test]
    fn interface_default_type_mismatch_with_unquote_hint() {
        let mut spec = Dict::new();
        spec.insert(
            Value::Str("type".to_string()),
            Value::Str("integer".to_string()),
        );
        spec.insert(
            Value::Str("default".to_string()),
            Value::Str("3".to_string()),
        );
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("x".to_string()), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let errors = interface_default_type_errors(&Value::Dict(dict));
        // Recorded directly from `core.document_check.interface_default_type_errors`.
        assert_eq!(
            errors,
            vec![
                "interface.inputs.x.default: declared type 'integer' but '3' is str — \
                 quoting it makes it a string; remove the quotes to declare it as integer."
                    .to_string()
            ]
        );
    }

    #[test]
    fn interface_unknown_type_error_matches_circuitry_exactly() {
        let mut spec = Dict::new();
        spec.insert(
            Value::Str("type".to_string()),
            Value::Str("stringg".to_string()),
        );
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("x".to_string()), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let errors = interface_unknown_type_errors(&Value::Dict(dict));
        // Recorded directly from `core.document_check.interface_unknown_type_errors`.
        assert_eq!(
            errors,
            vec![
                "interface.inputs.x.type: 'stringg' is not a recognized type — expected \
                 one of string, number, integer, boolean, array, object."
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_date_dict_key_reports_its_unqualified_class_name() {
        let date = Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let mut dict = Dict::new();
        dict.insert(date, Value::Str("x".to_string()));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let warnings = unknown_key_warnings(&Value::Dict(dict));
        assert_eq!(warnings.len(), 1);
        // Python's `type(key).__name__` for a `datetime.date` is `"date"`,
        // not the module-qualified `"datetime.date"` -- F5.
        assert!(
            warnings[0].contains("(YAML read the unquoted key as a date)"),
            "{warnings:?}"
        );
        assert!(!warnings[0].contains("datetime.date)"), "{warnings:?}");
    }

    #[test]
    fn interface_errors_render_a_non_string_key_with_str_not_blank() {
        let mut spec = Dict::new();
        spec.insert(
            Value::Str("type".to_string()),
            Value::Str("stringg".to_string()),
        );
        let mut inputs = Dict::new();
        inputs.insert(Value::from(1i64), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let errors = interface_unknown_type_errors(&Value::Dict(dict));
        assert_eq!(errors.len(), 1);
        // `f"interface.inputs.{key}..."` renders an int key as `str(key)`
        // ("1"), not the blank text `key.as_str().unwrap_or_default()`
        // -- F5.
        assert!(
            errors[0].starts_with("interface.inputs.1.type:"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_null_effects_key_falls_back_to_steps_like_an_absent_one() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::None);
        dict.insert(
            Value::Str("steps".to_string()),
            Value::List(vec![effect(vec![
                ("type", Value::Str("tool".to_string())),
                ("name", Value::Str("t".to_string())),
                ("provider", Value::Str("json".to_string())),
                ("prams", Value::Dict(Dict::new())),
            ])]),
        );
        let errors = unknown_key_errors(&Value::Dict(dict));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("did you mean 'params'?"), "{errors:?}");
    }

    #[test]
    fn an_empty_list_effects_key_does_not_fall_back_to_steps() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        dict.insert(
            Value::Str("steps".to_string()),
            Value::List(vec![effect(vec![
                ("type", Value::Str("tool".to_string())),
                ("name", Value::Str("t".to_string())),
                ("provider", Value::Str("json".to_string())),
                ("prams", Value::Dict(Dict::new())),
            ])]),
        );
        assert_eq!(unknown_key_errors(&Value::Dict(dict)), Vec::<String>::new());
    }

    /// Replays `electricity/scripts/generate_compiler_difflib_corpus.py`'s
    /// golden cases -- `core.document_check._near_miss` (the `_MISTAKEN_FOR`
    /// table plus `difflib.get_close_matches`, cutoff 0.8) against
    /// Circuitry's own real known-key sets, not a hand-typed value (F9).
    #[derive(serde::Deserialize)]
    struct DifflibCase {
        word: String,
        known_set: String,
        expected: Option<String>,
    }

    #[test]
    fn difflib_matches_cpython_recorded_values() {
        let text = include_str!("../tests/golden/difflib.json");
        let cases: Vec<DifflibCase> =
            serde_json::from_str(text).expect("golden/difflib.json is valid JSON");
        assert!(!cases.is_empty());
        for case in cases {
            let known = known_effect_keys(&case.known_set);
            let actual = near_miss(&case.word, &known);
            assert_eq!(
                actual, case.expected,
                "word {:?} against the {:?} known-key set",
                case.word, case.known_set
            );
        }
    }
}
