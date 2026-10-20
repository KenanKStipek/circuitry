//! Regression tests for PR #502's round-6 fix, each pinned against the
//! exact schema/instance shape `probe-r6`/`probe-nan`/`probe-refgraph`
//! record against real `cof run`.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema, value_from_json};
use electricity_value::{Dict, Value};
use serde_json::json;

fn check(schema: &serde_json::Value, instance: &serde_json::Value) -> PromptSchemaCheck {
    validate_prompt_schema(&value_from_json(schema), &value_from_json(instance))
}

// --- lazy `is_valid` leaking into `anyOf`/`oneOf` branch evaluation,
// giving a wrong verdict under `not`/`if`/`contains` -------------------

/// `probe-r6` c0: once the first `anyOf` branch fails, lazy mode must
/// never make the *second* branch stop after its own first keyword and
/// count as valid regardless of what that keyword decided -- both
/// branches genuinely fail `5`, so `not` of that `anyOf` is `Valid`.
#[test]
fn not_wrapping_a_failing_any_of_is_valid_lazy_mode_never_leaks_into_the_second_branch() {
    let schema = json!({
        "not": {
            "anyOf": [
                {"type": "string"},
                {"type": "integer", "minimum": 10}
            ]
        }
    });
    assert_eq!(check(&schema, &json!(5)), PromptSchemaCheck::Valid);
}

/// `probe-r6` c1: `if`'s own condition is the same `anyOf`, so taking
/// the wrong branch here would also flip which of `then`/`else` runs.
#[test]
fn if_wrapping_a_failing_any_of_takes_the_else_branch() {
    let schema = json!({
        "if": {
            "anyOf": [
                {"type": "string"},
                {"type": "integer", "minimum": 10}
            ]
        },
        "then": {"const": "T"},
        "else": {"const": 5}
    });
    assert_eq!(check(&schema, &json!(5)), PromptSchemaCheck::Valid);
}

/// `probe-r6` c2: `contains`'s own `oneOf` must reject `5` the same
/// way -- the array element genuinely matches neither branch, so
/// `contains` finds nothing and the whole array is invalid.
#[test]
fn contains_wrapping_a_failing_one_of_rejects_the_array() {
    let schema = json!({
        "contains": {
            "oneOf": [
                {"type": "string"},
                {"type": "integer", "minimum": 10}
            ]
        }
    });
    assert!(matches!(
        check(&schema, &json!([5])),
        PromptSchemaCheck::Invalid(_)
    ));
}

// --- a NaN (or zero) `multipleOf` divisor panicking -------------------

// `serde_json::Value` can't hold a `NaN`/`Infinity` float at all (`json!`
// silently turns one into `null`), so every schema/instance here that
// needs one is built directly as an `electricity_value::Value`, the same
// way YAML's own `.nan`/`.inf` reaches this engine for real.
fn dict(pairs: Vec<(&str, Value)>) -> Value {
    let mut d = Dict::new();
    for (k, v) in pairs {
        d.insert(Value::Str(k.to_string()), v);
    }
    Value::Dict(d)
}

/// `probe-nan`'s own schema shape: a `$ref` into an unknown key
/// (`x-sub`) skips the metaschema check that would otherwise reject a
/// non-positive `multipleOf`, so a `NaN`/zero divisor must reach
/// `multiple_of_failed` at all -- it used to `.expect()` a `None`
/// `float_exact_ratio` and panic (exit 101, no state) rather than
/// recording a generic failure.
fn multiple_of_schema(divisor: Value) -> Value {
    dict(vec![
        ("$ref", Value::Str("#/x-sub".to_string())),
        ("x-sub", dict(vec![("multipleOf", divisor)])),
    ])
}

#[test]
fn a_nan_divisor_is_a_generic_failure_never_a_panic() {
    let schema = multiple_of_schema(Value::Float(f64::NAN));
    assert_eq!(
        validate_prompt_schema(&schema, &Value::Float(1.5)),
        PromptSchemaCheck::SchemaError("cannot convert float NaN to integer".to_string())
    );
}

#[test]
fn a_nan_divisor_against_an_integer_instance_is_a_generic_failure_never_a_panic() {
    let schema = multiple_of_schema(Value::Float(f64::NAN));
    assert_eq!(
        validate_prompt_schema(&schema, &value_from_json(&json!(2))),
        PromptSchemaCheck::SchemaError("cannot convert float NaN to integer".to_string())
    );
}

#[test]
fn an_integer_zero_divisor_against_an_integer_instance_is_a_generic_failure() {
    let schema = multiple_of_schema(value_from_json(&json!(0)));
    assert_eq!(
        validate_prompt_schema(&schema, &value_from_json(&json!(2))),
        PromptSchemaCheck::SchemaError("integer modulo by zero".to_string())
    );
}

