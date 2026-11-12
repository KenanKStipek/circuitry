//! The Tape's Now area with nested parallel loops (issue #737), over
//! `tests/fixtures/nested_tree_loops`: a real recorded run of issue
//! #737's own document (a `setup` step, then a tree loop `groups`, 2
//! wide, each pass running another tree loop `files`, 2 wide, 5
//! passes, of a `dynamic` `build` -- `compile` then `check` -- beside
//! an unrelated `watcher` tool).

use std::path::{Path, PathBuf};

use oscilloscope_core::model::{ProcessState, RunModel, StatusKind, flatten_state};
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::render;
use oscilloscope_core::render::RowKind;
use oscilloscope_core::screens::{TapeItem, TapeRow, build_tape};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn load_plan() -> PlanTree {
    let doc = fixtures_dir().join("nested_tree_loops/do.yml");
    let program = oscilloscope_core::plan::compile(&doc, None, &indexmap::IndexMap::new())
        .unwrap_or_else(|e| panic!("nested_tree_loops/do.yml doesn't compile: {e}"));
    PlanTree::from_program(&program)
}

/// The snapshot at `index` in `snapshots.jsonl`, plus the `now` the
/// moment it was recorded at (its run's own start plus its own `t`),
/// so a replay reads exactly as live as the real run did. `None`
/// before the root node's own `created_at` has landed at all (the
/// run's very first snapshot or two).
fn load_snapshot(index: usize) -> Option<(Value, String)> {
    let dir = fixtures_dir().join("nested_tree_loops");
    let text = std::fs::read_to_string(dir.join("snapshots.jsonl")).expect("snapshots.jsonl");
    let line = text
        .lines()
        .nth(index)
        .unwrap_or_else(|| panic!("no snapshot at index {index}"));
    let wrapper: Value = serde_json::from_str(line).expect("valid JSON line");
    let state = wrapper["state"].clone();
    let t = wrapper["t"].as_f64().expect("snapshot has its own t");
    let created_at = state["prime"]["meta"]["created_at"].as_str()?;
    let run_start =
        chrono::DateTime::parse_from_rfc3339(created_at).expect("run_start is RFC-3339");
    let now = run_start + chrono::Duration::milliseconds((t * 1000.0).round() as i64);
    Some((
        state,
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    ))
}

fn load_events_up_to(now: &str) -> Vec<Value> {
    let now_dt = chrono::DateTime::parse_from_rfc3339(now).expect("now is RFC-3339");
    let dir = fixtures_dir().join("nested_tree_loops");
    std::fs::read_to_string(dir.join("events.jsonl"))
        .expect("events.jsonl")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("valid JSON line"))
        .filter(|e| {
            e.get("ts")
                .and_then(Value::as_str)
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .is_some_and(|ts| ts <= now_dt)
        })
        .collect()
}

fn build_tape_at(index: usize, selected: Option<&str>) -> oscilloscope_core::screens::TapeScreen {
    let plan = load_plan();
    let (state, now) =
        load_snapshot(index).unwrap_or_else(|| panic!("snapshot {index} has no created_at yet"));
    let mut model = RunModel::new();
    for raw in load_events_up_to(&now) {
        if let Some(event) = oscilloscope_core::observe::parse_event(&raw) {
            model.observe_event(&event);
        }
    }
    let rendered = render::build_folded(
        "nested_tree_loops/do.yml",
        "test",
        &plan,
        &mut model,
        Some(&state),
        ProcessState::Running,
        0.0,
        &now,
        &[],
        true,
        &Default::default(),
    );
    let flat = flatten_state(&state);
    build_tape(
        &rendered.rows,
        &rendered.rows,
        &rendered.header,
        &flat,
        &plan,
        &model,
        Some(&state),
        selected,
        true,
        &now,
    )
}

/// Every row from the Now band (bold starts) to the first row ahead
/// (bold ends) -- `tui::draw_tape_list`'s own rule for the same
/// boundary.
fn tree_rows(tape: &oscilloscope_core::screens::TapeScreen) -> Vec<&TapeRow> {
    let mut in_band = false;
    let mut out = Vec::new();
    for item in &tape.items {
        match item {
            TapeItem::NowBand { .. } => in_band = true,
            TapeItem::Row(row) if in_band && row.bold => out.push(row),
            TapeItem::Row(_) if in_band => break,
            _ => {}
        }
    }
    out
}

/// Nothing marked running (or ahead of it) ever sits above the Now
/// band -- the earlier bug this generalizes, "a running dynamic sorts
/// in among the finished rows" (issue #737).
fn assert_nothing_running_above_the_band(tape: &oscilloscope_core::screens::TapeScreen) {
    let mut above_band = true;
    for item in &tape.items {
        match item {
            TapeItem::NowBand { .. } => above_band = false,
            TapeItem::Row(r) if above_band => assert!(
                !matches!(
                    r.status.kind,
                    StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                ),
                "{} reads running above the Now band",
                r.label
            ),
            _ => {}
        }
    }
}

