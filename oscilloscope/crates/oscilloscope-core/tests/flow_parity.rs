//! Engine parity for the Flow screen (issue #635's own acceptance
//! item: "replays of both engines' recorded fixtures ... give the
//! same Flow model apart from times and durations: `◆ gate → then`,
//! `· else: chosen`, and a story with run start, the decision and run
//! end"). Reuses `engine_parity.rs`'s own recorded fixtures
//! (`PARITY_CASES`) and `do.yml`, builds the Flow screen from each
//! engine's recorded events and final snapshot, and compares the
//! decision/story facts the acceptance item names -- never a full
//! struct `assert_eq!`, since the run id and every clock time differ
//! between two independently recorded fixtures by construction.

use std::path::{Path, PathBuf};

use oscilloscope_core::diff::{Differ, LogKind, LogLine};
use oscilloscope_core::model::{ProcessState, RunModel, flatten_state};
use oscilloscope_core::observe::parse_event;
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::rail::{ForkKind, RailItem};
use oscilloscope_core::render;
use oscilloscope_core::screens::{FlowScreen, build_flow};
use serde_json::Value;

struct Utc;
impl oscilloscope_core::format::ClockZone for Utc {
    fn local(&self, utc: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::FixedOffset> {
        utc.with_timezone(&chrono::FixedOffset::east_opt(0).unwrap())
    }
}

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

/// `main.rs`'s own `push_story_line` filter (issue #635), reproduced
/// here rather than imported: it's a thin wrapper around a bounded
/// `Vec` that lives in the binary crate, not `oscilloscope-core`.
fn is_story_kind(kind: LogKind) -> bool {
    matches!(
        kind,
        LogKind::RunStart
            | LogKind::RunEnd
            | LogKind::Decision
            | LogKind::Report
            | LogKind::Failure
    )
}

/// Builds the Flow screen for one recorded fixture, from its own
/// `events.jsonl` (for the `■ run started` story line and every
/// event-sourced log line) and its final `snapshots.jsonl` entry (for
/// the state-sourced ones `diff`/`finish` still produce, and the Flow
/// model itself).
fn build_case_flow_screen(dir_name: &str) -> FlowScreen {
    let dir = fixtures_dir().join(dir_name);
    let doc = dir.join("do.yml");
    let program = oscilloscope_core::plan::compile(&doc, None, &indexmap::IndexMap::new())
        .unwrap_or_else(|e| panic!("{dir_name}: document doesn't compile: {e}"));
    let plan = PlanTree::from_program(&program);
    let state = load_final_snapshot(dir_name);
    let mut model = RunModel::new();
    let mut differ = Differ::new();
    let mut story: Vec<LogLine> = Vec::new();

    for raw in load_events(dir_name) {
        let Some(event) = parse_event(&raw) else {
            continue;
        };
        model.observe_event(&event);
        for line in differ.diff_event(&event, &plan, &model) {
            if is_story_kind(line.kind) {
                story.push(line);
            }
        }
    }
    for line in differ.diff(&state, &plan) {
        if is_story_kind(line.kind) {
            story.push(line);
        }
    }
    if let Some(line) = differ.finish(&state, None) {
        if is_story_kind(line.kind) {
            story.push(line);
        }
    }

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
    build_flow(
        &rendered.unfolded_rows,
        &rendered.rows,
        &flat,
        &plan,
        &model,
        &rendered.header,
        Some(&state),
        &story,
        None,
        &Utc,
        rendered.header.run_end_ts.as_deref(),
        "2026-01-01T00:10:00Z",
    )
}

/// Finds the station named `name`, anywhere in the rail (recursing
/// into open lanes).
fn find_station<'a>(
    items: &'a [RailItem],
    name: &str,
) -> Option<&'a oscilloscope_core::rail::Station> {
    for item in items {
        match item {
            RailItem::Station(s) if s.name == name => return Some(s),
            RailItem::Fork(fork) => {
                for lane in &fork.lanes {
                    if let Some(found) = find_station(&lane.items, name) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Finds the decision fork whose own station is named `name`,
/// anywhere in the rail (recursing into open lanes) -- both engines'
/// own fixtures have exactly one.
fn find_decision_fork<'a>(
    items: &'a [RailItem],
    name: &str,
) -> Option<&'a oscilloscope_core::rail::Fork> {
    for (i, item) in items.iter().enumerate() {
        if let RailItem::Station(s) = item {
            if s.name == name {
                if let Some(RailItem::Fork(fork)) = items.get(i + 1) {
                    if fork.kind == ForkKind::Decision {
                        return Some(fork);
                    }
                }
            }
        }
        if let RailItem::Fork(fork) = item {
            for lane in &fork.lanes {
                if let Some(found) = find_decision_fork(&lane.items, name) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Finds the first unnamed `if`'s own decision fork -- its station
/// path always ends `.__if` (`rail::build_unnamed_decision`'s own
/// synthetic path), never a real node of its own to find by name.
fn find_unnamed_decision_fork(items: &[RailItem]) -> Option<&oscilloscope_core::rail::Fork> {
    for (i, item) in items.iter().enumerate() {
        if let RailItem::Station(s) = item {
            if s.path.ends_with(".__if") {
                if let Some(RailItem::Fork(fork)) = items.get(i + 1) {
                    if fork.kind == ForkKind::Decision {
                        return Some(fork);
                    }
                }
            }
        }
        if let RailItem::Fork(fork) = item {
            for lane in &fork.lanes {
                if let Some(found) = find_unnamed_decision_fork(&lane.items) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// The decision fork named `gate` reads its branch and its untaken
/// lane's own ghost name the same way on both engines -- `do.yml`'s
/// own `then`/`else` both name their one step `chosen` (the parity
/// case), so the taken lane's own real station must still be there
/// too, never hidden alongside the untaken side's own identical
/// name.
fn assert_gate_decision(
    screen: &FlowScreen,
    case: &str,
    expect_branch: &str,
    expect_untaken: &str,
) {
    let gate = find_decision_fork(&screen.rail.items, "gate")
        .unwrap_or_else(|| panic!("{case}: no decision fork named 'gate'"));
    assert_eq!(
        gate.source_status,
        oscilloscope_core::model::StatusKind::Done,
        "{case}: gate itself finished"
    );
    let taken_lane = gate.lanes.first().expect("a taken lane");
    let taken_station = taken_lane.items.iter().find_map(|it| match it {
        RailItem::Station(s) if s.name == "chosen" => Some(s),
        _ => None,
    });
    let taken_station = taken_station
        .unwrap_or_else(|| panic!("{case}: the taken branch's own 'chosen' station is missing"));
    assert_ne!(
        taken_station.status,
        oscilloscope_core::model::StatusKind::Skipped,
        "{case}: 'chosen' is the taken step, not the hidden untaken one"
    );
    let ghost = gate
        .lanes
        .iter()
        .find_map(|l| l.ghost.as_deref())
        .unwrap_or_else(|| panic!("{case}: no untaken ghost lane"));
    assert!(
        ghost.contains(expect_untaken),
        "{case}: untaken ghost name was {ghost:?}"
    );
    let gate_station = find_station(&screen.rail.items, "gate")
        .unwrap_or_else(|| panic!("{case}: no 'gate' station"));
    assert_eq!(
        gate_station.headline,
        oscilloscope_core::rail::Headline::Branch(Some(expect_branch.to_string())),
        "{case}: gate's own branch"
    );
}

/// The story has a run-start line, the decision, and a run-end line,
/// in that order, with the clock prefix and run id stripped -- the
/// only two things that can never match between two independently
/// recorded fixtures.
fn assert_story_has_run_start_decision_and_run_end(screen: &FlowScreen, case: &str, branch: &str) {
    let texts: Vec<&str> = screen.story.iter().map(|e| e.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("run started")),
        "{case}: no run-start line in the story: {texts:?}"
    );
    let decision_text = format!("gate → {branch}");
    assert!(
        texts.iter().any(|t| t.contains(&decision_text)),
        "{case}: no decision line in the story: {texts:?}"
    );
    // Issue #717: the exit summary now carries the run's own name
    // between "run" and "ok" (`■ run swift-jasper  ok  0.0s`), so this
    // checks for both words rather than the literal "run ok".
    assert!(
        texts
            .iter()
            .any(|t| t.contains("■ run ") && t.contains("ok  ")),
        "{case}: no run-end line in the story: {texts:?}"
    );
}

#[test]
fn flow_parity_if_taken() {
    let cof = build_case_flow_screen("parity_if_taken_cof");
    let electricity = build_case_flow_screen("parity_if_taken_electricity");
    assert_gate_decision(&cof, "cof", "then", "chosen");
    assert_gate_decision(&electricity, "electricity", "then", "chosen");
    assert_story_has_run_start_decision_and_run_end(&cof, "cof", "then");
    assert_story_has_run_start_decision_and_run_end(&electricity, "electricity", "then");
}

/// The unnamed `if` (`parity_if_unnamed`) writes no node at all, so
/// its own decision is inferred, never read from `meta.branch` --
/// both engines still produce the same inferred branch and untaken
/// ghost, since the inference reads only which branch's own step
/// actually appeared.
#[test]
fn flow_parity_if_unnamed() {
    let cof = build_case_flow_screen("parity_if_unnamed_cof");
    let electricity = build_case_flow_screen("parity_if_unnamed_electricity");
    for (case, screen) in [("cof", &cof), ("electricity", &electricity)] {
        let decision = find_unnamed_decision_fork(&screen.rail.items)
            .unwrap_or_else(|| panic!("{case}: no unnamed decision fork"));
        let taken_lane = decision.lanes.first().expect("a taken lane");
        let taken = taken_lane.items.iter().find_map(|it| match it {
            RailItem::Station(s) if s.name == "then_branch" => Some(s),
            _ => None,
        });
        let taken = taken
            .unwrap_or_else(|| panic!("{case}: the taken branch's own 'then_branch' is missing"));
        assert_ne!(
            taken.status,
            oscilloscope_core::model::StatusKind::Skipped,
            "{case}: 'then_branch' is the taken step"
        );
        let ghost = decision
            .lanes
            .iter()
            .find_map(|l| l.ghost.as_deref())
            .unwrap_or_else(|| panic!("{case}: no untaken ghost lane"));
        assert!(
            ghost.contains("else_branch"),
            "{case}: untaken ghost name was {ghost:?}"
        );
    }
}

#[test]
fn flow_parity_if_untaken() {
    let cof = build_case_flow_screen("parity_if_untaken_cof");
    let electricity = build_case_flow_screen("parity_if_untaken_electricity");
    assert_gate_decision(&cof, "cof", "else", "chosen");
    assert_gate_decision(&electricity, "electricity", "else", "chosen");
    assert_story_has_run_start_decision_and_run_end(&cof, "cof", "else");
    assert_story_has_run_start_decision_and_run_end(&electricity, "electricity", "else");
}
