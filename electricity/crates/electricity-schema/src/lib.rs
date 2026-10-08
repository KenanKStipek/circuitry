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
//! text it wraps, which is the carved-out part). One piece of
//! `_describe_schema_error`'s own output is applied here too, because it
//! transforms that third-party message text rather than changing the
//! wrapping format: the `repr(err.instance)` -> `"value"` substitution
//! ([`substitute_value_prefix`]) -- `jsonschema` 0.26 starts its own
//! `type`/`enum`/`anyOf`/`oneOf` messages with `self.instance` rendered as
//! JSON (`error.rs`'s `Display` impl in that crate) exactly where Python's
//! message starts with `repr(err.instance)`, so the same prefix-replace
//! applies even though the two renderings are spelled differently. The
//! best-matching `oneOf`/`anyOf` sub-error suffix is **not** replicated and
//! has no known-gap workaround short of re-validating every branch by hand:
//! `jsonschema` 0.26's `ValidationErrorKind::OneOfNotValid`/`AnyOf` carry no
//! sub-error list to pick from (`error.rs` in that crate). `DESIGN.md` §4
//! step 2.2 and `docs/spec/runtime-semantics.md`'s "deepest `oneOf`
//! sub-error appended" line describe Circuitry's own compile-time pipeline,
//! which this crate's caller (not this crate) is responsible for keeping
//! accurate against that gap.
//!
//! Two pieces of wrapping *text* this crate deliberately does not produce,
//! because they belong to callers this crate doesn't have (and doesn't
//! depend on): the `"Orchestration validation failed:\n  - ..."` prefix
//! `DESIGN.md` §4 step 2 gives the compiler's structural-checks gate, and
//! the `"Profile {name!r} at {path} failed schema validation:"` header
//! plus its `"  - {location}: {message}"` lines `cli.profiles`'
//! `_validate_profile_schema` writes (`DESIGN.md` §6.11). Both callers are
//! meant to build that text themselves from [`SchemaError`]'s `location`
//! and `message` (or [`SchemaError::describe`] for the per-line part) --
//! the compiler lane (`DESIGN.md` §4 step 2, milestone M0-G) and the
//! profile-loading lane (`DESIGN.md` §6.11, milestone M1-I) respectively,
//! neither of which exists yet. A byte-exact header also can't be produced
//! from this crate's output alone even once a caller exists: Python sorts
//! profile errors by `str(err)` (`cli/profiles.py`), which is third-party
//! `jsonschema` text this crate never has to match, so the *line order* of
//! a multi-error profile failure is not reproducible from `SchemaError`
//! data regardless of which crate assembles the header.
//!
//! `pattern` validation also gets two narrow, deliberate adjustments so
//! `fancy-regex` agrees with Python's `re` on the exact cases Circuitry's
//! own schemas exercise ([`normalize_pattern_schema`]): `$` is rewritten to
//! match just before a single trailing `\n` too (Python `re`'s default,
//! `fancy-regex`'s is not), and `\d` is rewritten to the Unicode decimal
//! digit class (Python `re`'s Unicode-aware default; the `jsonschema` crate
//! itself rewrites a bare `\d` to ASCII `[0-9]` as part of its ECMA-262
//! translation, `ecma.rs`). Only the schema's own `"pattern"` keyword value
//! is rewritten -- not a `"pattern"` string that happens to appear inside
//! `enum`/`const`/`default`/`examples` *data*, which is walked but left
//! untouched as opaque instance data, never a sub-schema.
//!
//! Nothing else about `fancy-regex` vs Python `re` is adjusted: `\D`, `\w`,
//! `\W`, `\s`, `\S`, `\b`, `\B`, a `$` that isn't the pattern's last
//! character, inline flags, and `patternProperties` keys all differ between
//! the two engines in ways this crate does not correct, and both engines
//! *support* every one of these constructs -- they just disagree on what it
//! means, a materially different, more dangerous gap than a construct one
//! engine lacks entirely, because it fails silently (both sides accept a
//! pattern, but disagree on some inputs) rather than loudly. None of
//! Circuitry's bundled schemas uses any of them today; the unit test
//! `bundled_schema_patterns_use_only_corrected_constructs` fails the build
//! the moment one does, so a future schema edit can't silently start
//! disagreeing between engines even though `generate_schema_copy.py --check`
//! would still pass.