#[test]
fn the_now_area_is_a_tree_with_both_loops_busy_and_the_watcher_top_level() {
    // Early in the run (index 3): every pass's own first step
    // (`compile`) is running, nothing has finished yet, and three of
    // each files loop's own five passes still wait for a slot.
    let tape = build_tape_at(3, None);
    let tree = tree_rows(&tape);

    // No container row (a loop pass, `build`, the tree-flow `work`)
    // is ever its own line -- only loops and leaves.
    assert!(
        tree.iter().all(|r| matches!(
            r.kind,
            RowKind::Loop
                | RowKind::Tool
                | RowKind::Prompt
                | RowKind::Use
                | RowKind::Yield
                | RowKind::Reflector
        )),
        "{:?}",
        tree.iter().map(|r| (&r.label, r.kind)).collect::<Vec<_>>()
    );

    let watcher = tree
        .iter()
        .find(|r| r.label == "watcher")
        .expect("watcher is a top-level running row");
    assert_eq!(watcher.indent, 0);

    let groups = tree
        .iter()
        .find(|r| r.kind == RowKind::Loop && r.label == "groups")
        .expect("the groups loop has its own tree row");
    assert_eq!(groups.indent, 0);
    assert_eq!(groups.what, "2 of 2 Slots Busy");

    let files: Vec<&&TapeRow> = tree
        .iter()
        .filter(|r| r.kind == RowKind::Loop && r.label.ends_with("files"))
        .collect();
    assert_eq!(
        files.len(),
        2,
        "{:?}",
        files.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(files.iter().all(|r| r.indent == 1));
    // 5 passes, 2 wide: two running, none done yet -- 3 left queued.
    assert!(
        files
            .iter()
            .all(|r| r.what == "2 of 2 Slots Busy \u{b7} 3 Passes Queued"),
        "{:?}",
        files
            .iter()
            .map(|r| (&r.label, &r.what))
            .collect::<Vec<_>>()
    );
    // `iter_0 › files` and `iter_1 › files`: the same loop at two
    // different passes of `groups`, so their own breadcrumbs must
    // still read apart.
    let labels: std::collections::BTreeSet<&str> = files.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        ["iter_0 \u{203a} files", "iter_1 \u{203a} files"]
            .into_iter()
            .collect()
    );

    // Every `compile` leaf is running, two levels deep under its own
    // files loop.
    let compiles: Vec<&&TapeRow> = tree
        .iter()
        .filter(|r| r.kind == RowKind::Tool && r.label.ends_with("compile"))
        .collect();
    assert_eq!(
        compiles.len(),
        4,
        "{:?}",
        compiles.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(compiles.iter().all(|r| r.indent == 2));

    // Two siblings under the same files loop never read the same
    // (`groups.iter_0.files`'s own two compiles, `iter_0 › build ›
    // compile` and `iter_1 › build › compile`) -- the two different
    // files loops' own compiles are never siblings of each other, so
    // their own relative labels are free to coincide (`iter_0 ›
    // build › compile` under both), the same as two files in two
    // different folders sharing a name.
    for files_prefix in ["work.groups.iter_0.files.", "work.groups.iter_1.files."] {
        let own: Vec<&str> = compiles
            .iter()
            .filter(|r| r.path.contains(files_prefix))
            .map(|r| r.label.as_str())
            .collect();
        let unique: std::collections::BTreeSet<&str> = own.iter().copied().collect();
        assert_eq!(own.len(), unique.len(), "{files_prefix}: {own:?}");
    }

    // No two finished or ahead rows (never the tree's own) share a
    // label.
    let mut seen = std::collections::HashMap::new();
    for item in &tape.items {
        if let TapeItem::Row(r) = item {
            if !r.bold {
                *seen.entry(r.label.clone()).or_insert(0) += 1;
            }
        }
    }
    let dup: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
    assert!(dup.is_empty(), "{dup:?}");

    assert_nothing_running_above_the_band(&tape);
}

#[test]
fn queued_passes_drop_from_three_to_one_as_the_first_two_finish() {
    // Index 3: round 1 (two passes dispatched at once, three still
    // queued of five). Index 6: round 1's own two passes have both
    // finished, round 2 (`iter_2`/`iter_3`) has started, and only
    // `iter_4` (the fifth, odd one out) is left queued.
    let early = build_tape_at(3, None);
    let early_files = tree_rows(&early)
        .into_iter()
        .find(|r| r.kind == RowKind::Loop && r.label == "iter_0 \u{203a} files")
        .expect("iter_0 \u{203a} files is running")
        .what
        .clone();
    assert_eq!(early_files, "2 of 2 Slots Busy \u{b7} 3 Passes Queued");

    let later = build_tape_at(6, None);
    let later_tree = tree_rows(&later);
    let later_files = later_tree
        .iter()
        .find(|r| r.kind == RowKind::Loop && r.label == "iter_0 \u{203a} files")
        .expect("iter_0 \u{203a} files is still running");
    assert_eq!(later_files.what, "2 of 2 Slots Busy \u{b7} 1 Pass Queued");
    assert!(
        later_tree.iter().any(|r| r.kind == RowKind::Tool
            && r.label.ends_with("iter_2 \u{203a} build \u{203a} compile")),
        "{:?}",
        later_tree.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert_nothing_running_above_the_band(&later);
}

#[test]
fn the_last_odd_pass_leaves_one_slot_idle_with_nothing_queued() {
    // Index 10: rounds 1 and 2 (four of five passes) have both
    // finished; only `iter_4`, the fifth and last pass, is running --
    // one of the loop's own two slots idle, nothing left to queue.
    let tape = build_tape_at(10, None);
    let tree = tree_rows(&tape);
    let files: Vec<&&TapeRow> = tree
        .iter()
        .filter(|r| r.kind == RowKind::Loop && r.label.ends_with("files"))
        .collect();
    assert_eq!(
        files.len(),
        2,
        "{:?}",
        files.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(
        files.iter().all(|r| r.what == "1 of 2 Slots Busy"),
        "{:?}",
        files
            .iter()
            .map(|r| (&r.label, &r.what))
            .collect::<Vec<_>>()
    );
    assert_nothing_running_above_the_band(&tape);
}
