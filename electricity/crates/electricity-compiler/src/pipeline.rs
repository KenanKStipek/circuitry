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
use electricity_value::Value;
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
/// word for word). Never reaches the `array`/`object`/plain-`string`
/// arms from [`check_interface_inputs_error`] today (no probe needs
/// them), but implemented for every `_TYPE_NAMES` entry `_coerce`
/// itself handles, not just the ones exercised so far.
fn coerce_interface_value(raw: &str, declared_type: &str) -> Result<Value, String> {
    match declared_type {
        "number" => parse_python_int(raw)
            .map(Value::Int)
            .or_else(|| raw.trim().parse::<f64>().ok().map(Value::Float))
            .ok_or_else(|| {
                format!(
                    "could not convert string to float: {}",
                    python_repr_str(raw)
                )
            }),
        "integer" => parse_python_int(raw).map(Value::Int).ok_or_else(|| {
            format!(
                "invalid literal for int() with base 10: {}",
                python_repr_str(raw)
            )
        }),
        "boolean" => {
            let lowered = raw.trim().to_lowercase();
            if TRUE_WORDS.contains(&lowered.as_str()) {
                Ok(Value::Bool(true))
            } else if FALSE_WORDS.contains(&lowered.as_str()) {
                Ok(Value::Bool(false))
            } else {
                Err(format!("{} is not a boolean", python_repr_str(raw)))
            }
        }
        "array" | "object" => electricity_json::load_json(raw).map_err(|e| e.to_string()),
        _ => Ok(Value::Str(raw.to_string())),
    }
}

