//! Engine parity for the Time screen (issue #650's own acceptance
//! item: "replays of both engines' recorded fixtures give the same
//! Time model apart from times"). Reuses `engine_parity.rs`'s own
//! recorded fixtures and `do.yml`, builds the Time screen from each
//! engine's recorded events and final snapshot, and compares the
//! row labels/paths/statuses/loop-expandability the acceptance item
//! names -- never the row's own timing, which two independently
//! recorded fixtures never share by construction.

use std::path::{Path, PathBuf};

use oscilloscope_core::diff::Differ;
use oscilloscope_core::model::{ProcessState, RunModel, flatten_state};
use oscilloscope_core::observe::parse_event;
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render;
use oscilloscope_core::screens::{TimeDisplayRow, TimeScreen, build_time_screen};
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

fn load_events(dir_name: &str) -> Vec<Value> {
    let dir = fixtures_dir().join(dir_name);
    let text = std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_else(|e| panic!("{dir_name}: couldn't read events.jsonl: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSON line"))
        .collect()
}

/// Builds the Time screen for one recorded fixture, from its own
/// `events.jsonl` (for `RunModel::step_intervals`) and its final
/// `snapshots.jsonl` entry (for the state-sourced rows and the Time
/// model itself) -- the same two-source build `build_case_flow_
/// screen` in `flow_parity.rs` does.
fn build_case_time_screen(dir_name: &str) -> TimeScreen {
    let dir = fixtures_dir().join(dir_name);
    let doc = dir.join("do.yml");
    let program = oscilloscope_core::plan::compile(&doc, None, &indexmap::IndexMap::new())
        .unwrap_or_else(|e| panic!("{dir_name}: document doesn't compile: {e}"));
    let plan = PlanTree::from_program(&program);
    let state = load_final_snapshot(dir_name);
    let mut model = RunModel::new();
    let mut differ = Differ::new();

    for raw in load_events(dir_name) {
        let Some(event) = parse_event(&raw) else {
            continue;
        };
        model.observe_event(&event);
        let _ = differ.diff_event(&event, &plan, &model);
    }
    let _ = differ.diff(&state, &plan);
    let _ = differ.finish(&state, None);

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
    build_time_screen(
        &rendered.rows,
        &flat,
        &plan,
        &model,
        &rendered.header,
        Some(&state),
        &[],
        None,
        &Default::default(),
        "2026-01-01T00:10:00Z",
    )
}

/// Every real row's own path, label and status kind, in order --
/// the shape two independently recorded fixtures must still agree
/// on, apart from timing.
fn row_shapes(screen: &TimeScreen) -> Vec<(String, String, oscilloscope_core::model::StatusKind)> {
    screen
        .rows
        .iter()
        .filter_map(|r| match r {
            TimeDisplayRow::Row(row) => {
                Some((row.path.clone(), row.label.clone(), row.status.kind))
            }
            TimeDisplayRow::Pass(_) => None,
        })
        .collect()
}

#[test]
fn time_parity_tool_ok() {
    let cof = build_case_time_screen("parity_tool_ok_cof");
    let electricity = build_case_time_screen("parity_tool_ok_electricity");
    assert_eq!(
        row_shapes(&cof),
        row_shapes(&electricity),
        "parity_tool_ok: row shapes must match apart from timing"
    );
    assert!(cof.axis.start_ts.is_some(), "cof: axis has a start");
    assert!(
        electricity.axis.start_ts.is_some(),
        "electricity: axis has a start"
    );
    assert!(!cof.axis.show_now, "cof: a finished run shows no now line");
    assert!(
        !electricity.axis.show_now,
        "electricity: a finished run shows no now line"
    );
}

#[test]
fn time_parity_dynamic_chain() {
    let cof = build_case_time_screen("parity_dynamic_chain_cof");
    let electricity = build_case_time_screen("parity_dynamic_chain_electricity");
    let cof_shapes = row_shapes(&cof);
    let electricity_shapes = row_shapes(&electricity);
    assert_eq!(
        cof_shapes, electricity_shapes,
        "parity_dynamic_chain: row shapes must match apart from timing"
    );
    // `a` and `b` are both quick, finished tool steps, so folding
    // (DESIGN.md's own rule, on by default) merges them into one
    // summary row -- the same on both engines, which `cof_shapes ==
    // electricity_shapes` above already confirms; this only pins
    // what that shape actually is, so a future change to either
    // folding or this fixture's own timing doesn't silently stop
    // testing the folded case at all.
    for (case, shapes) in [("cof", &cof_shapes), ("electricity", &electricity_shapes)] {
        let labels: Vec<&str> = shapes.iter().map(|(_, label, _)| label.as_str()).collect();
        assert!(
            labels.contains(&"2 quick steps"),
            "{case}: expected a's and b's own folded summary row: {labels:?}"
        );
    }
}
