//! Engine parity for the Tape (issue #678, replacing the old
//! overview's own parity test): recorded fixtures from both engines
//! give the same Tape text, apart from durations and clock times.
//! Reuses `engine_parity.rs`'s own recorded fixtures (`PARITY_CASES`)
//! and `do.yml`, builds the Tape from each engine's final snapshot,
//! and compares the two after normalizing every timestamp and
//! duration field away.

use std::path::{Path, PathBuf};

use oscilloscope_core::model::{ProcessState, RunModel, flatten_state};
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render;
use oscilloscope_core::screens::{TapeHead, TapeItem, TapeScreen, build_tape};
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

fn build_case_tape(dir_name: &str) -> TapeScreen {
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
    build_tape(
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        None,
        true,
        "2026-01-01T00:10:00Z",
    )
}

/// Zeroes every field a real run's own timing (never byte-identical
/// between two independently recorded fixtures) touches, leaving
/// everything else -- labels, statuses, counts, what-text, tokens --
/// to compare for real.
fn normalize(mut tape: TapeScreen) -> TapeScreen {
    for item in &mut tape.items {
        match item {
            TapeItem::RunStarted { ts, .. } => *ts = None,
            TapeItem::Row(row) => row.ts = None,
            TapeItem::NowBand { ts, elapsed_s } => {
                *ts = None;
                *elapsed_s = 0.0;
            }
            TapeItem::RunEnd {
                usual_wall_s,
                duration_s,
                eta_ts,
                ..
            } => {
                *usual_wall_s = None;
                *duration_s = None;
                *eta_ts = None;
            }
            TapeItem::SlotsRule { .. } => {}
            TapeItem::HeadRule => {}
        }
    }
    if let TapeHead::Ended { duration_s, .. } = &mut tape.head {
        *duration_s = 0.0;
    }
    if let Some(detail) = &mut tape.ended_detail {
        detail.duration_s = 0.0;
        detail.start_ts = None;
        detail.end_ts = None;
    }
    if let Some(detail) = &mut tape.failed_detail {
        detail.duration_s = 0.0;
        detail.start_ts = None;
    }
    if let Some(detail) = &mut tape.starting_detail {
        detail.eta_ts = None;
    }
    tape
}

macro_rules! tape_parity_test {
    ($test_name:ident, $case:literal) => {
        #[test]
        fn $test_name() {
            let cof = normalize(build_case_tape(concat!($case, "_cof")));
            let electricity = normalize(build_case_tape(concat!($case, "_electricity")));
            assert_eq!(
                cof, electricity,
                "cof and electricity produced a different Tape for {}",
                $case
            );
        }
    };
}

tape_parity_test!(tape_parity_tool_ok, "parity_tool_ok");
tape_parity_test!(tape_parity_if_taken, "parity_if_taken");
tape_parity_test!(tape_parity_dynamic_chain, "parity_dynamic_chain");
