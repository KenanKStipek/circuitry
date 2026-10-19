//! Issue #632's acceptance item "the stores' totals equal the run's
//! totals on both engines' recorded fixtures, including a retried
//! prompt": every recorded fixture with at least one `prompt` effect,
//! loaded from its own `snapshots.jsonl` (never hand-copied JSON),
//! fed through the Cost screen's own `largest_stores` the way
//! `tui.rs` itself does, and checked against `runtime.last_run.totals`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oscilloscope_core::cost::largest_stores;
use oscilloscope_core::model::{RowStatus, flatten_state};
use oscilloscope_core::render::RowKind;
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// *name*'s own last recorded `snapshots.jsonl` line, unwrapped --
/// the fixture's real final state, exactly as `cof`/`electricity`
/// wrote it (never hand-copied).
fn load_final_state(name: &str) -> Value {
    let dir = fixtures_dir().join(name);
    let text = std::fs::read_to_string(dir.join("snapshots.jsonl"))
        .unwrap_or_else(|e| panic!("{name}: couldn't read snapshots.jsonl: {e}"));
    let last_line = text
        .lines()
        .rfind(|l| !l.is_empty())
        .unwrap_or_else(|| panic!("{name}: snapshots.jsonl has no lines"));
    let wrapper: Value = serde_json::from_str(last_line).expect("valid JSON line");
    wrapper["state"].clone()
}

/// Checks *name*'s own largest-stores totals (in and out) against the
/// state's own `runtime.last_run.totals`, and that the footer's gap
/// is *expected_gap* -- 0 for every fixture here, none of which has a
/// declared-outputs `use` child whose effects complete without
/// landing in the final state tree (DESIGN.md \u00a71.2, the one case
/// the footer's gap exists to surface).
fn assert_totals_match(name: &str, expected_gap: u64) {
    let state = load_final_state(name);
    let flat = flatten_state(&state);
    let statuses: BTreeMap<String, RowStatus> = BTreeMap::new();
    let kinds: BTreeMap<String, RowKind> = BTreeMap::new();
    let result = largest_stores("prime", &flat, &statuses, &kinds, Some(&state));

    let totals = state
        .get("runtime")
        .and_then(|r| r.get("last_run"))
        .and_then(|r| r.get("totals"))
        .unwrap_or_else(|| panic!("{name}: no runtime.last_run.totals in the recorded state"));
    let run_sent = totals["tokens_sent"].as_u64().unwrap();
    let run_received = totals["tokens_received"].as_u64().unwrap();

    let table_sent: u64 = result.rows.iter().map(|r| r.tokens_sent).sum();
    let table_received: u64 = result.rows.iter().map(|r| r.tokens_received).sum();

    assert_eq!(
        table_sent, run_sent,
        "{name}: the largest-stores table's own tokens-in sum must equal the run's totals"
    );
    assert_eq!(
        table_received, run_received,
        "{name}: the largest-stores table's own tokens-out sum must equal the run's totals"
    );
    assert_eq!(
        result.footer.run_tokens_sent, run_sent,
        "{name}: the footer's own run-tokens-in must equal the run's totals"
    );
    assert_eq!(
        result.footer.run_tokens_received, run_received,
        "{name}: the footer's own run-tokens-out must equal the run's totals"
    );
    assert_eq!(
        result.footer.gap_tokens, expected_gap,
        "{name}: the footer's own gap must be exactly {expected_gap}"
    );
}

#[test]
fn prompt_chain_fixture_totals_match_the_run() {
    assert_totals_match("prompt_chain", 0);
}

#[test]
fn prompt_failing_fixture_totals_match_the_run() {
    // The one prompt never succeeds (every adapter attempt fails
    // before reporting any tokens), so both the run's own totals and
    // the table are 0/0 -- the gap is still 0, not a mismatch to
    // paper over.
    assert_totals_match("prompt_failing", 0);
}

#[test]
fn prompt_tree_loop_concurrency_fixture_totals_match_the_run() {
    assert_totals_match("prompt_tree_loop_concurrency", 0);
}

#[test]
fn prompt_retried_cof_fixture_totals_match_the_run() {
    // A real `cof run` of a `prompt_type: json` effect that fails
    // decode on its first attempt (tokens still spent on that
    // attempt) and succeeds on a retry, plus a second, plain prompt
    // -- two stores, exercising the `_total` accounting rule
    // `cost.rs::retried_prompt_state`'s hand-written state already
    // covers, now against a real recorded run.
    assert_totals_match("prompt_retried_cof", 0);
}

#[test]
fn prompt_retried_electricity_fixture_totals_match_the_run() {
    // The identical scenario and identical replies file, recorded
    // against electricity instead -- byte-identical to the cof
    // recording on every field the Cost screen reads (tokens,
    // `_total`s, `retries_used`, `runtime.last_run.totals`).
    assert_totals_match("prompt_retried_electricity", 0);
}