use std::sync::OnceLock;

use jsonschema::paths::{LazyLocation, Location};
use jsonschema::primitive_type::PrimitiveType;
use jsonschema::{Keyword, ValidationError};
use serde_json::{Map, Number, Value};

/// The marker key `electricity-compiler`'s `schema_instance.rs` wraps a
/// `Date`/`DateTime`/`Bytes`/`NaN`/`Infinity` value's text under,
/// standing in for a Python value with no `serde_json::Value` shape of
/// its own -- defined here, not there, because [`type_keyword_factory`]
/// (this crate's own override of the `"type"` keyword, registered on
/// [`orchestration_validator`]) needs to recognize it at validation time,
/// and [`json_path_from_pointer`] needs to decode [`NON_STRING_KEY_PREFIX`]
/// back into Python's own `json_path` rendering for a non-string `Dict`
/// key. See `electricity-compiler`'s own module docs for why a dedicated
/// marker is needed at all.
pub const NON_JSON_SCALAR_MARKER: &str = "$circuitry_non_json_scalar";

/// Set (to `true`) on a [`non_json_scalar`] marker that stands in for a
/// Python value `jsonschema`'s own `"number"` type keyword would still
/// accept -- `NaN`, `Infinity` and `-Infinity` all three (Python's own
/// `isinstance(x, float)` is `True` for every one of them; see
/// `schema_instance.rs`'s own `float_to_json_number`) -- never set for a
/// `Date`/`DateTime`/`Bytes` marker, which no JSON Schema `"type"` should
/// ever accept.
pub const NON_JSON_SCALAR_NUMBER_LIKE: &str = "$circuitry_non_json_scalar_number_like";

/// Prefixes a non-string `Dict` key's stand-in string (a NUL byte, which
/// cannot appear in a YAML/JSON *text* key any real document writes,
/// followed by the key's own `repr()`-style text) so a `"properties"`/
/// `"required"` check against a real (string) property name behaves
/// exactly as Python's own `dict.get`/`in` would against a key that
/// compares unequal to every string.
pub const NON_STRING_KEY_PREFIX: &str = "\u{0}non_string_key:";

/// Builds the marker [`NON_JSON_SCALAR_MARKER`] stands for -- see its own
/// doc comment and [`NON_JSON_SCALAR_NUMBER_LIKE`]'s.
pub fn non_json_scalar(label: &str, number_like: bool) -> Value {
    let mut map = Map::with_capacity(2);
    map.insert(
        NON_JSON_SCALAR_MARKER.to_string(),
        Value::String(label.to_string()),
    );
    if number_like {
        map.insert(NON_JSON_SCALAR_NUMBER_LIKE.to_string(), Value::Bool(true));
    }
    Value::Object(map)
}

