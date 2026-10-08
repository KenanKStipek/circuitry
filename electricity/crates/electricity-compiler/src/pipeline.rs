//! Lane B: `check_report`/`check_for_run`, matching
//! `cli/runtime_shim.py`'s `validate(...)` and
//! `run(RunRequest(..., validate_only=True))` -- the ordering of
//! structural checks, concurrency-configuration errors, compilation,
//! and the compile-time half of #406, and the two surfaces' different
//! error shapes.

use crate::{
    CheckOptions, CheckReport, DocumentOrigin, RunCheckError, compile_document, digest,
    load_document, not_implemented, structural_errors, unknown_key_warnings,
};
use electricity_bytecode::Program;
use electricity_value::Value;
use std::path::Path;

/// `runtime.max_concurrency`/`runtime.concurrency_groups` configuration
/// errors (`core/concurrency.py`'s `parse_max_concurrency`/
/// `parse_concurrency_groups`), checked against the merged config
/// before a document even compiles -- not an effect's `group:`
/// reference into it (that's `groups::unknown_group_errors`, lane C).
///
/// Stub (lane A): always fails until lane B lands.
pub(crate) fn concurrency_config_errors(runtime_block: Option<&Value>) -> Vec<String> {
    let _ = runtime_block;
    vec![not_implemented("pipeline::concurrency_config_errors", "B")]
}

/// Matches `runtime_shim.validate(path, config=None, skip_preflight=...,
/// trust_document=...)`.
///
/// Order (once lane B lands): [`load_document`], [`structural_errors`]/
/// [`unknown_key_warnings`], then [`concurrency_config_errors`].
///
/// Stub (lane A): always reports `ok: false` with one error until lane B
/// lands.
pub fn check_report(path: &Path, options: &CheckOptions) -> CheckReport {
    let _ = options;
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
    let mut errors = structural_errors(&document);
    errors.extend(concurrency_config_errors(None));
    let warnings = unknown_key_warnings(&document);
    CheckReport {
        ok: errors.is_empty(),
        errors,
        warnings,
    }
}

/// The exact error text a `cof run` of *path* reports, matching
/// `runtime_shim.run(RunRequest(..., validate_only=True,
/// skip_preflight=..., trust_document=...))`.
///
/// Order (once lane B lands): [`load_document`], the structural and
/// concurrency-configuration errors (as [`RunCheckError::Structural`]),
/// then [`compile_document`] (lane C, as [`RunCheckError::Compile`]).
/// `Program.document` ([`electricity_bytecode::DocumentInfo`]) is
/// filled in here, not by `compile_document` itself -- this function
/// has *path*'s raw bytes (via [`digest::document_content_digest`]),
/// which `compile_document` never sees.
///
/// Stub (lane A): always fails until lane B lands.
pub fn check_for_run(path: &Path, options: &CheckOptions) -> Result<Program, RunCheckError> {
    let _ = options;
    let document = load_document(path).map_err(|err| RunCheckError::Compile(err.0))?;

    let mut structural = structural_errors(&document);
    structural.extend(concurrency_config_errors(None));
    if !structural.is_empty() {
        return Err(RunCheckError::Structural(structural));
    }

    // A rough stand-in for the nearest-`config.json` walk
    // (`core/prompt_files.py::default_project_root`, not yet ported to
    // Rust): the document's own directory, which is what every case
    // with no project config above it resolves to anyway. Lane B
    // replaces this with the real walk when it lands.
    let confinement_root = path.parent().unwrap_or(path).to_path_buf();
    let origin = DocumentOrigin::File {
        document_dir: confinement_root.clone(),
        confinement_root: confinement_root.clone(),
    };
    let mut program =
        compile_document(&document, &origin).map_err(|err| RunCheckError::Compile(err.0))?;

    if let Ok(computed_digest) = digest::document_content_digest(path, &document, &confinement_root)
    {
        if let Some(document_info) = &mut program.document {
            document_info.digest = computed_digest;
        }
    }

    Ok(program)
}
