//! Issue #702's own rules, against the recorded fixtures
//! (`tests/fixtures/agent_live_pi`, `agent_live_claude_code`,
//! `agent_live_pi_cancelled`) rather than hand-copied JSON, fed
//! through the real `render::build`/`screens::build_tape`/
//! `screens::build_signal`/`store::build_store` pipeline the way
//! `tui.rs` itself drives it, one snapshot at a time so the model's
//! own bookkeeping (retries, the Signal trace) builds up exactly as
//! it would during a real run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oscilloscope_core::model::{ProcessState, RunModel, flatten_state};
use oscilloscope_core::render::{self, RowKind};
use oscilloscope_core::screens::{TapeItem, build_signal, build_tape};
use oscilloscope_core::store::build_store;
use serde_json::Value;

/// `agent_live_pi`'s own recorded clock range (`t=0.393`..`10.528`
/// relative to the real run's own start): a "now" of 1s after
/// `top_agent`'s own turn-1 heartbeat (snapshot index 2) lands well
/// inside the 5s recency window.
const PI_RUNNING_NOW: &str = "2026-10-20T02:08:51Z";
/// 1s after the real run's own end (`prime`'s own `completed_at`).
const PI_ENDED_NOW: &str = "2026-10-20T02:09:01Z";
/// `agent_live_claude_code`'s own recorded clock range: 1s after its
/// turn-1 heartbeat.
const CLAUDE_CODE_RUNNING_NOW: &str = "2026-10-20T02:09:02Z";
/// `agent_live_pi_cancelled`'s own final snapshot: ~9s after
/// `top_agent`'s own last heartbeat (`updated_at`
/// `03:49:50.565127Z`), well past the 5s recency window, so its
/// leftover figures read as last-known, not live.
const CANCELLED_STALE_NOW: &str = "2026-10-20T03:50:00Z";

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

/// Replays *name*'s own recorded snapshots through `render::build`,
/// one at a time (the same one-build-per-poll the real TUI does),
/// up to and including `upto` (0-indexed) -- returns the model left
/// with every earlier snapshot's own bookkeeping already folded in,
/// the render state for `upto` itself, and `upto`'s own raw state.
fn replay_through(
    name: &str,
    upto: usize,
    process: ProcessState,
    now: &str,
) -> (RunModel, render::RenderState, Value) {
    let snapshots = load_snapshots(name);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let mut model = RunModel::new();
    let mut rendered = None;
    for (i, state) in snapshots.iter().enumerate().take(upto + 1) {
        let this_process = if i == upto {
            process.clone()
        } else {
            ProcessState::Running
        };
        rendered = Some(render::build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            this_process,
            i as f64,
            now,
            &[],
        ));
    }
    (
        model,
        rendered.unwrap_or_else(|| panic!("{name}: no snapshot at index {upto}")),
        snapshots[upto].clone(),
    )
}

/// Rule 1/2: `agent_live_pi`'s own turn-1 snapshot (index 2, real
/// recorded `tokens_sent`/`tokens_received` of 2100/40) lands in the
/// Totals row's own live estimate while `top_agent` is still
/// running, exactly as a prompt's live figures already do.
#[test]
fn totals_row_counts_a_running_agents_live_tokens_from_the_recorded_fixture() {
    let (_model, rendered, _state) =
        replay_through("agent_live_pi", 2, ProcessState::Running, PI_RUNNING_NOW);
    assert_eq!(rendered.header.tokens_sent, 0);
    assert_eq!(rendered.header.tokens_received, 0);
    assert_eq!(rendered.header.tokens_sent_live_estimate, 2100);
    assert_eq!(rendered.header.tokens_received_live_estimate, 40);
}

/// Rule 3: the Tape's own token column for `top_agent`'s row draws
/// while it runs (faint, the live figures) and again once it has
/// finished (exact, bold if largest) -- both read from the same
/// recorded fixture, never a hand-built row.
#[test]
fn tape_token_column_draws_for_a_running_and_a_finished_agent_row() {
    let plan = oscilloscope_core::plan::PlanTree::empty();

    let (model, rendered, state) =
        replay_through("agent_live_pi", 2, ProcessState::Running, PI_RUNNING_NOW);
    let flat = flatten_state(&state);
    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        None,
        true,
        PI_RUNNING_NOW,
    );
    let running_row = tape
        .items
        .iter()
        .find_map(|item| match item {
            TapeItem::Row(row) if row.path == "prime.top_agent" => Some(row),
            _ => None,
        })
        .expect("top_agent's own row");
    assert_eq!(running_row.tokens.sent, Some(2100));
    assert_eq!(running_row.tokens.received, Some(40));
    assert!(
        running_row.tokens.live,
        "a running agent's own bar draws faint"
    );

    let (model, rendered, state) =
        replay_through("agent_live_pi", 10, ProcessState::Running, PI_ENDED_NOW);
    let flat = flatten_state(&state);
    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        None,
        true,
        PI_ENDED_NOW,
    );
    let finished_row = tape
        .items
        .iter()
        .find_map(|item| match item {
            TapeItem::Row(row) if row.path == "prime.top_agent" => Some(row),
            _ => None,
        })
        .expect("top_agent's own row");
    assert_eq!(finished_row.tokens.sent, Some(11550));
    assert_eq!(finished_row.tokens.received, Some(130));
    assert!(
        !finished_row.tokens.live,
        "a finished agent's own figures are exact"
    );
}

