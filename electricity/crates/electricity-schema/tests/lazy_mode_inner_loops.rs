//! Issue #507's own item 1: Python's `Validator.is_valid` (`not`/`if`/
//! `contains`'s own building block) is `next(iter_errors(instance),
//! None) is None` -- one value pulled from the *entire* nested
//! `iter_errors` generator tree, `yield from` flattening every nested
//! validator call into it. [`evaluate`]'s own per-keyword lazy check
//! already stopped between a schema's own top-level keywords (PR #502's
//! round-7 fix), but not *inside* one keyword's own loop over several
//! properties/elements/subschemas -- a later sibling that loop would
//! otherwise reach, once an earlier one already failed, used to still
//! run, so its own exception (a dangling `$ref`, here) failed the whole
//! attempt even though Python's own laziness never reaches it at all.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};

fn check(schema_json: &str, instance_json: &str) -> PromptSchemaCheck {
    let schema = electricity_json::loads(schema_json).expect("schema_json is valid JSON");
    let instance = electricity_json::loads(instance_json).expect("instance_json is valid JSON");
    validate_prompt_schema(&schema, &instance)
}

/// The issue's own example: `a`'s own `type` violation is the first and
/// only one Python's `next()` ever sees -- `b`'s dangling `$ref` is
/// never reached, so it never raises at all, and `not` itself is
/// `Valid` since its own `properties` is not.
#[test]
fn not_wrapping_properties_stops_at_the_first_failing_property() {
    let schema = r##"{
        "not": {
            "properties": {
                "a": {"type": "string"},
                "b": {"$ref": "#/missing"}
            }
        }
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 1, "b": 2}"#),
        PromptSchemaCheck::Valid
    );
}

/// The same shape under `if`: the `if` branch itself fails on `a`
/// before `b`'s own dangling `$ref` is ever reached, so `is_valid`
/// returns `false` cleanly (no `PointerToNowhere`) and `else` runs --
/// which itself then fails on its own, unrelated `const` mismatch, the
/// same way Python's own equally lazy `if` would.
#[test]
fn if_wrapping_properties_stops_at_the_first_failing_property() {
    let schema = r##"{
        "if": {
            "properties": {
                "a": {"type": "string"},
                "b": {"$ref": "#/missing"}
            }
        },
        "then": {"type": "object"},
        "else": {"const": "unreachable"}
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 1, "b": 2}"#),
        PromptSchemaCheck::Invalid("'unreachable' was expected".to_string())
    );
}

/// `contains`' own per-element `is_valid` call is the same lazy
/// building block: `a`'s failure must stop that one element's own
/// check before it ever reaches `b`'s dangling `$ref`.
#[test]
fn contains_wrapping_properties_stops_at_the_first_failing_property() {
    let schema = r##"{
        "type": "array",
        "contains": {
            "properties": {
                "a": {"type": "string"},
                "b": {"$ref": "#/missing"}
            }
        }
    }"##;
    assert_eq!(
        check(schema, r#"[{"a": 1, "b": 2}]"#),
        PromptSchemaCheck::Invalid(
            "[{'a': 1, 'b': 2}] does not contain items matching the given schema".to_string()
        ),
    );
}

/// `patternProperties`' own double loop (patterns x property names)
/// needs the same per-sibling stop: `a` matches `^a` and fails `type`
/// first; `b` would only match `^b`, reached later in the same pattern
/// loop, and must never run its own dangling `$ref` once `a` already
/// failed.
#[test]
fn not_wrapping_pattern_properties_stops_at_the_first_failing_property() {
    let schema = r##"{
        "not": {
            "patternProperties": {
                "^a": {"type": "string"},
                "^b": {"$ref": "#/missing"}
            }
        }
    }"##;
    assert_eq!(
        check(schema, r#"{"a": 1, "b": 2}"#),
        PromptSchemaCheck::Valid
    );
}

/// `allOf`'s own loop over subschemas: the first branch's own `type`
/// violation must stop it before the second branch's dangling `$ref`
/// ever runs.
#[test]
fn not_wrapping_all_of_stops_at_the_first_failing_branch() {
    let schema = r##"{
        "not": {
            "allOf": [
                {"type": "string"},
                {"$ref": "#/missing"}
            ]
        }
    }"##;
    assert_eq!(check(schema, "1"), PromptSchemaCheck::Valid);
}