/// `Some(number_like)` iff *instance* is a [`non_json_scalar`] marker;
/// `None` for an ordinary value, including a real document object that
/// merely happens to carry [`NON_JSON_SCALAR_MARKER`] as one property
/// name among others of its own -- a marker object only ever carries
/// exactly that key alone, or that key plus exactly
/// [`NON_JSON_SCALAR_NUMBER_LIKE`] and nothing else, so both the size
/// and the second key's own name (not just its count) are checked.
fn scalar_marker_number_like(instance: &Value) -> Option<bool> {
    let map = instance.as_object()?;
    if !map.contains_key(NON_JSON_SCALAR_MARKER) {
        return None;
    }
    match map.len() {
        1 => Some(false),
        2 if map.contains_key(NON_JSON_SCALAR_NUMBER_LIKE) => Some(
            map.get(NON_JSON_SCALAR_NUMBER_LIKE)
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        _ => None,
    }
}

/// Builds [`NON_STRING_KEY_PREFIX`]'s marker for a `Dict` key whose own
/// `repr()`-style text is *repr_text*.
pub fn non_string_key_marker(repr_text: &str) -> String {
    format!("{NON_STRING_KEY_PREFIX}{repr_text}")
}

/// `true` iff *key* is [`non_string_key_marker`]'s stand-in for a `Dict`
/// key that wasn't a string.
pub fn is_non_string_key_marker(key: &str) -> bool {
    key.starts_with(NON_STRING_KEY_PREFIX)
}

/// Overrides the built-in `"type"` keyword (registered by name --
/// `compiler.rs`'s own keyword dispatch checks a caller-registered
/// factory before the standard definitions, so this *replaces* rather
/// than supplements it) so a [`non_json_scalar`] marker is checked
/// against Python's own `isinstance` verdict instead of whichever
/// `serde_json::Value` variant the marker happens to use: it fails
/// every declared type except -- for a number-like marker (`NaN`/
/// `Infinity`/`-Infinity`) -- `"number"` itself (which the marker's own
/// JSON-object shape would otherwise always fail, same as it would
/// otherwise always *pass* `"object"` -- Python's `isinstance(value,
/// dict)` is `False` for every one of these values either way).
/// Reimplements ordinary `"type"` semantics for every non-marker value
/// instead of delegating to it, since the override replaces the
/// built-in keyword wholesale rather than running alongside it -- both
/// `jsonschema`'s own `type_.rs` validators that back it are
/// `pub(crate)`, not reachable from here.
struct TypeValidator {
    types: Vec<PrimitiveType>,
    location: Location,
}

impl Keyword for TypeValidator {
    fn validate<'i>(
        &self,
        instance: &'i Value,
        location: &LazyLocation,
    ) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            let wanted = self
                .types
                .iter()
                .map(PrimitiveType::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Err(ValidationError::custom(
                self.location.clone(),
                location.into(),
                instance,
                format!("{instance} is not of type {wanted}"),
            ))
        }
    }

    fn is_valid(&self, instance: &Value) -> bool {
        match scalar_marker_number_like(instance) {
            Some(number_like) => number_like && self.types.contains(&PrimitiveType::Number),
            None => self
                .types
                .iter()
                .any(|t| primitive_type_matches(*t, instance)),
        }
    }
}

/// `jsonschema::keywords::type_::is_integer`, reimplemented here since
/// that one is `pub(crate)` in the `jsonschema` crate: a `Number` with
/// no fractional part, `u64`/`i64`-representable or not.
fn is_integer_number(n: &Number) -> bool {
    n.is_u64() || n.is_i64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
}

fn primitive_type_matches(declared: PrimitiveType, instance: &Value) -> bool {
    match declared {
        PrimitiveType::Null => instance.is_null(),
        PrimitiveType::Boolean => instance.is_boolean(),
        PrimitiveType::Object => instance.is_object(),
        PrimitiveType::Array => instance.is_array(),
        PrimitiveType::String => instance.is_string(),
        PrimitiveType::Number => instance.is_number(),
        PrimitiveType::Integer => matches!(instance, Value::Number(n) if is_integer_number(n)),
    }
}

// `ValidationOptions::with_keyword`'s own factory signature (jsonschema
// 0.26), not ours to shrink -- `Err` is only ever reached if a future
// draft starts rejecting "type"'s own value shape, which the bundled
// schema never does.
#[allow(clippy::result_large_err)]
fn type_keyword_factory<'a>(
    _parent: &'a Map<String, Value>,
    value: &'a Value,
    path: Location,
) -> Result<Box<dyn Keyword>, ValidationError<'a>> {
    let types: Vec<PrimitiveType> = match value {
        Value::String(s) => PrimitiveType::try_from(s.as_str()).into_iter().collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|s| PrimitiveType::try_from(s).ok())
            .collect(),
        _ => Vec::new(),
    };
    Ok(Box::new(TypeValidator {
        types,
        location: path,
    }))
}

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

/// Keys whose value is literal instance *data* (an `enum` member list, a
/// `const`/`default` value, `examples`), never a sub-schema -- walked no
/// further, so a `"pattern"` key that happens to appear inside one of
/// these (as ordinary JSON data, not the schema keyword) is never mistaken
/// for a regex to rewrite.
const SCHEMA_DATA_KEYS: [&str; 4] = ["enum", "const", "default", "examples"];

