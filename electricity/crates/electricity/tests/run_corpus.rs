//! Exercises the golden run corpus `electricity/scripts/generate_vm_run_
//! corpus.py` writes (issue #431's Test strategy section). For now (lane
//! A), only the corpus's own shape is checked -- every case parses, and
//! a few sanity properties about its result hold. The comparison tests
//! (running `electricity::run_orchestration`'s eventual VM output against
//! each case and asserting it matches) are `#[ignore = "needs lanes
//! B-D"]`: there is no VM yet, so they would either always fail or
//! always vacuously pass, neither of which is useful signal today.

mod support;

use support::run_corpus::load_corpus;

#[test]
fn the_corpus_has_at_least_the_four_smoke_cases() {
    let cases = load_corpus();
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for expected in [
        "json_tool_chain",
        "cel_if",
        "tree_dynamic",
        "json_parse_failure",
    ] {
        assert!(
            names.contains(&expected),
            "expected a case named {expected:?}, got {names:?}"
        );
    }
}

#[test]
fn every_case_name_is_unique() {
    let cases = load_corpus();
    let mut names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    let unique_count = {
        names.sort_unstable();
        names.dedup();
        names.len()
    };
    assert_eq!(unique_count, load_corpus().len());
}

#[test]
fn a_successful_case_has_a_zero_returncode_and_a_final_state() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "json_tool_chain")
        .expect("json_tool_chain case");
    assert_eq!(case.result.returncode, 0);
    assert!(case.result.state.is_some());
    assert!(!case.result.events.is_empty());
}

#[test]
fn the_failure_case_has_a_nonzero_returncode() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "json_parse_failure")
        .expect("json_parse_failure case");
    assert_ne!(case.result.returncode, 0);
}

#[test]
fn every_events_entry_has_a_sequence_number_and_a_kind() {
    let cases = load_corpus();
    for case in &cases {
        for event in &case.result.events {
            assert!(
                event.get("seq").is_some_and(|v| v.is_number()),
                "{}: an --events entry is missing a numeric 'seq': {event}",
                case.name
            );
            assert!(
                event.get("ev").is_some_and(|v| v.is_string()),
                "{}: an --events entry is missing a string 'ev': {event}",
                case.name
            );
        }
    }
}

#[test]
fn the_tree_case_has_a_dispatch_event_with_two_branches() {
    let cases = load_corpus();
    let case = cases
        .iter()
        .find(|case| case.name == "tree_dynamic")
        .expect("tree_dynamic case");
    let dispatch = case
        .result
        .events
        .iter()
        .find(|event| event.get("ev").and_then(|v| v.as_str()) == Some("dispatch"))
        .expect("a dispatch event");
    assert_eq!(dispatch["branches"].as_u64(), Some(2));
}

#[test]
#[ignore = "needs lanes B-D"]
fn electricity_run_orchestration_matches_the_corpus_state() {
    // Lane B-D's own comparison: run each case's document through
    // `electricity::run_orchestration` (once it actually has a VM) and
    // assert its `--out` state matches `CorpusCase::result::state`
    // exactly, after re-normalizing the same way
    // `electricity/scripts/_run_corpus.py` does.
    unreachable!("no VM to run a case against yet (issue #431, lanes B-D)")
}

#[test]
#[ignore = "needs lanes B-D"]
fn electricity_events_match_the_corpus_events() {
    // Lane D's own comparison: `--events` from `electricity::
    // run_orchestration`, normalized the same way, matched against
    // `CorpusCase::result::events` -- per issue #431's acceptance
    // criteria, a tree case compares the start-before-child/child-end-
    // before-container partial order and the per-path multiset, not the
    // raw interleaving.
    unreachable!("no VM to run a case against yet (issue #431, lanes B-D)")
}

#[test]
#[ignore = "needs lanes B-D"]
fn electricity_live_state_matches_the_out_state() {
    // Lane D's own comparison: the final `--live-state` write must be
    // byte-identical to `--out` (issue #431's acceptance criteria).
    unreachable!("no VM to run a case against yet (issue #431, lanes B-D)")
}
