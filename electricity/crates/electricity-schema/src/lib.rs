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
//! segments, e.g. `effects/summarize/model`), and -- also exactly -- the
//! `"{location}: {message}"` wrapping format itself
//! ([`SchemaError::describe`]), the "dynamic-wrapped" format `DESIGN.md` §1
//! names as needing to match word for word (distinct from the `{message}`
//! text it wraps, which is the carved-out part). Two further pieces of
//! `_describe_schema_error`'s own output are deliberately not replicated,
//! because they transform *that* third-party message text rather than
//! change the wrapping format: the `repr(err.instance)` -> `"value"`
//! substitution (there is no equivalent Python `repr` text on this side to
//! match against -- `jsonschema` 0.26 renders the instance as JSON, not a
//! Python repr) and the best-matching `oneOf`/`anyOf` sub-error suffix
//! (`jsonschema` 0.26's `ValidationErrorKind::OneOfNotValid`/`AnyOf` carry
//! no sub-error list to pick from -- see `error.rs` in that crate -- so
//! producing it would mean re-validating every branch by hand, a
//! materially bigger feature than matching a message format).
//!
//! `pattern` validation also gets two narrow, deliberate adjustments so
//! `fancy-regex` agrees with Python's `re` on the exact cases Circuitry's
//! own schemas exercise ([`normalize_pattern_schema`]): `$` is rewritten to
//! match just before a single trailing `\n` too (Python `re`'s default,
//! `fancy-regex`'s is not), and `\d` is rewritten to the Unicode decimal
//! digit class (Python `re`'s Unicode-aware default; the `jsonschema` crate
//! itself rewrites a bare `\d` to ASCII `[0-9]` as part of its ECMA-262
//! translation, `ecma.rs`). Nothing else about `fancy-regex` vs Python `re`
//! is adjusted; a true Python-`re`-only construct neither engine supports
//! is `DESIGN.md` §7.3's named, unclosed parity gap.

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

impl SchemaError {
    /// Circuitry's own "dynamic-wrapped" format, matched word for word
    /// (`DESIGN.md` §1): `"{location}: {message}"`. `message` itself is the
    /// underlying `jsonschema` crate's own text -- not required to match
    /// Circuitry's Python `jsonschema` text (see the crate docs).
    pub fn describe(&self) -> String {
        format!("{}: {}", self.location, self.message)
    }
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

fn load_schema(json_text: &str) -> Value {
    let schema = serde_json::from_str(json_text).expect("bundled schema file is valid JSON");
    normalize_pattern_schema(schema)
}

/// Rewrites every `"pattern"` keyword's value in `schema` so `fancy-regex`
/// agrees with Python `re` on the two narrow cases Circuitry's own
/// `NamePattern` exercises -- see the crate docs' `pattern` section. Applied
/// only to the in-memory schema used to build the [`jsonschema::Validator`];
/// the bundled schema *files* stay byte-identical to Circuitry's own copies
/// (`generate_schema_copy.py` checks that).
fn normalize_pattern_schema(mut schema: Value) -> Value {
    normalize_pattern_schema_in_place(&mut schema);
    schema
}

fn normalize_pattern_schema_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(pattern)) = map.get_mut("pattern") {
                *pattern = align_pattern_with_python_re(pattern);
            }
            for child in map.values_mut() {
                normalize_pattern_schema_in_place(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_pattern_schema_in_place(item);
            }
        }
        _ => {}
    }
}

/// `\d` -> the Unicode decimal-digit class, then a trailing unescaped `$`
/// -> "optionally followed by `\n`, then end": Python `re`'s own defaults
/// for a `str` pattern, neither of which `fancy-regex`/the `jsonschema`
/// crate's ECMA-262 translation (`ecma.rs`) applies.
fn align_pattern_with_python_re(pattern: &str) -> String {
    rewrite_trailing_dollar(&rewrite_digit_class(pattern))
}

