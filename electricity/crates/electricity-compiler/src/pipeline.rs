//! Lane B: `check_report`/`check_for_run`, matching
//! `cli/runtime_shim.py`'s `validate(...)` and
//! `run(RunRequest(..., validate_only=True))` -- the ordering of
//! structural checks, concurrency-configuration errors, compilation,
//! and the compile-time half of #406, and the two surfaces' different
//! error shapes.
//!
//! The two surfaces check the concurrency configuration at different
//! points, matching Circuitry's own asymmetry exactly (confirmed
//! directly against `cli/runtime_shim.py`): `validate()` only parses
//! `runtime.max_concurrency`/`concurrency_groups` *after* a document
//! compiles, and reports a bare, unwrapped error list
//! ([`concurrency_config_errors`]'s own messages, unprefixed) --
//! `run()` instead builds a `RunConcurrencyLimiter` from the merged
//! runtime config *before* the document even loads its structural
//! checks, and on failure raises a single message of its own,
//! `"Invalid runtime concurrency configuration:\n  - ..."`, wrapped as
//! a [`CompileError`]-shaped passthrough (`RunCheckError::Compile`)
//! since that's exactly the shape `str(e)` from Circuitry's own broad
//! `except Exception` would produce for it -- not
//! [`RunCheckError::Structural`], which is reserved for the
//! `"Orchestration validation failed:"`-prefixed shape structural and
//! concurrency-*group* errors share.

use crate::{
    CheckOptions, CheckReport, DocumentOrigin, RunCheckError, compile_document, cycles, digest,
    groups, load, load_document, structural_errors, unknown_key_warnings,
};
use electricity_bytecode::{DocumentInfo, Program};
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `runtime:` keys a document may set without the "Applied host
/// settings" notice naming them -- `cli/effective_settings.py`'s
/// `ORCHESTRATION_RUNTIME_KEYS`.
const DOCUMENT_RUNTIME_KEYS: [&str; 2] = ["complexity", "state"];

/// `cli/effective_settings.py`'s `_NOTICE_KEY_DEPTH`.
const NOTICE_KEY_DEPTH: usize = 2;

fn runtime_block(document: &Value) -> Option<&Value> {
    document.as_dict()?.get(&Value::Str("runtime".to_string()))
}

/// The document's `runtime:` block merged over *options*' config-file
/// runtime block, document key winning (issue #408's CLI section:
/// "Merge the document's runtime: block over the config file's, key by
/// key" -- electricity trusts every document, so there is no
/// untrusted-split branch to reproduce here, only the merge itself).
/// Corpus cases never carry a `config_runtime` (Circuitry's own
/// ground truth always calls `validate`/`run` with `config=None`), so
/// this reduces to the document's own `runtime:` block verbatim for
/// every golden case -- the config-file merge is exercised by the CLI
/// alone.
fn merged_runtime_block(options: &CheckOptions, document: &Value) -> Option<Value> {
    let document_runtime = runtime_block(document);
    match (&options.config_runtime, document_runtime) {
        (None, None) => None,
        (Some(config), None) => Some(config.clone()),
        (None, Some(doc_runtime)) => Some(doc_runtime.clone()),
        (Some(config), Some(doc_runtime)) => {
            let mut merged = config.as_dict().cloned().unwrap_or_default();
            if let Some(doc_dict) = doc_runtime.as_dict() {
                for (key, value) in doc_dict {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Some(Value::Dict(merged))
        }
    }
}

/// `core/concurrency.py::parse_max_concurrency`'s own error, if any.
fn max_concurrency_error(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::None) => None,
        Some(Value::Int(i)) if !i.is_negative() && !i.is_zero() => None,
        Some(other) => Some(format!(
            "runtime.max_concurrency must be a positive integer, got {}.",
            other.py_repr()
        )),
    }
}

/// `core/concurrency.py::parse_concurrency_groups`: the valid group
/// names (`frozenset(groups)`, used as `groups::unknown_group_errors`'s
/// `known_groups`) and the configuration errors.
fn parse_concurrency_groups(value: Option<&Value>) -> (BTreeSet<String>, Vec<String>) {
    let mut names = BTreeSet::new();
    let mut errors = Vec::new();
    let dict = match value {
        None | Some(Value::None) => return (names, errors),
        Some(Value::Dict(d)) => d,
        Some(other) => {
            errors.push(format!(
                "runtime.concurrency_groups must be a mapping of group name to a positive \
                 integer limit, got {}.",
                crate::structural::py_class_name(other)
            ));
            return (names, errors);
        }
    };
    for (name, limit) in dict {
        let name_str = match name {
            Value::Str(s) if !s.trim().is_empty() => s,
            _ => {
                errors.push(format!(
                    "runtime.concurrency_groups has a non-string or empty group name: {}.",
                    name.py_repr()
                ));
                continue;
            }
        };
        match limit {
            Value::Int(i) if !i.is_negative() && !i.is_zero() => {
                names.insert(name_str.clone());
            }
            other => errors.push(format!(
                "runtime.concurrency_groups.{name_str} must be a positive integer, got {}.",
                other.py_repr()
            )),
        }
    }
    (names, errors)
}

/// `runtime.max_concurrency`/`runtime.concurrency_groups` configuration
/// errors (`core/concurrency.py`'s `parse_max_concurrency`/
/// `parse_concurrency_groups`), checked against the merged config
/// before a document even compiles -- not an effect's `group:`
/// reference into it (that's `groups::unknown_group_errors`, lane C).
pub(crate) fn concurrency_config_errors(runtime_block: Option<&Value>) -> Vec<String> {
    let dict = runtime_block.and_then(Value::as_dict);
    let max_concurrency = dict.and_then(|d| d.get(&Value::Str("max_concurrency".to_string())));
    let concurrency_groups =
        dict.and_then(|d| d.get(&Value::Str("concurrency_groups".to_string())));
    let mut errors: Vec<String> = max_concurrency_error(max_concurrency).into_iter().collect();
    errors.extend(parse_concurrency_groups(concurrency_groups).1);
    errors
}

fn concurrency_group_names(runtime_block: Option<&Value>) -> BTreeSet<String> {
    let dict = runtime_block.and_then(Value::as_dict);
    let concurrency_groups =
        dict.and_then(|d| d.get(&Value::Str("concurrency_groups".to_string())));
    parse_concurrency_groups(concurrency_groups).0
}

/// `cli/effective_settings.py::_key_label`: `str(key)`, since every
/// key this notice ever names (a `runtime:`/nested-dict key, a
/// `plugins:` entry) is ordinary document data, never one of the
/// control-character edge cases `_key_label`'s own `isprintable()`
/// guard exists for.
fn key_label(key: &Value) -> String {
    match key {
        Value::Str(s) => s.clone(),
        other => other.py_str(),
    }
}

/// `cli/effective_settings.py::_key_paths`.
fn key_paths(prefix: &str, value: &Value, depth: usize) -> Vec<String> {
    if depth == 0 {
        return vec![prefix.to_string()];
    }
    match value.as_dict() {
        Some(dict) if !dict.is_empty() => dict
            .iter()
            .flat_map(|(key, sub)| {
                key_paths(&format!("{prefix}.{}", key_label(key)), sub, depth - 1)
            })
            .collect(),
        _ => vec![prefix.to_string()],
    }
}

