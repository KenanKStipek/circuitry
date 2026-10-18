//! Issue #507's own items 3-5: `multipleOf`'s own metaschema and
//! divisor-arithmetic parity gaps left after PR #502's round-7 fix.
//! Every case here is pinned against real `cof run` (`jsonschema`
//! 4.26, CPython 3.11, confirmed directly with this workspace's own
//! `.venv`).

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};

fn check(schema_json: &str, instance_json: &str) -> PromptSchemaCheck {
    let schema = electricity_json::loads(schema_json).expect("schema_json is valid JSON");
    let instance = electricity_json::loads(instance_json).expect("instance_json is valid JSON");
    validate_prompt_schema(&schema, &instance)
}

/// Item 3: Python's own `multipleOf` metaschema entry is `{"type":
/// "number", "exclusiveMinimum": 0}` -- `NaN <= 0` is `False`, so a
/// schema-literal `multipleOf: NaN` passes that check, and `multipleOf`
/// never applies to a string instance anyway.
#[test]
fn top_level_multiple_of_nan_is_a_well_formed_schema() {
    let schema = r#"{"multipleOf": NaN}"#;
    assert_eq!(
        check(schema, r#""a string reply""#),
        PromptSchemaCheck::Valid
    );
}

/// The same schema against a numeric instance: Python's own
/// `int(instance / NaN)` raises `ValueError: cannot convert float NaN
/// to integer` -- a generic, non-retryable failure, never
/// `SchemaError` from the metaschema check itself.
#[test]
fn top_level_multiple_of_nan_against_a_number_is_a_generic_failure() {
    let schema = r#"{"multipleOf": NaN}"#;
    assert_eq!(
        check(schema, "5"),
        PromptSchemaCheck::SchemaError("cannot convert float NaN to integer".to_string())
    );
}

/// `multipleOf: Infinity` passes the same metaschema check (`Infinity
/// <= 0` is also `False`) -- unlike `-Infinity`, which genuinely fails
/// it (`-Infinity <= 0` is `True`).
#[test]
fn top_level_multiple_of_positive_infinity_is_a_well_formed_schema() {
    let schema = r#"{"multipleOf": Infinity}"#;
    assert_eq!(check(schema, r#""x""#), PromptSchemaCheck::Valid);
}

#[test]
fn top_level_multiple_of_negative_infinity_is_a_schema_error() {
    let schema = r#"{"multipleOf": -Infinity}"#;
    match check(schema, r#""x""#) {
        PromptSchemaCheck::SchemaError(_) => {}
        other => panic!("expected a SchemaError, got {other:?}"),
    }
}

/// Item 4: `2^60 + 2` isn't exactly representable as `f64` (it rounds
/// to `2^60`) -- confirmed directly: `Fraction(2**60 + 2) /
/// Fraction(3 * 2.0**-1000)` has denominator `1` (an exact multiple),
/// where `Fraction(float(2**60 + 2)) / Fraction(3 * 2.0**-1000)` does
/// not. The instance/divisor pair is chosen so `instance / divisor`
/// itself overflows to `Infinity` (forcing the `Fraction` fallback,
/// `jsonschema`'s own `OverflowError` catch) while staying well under
/// the ~309-digit threshold where `int` itself can't convert to `f64`
/// at all (issue #507's own item 6, a separate, accepted,
/// text-only gap) -- `instance` here is `1152921504606846978`.
#[test]
fn multiple_of_fraction_fallback_is_exact_for_an_int_instance_past_2_pow_53() {
    let schema = r#"{"multipleOf": 2.7997908555096566e-301}"#;
    assert_eq!(
        check(schema, "1152921504606846978"),
        PromptSchemaCheck::Valid
    );
}

/// The same divisor against the *rounded* value (`2^60`, what the old,
/// lossy `f64`-cast-first fallback effectively checked instead) is
/// genuinely not an exact multiple -- proving the fix is sensitive to
/// which exact value reaches `Fraction`, not just that *some* value
/// does.
#[test]
fn multiple_of_fraction_fallback_rejects_the_nearby_rounded_value() {
    let schema = r#"{"multipleOf": 2.7997908555096566e-301}"#;
    match check(schema, "1152921504606846976") {
        PromptSchemaCheck::Invalid(_) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// Item 5: a non-numeric `multipleOf` divisor reached only through a
/// `$ref` into a key no schema-structure walk (Python's own
/// `check_schema`, or this engine's `schema_is_fully_supported`)
/// recognizes as housing a subschema at all -- confirmed directly:
/// `check_schema` passes this document (`myCustomDefs` isn't a
/// metaschema-recognized applicator the way `$defs`/`properties` are),
/// yet `$ref` resolution (a plain JSON Pointer walk, oblivious to
/// keyword semantics) still reaches it at validation time, where
/// `isinstance(dB, float)` being `False` sends Python's own
/// `multipleOf` validator down its `%` branch regardless of operand
/// type, raising a `TypeError` before computing anything.
#[test]
fn multiple_of_string_divisor_reached_through_an_unknown_key_is_a_type_error() {
    let schema = r##"{
        "myCustomDefs": {"weird": {"multipleOf": "nope"}},
        "properties": {"a": {"$ref": "#/myCustomDefs/weird"}}
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 5}"#),
        PromptSchemaCheck::SchemaError(
            "unsupported operand type(s) for %: 'int' and 'str'".to_string()
        )
    );
}

/// The same shape against a `float` instance -- Python's own error
/// text names the instance's own type, not always `'int'`.
#[test]
fn multiple_of_string_divisor_against_a_float_instance_names_float() {
    let schema = r##"{
        "myCustomDefs": {"weird": {"multipleOf": "nope"}},
        "properties": {"a": {"$ref": "#/myCustomDefs/weird"}}
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 5.5}"#),
        PromptSchemaCheck::SchemaError(
            "unsupported operand type(s) for %: 'float' and 'str'".to_string()
        )
    );
}

/// `true` as a divisor is Python arithmetic (`bool` is an `int`
/// subtype): `5 % True` is `0`, never a `TypeError`.
#[test]
fn multiple_of_true_divisor_reached_through_an_unknown_key_is_arithmetic() {
    let schema = r##"{
        "myCustomDefs": {"weird": {"multipleOf": true}},
        "properties": {"a": {"$ref": "#/myCustomDefs/weird"}}
    }"##;
    assert_eq!(check(schema, r#"{"a": 5}"#), PromptSchemaCheck::Valid);
}

/// `false` as a divisor is `ZeroDivisionError`'s own `"integer modulo
/// by zero"` text, the same as an ordinary `multipleOf: 0`.
#[test]
fn multiple_of_false_divisor_reached_through_an_unknown_key_is_a_zero_divisor() {
    let schema = r##"{
        "myCustomDefs": {"weird": {"multipleOf": false}},
        "properties": {"a": {"$ref": "#/myCustomDefs/weird"}}
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 5}"#),
        PromptSchemaCheck::SchemaError("integer modulo by zero".to_string())
    );
}