#[test]
fn an_integer_zero_divisor_against_a_float_instance_is_a_generic_failure() {
    let schema = multiple_of_schema(value_from_json(&json!(0)));
    assert_eq!(
        validate_prompt_schema(&schema, &Value::Float(1.5)),
        PromptSchemaCheck::SchemaError("float modulo".to_string())
    );
}

#[test]
fn a_float_zero_divisor_against_an_integer_instance_is_a_generic_failure() {
    let schema = multiple_of_schema(Value::Float(0.0));
    assert_eq!(
        validate_prompt_schema(&schema, &value_from_json(&json!(2))),
        PromptSchemaCheck::SchemaError("float division by zero".to_string())
    );
}

#[test]
fn a_float_zero_divisor_against_a_float_instance_is_a_generic_failure() {
    let schema = multiple_of_schema(Value::Float(0.0));
    assert_eq!(
        validate_prompt_schema(&schema, &Value::Float(1.5)),
        PromptSchemaCheck::SchemaError("float division by zero".to_string())
    );
}

/// An infinite instance against an infinite divisor is `NaN`'s own
/// text, not the finite-instance "cannot convert Infinity to integer
/// ratio" case -- `inf / inf` is itself `NaN` before `int()` ever runs
/// (confirmed directly against CPython).
#[test]
fn an_infinite_instance_against_an_infinite_divisor_is_the_nan_text() {
    let schema = multiple_of_schema(Value::Float(f64::INFINITY));
    assert_eq!(
        validate_prompt_schema(&schema, &Value::Float(f64::INFINITY)),
        PromptSchemaCheck::SchemaError("cannot convert float NaN to integer".to_string())
    );
}

// --- a factorial `$ref`-graph walk in `schema_is_fully_supported`

/// *n* mutually-referencing `$defs`, each `{type: object, properties:
/// {c: {type: array, items: {anyOf: [$ref to every N0..N(n-1)]}}}}`,
/// root `$ref: N0` -- `probe-refgraph`'s own shape. Before the fix,
/// `visited` was popped after each `$ref` descent, so every simple path
/// through this complete-graph-like structure got re-walked from
/// scratch (factorial in *n*); ten of them took several seconds.
fn mutual_ref_schema(n: usize) -> serde_json::Value {
    let mut defs = serde_json::Map::new();
    for i in 0..n {
        let any_of: Vec<serde_json::Value> = (0..n)
            .map(|j| json!({"$ref": format!("#/$defs/N{j}")}))
            .collect();
        defs.insert(
            format!("N{i}"),
            json!({
                "type": "object",
                "properties": {
                    "c": {"type": "array", "items": {"anyOf": any_of}}
                }
            }),
        );
    }
    json!({"$defs": defs, "$ref": "#/$defs/N0"})
}

#[test]
fn twelve_mutually_referencing_defs_stay_well_under_a_second() {
    let schema = mutual_ref_schema(12);
    let instance = json!({"c": [{"c": []}]});
    let start = std::time::Instant::now();
    let result = check(&schema, &instance);
    assert_eq!(result, PromptSchemaCheck::Valid);
    assert!(
        start.elapsed().as_secs() < 1,
        "12 mutually-referencing defs took {:?}",
        start.elapsed()
    );
}

#[test]
fn twenty_mutually_referencing_defs_stay_well_under_a_second() {
    let schema = mutual_ref_schema(20);
    let instance = json!({"c": [{"c": []}]});
    let start = std::time::Instant::now();
    let result = check(&schema, &instance);
    assert_eq!(result, PromptSchemaCheck::Valid);
    assert!(
        start.elapsed().as_secs() < 1,
        "20 mutually-referencing defs took {:?}",
        start.elapsed()
    );
}

/// A chain *depth* levels deep, each level's own two sibling properties
/// (`"a"`/`"b"`) both `$ref`-ing the *same* next level -- without
/// memoization, the number of simple paths through this DAG doubles
/// every level (`2^depth`), even though there are only `depth + 1`
/// distinct nodes to actually walk.
fn fan_out_chain_schema(depth: usize) -> serde_json::Value {
    let mut defs = serde_json::Map::new();
    for i in 0..depth {
        defs.insert(
            format!("L{i}"),
            json!({
                "type": "object",
                "properties": {
                    "a": {"$ref": format!("#/$defs/L{}", i + 1)},
                    "b": {"$ref": format!("#/$defs/L{}", i + 1)}
                }
            }),
        );
    }
    defs.insert(format!("L{depth}"), json!({"type": "object"}));
    json!({"$defs": defs, "$ref": "#/$defs/L0"})
}

#[test]
fn a_depth_30_fan_out_chain_stays_well_under_a_second() {
    let schema = fan_out_chain_schema(30);
    let instance = json!({});
    let start = std::time::Instant::now();
    let result = check(&schema, &instance);
    assert_eq!(result, PromptSchemaCheck::Valid);
    assert!(
        start.elapsed().as_secs() < 1,
        "a depth-30 fan-out chain took {:?}",
        start.elapsed()
    );
}