/// `cli/effective_settings.py::orchestration_host_setting_warnings`'s
/// trusted branch (`_applied_host_settings_notice`) -- the only branch
/// electricity ever reaches, since both [`check_report`] and
/// [`check_for_run`] always trust the document (DESIGN.md §11) and
/// never carry a host `plugins`/`enabled_plugins` allowlist of their
/// own (`cfg or CircuitryConfig()` with `config=None`, Circuitry's own
/// ground truth for every golden case): the untrusted split
/// (`_split_orchestration_runtime`/`_split_orchestration_plugins`) and
/// the ceiling-intersection notice suffix (`_ceiling_intersected_
/// paths`, always empty here since there is no host `runtime.plugins.
/// shell.allowed_commands`-style pin to intersect against) are both
/// unreachable from either surface and are not ported.
fn host_setting_warnings(document: &Value, document_name: &str) -> Vec<String> {
    let dict = document.as_dict();
    let orch_plugins = dict
        .and_then(|d| d.get(&Value::Str("plugins".to_string())))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    let orch_runtime = dict
        .and_then(|d| d.get(&Value::Str("runtime".to_string())))
        .and_then(Value::as_dict);

    let mut runtime_paths = Vec::new();
    if let Some(runtime) = orch_runtime {
        for (key, value) in runtime {
            let key_str = match key {
                Value::Str(s) => s.clone(),
                other => other.py_str(),
            };
            if DOCUMENT_RUNTIME_KEYS.contains(&key_str.as_str()) {
                continue;
            }
            runtime_paths.extend(key_paths(
                &format!("runtime.{}", key_label(key)),
                value,
                NOTICE_KEY_DEPTH,
            ));
        }
    }
    let plugin_ids: Vec<String> = orch_plugins
        .iter()
        .filter_map(|p| p.as_str().map(|s| s.to_string()))
        .collect();

    if runtime_paths.is_empty() && plugin_ids.is_empty() {
        return Vec::new();
    }
    let mut parts = runtime_paths;
    if !plugin_ids.is_empty() {
        parts.push(format!("plugins: {}", plugin_ids.join(", ")));
    }
    vec![format!(
        "Applied host settings from {document_name}: {}",
        parts.join(", ")
    )]
}

/// `orchestration_path.resolve().parent` -- an absolute, symlink-
/// resolved directory. Falls back to the lexical (non-canonicalized)
/// parent when the path can't be resolved on disk (Python's own
/// `Path.resolve()` is non-strict by default and still returns a best-
/// effort absolute path for a component that doesn't exist; neither
/// surface reaches this point for a path that failed to load moments
/// earlier, so the fallback is only ever exercised by a race, never by
/// an ordinary document).
fn resolve_document_dir(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    resolved.parent().unwrap_or(&resolved).to_path_buf()
}

fn document_origin(path: &Path) -> (DocumentOrigin, PathBuf, PathBuf) {
    let document_dir = resolve_document_dir(path);
    let confinement_root = crate::project_root::default_project_root(&document_dir);
    (
        DocumentOrigin::File {
            document_dir: document_dir.clone(),
            confinement_root: confinement_root.clone(),
        },
        document_dir,
        confinement_root,
    )
}

/// The 6 ASCII characters Python's `str.strip()` treats as whitespace
/// with no argument -- used only for [`check_report`]'s own
/// "Orchestration file is empty." check (`cli/runtime_shim.py::
/// validate`'s `text.strip()`), which runs on the raw file text before
/// any format-specific parsing. **Known divergence, not a full port**:
/// Python's `str.strip()` actually strips every Unicode codepoint
/// `str.isspace()` considers whitespace (Unicode category `Zs`/`Zl`/`Zp`
/// plus the bidirectional `WS`/`B`/`S` properties -- a strictly larger,
/// and not identical, set than Rust's own `char::is_whitespace()`), so a
/// document whose raw text is non-ASCII whitespace only reports non-empty
/// here where Python would report "Orchestration file is empty." -- an
/// edge case this function approximates with just the ASCII subset.
fn is_python_strip_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// Python truthiness, for `orch.get("plugins") or []`/`orch.get(
/// "runtime") or {}` below -- a missing key (`None` here), `None`,
/// `False`, a numeric zero, and any empty `str`/`bytes`/`list`/`dict`
/// are falsy; everything else is truthy.
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

/// `cli/effective_settings.py::resolve_effective_settings`'s own shape
/// checks on the document's raw `plugins:`/`runtime:` blocks -- only
/// [`check_for_run`] reaches these (`validate()`/[`check_report`] never
/// calls `resolve_effective_settings` at all): `orch.get("plugins") or
/// []` must be a list if truthy; `orch.get("runtime") or {}` must be an
/// object if truthy; then, since a bare path/library run never carries
/// a CLI `--plugins` override of its own (`cli_plugins` is always
/// `None` for every surface this crate exposes) and the host config is
/// always `CircuitryConfig()`'s own empty default list (`config=None`
/// in every golden case's own ground truth), every document `plugins:`
/// entry (already passed through unchanged by `_split_orchestration_
/// plugins`'s trusted branch -- `trust_document` is always `true` here)
/// must be a string.
fn effective_settings_shape_error(document: &Value) -> Option<String> {
    let dict = document.as_dict();
    let plugins_raw = dict.and_then(|d| d.get(&Value::Str("plugins".to_string())));
    let orch_plugins = is_truthy(plugins_raw).then(|| plugins_raw.unwrap());
    if let Some(value) = orch_plugins {
        if !matches!(value, Value::List(_)) {
            return Some("Orchestration 'plugins' must be a list if provided.".to_string());
        }
    }
    let runtime_raw = dict.and_then(|d| d.get(&Value::Str("runtime".to_string())));
    let orch_runtime = is_truthy(runtime_raw).then(|| runtime_raw.unwrap());
    if let Some(value) = orch_runtime {
        if !matches!(value, Value::Dict(_)) {
            return Some("Orchestration 'runtime' must be an object if provided.".to_string());
        }
    }
    if let Some(Value::List(items)) = orch_plugins {
        if items.iter().any(|item| !matches!(item, Value::Str(_))) {
            return Some("Plugins must be strings.".to_string());
        }
    }
    None
}

/// Python `repr(s)` of a plain Rust `&str` -- shared by
/// [`check_interface_inputs_error`]'s own coercion-failure messages,
/// which quote a raw CLI-shaped string value the same way Python's
/// `{value!r}`/`{raw!r}` f-string interpolation does.
fn python_repr_str(s: &str) -> String {
    Value::Str(s.to_string()).py_repr()
}

/// `core/interface_inputs.py::_TRUE_WORDS`/`_FALSE_WORDS`.
const TRUE_WORDS: [&str; 6] = ["true", "t", "yes", "y", "on", "1"];
const FALSE_WORDS: [&str; 6] = ["false", "f", "no", "n", "off", "0"];

