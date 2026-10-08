//! Lane B: `check_report`/`check_for_run`, matching
//! `cli/runtime_shim.py`'s `validate(...)` and
//! `run(RunRequest(..., validate_only=True))` — the ordering of
//! structural checks, concurrency-configuration errors, compilation,
//! and the compile-time half of #406, and the two surfaces' different
//! error shapes.

use crate::{CheckOptions, CheckReport, RunCheckError, not_implemented};
use electricity_bytecode::Program;
use std::path::Path;

/// Matches `runtime_shim.validate(path, config=None, skip_preflight=...,
/// trust_document=...)`.
///
/// Stub (lane A): always reports `ok: false` with one error until lane B
/// lands.
pub fn check_report(path: &Path, options: &CheckOptions) -> CheckReport {
    let _ = (path, options);
    CheckReport {
        ok: false,
        errors: vec![not_implemented("check_report", "B")],
        warnings: Vec::new(),
    }
}

/// The exact error text a `cof run` of *path* reports, matching
/// `runtime_shim.run(RunRequest(..., validate_only=True,
/// skip_preflight=..., trust_document=...))`.
///
/// Stub (lane A): always fails until lane B lands.
pub fn check_for_run(path: &Path, options: &CheckOptions) -> Result<Program, RunCheckError> {
    let _ = (path, options);
    Err(RunCheckError::NotImplemented(not_implemented(
        "check_for_run",
        "B",
    )))
}
