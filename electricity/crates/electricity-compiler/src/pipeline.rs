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
                other.type_name()
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

/// Python's `str.strip()` character set: space, tab, newline, carriage
/// return, vertical tab, form feed -- used only for
/// [`check_report`]'s own "Orchestration file is empty." check
/// (`cli/runtime_shim.py::validate`'s `text.strip()`), which runs on
/// the raw file text before any format-specific parsing.
fn is_python_strip_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
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

    if let Err(err) = cycles::detect_cycles(&document, &origin) {
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

    let merged_runtime = merged_runtime_block(options, &document);
    let config_errors = concurrency_config_errors(merged_runtime.as_ref());
    if !config_errors.is_empty() {
        let lines: Vec<String> = config_errors.iter().map(|e| format!("  - {e}")).collect();
        return Err(RunCheckError::Compile(format!(
            "Invalid runtime concurrency configuration:\n{}",
            lines.join("\n")
        )));
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

    cycles::detect_cycles(&document, &origin).map_err(|err| RunCheckError::Cycle(err.0))?;

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
}
