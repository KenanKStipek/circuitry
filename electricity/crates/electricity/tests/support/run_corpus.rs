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
//! (`../run_corpus.rs`'s own `#[ignore = "needs lanes B-C"]` tests).

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

fn golden_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/run_corpus.json"
    ))
}

/// Loads and parses `tests/golden/run_corpus.json` -- panics (this is
/// test support, not a library API) on a missing file, invalid JSON, or
/// a case missing one of [`CorpusResult`]'s own required fields.
pub fn load_corpus() -> Vec<CorpusCase> {
    let text = fs::read_to_string(golden_path())
        .unwrap_or_else(|err| panic!("{}: {err}", golden_path().display()));
    let raw: Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{}: invalid JSON: {err}", golden_path().display()));
    let cases = raw
        .as_array()
        .unwrap_or_else(|| panic!("{}: expected a JSON array", golden_path().display()));

    cases
        .iter()
        .map(|case| {
            let name = case["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case missing a string 'name': {case}"))
                .to_string();
            let inputs = case["inputs"].clone();
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
