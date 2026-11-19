//! Issue #628: the `decision` `--events` line is new; this build's
//! own Flow/Tape screens don't read it yet. `named_if_taken`
//! predates the engines emitting it, so inserting the `decision` line
//! either engine would now write right after `prime.gate`'s own
//! `start` -- before the chosen branch starts, the order `--events`
//! documents --
//! must draw and log exactly as the fixture's own recorded transcript
//! does without it.

use std::path::{Path, PathBuf};

use oscilloscope_core::diff::{Differ, LogLine};
use oscilloscope_core::model::{ProcessState, RunModel};
use oscilloscope_core::observe::parse_event;
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render::{self, RenderState};
use serde_json::Value;

const FIXTURE: &str = "named_if_taken";

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

fn load_events(dir_name: &str) -> String {
    let dir = fixtures_dir().join(dir_name);
    std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_else(|e| panic!("{dir_name}: couldn't read events.jsonl: {e}"))
}

/// Inserts a `decision` line right after `prime.gate`'s own `start`
/// line -- the point both engines now write one at -- so this
/// fixture's recorded transcript (from before either engine emitted
/// the line) reads the same as a fresh recording would.
fn with_decision_line_inserted(events_text: &str) -> String {
    let mut out = String::new();
    let mut inserted = false;
    for line in events_text.lines() {
        out.push_str(line);
        out.push('\n');
        if !inserted && line.contains(r#""path":"prime.gate""#) && line.contains(r#""ev":"start""#)
        {
            out.push_str(
                r#"{"v":1,"seq":99,"ts":"2026-10-09T13:31:38.313Z","ev":"decision","path":"prime.gate","kind":"if","mode":"cel","branch":"then","values":{"state.prime.flag":true}}"#,
            );
            out.push('\n');
            inserted = true;
        }
    }
    assert!(
        inserted,
        "fixture's events.jsonl has no prime.gate start line to insert after"
    );
    out
}

fn build(dir_name: &str, events_text: &str) -> (RenderState, Vec<LogLine>) {
    let dir = fixtures_dir().join(dir_name);
    let doc = dir.join("do.yml");
    let program = oscilloscope_core::plan::compile(&doc, None, &indexmap::IndexMap::new())
        .unwrap_or_else(|e| panic!("{dir_name}: document doesn't compile: {e}"));
    let plan = PlanTree::from_program(&program);
    let state = load_final_snapshot(dir_name);
    let mut model = RunModel::new();
    let mut differ = Differ::new();
    let mut log: Vec<LogLine> = Vec::new();
    for raw in events_text.lines().filter(|l| !l.is_empty()) {
        let value: Value = serde_json::from_str(raw).expect("valid JSON line");
        let Some(event) = parse_event(&value) else {
            continue;
        };
        model.observe_event(&event);
        log.extend(differ.diff_event(&event, &plan, &model));
    }
    log.extend(differ.diff(&state, &plan));
    if let Some(line) = differ.finish(&state, None) {
        log.push(line);
    }

    let process = ProcessState::exited(true);
    let rendered = render::build_folded(
        "do.yml",
        "test",
        &plan,
        &mut model,
        Some(&state),
        process,
        0.0,
        "2026-01-01T00:10:00Z",
        &log,
        true,
        &Default::default(),
    );
    (rendered, log)
}

/// A transcript with a `decision` line draws the same rows, state
/// rows and log as the same transcript without it (issue #628's own
/// requirement that osi ignore the line until the Flow screen reads
/// it) -- not just the same `RunModel::event_status`, which a `decision`
/// line can't change either way, since nothing in `RunModel` reads `ev:
/// "decision"` at all.
#[test]
fn a_decision_line_draws_the_same_rows_and_log_as_without_it() {
    let plain_events = load_events(FIXTURE);
    let decorated_events = with_decision_line_inserted(&plain_events);
    assert_ne!(
        plain_events, decorated_events,
        "the fixture should have gained a decision line"
    );

    let (plain_rendered, plain_log) = build(FIXTURE, &plain_events);
    let (decorated_rendered, decorated_log) = build(FIXTURE, &decorated_events);

    assert_eq!(plain_rendered, decorated_rendered);
    assert_eq!(plain_log, decorated_log);
}
