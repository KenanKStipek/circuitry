//! Regression tests for PR #502's round-5 fix (issue #498's own
//! follow-up) -- each pinned against
//! the exact schema/instance shape `probe-schema9`/`probe-pct` record
//! against real `cof run`. Parsed through [`electricity_json::loads`],
//! not `serde_json::Value` (a `BTreeMap`-backed `Map`, alphabetical
//! regardless of the source text's own order -- several cases here
//! depend on the schema's own authored key order, the same reason
//! `prompt_schema_corpus.rs` parses that way).

use electricity_schema::{PromptSchemaCheck, validate_prompt_schema};

fn check(schema_json: &str, instance_json: &str) -> PromptSchemaCheck {
    let schema = electricity_json::loads(schema_json).expect("schema_json is valid JSON");
    let instance = electricity_json::loads(instance_json).expect("instance_json is valid JSON");
    validate_prompt_schema(&schema, &instance)
}

/// A `$ref` that doesn't resolve, once an instance actually
/// reaches it, is Python's own `referencing.exceptions.PointerToNowhere`
/// -- wrapped by `jsonschema`'s own `_WrappedReferencingError.__str__`
/// into `f"{type(cause).__name__}: {cause}"` -- never an ordinary
/// `jsonschema.ValidationError`, so `core/prompt.py::_validate_schema`
/// never catches it: a generic, non-retryable failure, never a silent
/// accept (probe-schema9 c0).
#[test]
fn a_ref_that_doesnt_resolve_once_reached_is_pointer_to_nowhere_not_a_silent_accept() {
    let schema = r##"{
        "type": "object",
        "properties": {"a": {"$ref": "#/$defs/item"}},
        "definitions": {"item": {"type": "integer"}}
    }"##;
    let result = check(schema, r#"{"a": 1}"#);
    match result {
        PromptSchemaCheck::SchemaError(message) => {
            assert!(
                message.starts_with("PointerToNowhere: '/$defs/item' does not exist within "),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected a PointerToNowhere SchemaError, got {other:?}"),
    }
}

/// `jsonschema.validate`'s own `check_schema` step validates
/// every `pattern`/`patternProperties` key against its draft's
/// metaschema *before* anything else, regardless of where in the
/// document it sits -- `\p{L}+` is genuinely Python-`re`-invalid (`bad
/// escape \p`), so this is always a `SchemaError`, never routed to
/// crate authority (whose own ECMA-262 regex engine silently accepts
/// it) and never a wrong accept (probe-schema9 c1).
#[test]
fn a_python_invalid_pattern_nested_under_properties_is_a_schema_error() {
    let schema = r#"{"properties": {"z": {"pattern": "\\p{L}+"}}}"#;
    assert_eq!(
        check(schema, r#"{"z": "abc"}"#),
        PromptSchemaCheck::SchemaError("'\\\\p{L}+' is not a 'regex'".to_string())
    );
}

/// Likewise a `(?<name>...)` named group -- Python's own `re`
/// only ever accepts `(?P<name>...)`.
#[test]
fn a_python_invalid_named_group_pattern_is_a_schema_error() {
    let schema = r#"{"properties": {"z": {"pattern": "(?<year>\\d{4})"}}}"#;
    assert_eq!(
        check(schema, r#"{"z": "2024"}"#),
        PromptSchemaCheck::SchemaError("'(?<year>\\\\d{4})' is not a 'regex'".to_string())
    );
}

/// A pattern that translates (Python's own `re.compile`
/// accepts it) but `fancy-regex` can't build at all -- a backreference
/// inside a lookbehind -- must never be silently treated as a match; it
/// is an explicit, documented failure instead (probe-schema9 c4,
/// `electricity-tools::regex`'s own module docs for the same gap).
#[test]
fn a_pattern_fancy_regex_cannot_build_is_an_explicit_failure_never_a_silent_match() {
    let schema = r#"{"properties": {"z": {"pattern": "(ab)(?<=\\1)"}}}"#;
    match check(schema, r#"{"z": "xyz"}"#) {
        PromptSchemaCheck::SchemaError(message) => {
            assert!(
                message.contains("cannot compile the pattern"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected a SchemaError, got {other:?}"),
    }
}

/// A pattern that exhausts `fancy-regex`'s own backtracking
/// limit -- even sized the same way `electricity-tools::regex` sizes its
/// own (`electricity_pyre::backtrack_limit_for`) -- must never be
/// silently treated as a match either (probe-schema9 c5: `[a-z]+$`
/// against 1500 `a`s plus a trailing `!`, which Python's own `re` --
/// no such limit at all -- rejects instantly).
#[test]
fn a_backtrack_limit_error_is_an_explicit_failure_never_a_silent_match() {
    let schema = r#"{"properties": {"z": {"pattern": "[a-z]+$"}}}"#;
    let instance = format!(r#"{{"z": "{}!"}}"#, "a".repeat(1500));
    match check(schema, &instance) {
        PromptSchemaCheck::SchemaError(_) | PromptSchemaCheck::Invalid(_) => {}
        PromptSchemaCheck::Valid => panic!("a non-matching instance must never be accepted"),
    }
}

/// `_keywords.multipleOf`'s own `Fraction` fallback --
/// `int(instance / dB)` overflows for a large `instance`/small `float`
/// divisor pair, caught only to retry exactly via `Fraction`, which is
/// exact and never overflows (probe-schema9 c6: `1e308` against
/// `multipleOf: 0.5` is a real multiple, since `2 * 1e308`'s own exact
/// binary value has no fractional part).
#[test]
fn multiple_of_overflow_falls_back_to_the_exact_fraction_check() {
    let schema = r#"{"multipleOf": 0.5}"#;
    let instance = electricity_value::Value::Float(1e308);
    assert_eq!(
        validate_prompt_schema(&electricity_json::loads(schema).unwrap(), &instance),
        PromptSchemaCheck::Valid
    );
}

/// Each `patternProperties` key can translate fine on its
/// own while the `"|"`-joined alternation `additionalProperties`'s own
/// filter builds from all of them together is genuinely Python-invalid
/// (a global inline flag, `(?i)`, must lead the *whole* expression, not
/// reappear after the first alternative) -- never silently treated as
/// "doesn't match", which would wrongly report `schema_invalid` instead
/// of the real, non-retryable failure (probe-schema9 c7).
#[test]
fn a_joined_pattern_properties_alternation_that_fails_to_compile_is_a_schema_error() {
    let schema = r#"{
        "patternProperties": {"(?i)^x-": {}, "(?i)^y-": {}},
        "additionalProperties": false
    }"#;
    assert_eq!(
        check(schema, r#"{"zz": 1}"#),
        PromptSchemaCheck::SchemaError(
            "global flags not at the start of the expression at position 8".to_string()
        )
    );
}

/// `Validator.is_valid` (`not`/`if`/`contains`'s own
/// building block) is `next(iter_errors(instance), None) is None` --
/// lazy, pulling at most *one* error from the entire nested generator
/// tree. `type` is `5`'s own only violation here, and it comes first:
/// `$ref: "#"` is never even reached, so no recursion ever happens
/// either (`not` is therefore satisfied -- Python's own real result --
/// not electricity's former eager `is_valid`, which walked `$ref` anyway
/// and hit the real cycle-detection fallback instead).
#[test]
fn not_with_a_ref_sibling_a_prior_keyword_already_failed_never_reaches_it() {
    let schema = r##"{"not": {"type": "string", "$ref": "#"}}"##;
    assert_eq!(check(schema, "5"), PromptSchemaCheck::Valid);
}

/// A `$ref` fragment containing a `%` immediately followed
/// by a multi-byte UTF-8 character used to byte-slice the original
/// `&str` by an offset landing inside that character's own encoding,
/// panicking -- never reached now that percent-decoding works on raw
/// bytes throughout (the pointer still doesn't resolve, so this is a
/// `PointerToNowhere` `SchemaError`, never a crash).
#[test]
fn a_ref_fragment_with_a_percent_before_a_multibyte_character_never_panics() {
    let schema = r##"{"$ref": "#/$defs/%1\u00e9"}"##;
    match check(schema, "1") {
        PromptSchemaCheck::SchemaError(_) => {}
        other => panic!("expected a SchemaError (dangling ref), got {other:?}"),
    }
}

/// The same, randomized: an arbitrary `$ref` fragment -- percent
/// signs, multi-byte UTF-8, JSON Pointer escapes, all mixed together in
/// any order -- must never panic, whatever it resolves to (or doesn't).
#[test]
fn ref_resolution_never_panics_on_an_arbitrary_fragment() {
    let alphabet = ['%', '1', '2', 'f', 'F', '~', '0', '/', '#', 'é', '日'];
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let len = (next() % 12) as usize;
        let fragment: String = (0..len)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let schema_json = format!("{{\"$ref\": \"#{fragment}\"}}");
        let schema = electricity_json::loads(&schema_json).expect("schema_json is valid JSON");
        let instance = electricity_value::Value::Int(1i64.into());
        let _ = validate_prompt_schema(&schema, &instance);
    }
}