/// `core/interface_inputs.py::_coerce`: converts a CLI-`-e`/Mustache-
/// rendered-shaped string to *declared_type*, Python's own exact
/// `int()`/`float()`/custom-boolean error text on failure (its own
/// `json.loads` text for `array`/`object` is third-party, not matched
/// word for word). `array`/`object` use plain `json.loads` -- a
/// duplicate object key silently keeps the last value, not
/// `core/json_load.py`'s stricter path-naming `DuplicateKeyError`
/// orchestration documents themselves get (`_coerce` imports the stdlib
/// `json` module directly, same as [`electricity_json::loads`]'s own
/// doc comment lists `cli/app.py`'s `-e` values among its callers).
///
/// `int()`'s own C-level error formatting caps the quoted literal at
/// 200 characters with no ellipsis (CPython's `PyErr_Format(...,
/// "invalid literal for %s() with base %d: %.200R", ...)` -- the
/// `.200` precision on a `%R` conversion is a hard character cap, not a
/// minimum width), via [`truncate_int_repr`]. `float()`'s own
/// `PyFloat_FromString` error is *not* capped the same way -- confirmed
/// directly (`python3 -c 'float("x"*300)'`'s message quotes the full
/// 300-character literal, where the equivalent `int()` call quotes
/// only 200) -- so `coerce_interface_value`'s `"number"`/`"integer"`
/// arms intentionally differ: only `"integer"`'s message is truncated.
fn coerce_interface_value(raw: &str, declared_type: &str) -> Result<Value, String> {
    match declared_type {
        "number" => parse_python_int(raw)
            .map(Value::Int)
            .or_else(|| parse_python_float(raw).map(Value::Float))
            .ok_or_else(|| {
                format!(
                    "could not convert string to float: {}",
                    python_repr_str(raw)
                )
            }),
        "integer" => parse_python_int(raw).map(Value::Int).ok_or_else(|| {
            format!(
                "invalid literal for int() with base 10: {}",
                truncate_int_repr(raw)
            )
        }),
        "boolean" => {
            let lowered = raw.trim_matches(is_python_strip_whitespace).to_lowercase();
            if TRUE_WORDS.contains(&lowered.as_str()) {
                Ok(Value::Bool(true))
            } else if FALSE_WORDS.contains(&lowered.as_str()) {
                Ok(Value::Bool(false))
            } else {
                Err(format!("{} is not a boolean", python_repr_str(raw)))
            }
        }
        "array" | "object" => electricity_json::loads(raw).map_err(|e| e.to_string()),
        _ => Ok(Value::Str(raw.to_string())),
    }
}

/// `python_repr_str(raw)`, truncated to 200 `char`s -- CPython's own
/// `%.200R` cap on `int()`'s error text (see [`coerce_interface_value`]'s
/// own doc comment), applied to the already-quoted repr (a cap on the
/// *formatted* text, not the input's own length, so a short value's
/// repr -- quotes, escapes and all -- is always returned unchanged).
fn truncate_int_repr(raw: &str) -> String {
    python_repr_str(raw).chars().take(200).collect()
}

/// Python's own underscore-digit-separator rule (PEP 515), applied to
/// any numeric-literal text before [`parse_python_int`]/
/// [`parse_python_float`] see it: an `_` is valid only directly between
/// two ASCII digits -- never leading, trailing, doubled, or next to a
/// sign/`.`/`e`/`E`. Returns `None` (an invalid literal, same as
/// CPython's `int()`/`float()` would raise on it) on any other
/// placement; otherwise every `_` is stripped and the remaining text
/// returned. A CLI `-e` value with no `_` at all (the overwhelming
/// majority) is returned unchanged.
fn strip_python_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '_' {
            continue;
        }
        let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
        let next_digit = i + 1 < chars.len() && chars[i + 1].is_ascii_digit();
        if !prev_digit || !next_digit {
            return None;
        }
    }
    Some(chars.into_iter().filter(|c| *c != '_').collect())
}

/// Python `int(s)`: optional surrounding whitespace, an optional
/// leading `+`/`-`, then one or more ASCII digits (PEP 515 underscores
/// allowed between digits, via [`strip_python_underscores`]) --
/// arbitrary precision, via [`electricity_value::IntValue::parse_decimal`].
fn parse_python_int(raw: &str) -> Option<electricity_value::IntValue> {
    let trimmed = raw.trim_matches(is_python_strip_whitespace);
    let destressed = strip_python_underscores(trimmed)?;
    let (negative, digits) = match destressed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (
            false,
            destressed.strip_prefix('+').unwrap_or(destressed.as_str()),
        ),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits.to_string()
    };
    electricity_value::IntValue::parse_decimal(&signed)
}

/// Python `float(s)`: PEP 515 underscores (via
/// [`strip_python_underscores`]), then Rust's own `f64::from_str` --
/// which already accepts the same decimal/exponent grammar plus
/// `inf`/`infinity`/`nan` case-insensitively with an optional sign, so
/// nothing further is needed. Only reached from [`coerce_interface_value`]'s
/// `"number"` arm, where any successfully parsed `f64` already satisfies
/// `matches_type(_, "number")` regardless of its actual value -- so unlike
/// [`parse_python_int`], precision edge cases here can never change
/// [`check_interface_inputs_error`]'s verdict, only whether parsing
/// succeeds at all.
fn parse_python_float(raw: &str) -> Option<f64> {
    let trimmed = raw.trim_matches(is_python_strip_whitespace);
    let destressed = strip_python_underscores(trimmed)?;
    destressed.parse::<f64>().ok()
}

/// The CLI's own `-e` entries, JSON-sniffed per key
/// (`cli/app.py::_parse_env_vars`: `-e start=5` becomes the int `5`,
/// `-e x=1.50` becomes the float `1.5`, a value that isn't valid JSON
/// -- `-e name=World`, `-e n=1_000` -- stays the literal text), then
/// with the original `-e` text substituted back in, unconditionally,
/// for any key *iface_inputs* declares `type: string`
/// (`_restore_raw_text_for_string_inputs` -- so a declared-string input
/// never loses text to JSON's own numeric/boolean coercion, *and* a
/// JSON null given to a string input stays the literal text `"null"`
/// rather than becoming absent).
fn cli_inline_entries(options: &CheckOptions, iface_inputs: &Dict) -> IndexMap<String, Value> {
    let mut inline: IndexMap<String, Value> = IndexMap::new();
    for (key, raw) in &options.inputs {
        let sniffed = electricity_json::loads(raw).unwrap_or_else(|_| Value::Str(raw.clone()));
        inline.insert(key.clone(), sniffed);
    }
    for (key, spec) in iface_inputs {
        let (Value::Str(key_str), Some(spec_dict)) = (key, spec.as_dict()) else {
            continue;
        };
        let declared_type_is_string = matches!(
            spec_dict.get(&Value::Str("type".to_string())),
            Some(Value::Str(s)) if s == "string"
        );
        if declared_type_is_string {
            if let Some(raw) = options.inputs.get(key_str) {
                inline.insert(key_str.clone(), Value::Str(raw.clone()));
            }
        }
    }
    inline
}

