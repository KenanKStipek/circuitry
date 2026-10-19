//! Engine parity for the overview (issue #626's own acceptance item:
//! "recorded fixtures from both engines give the same overview text,
//! apart from durations and clock times"). Reuses `engine_parity.rs`'s
//! own recorded fixtures (`PARITY_CASES`) and `do.yml`, builds the
//! overview from each engine's final snapshot, and compares the two
//! after normalizing every timestamp and duration field away.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::{ProcessState, RunModel, flatten_state};
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render;
use oscilloscope_core::screens::{FlowEvent, NowFocus, Overview, build_overview};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn load_final_snapshot(dir_name: &str) -> Value {
    let dir = fixtures_dir().join(dir_name);
    let text = std::fs::read_to_string(dir.join("snapshots.jsonl"))
        .unwrap_or_else(|e| panic!("{dir_name}: couldn't read snapshots.jsonl: {e}"));
    let last_line = text
        .lines()
        .rfind(|l| !l.is_empty())
        .unwrap_or_else(|| panic!("{dir_name}: snapshots.jsonl has no lines"));
    let wrapper: Value = serde_json::from_str(last_line).expect("valid JSON line");
    wrapper["state"].clone()
}

fn build_case_overview(dir_name: &str) -> Overview {
    let dir = fixtures_dir().join(dir_name);
    let doc = dir.join("do.yml");
    let program = oscilloscope_core::plan::compile(&doc, None, &indexmap::IndexMap::new())
        .unwrap_or_else(|e| panic!("{dir_name}: document doesn't compile: {e}"));
    let plan = PlanTree::from_program(&program);
    let state = load_final_snapshot(dir_name);
    let mut model = RunModel::new();
    let process = ProcessState::exited(false);
    let rendered = render::build_folded(
        "do.yml",
        "test",
        &plan,
        &mut model,
        Some(&state),
        process,
        0.0,
        "2026-01-01T00:10:00Z",
        &[],
        true,
        &Default::default(),
    );
    let flat = flatten_state(&state);
    build_overview(
        &rendered.rows,
        &rendered.unfolded_rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        &rendered.token_bar,
        &rendered.treemap,
        &[],
        None,
        "2026-01-01T00:10:00Z",
    )
}

/// Zeroes every field a real run's own timing (never byte-identical
/// between two independently recorded fixtures) touches, leaving
/// everything else -- labels, statuses, counts, decisions, tokens --
/// to compare for real.
fn normalize(mut overview: Overview) -> Overview {
    for step in &mut overview.phase_strip {
        step.start_ts = None;
        step.duration_s = None;
    }
    overview.time_card.start_ts = None;
    overview.time_card.end_ts = None;
    overview.time_card.elapsed_s = 0.0;
    overview.time_card.eta_ts = None;
    overview.time_card.bar = overview
        .time_card
        .bar
        .into_iter()
        .map(|(label, _)| (label, 0.0))
        .collect();
    overview.time_card.slowest_finished = overview
        .time_card
        .slowest_finished
        .map(|(label, _)| (label, 0.0));
    match &mut overview.now_pane.focus {
        NowFocus::Ended(summary) => {
            summary.start_ts = None;
            summary.end_ts = None;
        }
        NowFocus::Running(step, _) | NowFocus::LastFinished(step) => {
            step.since_ts = None;
            step.elapsed_s = None;
        }
        NowFocus::None => {}
    }
    for event in &mut overview.flow_card.recent {
        match event {
            FlowEvent::Decision { ts, .. } | FlowEvent::Report { ts, .. } => *ts = None,
        }
    }
    overview.flow_card.quick_steps = overview
        .flow_card
        .quick_steps
        .map(|(count, _)| (count, 0.0));
    overview
}

macro_rules! overview_parity_test {
    ($test_name:ident, $case:literal) => {
        #[test]
        fn $test_name() {
            let cof = normalize(build_case_overview(concat!($case, "_cof")));
            let electricity = normalize(build_case_overview(concat!($case, "_electricity")));
            assert_eq!(
                cof, electricity,
                "cof and electricity produced a different overview for {}",
                $case
            );
        }
    };
}

overview_parity_test!(overview_parity_tool_ok, "parity_tool_ok");
overview_parity_test!(overview_parity_if_taken, "parity_if_taken");
overview_parity_test!(overview_parity_dynamic_chain, "parity_dynamic_chain");