/// Python `int(s)`: optional surrounding whitespace, an optional
/// leading `+`/`-`, then one or more ASCII digits -- arbitrary
/// precision, via [`electricity_value::IntValue::parse_decimal`].
/// Unlike CPython, does not accept an underscore digit separator
/// (`"1_000"`); no probe needs it, and `interface.inputs` defaults are
/// ordinary YAML/JSON scalars, not Python source text.
fn parse_python_int(raw: &str) -> Option<electricity_value::IntValue> {
    let trimmed = raw.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
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

/// `core/interface_inputs.py::check_interface_inputs`, specialized to
/// `inputs = {}` always (every call from [`check_for_run`] runs with no
/// input namespace of its own -- neither this crate's [`CheckOptions`]
/// nor the golden corpus's own ground truth (`validate`/`run` called
/// with no CLI `-e`/`--state` override) ever carries one): every key
/// starts absent, so the function reduces to, per declared input in
/// document order, a `default:` (type-checked the same way, and
/// returned as this input's value) or a `required: true` (an error
/// naming it) -- any other key (no default, not required) is simply
/// skipped, exactly as an absent, optional, undefaulted input already
/// is. Stops and returns the first violation, matching Python's own
/// eager `raise`.
fn check_interface_inputs_error(document: &Value) -> Option<String> {
    let interface = document
        .as_dict()?
        .get(&Value::Str("interface".to_string()))?
        .as_dict()?;
    let iface_inputs = interface
        .get(&Value::Str("inputs".to_string()))?
        .as_dict()?;
    for (key, spec) in iface_inputs {
        let Some(spec_dict) = spec.as_dict() else {
            continue;
        };
        let key_str = key.py_str();
        let value = if let Some(default) = spec_dict.get(&Value::Str("default".to_string())) {
            default.clone()
        } else if is_truthy(spec_dict.get(&Value::Str("required".to_string()))) {
            return Some(format!(
                "missing required input '{key_str}' declared in orchestration interface."
            ));
        } else {
            continue;
        };
        let declared_type = match spec_dict.get(&Value::Str("type".to_string())) {
            Some(Value::Str(s))
                if crate::structural::INTERFACE_TYPE_NAMES.contains(&s.as_str()) =>
            {
                s.as_str()
            }
            _ => continue,
        };
        if crate::structural::matches_type(&value, declared_type) {
            continue;
        }
        if declared_type == "string"
            && matches!(value, Value::Int(_) | Value::Float(_) | Value::Bool(_))
        {
            continue;
        }
        if let Value::Str(raw) = &value {
            match coerce_interface_value(raw, declared_type) {
                Ok(coerced) => {
                    if crate::structural::matches_type(&coerced, declared_type) {
                        continue;
                    }
                    return Some(format!(
                        "input '{key_str}' declared type '{declared_type}' but got {}.",
                        crate::structural::py_class_name(&coerced)
                    ));
                }
                Err(message) => {
                    return Some(format!(
                        "input '{key_str}' declared type '{declared_type}' but {} could not be \
                         converted: {message}",
                        python_repr_str(raw)
                    ));
                }
            }
        }
        return Some(format!(
            "input '{key_str}' declared type '{declared_type}' but got {}.",
            crate::structural::py_class_name(&value)
        ));
    }
    None
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
pub fn check_for_run(path: &Path, options: &CheckOptions) -> Result<Program, RunCheckError> {
    let document = load_document(path).map_err(|err| RunCheckError::Compile(err.0))?;

    // `resolve_effective_settings`'s own shape checks on the document's
    // raw `plugins:`/`runtime:` blocks -- `run()`'s own order, and
    // before even the concurrency-limiter construction below (`cli/
    // effective_settings.py`, confirmed directly: these run as part of
    // building `effective`, which `RunConcurrencyLimiter.from_runtime_
    // config` is built from immediately after).
    if let Some(message) = effective_settings_shape_error(&document) {
        return Err(RunCheckError::Compile(message));
    }

    let merged_runtime = merged_runtime_block(options, &document);
    let config_errors = concurrency_config_errors(merged_runtime.as_ref());
    if !config_errors.is_empty() {
        let lines: Vec<String> = config_errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(RunCheckError::Compile(format!(
            "Invalid runtime concurrency configuration:\n{}",
            lines.join("\n")
        )));
    }

    // `check_interface_inputs` against the top-level `interface.inputs`
    // -- `run()`'s own position, after the concurrency limiter and
    // before structural checks (`cli/runtime_shim.py::run`, confirmed
    // directly). Always run with an empty input namespace: neither
    // surface this crate exposes (`check_report`/`check_for_run`) takes
    // a CLI `-e`/`--state` value of its own, matching every golden
    // case's own ground truth (`validate`/`run` always called with no
    // such override) -- see [`check_interface_inputs_error`]'s own doc
    // comment.
    if let Some(message) = check_interface_inputs_error(&document) {
        return Err(RunCheckError::Compile(message));
    }

    let structural = structural_errors(&document);
    if !structural.is_empty() {
        return Err(RunCheckError::Structural(structural));
    }

    let (origin, document_dir, confinement_root) = document_origin(path);

    let mut program =
        compile_document(&document, &origin).map_err(|err| RunCheckError::Compile(err.0))?;

    let known_groups = concurrency_group_names(merged_runtime.as_ref());
    let group_errors = groups::unknown_group_errors(&program, &known_groups);
    if !group_errors.is_empty() {
        return Err(RunCheckError::Structural(group_errors));
    }

    cycles::detect_cycles(&document, Some(path)).map_err(|err| RunCheckError::Cycle(err.0))?;

    let computed_digest =
        digest::document_content_digest(path, &document, &confinement_root).unwrap_or_default();
    program.document = Some(DocumentInfo {
        path_as_given: path.display().to_string(),
        resolved_directory: document_dir,
        confinement_root,
        digest: computed_digest,
    });

    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

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
        let mut spec = Dict::new();
        for (k, v) in spec_pairs {
            spec.insert(Value::Str(k.to_string()), v);
        }
        let mut inputs = Dict::new();
        inputs.insert(Value::Str("x".to_string()), Value::Dict(spec));
        let mut interface = Dict::new();
        interface.insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let mut dict = Dict::new();
        dict.insert(Value::Str("interface".to_string()), Value::Dict(interface));
        Value::Dict(dict)
    }

    #[test]
    fn missing_required_input_with_no_default_is_an_error() {
        let doc = interface_doc(vec![
            ("type", Value::Str("string".to_string())),
            ("required", Value::Bool(true)),
        ]);
        assert_eq!(
            check_interface_inputs_error(&doc),
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
            check_interface_inputs_error(&doc),
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
            check_interface_inputs_error(&doc),
            Some("input 'x' declared type 'integer' but got list.".to_string())
        );
    }

    #[test]
    fn optional_undefaulted_input_is_not_an_error() {
        let doc = interface_doc(vec![("type", Value::Str("string".to_string()))]);
        assert_eq!(check_interface_inputs_error(&doc), None);
    }

    #[test]
    fn matching_default_is_not_an_error() {
        let doc = interface_doc(vec![
            ("type", Value::Str("integer".to_string())),
            ("default", Value::from(3i64)),
        ]);
        assert_eq!(check_interface_inputs_error(&doc), None);
    }
}