fn normalize_pattern_schema_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(pattern)) = map.get_mut("pattern") {
                *pattern = align_pattern_with_python_re(pattern);
            }
            for (key, child) in map.iter_mut() {
                if SCHEMA_DATA_KEYS.contains(&key.as_str()) {
                    continue;
                }
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

/// Every `pattern` keyword value and every `patternProperties` key in
/// `schema`, collected the same way [`normalize_pattern_schema_in_place`]
/// walks it (skipping [`SCHEMA_DATA_KEYS`]) -- used only by the guard test
/// that keeps a future schema edit from silently using a regex construct
/// `align_pattern_with_python_re` doesn't correct.
#[cfg(test)]
fn collect_patterns_for_guard(value: &Value, patterns: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(pattern)) = map.get("pattern") {
                patterns.push(pattern.clone());
            }
            if let Some(Value::Object(pattern_properties)) = map.get("patternProperties") {
                patterns.extend(pattern_properties.keys().cloned());
            }
            for (key, child) in map {
                if SCHEMA_DATA_KEYS.contains(&key.as_str()) {
                    continue;
                }
                collect_patterns_for_guard(child, patterns);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_patterns_for_guard(item, patterns);
            }
        }
        _ => {}
    }
}

/// `None` when `pattern` uses only constructs
/// [`align_pattern_with_python_re`] corrects (or needs no correction for);
/// otherwise names the uncorrected Python-`re`-vs-`fancy-regex` divergence
/// it found -- `\D`/`\w`/`\W`/`\s`/`\S`/`\b`/`\B` (`ecma.rs` in the
/// `jsonschema` crate rewrites these to fixed ASCII/odd sets, Python's own
/// are Unicode-aware), a `$` that isn't the pattern's last character (only
/// a trailing `$` is rewritten), or an inline flag group like `(?i)`
/// (`fancy-regex` and Python `re` support different flag letters).
#[cfg(test)]
fn uncorrected_regex_divergence(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let escaped = chars[i + 1];
            if matches!(escaped, 'D' | 'w' | 'W' | 's' | 'S' | 'b' | 'B') {
                return Some(format!("\\{escaped}"));
            }
            i += 2;
            continue;
        }
        if chars[i] == '$' && i != chars.len() - 1 {
            return Some("$ that is not the pattern's last character".to_string());
        }
        if chars[i] == '('
            && i + 2 < chars.len()
            && chars[i + 1] == '?'
            && matches!(chars[i + 2], 'i' | 'm' | 's' | 'x' | 'a' | 'L' | 'u')
        {
            return Some("an inline flag group".to_string());
        }
        i += 1;
    }
    None
}

fn orchestration_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| load_schema(include_str!("../schema/orchestration.schema.json")))
}

fn profile_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| load_schema(include_str!("../schema/profile.schema.json")))
}

/// The `"properties"` key names of `$defs.<def_name>` in the bundled
/// `orchestration.schema.json`, or `None` if `def_name` isn't defined
/// there -- generic schema introspection for a caller that needs to
/// know a schema definition's own key set without re-parsing the file
/// itself (`core.document_check`'s per-effect-type known-key table is
/// Circuitry's own business logic, built from this).
pub fn orchestration_def_properties(def_name: &str) -> Option<std::collections::BTreeSet<String>> {
    let defs = orchestration_schema().get("$defs")?.as_object()?;
    let properties = defs.get(def_name)?.get("properties")?.as_object()?;
    Some(properties.keys().cloned().collect())
}

/// The top-level `"properties"` key names of the bundled
/// `orchestration.schema.json`.
pub fn orchestration_top_level_properties() -> std::collections::BTreeSet<String> {
    orchestration_schema()
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

fn orchestration_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::draft7::options()
            .should_validate_formats(false)
            .with_keyword("type", type_keyword_factory)
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
        .filter(|err| !is_spurious_marker_error(err))
        .map(|err| SchemaError {
            location: orchestration_location(document, err.instance_path.as_str()),
            message: substitute_value_prefix(&err.instance, err.to_string()),
        })
        .collect()
}