/// `core/interface_inputs.py::check_interface_inputs(interface, inputs,
/// label="")`, against the CLI's own `-e` input namespace, built from
/// *options.inputs* exactly as `cli/runtime_shim.py::run`'s own choke
/// point does ([`cli_inline_entries`], then `core/state_ns.py::
/// migrate_legacy_state`'s own `input`-namespace rule --
/// [`crate::state_ns::migrate_legacy_input_namespace`]). Mutates and
/// returns that namespace, Python's own way (lane D's gate needs the
/// result, not just a pass/fail verdict -- #431's "Seams" section):
/// a document with no `interface`/`interface.inputs` at all returns the
/// namespace unchanged, Python's own `isinstance` early return. Per
/// declared input, in document order:
///
/// - absent (missing, or present as `null`): filled from `default:`
///   (falling through to the type check below rather than skipping it,
///   so a malformed default is still caught) -- appended at the
///   namespace's end if the key was missing, or left in its own
///   position if it only held `null` (`IndexMap::insert`'s own
///   semantics on an existing vs. a new key, matching a Python `dict`
///   assignment exactly); with no default, an error if `required:
///   true`, else the key is dropped from the namespace entirely
///   ([`indexmap::IndexMap::shift_remove`] -- `dict.pop`);
/// - present and already the declared `type`: untouched;
/// - present, declared `type: string`, but an `int`/`float`/`bool`:
///   replaced with its own `json.dumps` spelling ([`electricity_json::
///   dumps`]) -- Python's own "a `-e`/Mustache value parsed numeric/
///   boolean, convert it back to text" branch, a real mutation, not a
///   pure validation pass-through;
///   a `string` value otherwise coerced to its declared `type` with
///   [`coerce_interface_value`] (`core/interface_inputs.py::_coerce`)
///   replaces the namespace entry with the coerced value.
///
/// Keys the namespace carries but `iface_inputs` doesn't declare are
/// never looked at: extra `-e` input stays allowed. `Err` is the first
/// violation (unprefixed -- Circuitry's own call site always passes
/// `label=""`), matching Python's own eager `raise`.
fn build_input_namespace(
    document: &Value,
    options: &CheckOptions,
) -> Result<IndexMap<String, Value>, String> {
    let no_declared_inputs = Dict::new();
    let iface_inputs = document
        .as_dict()
        .and_then(|d| d.get(&Value::Str("interface".to_string())))
        .and_then(|i| i.as_dict())
        .and_then(|i| i.get(&Value::Str("inputs".to_string())))
        .and_then(|i| i.as_dict());

    let Some(iface_inputs) = iface_inputs else {
        return Ok(crate::state_ns::migrate_legacy_input_namespace(
            &cli_inline_entries(options, &no_declared_inputs),
        ));
    };

    let mut namespace =
        crate::state_ns::migrate_legacy_input_namespace(&cli_inline_entries(options, iface_inputs));

    for (key, spec) in iface_inputs {
        let Some(spec_dict) = spec.as_dict() else {
            continue;
        };
        let key_str = key.py_str();
        // A non-string `interface.inputs` key can never equal a
        // (always-string) namespace key -- the same no-op Python's own
        // `key not in inputs` dict-membership check produces for it.
        let Value::Str(key_as_str) = key else {
            continue;
        };
        let current = namespace.get(key_as_str).cloned();
        let absent = matches!(current, None | Some(Value::None));
        if absent {
            if let Some(default) = spec_dict.get(&Value::Str("default".to_string())) {
                namespace.insert(key_as_str.clone(), default.clone());
            } else if is_truthy(spec_dict.get(&Value::Str("required".to_string()))) {
                return Err(format!(
                    "missing required input '{key_str}' declared in orchestration interface."
                ));
            } else {
                namespace.shift_remove(key_as_str);
                continue;
            }
        }
        let declared_type = match spec_dict.get(&Value::Str("type".to_string())) {
            Some(Value::Str(s))
                if crate::structural::INTERFACE_TYPE_NAMES.contains(&s.as_str()) =>
            {
                s.clone()
            }
            _ => continue,
        };
        let value = namespace
            .get(key_as_str)
            .cloned()
            .expect("present: either matched above or just inserted");
        if crate::structural::matches_type(&value, &declared_type) {
            continue;
        }
        if declared_type == "string"
            && matches!(value, Value::Int(_) | Value::Float(_) | Value::Bool(_))
        {
            let json_text = electricity_json::dumps(&value, electricity_json::WriteMode::COMPACT)
                .unwrap_or_else(|_| value.py_str());
            namespace.insert(key_as_str.clone(), Value::Str(json_text));
            continue;
        }
        if let Value::Str(raw) = &value {
            match coerce_interface_value(raw, &declared_type) {
                Ok(coerced) => {
                    if crate::structural::matches_type(&coerced, &declared_type) {
                        namespace.insert(key_as_str.clone(), coerced);
                        continue;
                    }
                    return Err(format!(
                        "input '{key_str}' declared type '{declared_type}' but got {}.",
                        crate::structural::py_class_name(&coerced)
                    ));
                }
                Err(message) => {
                    return Err(format!(
                        "input '{key_str}' declared type '{declared_type}' but {} could not be \
                         converted: {message}",
                        python_repr_str(raw)
                    ));
                }
            }
        }
        return Err(format!(
            "input '{key_str}' declared type '{declared_type}' but got {}.",
            crate::structural::py_class_name(&value)
        ));
    }
    Ok(namespace)
}

/// Matches `runtime_shim.validate(path, config=None, skip_preflight=...,
/// trust_document=...)`.
///
/// Order: read the raw file text and fail with "Orchestration file is
/// empty." if it is blank (before any parsing, matching `validate()`'s
/// own `text.strip()` check, which runs *outside* the broad
/// `except Exception` the rest of the function is wrapped in);
/// [`load_document`]; [`unknown_key_warnings`] plus the "Applied host
/// settings" notice (both folded into `warnings` immediately, so they
/// survive every early return below, exactly as `validate()`'s own
/// `lint_warnings` variable does); [`structural_errors`]; compilation
/// ([`compile_document`], lane C); [`concurrency_config_errors`] against
/// the merged runtime config (checked *after* compilation here --
/// `validate()`'s own order, `cli/runtime_shim.py`'s own parse calls
/// following `compile_orchestration`); [`groups::unknown_group_errors`]
/// (lane C) against the same merged config's group names; then
/// [`cycles::detect_cycles`] (lane C). Each error shape is `validate()`'s
/// own: a flat `errors` list, with no `"Orchestration validation
/// failed:"` prefix anywhere.
pub fn check_report(path: &Path, options: &CheckOptions) -> CheckReport {
    let text = match load::read_universal_newlines(path) {
        Ok(text) => text,
        Err(err) => {
            return CheckReport {
                ok: false,
                errors: vec![err.0],
                warnings: Vec::new(),
            };
        }
    };
    if text.trim_matches(is_python_strip_whitespace).is_empty() {
        return CheckReport {
            ok: false,
            errors: vec!["Orchestration file is empty.".to_string()],
            warnings: Vec::new(),
        };
    }

    let document = match load_document(path) {
        Ok(document) => document,
        Err(err) => {
            return CheckReport {
                ok: false,
                errors: vec![err.0],
                warnings: Vec::new(),
            };
        }
    };

    let document_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let mut warnings = unknown_key_warnings(&document);
    if let Some(name) = &document_name {
        warnings.extend(host_setting_warnings(&document, name));
    }

    let errors = structural_errors(&document);
    if !errors.is_empty() {
        return CheckReport {
            ok: false,
            errors,
            warnings,
        };
    }

    let (origin, _document_dir, _confinement_root) = document_origin(path);

    let program = match compile_document(&document, &origin) {
        Ok(program) => program,
        Err(err) => {
            return CheckReport {
                ok: false,
                errors: vec![err.0],
                warnings,
            };
        }
    };

    let merged_runtime = merged_runtime_block(options, &document);
    let config_errors = concurrency_config_errors(merged_runtime.as_ref());
    if !config_errors.is_empty() {
        return CheckReport {
            ok: false,
            errors: config_errors,
            warnings,
        };
    }

    let known_groups = concurrency_group_names(merged_runtime.as_ref());
    let group_errors = groups::unknown_group_errors(&program, &known_groups);
    if !group_errors.is_empty() {
        return CheckReport {
            ok: false,
            errors: group_errors,
            warnings,
        };
    }

    if let Err(err) = cycles::detect_cycles(&document, Some(path)) {
        return CheckReport {
            ok: false,
            errors: vec![err.0],
            warnings,
        };
    }

    CheckReport {
        ok: true,
        errors: Vec::new(),
        warnings,
    }
}

