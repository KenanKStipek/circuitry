//! Offline JSON Schema (Draft 7) validation against Circuitry's own schemas
//! (`DESIGN.md` §4 step 3, §7.3).
//!
//! The schema files under `schema/` are synced byte for byte from
//! `src/circuitry/schema/` by `../../scripts/generate_schema_copy.py`
//! (`--check` in CI) rather than hand-ported into a Rust-native schema
//! builder, so this crate stays self-contained and the moment the two
//! schemas drift, that check fails loudly instead of silently producing a
//! different accept/reject verdict than `cof check`. Validation never
//! fetches a remote `$ref` (none of Circuitry's schemas have one; every
//! `$ref` is a local `#/$defs/...` anchor) and the `jsonschema` crate
//! already uses `fancy-regex` for the `pattern` keyword internally, the
//! same engine Circuitry's own JSON Schema validation and `regex` tool use
//! (§7.3) -- nothing further is added for either.
//!
//! Message *text* from the underlying `jsonschema` crate is not required to
//! match Circuitry's own (Python) `jsonschema` text word for word -- only to
//! fail at the same instance path, with a non-empty message (`DESIGN.md`
//! §1, §12: the third-party-library carve-out). What *is* matched exactly is
//! Circuitry's own location format: `core.document_check`'s
//! `_describe_schema_error` (orchestration documents, dotted/bracketed
//! `json_path`-style, e.g. `effects[0].each`) and `cli.profiles`'
//! `_validate_profile_schema` (profile documents, `/`-joined raw path
//! segments, e.g. `effects/summarize/model`).

use std::sync::OnceLock;

use serde_json::Value;

/// One schema violation: Circuitry's own location format (exact), plus the
/// underlying `jsonschema` crate's own message (not required to match
/// Circuitry's Python message text -- see the crate docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    pub location: String,
    pub message: String,
}

fn load_schema(json_text: &str) -> Value {
    serde_json::from_str(json_text).expect("bundled schema file is valid JSON")
}

fn orchestration_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| load_schema(include_str!("../schema/orchestration.schema.json")))
}

fn profile_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| load_schema(include_str!("../schema/profile.schema.json")))
}

fn orchestration_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::draft7::new(orchestration_schema())
            .expect("bundled orchestration.schema.json compiles as Draft 7")
    })
}

fn profile_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::draft7::new(profile_schema())
            .expect("bundled profile.schema.json compiles as Draft 7")
    })
}

/// Every schema violation in `document` against Circuitry's
/// `schema/orchestration.schema.json`; empty when it conforms. Mirrors
/// `core.document_check.schema_errors`'s accept/reject verdict and, for
/// each violation, its `_describe_schema_error`-computed location exactly.
pub fn orchestration_errors(document: &Value) -> Vec<SchemaError> {
    orchestration_validator()
        .iter_errors(document)
        .map(|err| SchemaError {
            location: orchestration_location(err.instance_path.as_str()),
            message: err.to_string(),
        })
        .collect()
}

/// Every schema violation in `document` against Circuitry's
/// `schema/profile.schema.json`; empty when it conforms. Mirrors
/// `cli.profiles._validate_profile_schema`'s accept/reject verdict and, for
/// each violation, its `/`-joined location exactly.
pub fn profile_errors(document: &Value) -> Vec<SchemaError> {
    profile_validator()
        .iter_errors(document)
        .map(|err| SchemaError {
            location: profile_location(err.instance_path.as_str()),
            message: err.to_string(),
        })
        .collect()
}

/// `core.document_check._describe_schema_error`'s location: Python
/// `jsonschema`'s own `json_path` (`jsonschema.exceptions._Error.json_path`)
/// with its leading `$` and (if present) the next `.` stripped, or
/// `"top level"` for the document root.
fn orchestration_location(instance_pointer: &str) -> String {
    let path = json_path_from_pointer(instance_pointer);
    let after_dollar = &path[1..]; // json_path_from_pointer always starts with "$".
    let after_dot = after_dollar.strip_prefix('.').unwrap_or(after_dollar);
    if after_dot.is_empty() {
        "top level".to_string()
    } else {
        after_dot.to_string()
    }
}