/// `true` iff *err* is one of the object-shape keywords (`required`,
/// `additionalProperties`, `maxProperties`, `minProperties`,
/// `propertyNames`, `unevaluatedProperties`) firing against a
/// [`non_json_scalar`] marker standing in for a `Date`/`DateTime`/`Bytes`/
/// `NaN`/`Infinity` value -- a marker is a real `serde_json::Value::Object`
/// (so these validators, which the `jsonschema` crate runs unconditionally
/// against any JSON object instance, see it as one), but Python's own
/// `jsonschema` validators for every one of these keywords return
/// immediately for a non-`dict` instance (`validator.is_type(instance,
/// "object")` is `False` for a `date`/`bytes`/`float`), so Python never
/// raises any of them for a value these keywords were never meant to see.
/// The overridden `"type"` keyword ([`type_keyword_factory`]) already
/// reports the one error a marker *should* produce at this position (or
/// none, at a position a number-like marker's `"number"` type legitimately
/// reaches); every other keyword's own verdict (`enum`, `const`, `oneOf`,
/// `anyOf`, ...) still runs unfiltered, since none of those are gated on
/// the instance being an object the way these are.
fn is_spurious_marker_error(err: &ValidationError<'_>) -> bool {
    use jsonschema::error::ValidationErrorKind;
    if scalar_marker_number_like(&err.instance).is_none() {
        return false;
    }
    matches!(
        err.kind,
        ValidationErrorKind::AdditionalProperties { .. }
            | ValidationErrorKind::Required { .. }
            | ValidationErrorKind::MaxProperties { .. }
            | ValidationErrorKind::MinProperties { .. }
            | ValidationErrorKind::PropertyNames { .. }
            | ValidationErrorKind::UnevaluatedProperties { .. }
    )
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
            message: substitute_value_prefix(&err.instance, err.to_string()),
        })
        .collect()
}

