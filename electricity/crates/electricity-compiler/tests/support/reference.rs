//! The JSON shape `electricity/scripts/_compiler_corpus.py` writes for
//! every case, and a comparison-mode-aware string match for a message
//! that may be Circuitry's own exact text or third-party text that only
//! needs to fail "at the same place" (DESIGN.md §1, §12).

// Lane A's own `golden_smoke.rs` only reads `name`/`files`/`entry`/
// `run_error`; `validate`/`definition`/`comparison`/`matches` exist for
// lane B/C/D's `golden_{load,compile,compose}.rs`, which this lane's
// stub has nothing to check them against yet.
#![allow(dead_code)]

use electricity_compiler::CheckOptions;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FileContent {
    Symlink { symlink: String },
    BytesHex { bytes_hex: String },
    Text(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidateResult {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comparison {
    pub validate_errors: Vec<String>,
    pub run_error: Option<String>,
}

/// The case's own `skip_preflight`/`trust_document` -- `_compiler_
/// corpus.py`'s `run_case` always records both (its own `options.get(
/// ..., True)` default), so this is never actually absent in a
/// generated file; `Option` on `Case::options` only covers a
/// hand-written fixture that omits it.
#[derive(Debug, Clone, Deserialize)]
pub struct CaseOptions {
    pub skip_preflight: bool,
    pub trust_document: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub name: String,
    pub files: BTreeMap<String, FileContent>,
    pub entry: String,
    #[serde(default)]
    pub options: Option<CaseOptions>,
    pub validate: ValidateResult,
    pub run_error: Option<String>,
    pub definition: Option<serde_json::Value>,
    #[serde(default)]
    pub definition_error: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
    pub comparison: Comparison,
    /// Names the lane whose stub the `check_report`/`validate` side of
    /// this case still runs into even once this lane's own
    /// `check_for_run` fully agrees with Circuitry -- e.g. a
    /// concurrency-configuration case: `validate()` only parses
    /// `runtime.max_concurrency`/`concurrency_groups` *after* a
    /// document compiles, so a case that exercises it needs a real
    /// compile on the `check_report` side even though `run()` (and so
    /// `check_for_run`) checks the same configuration *before*
    /// structural checks even start and needs no such thing (lane B's
    /// own `pipeline.rs` docs explain the asymmetry). Absent/`None` for
    /// every case lane B owns outright on both surfaces. Not written by
    /// `_compiler_corpus.py` (that helper has no notion of a
    /// surface-specific gap); set by hand in `generate_compiler_load_
    /// corpus.py`'s own `CASES` list for the handful that need it.
    #[serde(default)]
    pub check_report_needs: Option<String>,
}

impl Case {
    /// The [`CheckOptions`] this case was recorded against --
    /// `_compiler_corpus.py`'s own defaults (`skip_preflight: true,
    /// trust_document: true`) when the case carries no `options` of
    /// its own, matching `run_case`'s `options.get(..., True)`. Using
    /// `CheckOptions::default()` instead (`false`/`false`) would run
    /// every golden case against the wrong settings.
    pub fn check_options(&self) -> CheckOptions {
        // `config_runtime` is always `None`: every golden case is
        // recorded by `_compiler_corpus.py::run_case` calling
        // `validate`/`run` with `config=None`, so there is never a
        // config-file runtime block to merge in on the Rust side either.
        match &self.options {
            Some(options) => CheckOptions {
                skip_preflight: options.skip_preflight,
                trust_document: options.trust_document,
                config_runtime: None,
            },
            None => CheckOptions {
                skip_preflight: true,
                trust_document: true,
                config_runtime: None,
            },
        }
    }
}

/// `expected` compared against `actual` per *mode*: `"exact"` is a
/// literal match; `"location"` only checks `actual` is non-empty
/// (third-party text, DESIGN.md §1/§12 -- never compared word for word).
pub fn matches(mode: &str, expected: &str, actual: &str) -> bool {
    match mode {
        "location" => !actual.is_empty(),
        _ => expected == actual,
    }
}

/// Replaces *root*'s own path with `<root>` in *actual* -- the Rust-
/// side counterpart of `_compiler_corpus.py`'s own normalization, so an
/// error or digest produced against a freshly materialized temp case
/// compares equal to the golden file's own `<root>`-normalized text.
pub fn normalize(actual: &str, root: &Path) -> String {
    actual.replace(&*root.to_string_lossy(), "<root>")
}
