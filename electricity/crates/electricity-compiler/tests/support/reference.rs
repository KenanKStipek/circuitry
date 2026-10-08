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
use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FileContent {
    Symlink {
        symlink: String,
    },
    BytesHex {
        bytes_hex: String,
        /// An octal permission string (e.g. `"000"`) applied with
        /// `chmod` after the file is written -- `_compiler_corpus.py`'s
        /// own optional `"mode"` key, for a lane D case exercising an
        /// unreadable prompt file (`core/prompt_files.py`'s "could not
        /// be read" branch). Not meaningful on Windows; a case that
        /// needs it is Unix-only by nature, same as a symlink-escape
        /// case already is -- see `support::corpus::materialize`.
        #[serde(default)]
        mode: Option<String>,
    },
    /// *repeat* written *count* times -- `_compiler_corpus.py`'s own
    /// `{"repeat": text, "count": int}`, a generated-content spec for
    /// a case whose file content is large and uniform (a lane D case
    /// one byte over the prompt-file size limit, say), so the golden
    /// file records the short spec rather than the file's own megabyte
    /// of bytes.
    Repeat {
        repeat: String,
        count: usize,
    },
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
    /// The CLI's `-e key=value` text pairs, in document/JSON-object
    /// order -- `_compiler_corpus.py`'s own optional `"inputs"` key
    /// (issue #429), absent for every case that doesn't pass any.
    #[serde(default)]
    pub inputs: IndexMap<String, String>,
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
    /// `core.lint.lint_orchestration(orch)`'s own output alone, not the
    /// rest of `validate()`'s combined `warnings` list -- `check_report`
    /// does not reproduce these advisories at all (issue #428, lint
    /// parity, not assigned to any #408 lane). A case whose document
    /// trips a lint rule carries it here, not in `validate.warnings`;
    /// callers subtract this list before comparing (see
    /// [`Case::non_lint_warnings`]). Empty for every case whose document
    /// never loaded far enough for `validate()` to compute it either.
    #[serde(default)]
    pub lint_warnings: Vec<String>,
}

impl Case {
    /// The [`CheckOptions`] this case was recorded against --
    /// `_compiler_corpus.py`'s own defaults (`skip_preflight: true,
    /// trust_document: true`) when the case carries no `options` of
    /// its own, matching `run_case`'s `options.get(..., True)`. Using
    /// `CheckOptions::default()` instead (`false`/`false`) would run
    /// every golden case against the wrong settings.
    /// `self.validate.warnings`, with every warning `lint_warnings`
    /// also names removed (first match only, so a warning that happens
    /// to repeat isn't over-subtracted) -- what `check_report`'s own
    /// `warnings` is expected to equal, now that it does not reproduce
    /// Circuitry's lint advisories (issue #428).
    pub fn non_lint_warnings(&self) -> Vec<String> {
        let mut remaining = self.lint_warnings.clone();
        self.validate
            .warnings
            .iter()
            .filter(
                |warning| match remaining.iter().position(|w| w == *warning) {
                    Some(index) => {
                        remaining.remove(index);
                        false
                    }
                    None => true,
                },
            )
            .cloned()
            .collect()
    }

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
                inputs: options.inputs.clone(),
            },
            None => CheckOptions {
                skip_preflight: true,
                trust_document: true,
                config_runtime: None,
                inputs: IndexMap::new(),
            },
        }
    }
}

/// `expected` compared against `actual` per *mode*: `"exact"` is a
/// literal match; `"location"` only checks `actual` is non-empty text
/// with no location structure of its own (a YAML/JSON parse error, a
/// `.toon` refusal -- pure third-party text, DESIGN.md §1/§12, never
/// compared word for word, not even its location); `"location_prefix"`
/// is for Circuitry's own `"{location}: {message}"`-shaped errors (a
/// schema violation, bare or wrapped in `"Orchestration validation
/// failed:\n  - ..."`) -- see [`matches_location`].
pub fn matches(mode: &str, expected: &str, actual: &str) -> bool {
    match mode {
        "location" => !actual.is_empty(),
        "location_prefix" => matches_location(expected, actual),
        _ => expected == actual,
    }
}

/// Compares Circuitry's own prefix of a `"{location}: {message}"`-shaped
/// error exactly, leaving the message past the first `": "` on each
/// line -- third-party `jsonschema` text, DESIGN.md §1/§12 --
/// unchecked beyond being present. Handles both shapes this crate's own
/// errors take: `check_report`'s bare `"<location>: <message>"` (one
/// error, no wrapper) and `check_for_run`'s `"Orchestration validation
/// failed:\n  - <location>: <message>\n  - ..."` (the wrapper, then one
/// `"  - "`-prefixed line per error) -- the wrapper and the per-line
/// location must all match exactly, and there must be the same number
/// of lines, or this returns `false` outright rather than guessing
/// which lines correspond.
pub fn matches_location(expected: &str, actual: &str) -> bool {
    const WRAPPER: &str = "Orchestration validation failed:\n";
    let (expected_wrapped, expected_body) = match expected.strip_prefix(WRAPPER) {
        Some(rest) => (true, rest),
        None => (false, expected),
    };
    let (actual_wrapped, actual_body) = match actual.strip_prefix(WRAPPER) {
        Some(rest) => (true, rest),
        None => (false, actual),
    };
    if expected_wrapped != actual_wrapped {
        return false;
    }
    let expected_lines: Vec<&str> = expected_body.lines().collect();
    let actual_lines: Vec<&str> = actual_body.lines().collect();
    if expected_lines.len() != actual_lines.len() || expected_lines.is_empty() {
        return false;
    }
    expected_lines
        .iter()
        .zip(actual_lines.iter())
        .all(|(expected_line, actual_line)| {
            line_location(expected_line) == line_location(actual_line)
        })
}

/// A `"  - <location>: <message>"` or bare `"<location>: <message>"`
/// line's own `<location>` -- the text up to the first `": "`, or the
/// whole (trimmed) line if it has none.
fn line_location(line: &str) -> &str {
    let trimmed = line.strip_prefix("  - ").unwrap_or(line);
    match trimmed.find(": ") {
        Some(index) => &trimmed[..index],
        None => trimmed,
    }
}

/// Replaces *root*'s own path with `<root>` in *actual* -- the Rust-
/// side counterpart of `_compiler_corpus.py`'s own normalization, so an
/// error or digest produced against a freshly materialized temp case
/// compares equal to the golden file's own `<root>`-normalized text.
pub fn normalize(actual: &str, root: &Path) -> String {
    actual.replace(&*root.to_string_lossy(), "<root>")
}