/// Every unescaped `\d` token -> `\p{Nd}` (Unicode decimal digit, Python
/// `re`'s default for a `str` pattern). The `jsonschema` crate itself
/// rewrites a bare `\d` to ASCII-only `[0-9]` as part of its ECMA-262
/// translation (`ecma.rs`), which is why this has to happen before the
/// pattern reaches it rather than by any crate option.
fn rewrite_digit_class(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            if chars[i + 1] == 'd' {
                out.push_str("\\p{Nd}");
            } else {
                out.push(chars[i]);
                out.push(chars[i + 1]);
            }
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A trailing, unescaped `$` -> `\n?$`: Python `re`'s `$` matches just
/// before a single trailing `\n` as well as at the true end of the string;
/// `fancy-regex`'s does not. Leaves an escaped `\$` (a literal dollar sign)
/// alone.
fn rewrite_trailing_dollar(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    if chars.last() != Some(&'$') {
        return pattern.to_string();
    }
    let mut preceding_backslashes = 0;
    let mut i = chars.len() - 1;
    while i > 0 && chars[i - 1] == '\\' {
        preceding_backslashes += 1;
        i -= 1;
    }
    if preceding_backslashes % 2 == 1 {
        return pattern.to_string();
    }
    let mut out: String = chars[..chars.len() - 1].iter().collect();
    out.push_str("\\n?$");
    out
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
        jsonschema::draft7::options()
            .should_validate_formats(false)
            .build(orchestration_schema())
            .expect("bundled orchestration.schema.json compiles as Draft 7")
    })
}

fn profile_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::draft7::options()
            .should_validate_formats(false)
            .build(profile_schema())
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
            location: orchestration_location(document, err.instance_path.as_str()),
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
fn orchestration_location(document: &Value, instance_pointer: &str) -> String {
    let path = json_path_from_pointer(document, instance_pointer);
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
/// was originally an array index or a digit-string *property* key (both
/// render identically as pointer text) -- Circuitry's `interface.inputs`,
/// `interface.outputs` and `use.outputs` keys are free-form
/// (`additionalProperties`), so a key like `"1"` is a real, reachable
/// case, not a hypothetical one. `document` disambiguates: walked
/// alongside the pointer segments, a `Value::Array` node means the next
/// segment is an index, a `Value::Object` node means it's a property name
/// (quoted like any other, regardless of whether it happens to look like a
/// number).
fn json_path_from_pointer(document: &Value, pointer: &str) -> String {
    let mut path = String::from("$");
    let mut node = document;
    for raw_segment in pointer_segments(pointer) {
        let segment = unescape_json_pointer_segment(raw_segment);
        let is_array_index = node.is_array();
        node = match node {
            Value::Array(items) => segment
                .parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .unwrap_or(&Value::Null),
            Value::Object(map) => map.get(segment.as_str()).unwrap_or(&Value::Null),
            _ => &Value::Null,
        };
        if is_array_index {
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
        let doc = json!({"weird key": 1, "it's": 1, "effects": [{"each": 1}]});
        assert_eq!(json_path_from_pointer(&doc, "/weird key"), "$['weird key']");
        assert_eq!(json_path_from_pointer(&doc, "/it's"), "$['it\\'s']");
        assert_eq!(
            json_path_from_pointer(&doc, "/effects/0/each"),
            "$.effects[0].each"
        );
        assert_eq!(json_path_from_pointer(&doc, ""), "$");
    }

    #[test]
    fn digit_string_property_key_is_bracket_quoted_not_an_array_index() {
        let doc = json!({"interface": {"inputs": {"1": {"type": 5}}}, "effects": []});
        let errors = orchestration_errors(&doc);
        assert!(
            errors
                .iter()
                .any(|e| e.location == "interface.inputs['1'].type"),
            "{errors:?}"
        );
    }

    #[test]
    fn describe_matches_circuitrys_wrapping_format() {
        let error = SchemaError {
            location: "effects[0].name".to_string(),
            message: "boom".to_string(),
        };
        assert_eq!(error.describe(), "effects[0].name: boom");
        assert_eq!(error.to_string(), error.describe());
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
