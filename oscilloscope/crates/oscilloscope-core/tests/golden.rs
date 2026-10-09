//! Golden tests over recorded runs (issue #424's Tests section):
//! replays each fixture under `../../../tests/fixtures/<case>/` through
//! `Differ`/`RunModel` and checks the status table after each
//! observation, and the `--log` line stream, against a committed
//! snapshot (`insta`).
//!
//! Every fixture's own status-table replay (`replay`, below) uses the
//! no-plan fallback (`PlanTree::empty()`) and only `snapshots.jsonl` —
//! the path that has to work with the compiler lanes still stubbed,
//! and the one every fixture recorded before #423 (`cof run --events`)
//! merged can still exercise. Every fixture now also carries a real
//! recorded `events.jsonl` (`cof` on the recording machine has
//! `--events` as of #423); `replay_events` below replays *that*
//! instead, through `RunModel::observe_event`/`Differ::diff_event`,
//! covering DESIGN.md §2.1's "With events (exact)" rules the
//! state-only replay never reaches.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oscilloscope_core::diff::{Differ, sort_log_lines};
use oscilloscope_core::model::{ProcessState, RowStatus, RunModel};
use oscilloscope_core::observe::parse_event;
use oscilloscope_core::plan::PlanTree;
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

struct Fixture {
    snapshots: Vec<Value>,
    exit_code: i32,
}

fn load_fixture(name: &str) -> Fixture {
    let dir = fixtures_dir().join(name);
    let snapshots_text = std::fs::read_to_string(dir.join("snapshots.jsonl"))
        .unwrap_or_else(|e| panic!("{name}: couldn't read snapshots.jsonl: {e}"));
    let snapshots: Vec<Value> = snapshots_text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).expect("valid JSON line");
            wrapper["state"].clone()
        })
        .collect();
    let exit_code: i32 = std::fs::read_to_string(dir.join("exit_code.txt"))
        .expect("exit_code.txt")
        .trim()
        .parse()
        .expect("integer exit code");
    Fixture {
        snapshots,
        exit_code,
    }
}

fn load_events(name: &str) -> Vec<Value> {
    let dir = fixtures_dir().join(name);
    let text = std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_else(|e| panic!("{name}: couldn't read events.jsonl: {e}"));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSON line"))
        .collect()
}

fn format_rows(rows: &BTreeMap<String, RowStatus>) -> String {
    let mut out = String::new();
    for (path, row) in rows {
        let _ = writeln!(
            out,
            "{path}: {:?} skip={:?} retrying={}",
            row.kind, row.skip_reason, row.retrying
        );
    }
    out
}

/// Replays `name`'s recorded snapshots through a fresh `Differ`/
/// `RunModel`, with no plan (today's only fully-working path, issue
/// #424's lane decision) — one block of status-table text and log
/// lines per observation, plus a final process-exit block.
fn replay(name: &str) -> String {
    let fixture = load_fixture(name);
    let plan = PlanTree::empty();
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut out = String::new();

    // `ProcessState::Exited { interrupted }`'s own contract
    // (model.rs): `interrupted` comes from `prime.meta.error` starting
    // with "Interrupted", never from the exit code alone, so an
    // abort with no final `prime.meta.error` at all (the second-SIGINT
    // case, no `prime` node ever reaching a terminal write) renders as
    // Aborted rather than Cancelled (F12).
    let interrupted_exit = fixture
        .snapshots
        .last()
        .and_then(|s| s.pointer("/prime/meta/error"))
        .and_then(Value::as_str)
        .is_some_and(|e| e.starts_with("Interrupted"));
    let last_index = fixture.snapshots.len().saturating_sub(1);

    for (i, state) in fixture.snapshots.iter().enumerate() {
        let is_last = i == last_index;
        let process = if is_last {
            ProcessState::Exited {
                interrupted: interrupted_exit,
            }
        } else {
            ProcessState::Running
        };

        let mut lines = differ.diff(state, &plan);
        // M1: `diff` itself never emits the `■ run ...` line any more
        // -- `finish` is called exactly once, on the last observation,
        // the same single call site `do_run`/`do_watch` use after
        // draining everything else.
        if is_last {
            if let Some(summary) = differ.finish(state, None) {
                lines.push(summary);
            }
            sort_log_lines(&mut lines);
        }
        let rows = model.observe(state, &plan, process);

        let _ = writeln!(out, "--- observation {i} ---");
        let _ = writeln!(out, "log:");
        for line in &lines {
            let _ = writeln!(out, "  {}", line.text);
        }
        let _ = writeln!(out, "status:");
        out.push_str(&format_rows(&rows));
    }

    let _ = writeln!(out, "--- exit ---");
    let _ = writeln!(out, "exit_code: {}", fixture.exit_code);
    out
}

