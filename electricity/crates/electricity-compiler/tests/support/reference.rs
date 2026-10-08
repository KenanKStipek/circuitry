//! The JSON shape `electricity/scripts/_compiler_corpus.py` writes for
//! every case, and a comparison-mode-aware string match for a message
//! that may be Circuitry's own exact text or third-party text that only
//! needs to fail "at the same place" (DESIGN.md §1, §12).

// Lane A's own `golden_smoke.rs` only reads `name`/`files`/`entry`/
// `run_error`; `validate`/`definition`/`comparison`/`matches` exist for
// lane B/C/D's `golden_{load,compile,compose}.rs`, which this lane's
// stub has nothing to check them against yet.
#![allow(dead_code)]

use serde::Deserialize;
use std::collections::BTreeMap;

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

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub name: String,
    pub files: BTreeMap<String, FileContent>,
    pub entry: String,
    pub validate: ValidateResult,
    pub run_error: Option<String>,
    pub definition: Option<serde_json::Value>,
    pub comparison: Comparison,
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
