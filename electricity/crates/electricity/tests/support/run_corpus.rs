//! Loads the golden run corpus `electricity/scripts/generate_vm_run_
//! corpus.py` writes (issue #431's Test strategy section, "Golden run
//! corpus") -- `tests/golden/run_corpus.json`, each case a real
//! `cof run --out --events --live-state`'s normalized result
//! (`electricity/scripts/_run_corpus.py`'s own doc comment has the
//! exact normalization rules: a run id/UUID, a timestamp, a process id,
//! and the case's own temporary root all become a stable placeholder).
//!
//! This lane (A) only shape-checks the corpus end to end -- that it
//! parses, that every case has the fields a real result always has, and
//! that a failure case's `returncode` is actually nonzero. Comparing a
//! case's *content* against `electricity::run_orchestration`'s own
//! output is lanes B-D's job, once there is a VM to run anything with
//! (`../run_corpus.rs`'s own `#[ignore = "needs lanes B-D"]` tests).

use serde_json::Value;
use std::fs;
use std::path::Path;

/// One case from the golden corpus: its own name, the `-e` inputs it was
/// run with, and [`CorpusResult`]. Some fields are only read by a later
/// lane's own comparison tests (`run_corpus.rs`'s own `#[ignore]`d
/// ones), not by this lane's shape checks -- `#[allow(dead_code)]`
/// rather than dropping them now and re-adding them later.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CorpusCase {
    pub name: String,
    pub inputs: Value,
    pub result: CorpusResult,
}

/// A case's own normalized `cof run --out --events --live-state` result.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CorpusResult {
    pub returncode: i64,
    pub stdout: String,
    pub stderr: String,
    /// The `--out` state, or `None` if nothing was written (a failure
    /// before any state exists -- not reachable by this lane's own
    /// smoke corpus, every one of whose cases runs a config-valid,
    /// structurally-valid document, but kept `Option` for a later
    /// lane's own corpus that adds one).
    pub state: Option<Value>,
    /// The parsed `--events` sequence, one entry per JSONL line.
    pub events: Vec<Value>,
    /// The final `--live-state` snapshot, or `None` if nothing was
    /// written.
    pub live_state: Option<Value>,
}

// This module is compiled fresh into each integration-test binary that
// declares `mod support;` -- `#[allow(dead_code)]` on these two because
// not every one of those binaries calls `load_corpus` itself (lane C's
// own `tool_run_corpus.rs` only ever calls `load_corpus_at` directly, a
// different golden file).
#[allow(dead_code)]
fn golden_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/run_corpus.json"
    ))
}

/// Loads and parses `tests/golden/run_corpus.json` -- panics (this is
/// test support, not a library API) on a missing file, invalid JSON, or
/// a case missing one of [`CorpusResult`]'s own required fields.
#[allow(dead_code)]
pub fn load_corpus() -> Vec<CorpusCase> {
    load_corpus_at(golden_path().to_str().expect("golden_path is valid UTF-8"))
}

/// Like [`load_corpus`], but for any golden file this same corpus shape
/// was written to -- every later lane's own `generate_*_run_corpus.py`
/// writes a file of this exact shape (`_run_corpus.py`'s own
/// `render_corpus`), just somewhere other than `tests/golden/run_corpus.
/// json`.
pub fn load_corpus_at(path: &str) -> Vec<CorpusCase> {
    let path = Path::new(path);
    let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let raw: Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{}: invalid JSON: {err}", path.display()));
    let cases = raw
        .as_array()
        .unwrap_or_else(|| panic!("{}: expected a JSON array", path.display()));

    cases
        .iter()
        .map(|case| {
            let name = case["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case missing a string 'name': {case}"))
                .to_string();
            let inputs = case.get("inputs").cloned().unwrap_or(Value::Null);
            let result = &case["result"];
            let returncode = result["returncode"]
                .as_i64()
                .unwrap_or_else(|| panic!("{name}: result missing an integer 'returncode'"));
            let stdout = result["stdout"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: result missing a string 'stdout'"))
                .to_string();
            let stderr = result["stderr"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: result missing a string 'stderr'"))
                .to_string();
            let state = match &result["state"] {
                Value::Null => None,
                value => Some(value.clone()),
            };
            let events = result["events"]
                .as_array()
                .unwrap_or_else(|| panic!("{name}: result missing an array 'events'"))
                .clone();
            let live_state = match &result["live_state"] {
                Value::Null => None,
                value => Some(value.clone()),
            };
            CorpusCase {
                name,
                inputs,
                result: CorpusResult {
                    returncode,
                    stdout,
                    stderr,
                    state,
                    events,
                    live_state,
                },
            }
        })
        .collect()
}