/// The exact error text a `cof run` of *path* reports, matching
/// `runtime_shim.run(RunRequest(..., validate_only=True,
/// skip_preflight=..., trust_document=...))`.
///
/// **Known divergence** (crate docs' own "Known divergences" list has the
/// full rationale): `run()` also raises, before any of the checks below,
/// from `resolve_complexity_settings`'s validation of a malformed
/// `runtime.complexity` block (inside `resolve_effective_settings`, before
/// the concurrency limiter) and, between the concurrency limiter and
/// `check_interface_inputs`, from `build_persistence_backend`'s validation
/// of a malformed `runtime.persistence` block. Neither is ported here --
/// complexity routing/decomposition and persistence backends have no IR
/// representation in this crate at all -- so a document whose only fault
/// is one of those two blocks passes this function where `cof run` would
/// fail.
///
/// Order: [`load_document`]; [`concurrency_config_errors`] against the
/// merged runtime config -- confirmed directly against
/// `cli/runtime_shim.py::run`: `RunConcurrencyLimiter.from_runtime_config`
/// runs *before* the document's structural checks even start, raising
/// its own `"Invalid runtime concurrency configuration:\n  - ..."`
/// message the instant the config itself is malformed, regardless of
/// whether the document would otherwise be structurally valid; then
/// [`structural_errors`] (as [`RunCheckError::Structural`]); then
/// [`compile_document`] (lane C, as [`RunCheckError::Compile`] --
/// already in final display form, including a composition-error
/// message's own `"Prompt composition errors:\n  - ..."` prefix, which
/// `compile_orchestration` bakes in itself); then
/// [`groups::unknown_group_errors`] (lane C, as
/// [`RunCheckError::Structural`], matching `run()`'s own
/// `"Orchestration validation failed:"`-prefixed `raise`) against the
/// same merged config's group names; then [`cycles::detect_cycles`]
/// (lane C, as [`RunCheckError::Cycle`]). `Program.document`
/// ([`electricity_bytecode::DocumentInfo`]) is filled in here, not by
/// `compile_document` itself -- this function has *path*'s raw bytes
/// (via [`digest::document_content_digest`]), which `compile_document`
/// never sees; a digest failure (lane D is still a stub) is tolerated
/// rather than failing the check (issue #408's lane B section).
/// A loaded, not-yet-checked document -- [`prepare_document`]'s own
/// result, and [`pre_state_checks`]/[`post_state_checks`]'s shared input.
/// Bundles [`document_origin`]'s result (infallible, and depending only
/// on *path* -- never on the document's own content or [`CheckOptions`])
/// with the document itself, so neither later phase re-reads the file or
/// re-resolves the document's directory.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub path: PathBuf,
    pub document: Value,
    pub origin: DocumentOrigin,
    pub document_dir: PathBuf,
    pub confinement_root: PathBuf,
}

/// Phase 1 of [`check_for_run`]'s own order (issue #431's gate lane,
/// "Seams" section): *path*'s load error, exactly as `check_for_run`
/// already reported it (`cli/runtime_shim.py::run`'s own `_load_
/// document` step, which a bad path/parse fails before anything else
/// does -- step 5 of issue #431's run-wiring table).
///
/// *options* is accepted, not yet read, for lane D: a later caller that
/// wants `skip_preflight`/`trust_document` to change how a document
/// loads (neither flag is consulted anywhere in this crate today) has
/// somewhere to add that without breaking this function's signature.
pub fn prepare_document(path: &Path, options: &CheckOptions) -> Result<Loaded, RunCheckError> {
    let _ = options;
    let document = load_document(path).map_err(|err| RunCheckError::Compile(err.0))?;
    let (origin, document_dir, confinement_root) = document_origin(path);
    Ok(Loaded {
        path: path.to_path_buf(),
        document,
        origin,
        document_dir,
        confinement_root,
    })
}

/// Phase 2 of [`check_for_run`]'s own order: the shape/concurrency/
/// complexity/persistence/interface-input checks `run()` makes *before*
/// `check_for_run`'s own structural checks (steps 6, 7, 9 and 10 of
/// issue #431's run-wiring table) -- and, unlike the pre-#431
/// `check_interface_inputs_error` this replaces as `check_for_run`'s own
/// step, *returns* [`build_input_namespace`]'s coerced/defaulted
/// namespace rather than only a pass/fail verdict, since lane D's run
/// wiring needs that namespace to seed `state["input"]` with (`cli/
/// runtime_shim.py::run`, ~:640-648) -- not just know it would have
/// succeeded.
///
/// *effective_runtime* is the already-merged config+document `runtime:`
/// block the concurrency/group checks run against -- [`check_for_run`]
/// passes [`merged_runtime_block`]'s own result (its pre-#431 shallow
/// merge, unchanged); a caller with a real [`electricity_config::
/// EffectiveSettings`] (lane D) passes its own `runtime` field instead,
/// once `electricity_config::merge_runtime` replaces [`merged_runtime_
/// block`] as the thing that produces it.
///
/// Order, exactly `check_for_run`'s own: [`effective_settings_shape_error`];
/// [`concurrency_config_errors`]; [`electricity_config::
/// validate_complexity_and_persistence`] (lane D's own hook -- a no-op
/// today, [`electricity_config`]'s own crate docs have the resulting,
/// already-documented divergence); [`build_input_namespace`].
pub fn pre_state_checks(
    loaded: &Loaded,
    options: &CheckOptions,
    effective_runtime: Option<&Value>,
) -> Result<IndexMap<String, Value>, RunCheckError> {
    if let Some(message) = effective_settings_shape_error(&loaded.document) {
        return Err(RunCheckError::Compile(message));
    }

    let config_errors = concurrency_config_errors(effective_runtime);
    if !config_errors.is_empty() {
        let lines: Vec<String> = config_errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(RunCheckError::Compile(format!(
            "Invalid runtime concurrency configuration:\n{}",
            lines.join("\n")
        )));
    }

    electricity_config::validate_complexity_and_persistence(&loaded.document, effective_runtime)
        .map_err(|err| RunCheckError::Compile(err.0))?;

    build_input_namespace(&loaded.document, options).map_err(RunCheckError::Compile)
}

