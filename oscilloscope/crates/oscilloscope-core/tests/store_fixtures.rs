//! Issue #695's acceptance item "the stores' sum equals the run's
//! totals on both engines' recorded fixtures, including a retried
//! prompt": every recorded fixture with at least one `prompt` effect,
//! loaded from its own `snapshots.jsonl` (never hand-copied JSON),
//! fed through the Store screen's own `build_store_tree` the way
//! `tui.rs` itself does, and checked against `runtime.last_run.totals`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oscilloscope_core::model::{RunModel, flatten_state};
use oscilloscope_core::render::RowKind;
use oscilloscope_core::store::{StoreNode, build_store_tree};
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

fn empty_header() -> oscilloscope_core::render::Header {
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let mut model = RunModel::new();
    oscilloscope_core::render::build_folded(
        "demo.yml",
        "cof",
        &plan,
        &mut model,
        None,
        oscilloscope_core::model::ProcessState::Running,
        0.0,
        "2026-01-01T00:00:00Z",
        &[],
        true,
        &Default::default(),
    )
    .header
}

fn sum_tokens(nodes: &[StoreNode]) -> (u64, u64) {
    nodes.iter().fold((0, 0), |(sent, received), node| {
        let (s, r) = match node {
            StoreNode::Leaf(leaf) => (leaf.tokens_sent, leaf.tokens_received),
            StoreNode::Container(c) => sum_tokens(&c.children),
        };
        (sent + s, received + r)
    })
}

/// Checks *name*'s own store tree's total tokens (in and out) against
/// the state's own `runtime.last_run.totals` -- the tree built with no
/// `RowKind` known for any path, so every real container (`use`,
/// `if`, a loop) is read transparently (issue #695's splice rule),
/// which still finds every leaf without creating a tile for any of
/// them; the sum is unaffected either way.
fn assert_totals_match(name: &str) {
    let state = load_final_state(name);
    let flat = flatten_state(&state);
    let kinds: BTreeMap<String, RowKind> = BTreeMap::new();
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let header = empty_header();
    let model = RunModel::new();
    let tree = build_store_tree(
        "prime",
        &flat,
        &kinds,
        &plan,
        &header,
        &model,
        "2026-01-01T00:00:00Z",
        true,
    );

    let totals = state
        .get("runtime")
        .and_then(|r| r.get("last_run"))
        .and_then(|r| r.get("totals"))
        .unwrap_or_else(|| panic!("{name}: no runtime.last_run.totals in the recorded state"));
    let run_sent = totals["tokens_sent"].as_u64().unwrap();
    let run_received = totals["tokens_received"].as_u64().unwrap();

    let (tree_sent, tree_received) = sum_tokens(&tree);
    assert_eq!(
        tree_sent, run_sent,
        "{name}: the store tree's own tokens-in sum must equal the run's totals"
    );
    assert_eq!(
        tree_received, run_received,
        "{name}: the store tree's own tokens-out sum must equal the run's totals"
    );
}

#[test]
fn prompt_chain_fixture_totals_match_the_run() {
    assert_totals_match("prompt_chain");
}

#[test]
fn prompt_failing_fixture_totals_match_the_run() {
    // The one prompt never succeeds (every adapter attempt fails
    // before reporting any tokens), so both the run's own totals and
    // the tree are 0/0.
    assert_totals_match("prompt_failing");
}

#[test]
fn prompt_tree_loop_concurrency_fixture_totals_match_the_run() {
    assert_totals_match("prompt_tree_loop_concurrency");
}

/// *name*'s own `prime.ask` -- the retried prompt in both
/// `prompt_retried_*` fixtures -- really did retry, not merely that
/// its tokens happen to add up: `assert_totals_match` alone can't
/// tell a retry's extra tokens apart from two effects that simply
/// both succeeded on the first attempt.
fn assert_the_retry_happened(name: &str) {
    let state = load_final_state(name);
    let retries_used = state["prime"]["ask"]["meta"]["retries_used"]
        .as_u64()
        .unwrap_or_else(|| panic!("{name}: prime.ask has no retries_used"));
    assert_eq!(
        retries_used, 1,
        "{name}: prime.ask must have actually retried once"
    );
}

#[test]
fn prompt_retried_cof_fixture_totals_match_the_run() {
    // A real `cof run` of a `prompt_type: json` effect that fails
    // decode on its first attempt (tokens still spent on that
    // attempt) and succeeds on a retry, plus a second, plain prompt
    // -- two stores, exercising the `_total` accounting rule against
    // a real recorded run.
    assert_totals_match("prompt_retried_cof");
    assert_the_retry_happened("prompt_retried_cof");
}

#[test]
fn prompt_retried_electricity_fixture_totals_match_the_run() {
    // The identical scenario and identical replies file, recorded
    // against electricity instead -- byte-identical to the cof
    // recording on every field the Store screen reads (tokens,
    // `_total`s, `retries_used`, `runtime.last_run.totals`).
    assert_totals_match("prompt_retried_electricity");
    assert_the_retry_happened("prompt_retried_electricity");
}
