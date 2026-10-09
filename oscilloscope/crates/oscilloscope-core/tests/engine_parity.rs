//! Engine parity golden tests (issue #431's lane E2, SCOPE item 5):
//! the same document, recorded against both `cof` and `electricity`
//! (`PARITY_CASES` in `oscilloscope/scripts/record_fixtures.py`,
//! `--parity-only`), must produce the same osp `--log` lines once
//! their own event streams are replayed through `Differ`/`RunModel`
//! -- apart from timing (each effect's own duration, normalized away
//! below before comparing).
//!
//! Unlike `golden.rs`'s own `insta` snapshots, this file asserts the
//! two engines' own outputs *against each other* directly: nothing
//! here is a snapshot a reviewer approves once and then trusts --
//! every run of this test re-derives both sides from their own
//! recorded fixture and checks they still agree.

use std::path::{Path, PathBuf};

use oscilloscope_core::diff::{Differ, sort_log_lines};
use oscilloscope_core::model::RunModel;
use oscilloscope_core::observe::parse_event;
use oscilloscope_core::plan::PlanTree;
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
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

/// Every log line `diff_event` produces for *dir_name*'s own recorded
/// events, each with its own duration token normalized away (the one
/// part of this text real elapsed time can touch) -- the no-plan
/// fallback, same as `golden.rs`'s `replay_events`, since neither
/// fixture here carries a plan of its own to join against.
fn normalized_log_lines(dir_name: &str) -> Vec<String> {
    let events = load_events(dir_name);
    let plan = PlanTree::empty();
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut lines = Vec::new();
    for raw in &events {
        let Some(event) = parse_event(raw) else {
            continue;
        };
        model.observe_event(&event);
        lines.extend(differ.diff_event(&event, &plan, &model));
    }
    sort_log_lines(&mut lines);
    lines
        .into_iter()
        .map(|line| normalize_duration(&line.text))
        .collect()
}

/// Replaces every `<digits>.<digits>s` duration token (`diff.rs`'s own
/// `{:.1}s` formatting) with a fixed placeholder -- the only part of
/// an effect's own log line that two real runs, even of the identical
/// document, can't be expected to agree on byte for byte.
fn normalize_duration(text: &str) -> String {
    // Operates on `char`s, not bytes: every log line here can carry
    // multi-byte glyphs (▶/✓/✗), so indexing by byte offset the way a
    // pure-ASCII scanner could would risk slicing mid-codepoint.
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = duration_token_end(&chars[i..]) {
            out.push_str("<DURATION>");
            i += end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Matches `^\d+\.\d+s` at the start of `chars`, returning the match
/// length in `char`s (digits, then `.`, then digits, then a literal
/// `s`), or `None` if `chars` doesn't start with that shape.
fn duration_token_end(chars: &[char]) -> Option<usize> {
    let mut i = 0;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i >= chars.len() || chars[i] != '.' {
        return None;
    }
    i += 1;
    let frac_start = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == frac_start || i >= chars.len() || chars[i] != 's' {
        return None;
    }
    Some(i + 1)
}

macro_rules! parity_test {
    ($test_name:ident, $case:literal) => {
        #[test]
        fn $test_name() {
            let cof_lines = normalized_log_lines(concat!($case, "_cof"));
            let electricity_lines = normalized_log_lines(concat!($case, "_electricity"));
            assert_eq!(
                cof_lines, electricity_lines,
                "cof and electricity produced different --log lines for {}",
                $case
            );
        }
    };
}

parity_test!(parity_tool_ok, "parity_tool_ok");
parity_test!(parity_tool_failed_continue, "parity_tool_failed_continue");
parity_test!(parity_if_taken, "parity_if_taken");
parity_test!(parity_finally, "parity_finally");
parity_test!(parity_dynamic_chain, "parity_dynamic_chain");

#[test]
fn duration_token_normalizes_a_real_log_line() {
    assert_eq!(
        normalize_duration("✓ prime.hello  0.0s  ok"),
        "✓ prime.hello  <DURATION>  ok"
    );
    assert_eq!(
        normalize_duration("✗ prime.flaky  json: parse failed (on_error: continue)"),
        "✗ prime.flaky  json: parse failed (on_error: continue)"
    );
}
