//! Issue #507's own item 5 needed the well-formedness probe to drop a
//! `$ref` key before building, since a fully-supported schema's own
//! string-valued `$ref`s are already confirmed to be this engine's own
//! lazily-resolved shape, where the crate's build step would otherwise
//! resolve them eagerly and fail. Dropping *every* key literally named
//! `"$ref"`, whatever its own value, went too far: a `"$ref"` key can
//! also be an ordinary property name or `$defs` entry, never this
//! engine's own keyword at all, and a malformed schema hiding behind
//! one must still reach the probe. Both cases here are pinned against
//! real `jsonschema.Draft202012Validator.check_schema` (confirmed
//! directly with this workspace's own `.venv`): both are a genuine
//! `SchemaError` in Python, never a schema this engine should accept.

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};

fn check(schema_json: &str, instance_json: &str) -> PromptSchemaCheck {
    let schema = electricity_json::loads(schema_json).expect("schema_json is valid JSON");
    let instance = electricity_json::loads(instance_json).expect("instance_json is valid JSON");
    validate_prompt_schema(&schema, &instance)
}

/// `check_schema({"$defs": {"x": {"$ref": 5}}})` raises `SchemaError:
/// 5 is not of type 'string'` in Python -- a `$ref` key whose own value
/// is an `Int`, never a string, which stripping every `"$ref"` key
/// unconditionally used to hide from the probe entirely.
#[test]
fn a_ref_key_with_a_non_string_value_inside_defs_is_still_a_schema_error() {
    let schema = r#"{"$defs": {"x": {"$ref": 5}}}"#;
    match check(schema, r#""anything""#) {
        PromptSchemaCheck::SchemaError(_) => {}
        other => panic!("expected a SchemaError, got {other:?}"),
    }
}

/// `check_schema({"properties": {"$ref": {"type": 5}}})` raises a
/// `SchemaError` too -- here `"$ref"` is an ordinary *property name*
/// (`properties`'s own key, mapping to a subschema, `{"type": 5}`,
/// that is itself malformed), never this engine's `$ref` keyword at
/// all, since it never sits where that keyword is recognized.
/// Stripping every `"$ref"`-named key regardless of position used to
/// drop this whole subschema before the probe ever saw it, silently
/// accepting the malformed `"type": 5` inside it.
#[test]
fn a_property_literally_named_ref_is_still_checked_for_well_formedness() {
    let schema = r#"{"properties": {"$ref": {"type": 5}}}"#;
    match check(schema, "{}") {
        PromptSchemaCheck::SchemaError(_) => {}
        other => panic!("expected a SchemaError, got {other:?}"),
    }
}