/// Phase 3 of [`check_for_run`]'s own order: structural checks, compile,
/// groups, cycles and the digest (steps 14 of issue #431's run-wiring
/// table, plus the digest [`check_for_run`] itself has always attached
/// to `Program.document` -- never part of `run()`'s own structural-check
/// step, but nowhere else in this crate's two-phase-to-three split would
/// fit it either).
///
/// *effective_runtime* is the same already-merged `runtime:` block
/// [`pre_state_checks`] took -- used here only for [`groups::
/// unknown_group_errors`]'s own known-group-name set, matching
/// `check_for_run`'s pre-#431 order (groups are checked *after*
/// compilation, against the compiled [`electricity_bytecode::Program`]).
pub fn post_state_checks(
    loaded: &Loaded,
    effective_runtime: Option<&Value>,
) -> Result<Program, RunCheckError> {
    let structural = structural_errors(&loaded.document);
    if !structural.is_empty() {
        return Err(RunCheckError::Structural(structural));
    }

    let mut program = compile_document(&loaded.document, &loaded.origin)
        .map_err(|err| RunCheckError::Compile(err.0))?;

    let known_groups = concurrency_group_names(effective_runtime);
    let group_errors = groups::unknown_group_errors(&program, &known_groups);
    if !group_errors.is_empty() {
        return Err(RunCheckError::Structural(group_errors));
    }

    cycles::detect_cycles(&loaded.document, Some(&loaded.path))
        .map_err(|err| RunCheckError::Cycle(err.0))?;

    // `None` on any digest failure, matching Circuitry's own `except
    // OSError: document_hash = None` exactly (`cli/runtime_shim.py::run`,
    // ~:678-683) -- a digest only matters for a future `--resume`, so a
    // failure computing it must never fail the run itself.
    let computed_digest =
        digest::document_content_digest(&loaded.path, &loaded.document, &loaded.confinement_root)
            .ok();
    program.document = Some(DocumentInfo {
        path_as_given: loaded.path.display().to_string(),
        resolved_directory: loaded.document_dir.clone(),
        confinement_root: loaded.confinement_root.clone(),
        digest: computed_digest,
    });

    Ok(program)
}