/// `core.document_check._describe_schema_error`'s `repr(err.instance)` ->
/// `"value"` substitution: when the failing instance is an object or array
/// and `message` starts with that instance rendered as JSON (`jsonschema`
/// 0.26's `Display` impl for `type`/`enum`/`anyOf`/`oneOf` messages starts
/// exactly this way -- `error.rs` in that crate), replace the rendering
/// with `"value"` so the path (already named by `location`) isn't repeated
/// as a dump of the whole failing object, matching Python's own rule for
/// when it drops `repr(err.instance)` the same way.
fn substitute_value_prefix(instance: &Value, message: String) -> String {
    if !matches!(instance, Value::Object(_) | Value::Array(_)) {
        return message;
    }
    let rendered = instance.to_string();
    match message.strip_prefix(rendered.as_str()) {
        Some(rest) => format!("value{rest}"),
        None => message,
    }
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
        } else if let Some(repr_text) = non_string_key_repr(&segment) {
            // Python's `json_path` checks `isinstance(elem, int)` before
            // the string-pattern branch -- true for `bool` too, a
            // subclass of `int` -- so a non-string `Dict` key that was an
            // `int`/`bool` renders the same bracket-without-quotes way an
            // array index does (`1:` -> `[1]`, `yes:` -> `[True]`), not
            // the quoted-string form below.
            path.push('[');
            path.push_str(&repr_text);
            path.push(']');
        } else if is_json_path_compatible_property(&segment) {
            path.push('.');
            path.push_str(&segment);
        } else {
            // Strips the marker prefix first, if any, so the fallback
            // for a non-int/bool non-string key (float, `None`, a
            // date/datetime, bytes -- where Python itself raises
            // `TypeError`, see `non_string_key_repr`'s own doc comment)
            // still quotes the key's own `repr()` text rather than the
            // marker's internal NUL-prefixed form.
            let display = segment
                .strip_prefix(NON_STRING_KEY_PREFIX)
                .unwrap_or(&segment);
            path.push_str("['");
            path.push_str(&display.replace('\\', "\\\\").replace('\'', "\\'"));
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

/// `Some(repr_text)` iff *segment* is [`non_string_key_marker`]'s stand-in
/// for a `Dict` key that was an `int` or `bool` (Python's own `json_path`
/// checks `isinstance(elem, int)`, true for `bool` too, before its
/// string-pattern branch) -- `None` for a string key, or for one of the
/// other hashable-but-non-string/int/bool key types YAML can still
/// produce (a float, `None`, a date/datetime, bytes): Python's own
/// `json_path` raises `TypeError` for those (`elem.replace` on a
/// non-`str`), collapsing the whole structural check into one error
/// rather than this location's own -- a narrow, documented divergence
/// (`electricity-compiler`'s own module docs) this crate doesn't
/// reproduce; the caller instead gets this location's own best-effort
/// quoted-bracket rendering of the key's `repr()` text, non-empty and at
/// the right node, just not Python's own crash-shaped result.
fn non_string_key_repr(segment: &str) -> Option<String> {
    let repr_text = segment.strip_prefix(NON_STRING_KEY_PREFIX)?;
    let is_int_repr = {
        let digits = repr_text.strip_prefix('-').unwrap_or(repr_text);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    };
    if repr_text == "True" || repr_text == "False" || is_int_repr {
        Some(repr_text.to_string())
    } else {
        None
    }
}

/// `jsonschema.exceptions._JSON_PATH_COMPATIBLE_PROPERTY_PATTERN`:
/// `^[a-zA-Z][a-zA-Z0-9_]*$` (note: no leading underscore, unlike
/// Circuitry's own orchestration `NamePattern` -- this is Python
/// `jsonschema`'s own, unrelated pattern). Python `re`'s unescaped `$`
/// (used unanchored by `.match`, i.e. anywhere a prefix match succeeds)
/// also matches just before a single trailing `\n`, so a property key
/// ending in exactly one `\n` is still path-compatible on the Python side;
/// strip one before checking the rest, the same rule `align_pattern_with_python_re`
/// bakes into `pattern` validation itself.
fn is_json_path_compatible_property(segment: &str) -> bool {
    let trimmed = segment.strip_suffix('\n').unwrap_or(segment);
    let mut chars = trimmed.chars();
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
    fn a_real_object_carrying_the_marker_key_and_an_unrelated_second_key_is_not_a_marker() {
        // F7 (second-round review): the marker key alone, or paired with
        // exactly `NON_JSON_SCALAR_NUMBER_LIKE`, is a marker -- any other
        // second key means it's a real document object that happens to
        // reuse the marker's own property name, not a stand-in.
        let real_object = json!({
            NON_JSON_SCALAR_MARKER: "x",
            "some_other_key": true,
        });
        assert_eq!(scalar_marker_number_like(&real_object), None);

        let marker_alone = json!({NON_JSON_SCALAR_MARKER: "x"});
        assert_eq!(scalar_marker_number_like(&marker_alone), Some(false));

        let number_like_marker =
            json!({NON_JSON_SCALAR_MARKER: "x", NON_JSON_SCALAR_NUMBER_LIKE: true});
        assert_eq!(scalar_marker_number_like(&number_like_marker), Some(true));
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

    #[test]
    fn rewrite_digit_class_leaves_an_escaped_backslash_then_d_alone() {
        assert_eq!(rewrite_digit_class(r"\\d"), r"\\d");
    }

    #[test]
    fn rewrite_trailing_dollar_leaves_an_escaped_dollar_alone() {
        assert_eq!(rewrite_trailing_dollar(r"a\$"), r"a\$");
    }

    #[test]
    fn rewrite_trailing_dollar_rewrites_past_an_escaped_backslash() {
        assert_eq!(rewrite_trailing_dollar(r"a\\$"), "a\\\\\\n?$");
    }

    #[test]
    fn value_prefix_is_substituted_for_a_failing_object_instance() {
        let doc = json!({"effects": [{}]});
        let errors = orchestration_errors(&doc);
        let oneof_error = errors
            .iter()
            .find(|e| e.location == "effects[0]" && e.message.starts_with("value "))
            .expect("one error at effects[0] starts with the substituted value prefix");
        assert!(!oneof_error.message.contains('{'));
    }

    #[test]
    fn property_key_ending_in_single_newline_still_uses_dotted_form() {
        let doc = json!({
            "interface": {"inputs": {"abc\n": {"type": 5}}},
            "effects": []
        });
        let errors = orchestration_errors(&doc);
        let locations: Vec<&str> = errors.iter().map(|e| e.location.as_str()).collect();
        assert!(
            locations.contains(&"interface.inputs.abc\n.type"),
            "{locations:?}"
        );
    }

    #[test]
    fn bundled_schema_patterns_use_only_corrected_constructs() {
        let schemas: [&str; 3] = [
            include_str!("../schema/orchestration.schema.json"),
            include_str!("../schema/profile.schema.json"),
            include_str!("../schema/curation-manifest.schema.json"),
        ];
        for text in schemas {
            let schema: Value = serde_json::from_str(text).expect("bundled schema is valid JSON");
            let mut patterns = Vec::new();
            collect_patterns_for_guard(&schema, &mut patterns);
            for pattern in &patterns {
                assert_eq!(
                    uncorrected_regex_divergence(pattern),
                    None,
                    "pattern {pattern:?} uses a Python-re-vs-fancy-regex divergence this crate doesn't correct"
                );
            }
        }
    }
}