/// `cli.profiles._validate_profile_schema`'s location: raw path segments
/// joined with `/`, or `"<root>"` for the document root.
fn profile_location(instance_pointer: &str) -> String {
    let segments: Vec<String> = pointer_segments(instance_pointer)
        .map(unescape_json_pointer_segment)
        .collect();
    if segments.is_empty() {
        "<root>".to_string()
    } else {
        segments.join("/")
    }
}

/// JSON Pointer segments, root (`""`) yielding none: `split('/')` always
/// produces a leading empty element (the pointer starts with `/` whenever
/// it is not the empty root string), so skipping it is unconditional.
fn pointer_segments(pointer: &str) -> impl Iterator<Item = &str> {
    pointer.split('/').skip(1)
}

/// Python `jsonschema`'s own `json_path` property
/// (`jsonschema.exceptions._Error.json_path`): `$`, then for every path
/// segment, `[N]` for an array index, `.name` for a property name matching
/// `^[a-zA-Z][a-zA-Z0-9_]*$`, or `['name']` (backslash/quote-escaped)
/// otherwise.
///
/// The Rust `jsonschema` crate only exposes the already-rendered JSON
/// Pointer (`/seg/0/...`), which loses whether a numeric-looking segment
/// was originally an array index or an all-digit *property* key (both
/// render identically as pointer text); every segment made of only ASCII
/// digits is treated here as an array index, which matches every document
/// this crate actually validates -- none of Circuitry's schemas ever use a
/// pure-digit-string property name.
fn json_path_from_pointer(pointer: &str) -> String {
    let mut path = String::from("$");
    for raw_segment in pointer_segments(pointer) {
        let segment = unescape_json_pointer_segment(raw_segment);
        if !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()) {
            path.push('[');
            path.push_str(&segment);
            path.push(']');
        } else if is_json_path_compatible_property(&segment) {
            path.push('.');
            path.push_str(&segment);
        } else {
            path.push_str("['");
            path.push_str(&segment.replace('\\', "\\\\").replace('\'', "\\'"));
            path.push_str("']");
        }
    }
    path
}

/// RFC 6901 unescaping: `~1` then `~0`, in that order (a literal `~`
/// encodes as `~0`, so decoding `~0` first could turn an encoded `~1` that
/// followed a real `~` into a spurious `/`).
fn unescape_json_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// `jsonschema.exceptions._JSON_PATH_COMPATIBLE_PROPERTY_PATTERN`:
/// `^[a-zA-Z][a-zA-Z0-9_]*$` (note: no leading underscore, unlike
/// Circuitry's own orchestration `NamePattern` -- this is Python
/// `jsonschema`'s own, unrelated pattern).
fn is_json_path_compatible_property(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_minimal_orchestration_has_no_errors() {
        let doc = json!({"effects": []});
        assert_eq!(orchestration_errors(&doc), vec![]);
    }

    #[test]
    fn missing_required_effects_reports_top_level() {
        let doc = json!({});
        let errors = orchestration_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].location, "top level");
        assert!(!errors[0].message.is_empty());
    }

    #[test]
    fn nested_array_index_and_property_use_dotted_bracket_form() {
        let doc = json!({"effects": [{"type": "prompt", "name": "x", "prompt_type": 5}]});
        let errors = orchestration_errors(&doc);
        assert!(
            errors
                .iter()
                .any(|e| e.location == "effects[0].prompt_type")
        );
    }

    #[test]
    fn property_name_needing_bracket_quoting() {
        assert_eq!(json_path_from_pointer("/weird key"), "$['weird key']");
        assert_eq!(json_path_from_pointer("/it's"), "$['it\\'s']");
        assert_eq!(
            json_path_from_pointer("/effects/0/each"),
            "$.effects[0].each"
        );
        assert_eq!(json_path_from_pointer(""), "$");
    }

    #[test]
    fn valid_minimal_profile_has_no_errors() {
        let doc = json!({});
        assert_eq!(profile_errors(&doc), vec![]);
    }

    #[test]
    fn profile_unknown_key_location_is_slash_joined() {
        let doc = json!({"effects": {"summarize": {"model": "x", "bogus": 1}}});
        let errors = profile_errors(&doc);
        assert!(
            errors
                .iter()
                .any(|e| e.location == "effects/summarize" && !e.message.is_empty())
        );
    }

    #[test]
    fn profile_root_type_error_location_is_root() {
        let doc = json!([]);
        let errors = profile_errors(&doc);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].location, "<root>");
    }
}