/// Replays `name`'s recorded `events.jsonl` through a fresh `Differ`/
/// `RunModel` — every `start`/`end` in file order, feeding both the
/// per-event log lines (`Differ::diff_event`) and the model's own
/// exact-status overlay (`RunModel::observe_event`), then one final
/// `RunModel::observe` against the fixture's own last snapshot to
/// render the status table events actually produce (DESIGN.md §2.1's
/// "With events (exact)" rules), the same no-plan fallback `replay`
/// uses.
fn replay_events(name: &str) -> String {
    let fixture = load_fixture(name);
    let events = load_events(name);
    let plan = PlanTree::empty();
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut out = String::new();

    let mut lines = Vec::new();
    for raw in &events {
        let Some(event) = parse_event(raw) else {
            continue;
        };
        model.observe_event(&event);
        lines.extend(differ.diff_event(&event, &plan, &model));
    }
    sort_log_lines(&mut lines);

    let _ = writeln!(out, "log:");
    for line in &lines {
        let _ = writeln!(out, "  {}", line.text);
    }

    let interrupted_exit = fixture
        .snapshots
        .last()
        .and_then(|s| s.pointer("/prime/meta/error"))
        .and_then(Value::as_str)
        .is_some_and(|e| e.starts_with("Interrupted"));
    let final_state = fixture.snapshots.last().cloned().unwrap_or(Value::Null);
    let rows = model.observe(
        &final_state,
        &plan,
        ProcessState::Exited {
            interrupted: interrupted_exit,
        },
    );
    let _ = writeln!(out, "status:");
    out.push_str(&format_rows(&rows));

    let _ = writeln!(out, "--- exit ---");
    let _ = writeln!(out, "exit_code: {}", fixture.exit_code);
    out
}

macro_rules! golden_test {
    ($test_name:ident, $fixture:literal) => {
        #[test]
        fn $test_name() {
            let rendered = replay($fixture);
            insta::assert_snapshot!($fixture, rendered);
        }
    };
}

macro_rules! golden_events_test {
    ($test_name:ident, $fixture:literal) => {
        #[test]
        fn $test_name() {
            let rendered = replay_events($fixture);
            insta::assert_snapshot!(concat!($fixture, "_events"), rendered);
        }
    };
}

golden_test!(simple_chain_ok, "simple_chain_ok");
golden_test!(on_error_continue, "on_error_continue");
golden_test!(on_error_skip, "on_error_skip");
golden_test!(sigint_once, "sigint_once");
golden_test!(sigint_twice, "sigint_twice");
golden_test!(sigterm, "sigterm");
golden_test!(aborted_second_sigint, "aborted_second_sigint");
golden_test!(named_if_taken, "named_if_taken");
golden_test!(each_chain_loop, "each_chain_loop");
golden_test!(each_tree_loop_concurrency, "each_tree_loop_concurrency");
golden_test!(prompt_chain, "prompt_chain");
golden_test!(prompt_tree_loop_concurrency, "prompt_tree_loop_concurrency");
golden_test!(prompt_failing, "prompt_failing");

golden_events_test!(simple_chain_ok_events, "simple_chain_ok");
golden_events_test!(prompt_chain_events, "prompt_chain");
golden_events_test!(
    each_tree_loop_concurrency_events,
    "each_tree_loop_concurrency"
);

/// Committed fixtures must never carry a local machine's own path (the
/// lane contract's standing rule for generated/recorded files): this
/// runs on every `cargo test`, not just when fixtures are re-recorded.
#[test]
fn fixtures_have_no_leaked_local_paths() {
    // No trailing slash on `/private`/`/var/folders`: K2's own bug left
    // exactly `/private<SCRUBBED>` behind, which a `/private/`-shaped
    // needle (requiring a second slash) never matched.
    let needles = [
        "/Users/",
        "/home/",
        "/var/folders",
        "/private",
        "/tmp/",
        ".pi/",
    ];
    let mut offenders = Vec::new();
    for entry in walk(&fixtures_dir()) {
        let Ok(text) = std::fs::read_to_string(&entry) else {
            continue;
        };
        for needle in needles {
            if text.contains(needle) {
                offenders.push(format!("{}: contains {needle:?}", entry.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "leaked local paths in committed fixtures:\n{}",
        offenders.join("\n")
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
