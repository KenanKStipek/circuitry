//! Issue #507's own item 2: Python's own `evolve(schema=...).
//! is_valid(...)` (`not`/`if`/`contains`'s own building block) never
//! calls `descend()` for the schema it's handed -- the only place a
//! `$id` subresource scope is ever pushed (confirmed directly against
//! `jsonschema.validators.py`) -- so a `$id` written directly on that
//! schema changes nothing a relative `$ref` inside it resolves
//! against: it still resolves against the *parent* document's base,
//! exactly as this engine's own `resolve_ref` (which never tracks any
//! `$id`-based base at all) already does. These schemas now stay under
//! engine authority rather than routing to the crate, which resolves
//! such a `$ref` against the nested `$id` per spec and would otherwise
//! disagree with Python's own verdict here.
//!
//! Every case pinned against real `cof run` (`jsonschema` 4.26, CPython
//! 3.11, confirmed directly with this workspace's own `.venv`).

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};

fn check(schema_json: &str, instance_json: &str) -> PromptSchemaCheck {
    let schema = electricity_json::loads(schema_json).expect("schema_json is valid JSON");
    let instance = electricity_json::loads(instance_json).expect("instance_json is valid JSON");
    validate_prompt_schema(&schema, &instance)
}

const NOT_SCHEMA: &str = r##"{
    "not": {
        "$id": "https://example.com/sub/",
        "properties": {"a": {"$ref": "#/$defs/str"}}
    },
    "$defs": {"str": {"type": "string"}}
}"##;

#[test]
fn not_with_nested_id_resolves_ref_against_the_parent_base() {
    // `a: "x"` makes the *inner* schema valid (the `$ref` resolves
    // against the parent's `$defs`, not the nested `$id`'s own base,
    // which has no `$defs` of its own at all) -- so `not` itself
    // fails.
    match check(NOT_SCHEMA, r#"{"a": "x"}"#) {
        PromptSchemaCheck::Invalid(_) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn not_with_nested_id_resolves_ref_against_the_parent_base_other_branch() {
    // `a: 1` makes the inner schema invalid (not a string) -- `not`
    // itself passes.
    assert_eq!(check(NOT_SCHEMA, r#"{"a": 1}"#), PromptSchemaCheck::Valid);
}

const IF_SCHEMA: &str = r##"{
    "if": {
        "$id": "https://example.com/sub/",
        "properties": {"a": {"$ref": "#/$defs/str"}}
    },
    "then": {"const": "then-branch"},
    "else": {"const": "else-branch"},
    "$defs": {"str": {"type": "string"}}
}"##;

#[test]
fn if_with_nested_id_resolves_ref_against_the_parent_base() {
    assert_eq!(
        check(IF_SCHEMA, r#"{"a": "x"}"#),
        PromptSchemaCheck::Invalid("'then-branch' was expected".to_string())
    );
}

#[test]
fn if_with_nested_id_resolves_ref_against_the_parent_base_other_branch() {
    assert_eq!(
        check(IF_SCHEMA, r#"{"a": 1}"#),
        PromptSchemaCheck::Invalid("'else-branch' was expected".to_string())
    );
}

const CONTAINS_SCHEMA: &str = r##"{
    "type": "array",
    "contains": {
        "$id": "https://example.com/sub/",
        "properties": {"a": {"$ref": "#/$defs/str"}}
    },
    "$defs": {"str": {"type": "string"}}
}"##;

#[test]
fn contains_with_nested_id_resolves_ref_against_the_parent_base() {
    assert_eq!(
        check(CONTAINS_SCHEMA, r#"[{"a": "x"}]"#),
        PromptSchemaCheck::Valid
    );
}

#[test]
fn contains_with_nested_id_resolves_ref_against_the_parent_base_other_branch() {
    match check(CONTAINS_SCHEMA, r#"[{"a": 1}]"#) {
        PromptSchemaCheck::Invalid(_) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// A `$id` reached *two* levels deep -- inside `not`'s own schema, but
/// behind a further `properties` applicator that *does* call
/// `descend()` -- still routes to the crate: `id_is_annotation` is
/// never threaded past the one direct call `not`/`if`/`contains` make.
#[test]
fn an_id_nested_two_levels_under_not_still_routes_to_the_crate() {
    let schema = r##"{
        "not": {
            "properties": {
                "a": {
                    "$id": "https://example.com/sub/",
                    "$ref": "#/$defs/str"
                }
            }
        },
        "$defs": {"str": {"type": "string"}}
    }"##;
    // The crate resolves `$ref` against the nested `$id`'s own base,
    // which has no matching `$defs` entry of its own -- a dangling
    // reference the crate reports its own way, never silently `Valid`.
    match check(schema, r#"{"a": "x"}"#) {
        PromptSchemaCheck::SchemaError(_) | PromptSchemaCheck::Invalid(_) => {}
        other => panic!("expected the crate's own failure shape, got {other:?}"),
    }
}