/// Rule 1: Claude Code's own real recorded turn-1 snapshot never
/// reports `tokens_received` at all (`agent_live_claude_code`'s
/// `top_agent` at turns=1) -- the Totals row's live estimate must
/// add `0` for it, never panic or poison the sum, and the Tape's own
/// detail pane must carry `None`, not a false `Some(0)`.
#[test]
fn claude_codes_null_received_never_becomes_a_false_zero() {
    let snapshots = load_snapshots("agent_live_claude_code");
    let idx = snapshots
        .iter()
        .position(|s| s.pointer("/prime/top_agent/meta/live/turns") == Some(&Value::from(1)))
        .expect("a turns=1 snapshot");
    let (model, rendered, state) = replay_through(
        "agent_live_claude_code",
        idx,
        ProcessState::Running,
        CLAUDE_CODE_RUNNING_NOW,
    );
    assert_eq!(rendered.header.tokens_sent_live_estimate, 1600);
    assert_eq!(rendered.header.tokens_received_live_estimate, 0);

    let flat = flatten_state(&state);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        Some("prime.top_agent"),
        true,
        CLAUDE_CODE_RUNNING_NOW,
    );
    let detail = tape
        .selected_detail
        .expect("top_agent's own selected detail");
    assert_eq!(detail.tokens_sent, Some(1600));
    assert_eq!(
        detail.tokens_received, None,
        "never reported, must read None, not a false 0"
    );
}

/// Rule 2: a running agent's own share never exceeds 100%, reading
/// the recorded fixture's own turn-1 snapshot (`top_agent`'s whole
/// 2140 tokens against a run with nothing else finished yet).
#[test]
fn a_running_agents_tape_share_never_exceeds_100_percent() {
    let (model, rendered, state) =
        replay_through("agent_live_pi", 2, ProcessState::Running, PI_RUNNING_NOW);
    let flat = flatten_state(&state);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        Some("prime.top_agent"),
        true,
        PI_RUNNING_NOW,
    );
    let share = tape.selected_share_pct.expect("a share for top_agent");
    assert!(share <= 100, "share read {share}%, over 100%");
    assert_eq!(share, 100);
}

/// An interrupted run's own Store share must agree with
/// the Tape's for the identical step -- both read `top_agent`'s own
/// stale leftover `meta.live` tokens as the denominator's exact
/// figure, never `0`, from the real recorded cancelled fixture.
#[test]
fn an_interrupted_runs_store_share_agrees_with_the_tapes() {
    let (model, rendered, state) = replay_through(
        "agent_live_pi_cancelled",
        2,
        ProcessState::Exited {
            interrupted: true,
            likely_failed: false,
            has_failure_reason: false,
            exited_at: None,
        },
        CANCELLED_STALE_NOW,
    );
    let flat = flatten_state(&state);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let kinds: BTreeMap<String, RowKind> = BTreeMap::new();

    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        Some("prime.top_agent"),
        true,
        CANCELLED_STALE_NOW,
    );
    let tape_share = tape.selected_share_pct.expect("a Tape share for top_agent");

    let store = build_store(
        &rendered.rows,
        &rendered.header,
        &flat,
        &kinds,
        &plan,
        &model,
        Some(&state),
        "prime",
        CANCELLED_STALE_NOW,
        None,
    );
    let store_row = store
        .table
        .iter()
        .find(|r| r.path == "prime.top_agent")
        .expect("top_agent's own Store table row");
    let store_share = store_row.share_pct.round() as u64;

    assert_eq!(
        tape_share, store_share,
        "the Tape and the Store must agree on the same step's own share"
    );
    assert_eq!(tape_share, 100);
}

/// Rule 4: the Signal's own columns for `top_agent`'s real recorded
/// run sum to its final tokens exactly, and once the run has ended,
/// its own Readings (following Now, not scrubbed) read the same
/// figure the Totals row does.
#[test]
fn signal_columns_sum_to_the_agents_final_tokens_and_readings_match_totals() {
    let (model, rendered, state) = replay_through(
        "agent_live_pi",
        10,
        ProcessState::Exited {
            interrupted: false,
            likely_failed: false,
            has_failure_reason: false,
            exited_at: None,
        },
        PI_ENDED_NOW,
    );
    let flat = flatten_state(&state);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let signal = build_signal(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        100,
        None,
        None,
        PI_ENDED_NOW,
    );
    let (sent, received) = signal
        .buckets
        .iter()
        .fold((0u64, 0u64), |(s, r), b| (s + b.sent, r + b.received));
    // top_agent and fan.iter_0.tree_agent both reach 11550/130 in
    // this fixture; the trace plots both.
    assert_eq!(sent, 23100);
    assert_eq!(received, 260);
    assert_eq!(signal.readings.sent, rendered.header.tokens_sent);
    assert_eq!(signal.readings.received, rendered.header.tokens_received);
}

/// Rule 2: the run summary's own Largest names an agent session,
/// not merely a prompt, once the real recorded run has ended -- on
/// main, an agent's tokens never counted here at all.
#[test]
fn the_run_summarys_largest_names_an_agent_session() {
    let (model, rendered, state) = replay_through(
        "agent_live_pi",
        10,
        ProcessState::Exited {
            interrupted: false,
            likely_failed: false,
            has_failure_reason: false,
            exited_at: None,
        },
        PI_ENDED_NOW,
    );
    let flat = flatten_state(&state);
    let plan = oscilloscope_core::plan::PlanTree::empty();
    let tape = build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        None,
        true,
        PI_ENDED_NOW,
    );
    let ended = tape
        .ended_detail
        .expect("the run has ended, so there must be an Ended detail");
    // `top_agent` and `fan.iter_0.tree_agent` both reach the same
    // 11680 total in this fixture -- either is a real agent session,
    // never the kind of small prompt main's own bug report found.
    assert_eq!(ended.largest_tokens, Some(11_680));
    assert!(
        ended.largest_label.is_some(),
        "Largest must name a real step, not read empty"
    );
}
