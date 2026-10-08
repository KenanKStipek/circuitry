//! Golden tests over recorded runs (issue #424's Tests section):
//! replays each fixture under `../../../tests/fixtures/<case>/` through
//! `Differ`/`RunModel` and checks the status table after each
//! observation, and the `--log` line stream, against a committed
//! snapshot (`insta`).
//!
//! Every fixture here was recorded with no `--events` (`cof run --help`
//! on the recording machine had none, DESIGN.md §3/issue #419 not yet
//! merged), so these exercise the state-only inference rules
//! (DESIGN.md §2.1's "From state alone") with the no-plan fallback
//! (`PlanTree::empty()`) — the path that has to work today while the
//! compiler lanes are still stubs. An events-plus-state fixture lands
//! once `cof run --events` is available to `oscilloscope/scripts/
//! record_fixtures.py`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oscilloscope_core::diff::Differ;
use oscilloscope_core::model::{ProcessState, RowStatus, RunModel};
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

    let interrupted_exit = matches!(fixture.exit_code, 129 | 130 | 143);
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

        let lines = differ.diff(state, &plan);
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

macro_rules! golden_test {
    ($test_name:ident, $fixture:literal) => {
        #[test]
        fn $test_name() {
            let rendered = replay($fixture);
            insta::assert_snapshot!($fixture, rendered);
        }
    };
}

golden_test!(simple_chain_ok, "simple_chain_ok");
golden_test!(on_error_continue, "on_error_continue");
golden_test!(sigint_once, "sigint_once");
golden_test!(sigint_twice, "sigint_twice");
golden_test!(sigterm, "sigterm");

/// Committed fixtures must never carry a local machine's own path (the
/// lane contract's standing rule for generated/recorded files): this
/// runs on every `cargo test`, not just when fixtures are re-recorded.
#[test]
fn fixtures_have_no_leaked_local_paths() {
    let needles = [
        "/Users/",
        "/home/",
        "/var/folders/",
        "/private/",
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