/// The exact error text a `cof run` of *path* reports -- [`prepare_document`]
/// (step 5), then [`pre_state_checks`] (steps 6, 7, 9, 10 -- its own
/// *effective_runtime* is [`merged_runtime_block`]'s result, this
/// function's pre-#431 computation, unchanged), then [`post_state_checks`]
/// (step 14 plus the digest). `check_for_run` stays exactly this
/// composition so every existing golden, `--dump-ir`, and osp's own use of
/// this crate keep seeing `check_for_run`'s pre-#431 behavior unchanged --
/// the three phases above exist so lane D's run wiring can call each on
/// its own, not to change what this function itself does.
pub fn check_for_run(path: &Path, options: &CheckOptions) -> Result<Program, RunCheckError> {
    let loaded = prepare_document(path, options)?;
    let effective_runtime = merged_runtime_block(options, &loaded.document);
    pre_state_checks(&loaded, options, effective_runtime.as_ref())?;
    post_state_checks(&loaded, effective_runtime.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    /// [`build_input_namespace`]'s error, if any -- every pre-#431 test
    /// below only needs the pass/fail verdict, not the namespace itself.
    fn check_interface_inputs_error(document: &Value, options: &CheckOptions) -> Option<String> {
        build_input_namespace(document, options).err()
    }

    fn runtime_doc(pairs: Vec<(&str, Value)>) -> Value {
        let mut runtime = Dict::new();
        for (k, v) in pairs {
            runtime.insert(Value::Str(k.to_string()), v);
        }
        let mut doc = Dict::new();
        doc.insert(Value::Str("runtime".to_string()), Value::Dict(runtime));
        Value::Dict(doc)
    }

    #[test]
    fn negative_max_concurrency_is_a_config_error() {
        let doc = runtime_doc(vec![("max_concurrency", Value::from(-1i64))]);
        let errors = concurrency_config_errors(runtime_block(&doc));
        assert_eq!(
            errors,
            vec!["runtime.max_concurrency must be a positive integer, got -1.".to_string()]
        );
    }

    #[test]
    fn bool_max_concurrency_is_rejected_like_python_isinstance_bool_check() {
        let doc = runtime_doc(vec![("max_concurrency", Value::Bool(true))]);
        let errors = concurrency_config_errors(runtime_block(&doc));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("must be a positive integer"));
    }

    #[test]
    fn valid_max_concurrency_has_no_error() {
        let doc = runtime_doc(vec![("max_concurrency", Value::from(4i64))]);
        assert_eq!(
            concurrency_config_errors(runtime_block(&doc)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn non_mapping_concurrency_groups_is_a_config_error() {
        let doc = runtime_doc(vec![("concurrency_groups", Value::from("nope"))]);
        let errors = concurrency_config_errors(runtime_block(&doc));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("must be a mapping of group name"));
    }

    #[test]
    fn concurrency_group_names_collects_only_valid_entries() {
        let mut groups = Dict::new();
        groups.insert(Value::Str("io".to_string()), Value::from(2i64));
        groups.insert(Value::Str("bad".to_string()), Value::from(-1i64));
        let doc = runtime_doc(vec![("concurrency_groups", Value::Dict(groups))]);
        let names = concurrency_group_names(runtime_block(&doc));
        assert!(names.contains("io"));
        assert!(!names.contains("bad"));
    }

    #[test]
    fn applied_host_settings_notice_names_dotted_runtime_paths() {
        let doc = runtime_doc(vec![("max_concurrency", Value::from(-1i64))]);
        let warnings = host_setting_warnings(&doc, "doc.yml");
        assert_eq!(
            warnings,
            vec!["Applied host settings from doc.yml: runtime.max_concurrency".to_string()]
        );
    }

    #[test]
    fn complexity_and_state_runtime_keys_are_author_level_and_silent() {
        let doc = runtime_doc(vec![("complexity", Value::from("low"))]);
        assert_eq!(host_setting_warnings(&doc, "doc.yml"), Vec::<String>::new());
    }

    fn doc_with_top_level(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    #[test]
    fn non_list_plugins_is_a_shape_error() {
        let doc = doc_with_top_level(vec![("plugins", Value::Str("foo".to_string()))]);
        assert_eq!(
            effective_settings_shape_error(&doc),
            Some("Orchestration 'plugins' must be a list if provided.".to_string())
        );
    }

    #[test]
    fn non_object_runtime_is_a_shape_error() {
        let doc = doc_with_top_level(vec![("runtime", Value::from(5i64))]);
        assert_eq!(
            effective_settings_shape_error(&doc),
            Some("Orchestration 'runtime' must be an object if provided.".to_string())
        );
    }

    #[test]
    fn non_string_plugin_entry_is_a_shape_error() {
        let doc = doc_with_top_level(vec![("plugins", Value::List(vec![Value::from(1i64)]))]);
        assert_eq!(
            effective_settings_shape_error(&doc),
            Some("Plugins must be strings.".to_string())
        );
    }

    #[test]
    fn empty_or_absent_plugins_and_runtime_have_no_shape_error() {
        assert_eq!(
            effective_settings_shape_error(&doc_with_top_level(vec![])),
            None
        );
        let doc = doc_with_top_level(vec![
            ("plugins", Value::List(vec![])),
            ("runtime", Value::Dict(Dict::new())),
        ]);
        assert_eq!(effective_settings_shape_error(&doc), None);
    }

    fn interface_doc(spec_pairs: Vec<(&str, Value)>) -> Value {
        interface_doc_named("x", spec_pairs)
    }

    fn interface_doc_named(key: &str, spec_pairs: Vec<(&str, Value)>) -> Value {
        let mut spec = Dict::new();
        for (k, v) in spec_pairs {
            spec.insert(Value::Str(k.to_string()), v);
        }
        let mut inputs = Dict::new();
        inputs.insert(Value::Str(key.to_string()), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        Value::Dict(dict)
    }

    fn no_inputs() -> CheckOptions {
        CheckOptions::default()
    }

    fn with_inputs(pairs: Vec<(&str, &str)>) -> CheckOptions {
        let mut options = CheckOptions::default();
        for (k, v) in pairs {
            options.inputs.insert(k.to_string(), v.to_string());
        }
        options
    }

    #[test]
    fn missing_required_input_with_no_default_is_an_error() {
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &no_inputs()),
            Some("missing required input 'x' declared in orchestration interface.".to_string())
        );
    }

    #[test]
    fn unconvertible_default_reports_pythons_own_int_error_text() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::Str("abc".to_string())),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &no_inputs()),
            Some(
                "input 'x' declared type 'integer' but 'abc' could not be converted: invalid \
                 literal for int() with base 10: 'abc'"
                    .to_string()
            )
        );
    }

    #[test]
    fn non_coercible_default_type_reports_got_the_actual_type() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::List(vec![Value::from(1i64)])),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &no_inputs()),
            Some("input 'x' declared type 'integer' but got list.".to_string())
        );
    }

    #[test]
    fn optional_undefaulted_input_is_not_an_error() {
        let doc = interface_doc(vec![("type", Value::Str("string".to_string()))]);
        assert_eq!(check_interface_inputs_error(&doc, &no_inputs()), None);
    }

    #[test]
    fn matching_default_is_not_an_error() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::from(3i64)),
        ]);
        assert_eq!(check_interface_inputs_error(&doc, &no_inputs()), None);
    }

    #[test]
    fn required_input_supplied_via_e_satisfies_it() {
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "hello")])),
            None
        );
    }

    #[test]
    fn provided_e_value_overrides_the_default() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::from(3i64)),
        ]);
        // An invalid override proves the default was actually replaced,
        // not merely satisfied alongside it.
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "abc")])),
            Some(
                "input 'x' declared type 'integer' but 'abc' could not be converted: invalid \
                 literal for int() with base 10: 'abc'"
                    .to_string()
            )
        );
    }

    #[test]
    fn integer_e_value_with_underscores_coerces_like_pythons_int() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "1_000")])),
            None
        );
    }

    #[test]
    fn e_value_that_json_sniffs_to_the_wrong_type_reports_the_sniffed_type() {
        // `-e x=5.0` JSON-sniffs to a float before `check_interface_inputs`
        // ever sees it (`cli/app.py::_parse_env_vars`), so a declared
        // `integer` input rejects it as a float, not as unparsable text.
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "5.0")])),
            Some("input 'x' declared type 'integer' but got float.".to_string())
        );
    }

    #[test]
    fn boolean_word_e_value_coerces() {
        let doc = interface_doc(vec![
            ("type", Value::Str("boolean".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "yes")])),
            None
        );
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "nope")])),
            Some(
                "input 'x' declared type 'boolean' but 'nope' could not be converted: 'nope' is \
                 not a boolean"
                    .to_string()
            )
        );
    }

    #[test]
    fn boolean_word_that_json_sniffs_to_an_int_reports_the_sniffed_type() {
        // `-e x=1` JSON-sniffs to the int `1` before `check_interface_
        // inputs` ever sees it -- `1` is also one of `_TRUE_WORDS`, but
        // that word list is only consulted for text `_coerce` actually
        // receives, and an int never reaches `_coerce` at all.
        let doc = interface_doc(vec![
            ("type", Value::Str("boolean".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "1")])),
            Some("input 'x' declared type 'boolean' but got int.".to_string())
        );
    }

    #[test]
    fn number_word_that_json_sniffs_to_a_bool_reports_the_sniffed_type() {
        let doc = interface_doc(vec![
            ("type", Value::Str("number".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "true")])),
            Some("input 'x' declared type 'number' but got bool.".to_string())
        );
    }

    #[test]
    fn array_value_that_json_sniffs_to_an_object_reports_the_sniffed_type() {
        let doc = interface_doc(vec![
            ("type", Value::Str("array".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "{\"a\": 1}")])),
            Some("input 'x' declared type 'array' but got dict.".to_string())
        );
    }

    #[test]
    fn required_non_string_input_given_json_null_is_still_missing() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "null")])),
            Some("missing required input 'x' declared in orchestration interface.".to_string())
        );
    }

    #[test]
    fn string_typed_e_value_keeps_its_exact_text_even_when_it_looks_like_json() {
        let doc = interface_doc(vec![("type", Value::Str("string".to_string()))]);
        // `1.50` JSON-sniffs to the float `1.5`, losing its trailing
        // zero -- the CLI restores the raw text for a declared `string`
        // input instead of accepting the JSON-sniffed value's `repr()`.
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "1.50")])),
            None
        );
    }

    #[test]
    fn required_string_input_given_json_null_text_stays_present_only_because_of_the_restore() {
        // Without the restore, `-e x=null` JSON-sniffs to `None` before
        // `check_interface_inputs` ever sees it, which counts as absent
        // -- same as not passing `-e x` at all -- and a required,
        // undefaulted input would report missing. The restore puts the
        // literal text `"null"` back for a declared `string` input, so
        // it stays present instead.
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "null")])),
            None
        );
    }

    #[test]
    fn string_input_given_array_shaped_text_passes_only_because_of_the_restore() {
        // Without the restore, `-e x=[1]` JSON-sniffs to the list `[1]`
        // before `check_interface_inputs` ever sees it; a declared
        // `string` input rejects a list outright (`check_interface_
        // inputs.py`'s own final `raise`, past the int/float/bool-to-
        // string branch, which a list never matches). The restore puts
        // the literal text `"[1]"` back, which satisfies `type: string`
        // directly.
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("x", "[1]")])),
            None
        );
    }

    #[test]
    fn undeclared_extra_e_input_is_allowed() {
        let doc = interface_doc(vec![("type", Value::Str("string".to_string()))]);
        assert_eq!(
            check_interface_inputs_error(&doc, &with_inputs(vec![("unrelated", "1")])),
            None
        );
    }

    #[test]
    fn underscore_prefixed_e_key_never_satisfies_a_declared_input_of_the_same_name() {
        // `core/state_ns.py::migrate_legacy_state` never lifts a
        // `_`-prefixed root key under `input` -- a document that
        // declares `_token` as a required input can't be satisfied by
        // `-e _token=...` at all.
        let doc = interface_doc_named(
            "_token",
            vec![
                ("type", Value::Str("string".to_string())),
                ("required", Value::Bool(true)),
            ],
        );
        let mut options = CheckOptions::default();
        options.inputs.insert("_token".to_string(), "x".to_string());
        assert_eq!(
            check_interface_inputs_error(&doc, &options),
            Some(
                "missing required input '_token' declared in orchestration interface.".to_string()
            )
        );
    }

    #[test]
    fn e_input_satisfies_a_required_input_via_the_input_namespace_key() {
        // `-e input={"name": "W"}` JSON-sniffs to a dict under the
        // literal key `input` -- `migrate_legacy_state` treats that key
        // as already namespaced and uses its value directly, rather
        // than lifting `input` itself as a declared input's own value.
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        let mut options = CheckOptions::default();
        options
            .inputs
            .insert("input".to_string(), "{\"x\": \"W\"}".to_string());
        assert_eq!(check_interface_inputs_error(&doc, &options), None);
    }

    #[test]
    fn e_input_non_dict_input_key_value_becomes_an_empty_namespace() {
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        let mut options = CheckOptions::default();
        options.inputs.insert("input".to_string(), "5".to_string());
        assert_eq!(
            check_interface_inputs_error(&doc, &options),
            Some("missing required input 'x' declared in orchestration interface.".to_string())
        );
    }

    #[test]
    fn e_input_key_present_means_no_other_e_key_is_lifted() {
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        let mut options = CheckOptions::default();
        options.inputs.insert("input".to_string(), "{}".to_string());
        options.inputs.insert("x".to_string(), "W".to_string());
        assert_eq!(
            check_interface_inputs_error(&doc, &options),
            Some("missing required input 'x' declared in orchestration interface.".to_string())
        );
    }

    #[test]
    fn declared_input_named_prime_can_never_be_satisfied_via_e() {
        // `prime` is a namespace name, so `migrate_legacy_state` never
        // lifts it under `input` either -- same as an underscore-prefixed
        // key.
        let doc = interface_doc_named(
            "prime",
            vec![
                ("type", Value::Str("integer".to_string())),
                ("required", Value::Bool(true)),
            ],
        );
        let mut options = CheckOptions::default();
        options.inputs.insert("prime".to_string(), "5".to_string());
        assert_eq!(
            check_interface_inputs_error(&doc, &options),
            Some("missing required input 'prime' declared in orchestration interface.".to_string())
        );
    }

    #[test]
    fn build_input_namespace_appends_a_missing_defaulted_key_at_the_end() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::from(3i64)),
        ]);
        let mut options = CheckOptions::default();
        options.inputs.insert("other".to_string(), "hi".to_string());
        let namespace = build_input_namespace(&doc, &options).unwrap();
        // `other` was already in the namespace before the declared `x`
        // input's default is filled in -- a brand-new key is appended at
        // the end, matching a Python `dict`'s own insertion-order rule.
        assert_eq!(namespace.keys().collect::<Vec<_>>(), vec!["other", "x"]);
        assert_eq!(namespace.get("x"), Some(&Value::Int(3.into())));
    }

    #[test]
    fn build_input_namespace_drops_an_optional_null_key_entirely() {
        // A non-`string`-typed input gets no raw-text restore, so `-e
        // x=null` JSON-sniffs to the real `null` -- present but treated
        // the same as absent, and (with no `default:`, not `required`)
        // dropped outright.
        let doc = interface_doc(vec![("type", Value::Str("integer".to_string()))]);
        let namespace = build_input_namespace(&doc, &with_inputs(vec![("x", "null")])).unwrap();
        assert!(!namespace.contains_key("x"));
    }

    #[test]
    fn build_input_namespace_keeps_a_present_null_keys_position_when_defaulted() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::from(3i64)),
        ]);
        let mut options = CheckOptions::default();
        options.inputs.insert("x".to_string(), "null".to_string());
        options.inputs.insert("other".to_string(), "hi".to_string());
        let namespace = build_input_namespace(&doc, &options).unwrap();
        // `x` already held a (JSON-sniffed) `null` -- its *position* (first,
        // `-e` order) is kept even though the default overwrites its value,
        // unlike a key that didn't exist at all.
        assert_eq!(namespace.keys().collect::<Vec<_>>(), vec!["x", "other"]);
        assert_eq!(namespace.get("x"), Some(&Value::Int(3.into())));
    }

    #[test]
    fn build_input_namespace_stringifies_a_numeric_value_for_a_string_typed_input() {
        let doc = interface_doc(vec![("type", Value::Str("string".to_string()))]);
        let mut options = CheckOptions::default();
        options.inputs.insert("x".to_string(), "10".to_string());
        let namespace = build_input_namespace(&doc, &options).unwrap();
        // `-e x=10` JSON-sniffs to the int `10`; a `string`-typed `x`
        // converts it back to text with `json.dumps`, not `py_str` (no
        // capital-vs-lowercase surprise for a bool, even though this case
        // is an int).
        assert_eq!(namespace.get("x"), Some(&Value::Str("10".to_string())));
    }

    #[test]
    fn build_input_namespace_coerces_a_string_e_value_to_its_declared_type() {
        let doc = interface_doc(vec![("type", Value::Str("integer".to_string()))]);
        let namespace = build_input_namespace(&doc, &with_inputs(vec![("x", "5")])).unwrap();
        assert_eq!(namespace.get("x"), Some(&Value::Int(5.into())));
    }

    #[test]
    fn build_input_namespace_passes_through_unchanged_with_no_declared_interface() {
        let doc = doc_with_top_level(vec![]);
        let mut options = CheckOptions::default();
        options.inputs.insert("x".to_string(), "hi".to_string());
        let namespace = build_input_namespace(&doc, &options).unwrap();
        assert_eq!(namespace.get("x"), Some(&Value::Str("hi".to_string())));
    }

    #[test]
    fn prepare_document_reports_the_same_error_check_for_run_does() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-pipeline-phases-test-missing-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let doc_path = dir.join("missing.yml");

        let prepared = prepare_document(&doc_path, &CheckOptions::default());
        let whole = check_for_run(&doc_path, &CheckOptions::default());
        assert_eq!(prepared.err(), whole.err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_three_phases_compose_to_the_same_result_as_check_for_run() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-pipeline-phases-test-ok-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let doc_path = dir.join("doc.yml");
        std::fs::write(
            &doc_path,
            "interface:\n  inputs:\n    name:\n      type: string\n      default: World\n\
             effects:\n  - type: tool\n    name: fetch\n    provider: json\n    params:\n      mode: parse\n",
        )
        .unwrap();
        let options = CheckOptions::default();

        let loaded = prepare_document(&doc_path, &options).unwrap();
        let effective_runtime = merged_runtime_block(&options, &loaded.document);
        let namespace = pre_state_checks(&loaded, &options, effective_runtime.as_ref()).unwrap();
        assert_eq!(
            namespace.get("name"),
            Some(&Value::Str("World".to_string()))
        );
        let program = post_state_checks(&loaded, effective_runtime.as_ref()).unwrap();

        let whole = check_for_run(&doc_path, &options).unwrap();
        assert_eq!(
            program.document.unwrap().digest,
            whole.document.unwrap().digest
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pre_state_checks_reports_a_concurrency_config_error_before_post_state_checks_runs() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-pipeline-phases-test-concurrency-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let doc_path = dir.join("doc.yml");
        std::fs::write(&doc_path, "runtime:\n  max_concurrency: -1\neffects: []\n").unwrap();
        let options = CheckOptions::default();

        let loaded = prepare_document(&doc_path, &options).unwrap();
        let effective_runtime = merged_runtime_block(&options, &loaded.document);
        let err = pre_state_checks(&loaded, &options, effective_runtime.as_ref()).unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid runtime concurrency configuration")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_for_run_fills_document_digest_as_some_not_an_empty_string() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-pipeline-digest-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let doc_path = dir.join("doc.yml");
        std::fs::write(&doc_path, "effects: []\n").unwrap();

        let program = check_for_run(&doc_path, &CheckOptions::default()).unwrap();
        let digest = program.document.unwrap().digest;
        // Not `unwrap_or_default()`'s old `Some("")` -- a real digest, and
        // never an empty string standing in for "unknown".
        assert!(matches!(digest, Some(ref d) if !d.is_empty()));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
