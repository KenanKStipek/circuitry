//! Issue #675: recorded fixtures from a real run against both
//! engines (`scripts/record_fixtures.py`'s `PROMPT_LIVE_STREAM_CASES`,
//! `tests/fixtures/prompt_live_stream_cof`/`prompt_live_stream_
//! electricity`) -- a raw loopback server standing in for a real
//! `ollama`, answering on its own real delay, since the `scripted`
//! adapter's own `chunks` deliver every delta at once with nothing
//! for a poll to ever catch mid-stream. Checked against what each
//! engine actually writes, never a hand-written shape this crate
//! invented on its own.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::{PromptLive, PromptLiveFigures, prompt_live_figures};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn load_snapshots(name: &str) -> Vec<Value> {
    let dir = fixtures_dir().join(name);
    let text = std::fs::read_to_string(dir.join("snapshots.jsonl"))
        .unwrap_or_else(|e| panic!("{name}: couldn't read snapshots.jsonl: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).expect("valid JSON line");
            wrapper["state"].clone()
        })
        .collect()
}

fn ask_meta(state: &Value) -> Option<&Value> {
    state.get("prime")?.get("ask")?.get("meta")
}

/// Every `meta.live` this fixture's own `ask` node ever carried,
/// oldest first -- `None` entries (before the node exists, or once
/// its own attempt has ended and `meta.live` is removed) are dropped,
/// not the snapshots that happen to have no `live` at all.
fn live_sequence(name: &str) -> Vec<PromptLive> {
    load_snapshots(name)
        .iter()
        .filter_map(|state| ask_meta(state)?.get("live"))
        .filter_map(PromptLive::from_value)
        .collect()
}

/// Both engines write the identical shape (issue #674's PR #680 for
/// `cof`, PR #683 for `electricity`): streaming, growing estimates,
/// a first-token instant, then a rate once there's been enough of a
/// second delta to measure one.
fn assert_stream_shape(name: &str) {
    let sequence = live_sequence(name);
    assert!(
        sequence.len() >= 3,
        "{name}: expected at least a waiting, a first-delta and a second-delta snapshot; got {}",
        sequence.len()
    );

    let waiting = &sequence[0];
    assert_eq!(waiting.adapter, "ollama");
    assert!(waiting.streaming);
    assert!(waiting.first_token_at.is_none());
    assert_eq!(waiting.tokens_received_estimate, Some(0));
    assert_eq!(waiting.chars_received, Some(0));

    let after_first_delta = sequence
        .iter()
        .find(|live| live.first_token_at.is_some())
        .unwrap_or_else(|| panic!("{name}: no snapshot ever reached a first delta"));
    assert_eq!(after_first_delta.chars_received, Some(7), "\"Thanks \"");
    assert_eq!(after_first_delta.tokens_received_estimate, Some(1));
    assert_eq!(
        after_first_delta.tokens_received, None,
        "an estimate, not yet exact"
    );

    let after_second_delta = sequence
        .iter()
        .find(|live| live.chars_received == Some(22))
        .unwrap_or_else(|| panic!("{name}: no snapshot ever reached the second delta"));
    assert_eq!(after_second_delta.tokens_received_estimate, Some(2));
    assert!(
        after_second_delta.tokens_per_second.unwrap() > 0.0,
        "{name}: a real rate once there's been a second delta to measure one"
    );
    assert!(
        after_second_delta.tokens_received_estimate > after_first_delta.tokens_received_estimate,
        "{name}: the received estimate must grow across deltas"
    );
}

#[test]
fn prompt_live_stream_cof_fixture_shows_a_real_streaming_prompt() {
    assert_stream_shape("prompt_live_stream_cof");
}

#[test]
fn prompt_live_stream_electricity_fixture_shows_a_real_streaming_prompt() {
    assert_stream_shape("prompt_live_stream_electricity");
}

/// The attempt's own `meta.live` is gone once it ends, success or
/// failure (DESIGN.md), and the final state carries the exact,
/// settled totals the mock's own `prompt_eval_count`/`eval_count`
/// named -- never left as the last estimate `meta.live` ever showed.
fn assert_settles_to_exact(name: &str) {
    let states = load_snapshots(name);
    let last = states.last().expect("at least one snapshot");
    let meta = ask_meta(last).expect("ask must exist in the final state");
    assert!(meta.get("live").is_none_or(Value::is_null));
    assert_eq!(meta["tokens_sent"], 8);
    assert_eq!(meta["tokens_received"], 4);
    assert!(meta["completed_at"].is_string());
}

#[test]
fn prompt_live_stream_cof_fixture_settles_to_the_exact_totals() {
    assert_settles_to_exact("prompt_live_stream_cof");
}

#[test]
fn prompt_live_stream_electricity_fixture_settles_to_the_exact_totals() {
    assert_settles_to_exact("prompt_live_stream_electricity");
}

/// `prompt_live_figures` reads the same real `meta.live` the same
/// way regardless of which engine wrote it -- the row text it drives
/// (`tape_running_prompt_what`'s own inputs) must actually need no
/// per-engine special casing.
#[test]
fn prompt_live_figures_reads_both_engines_mid_stream_snapshots_the_same_way() {
    for name in ["prompt_live_stream_cof", "prompt_live_stream_electricity"] {
        let states = load_snapshots(name);
        let mid_stream = states
            .iter()
            .find(|state| {
                ask_meta(state)
                    .and_then(|m| m.get("live"))
                    .and_then(|l| l.get("chars_received"))
                    .and_then(Value::as_u64)
                    == Some(22)
            })
            .unwrap_or_else(|| panic!("{name}: no mid-stream snapshot at chars_received=22"));
        let node = oscilloscope_core::model::flatten_state(mid_stream);
        let model = oscilloscope_core::model::RunModel::new();
        let now = mid_stream["prime"]["ask"]["meta"]["live"]["updated_at"]
            .as_str()
            .unwrap();
        let live: PromptLiveFigures =
            prompt_live_figures(node.get("prime.ask"), "prime.ask", &model, now)
                .unwrap_or_else(|| panic!("{name}: prompt_live_figures returned None"));
        assert!(live.streaming);
        assert_eq!(live.tokens_received_or_estimate(), Some((2, true)));
        assert!(live.tokens_per_second.unwrap() > 0.0);
    }
}
