//! Replays `electricity/scripts/generate_compiler_smoke_corpus.py`'s
//! golden cases against this lane's own stub `check_for_run` -- the
//! harness itself (`tests/support/`) exercised end to end (issue #408's
//! Scope section: "Plus ONE example generator ... so the harness is
//! exercised end to end").
//!
//! Lane A's stub always fails, so a case only passes here where
//! Circuitry's own `run_error` is also non-empty (both sides "fail",
//! `DESIGN.md` §1/§12's "fail at the same place" standard, not matching
//! text -- the stub's message names lane A/the real lane, never
//! Circuitry's own). A case whose real document is *valid* (`run_error`
//! is `null`) disagrees with the stub by design and is `#[ignore]`d,
//! naming the lane that un-ignores it.

mod support;

use electricity_compiler::{CheckOptions, check_for_run};
use support::corpus::materialize;
use support::reference::Case;

fn load_case(name: &str) -> Case {
    let text = include_str!("golden/smoke.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden/smoke.json is valid JSON");
    cases
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no case named {name:?} in golden/smoke.json"))
}

fn assert_stub_fails(case: &Case) {
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let result = check_for_run(&entry, &CheckOptions::default());
    assert!(
        result.is_err(),
        "case {:?}: lane A's check_for_run stub must always fail, got {result:?}",
        case.name
    );
}

#[test]
fn empty_document_fails_under_the_stub_same_as_circuitry() {
    let case = load_case("empty_document");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_stub_fails(&case);
}

#[test]
fn duplicate_effect_name_fails_under_the_stub_same_as_circuitry() {
    let case = load_case("duplicate_effect_name");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_stub_fails(&case);
}

#[test]
fn unsupported_suffix_fails_under_the_stub_same_as_circuitry() {
    let case = load_case("unsupported_suffix");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_stub_fails(&case);
}

#[test]
fn non_mapping_root_fails_under_the_stub_same_as_circuitry() {
    let case = load_case("non_mapping_root");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_stub_fails(&case);
}

#[test]
#[ignore = "passes against Circuitry (run_error is null); lane B's check_for_run \
            makes the stub agree -- un-ignore once load_document/check_report land"]
fn minimal_valid_prompt_succeeds_against_circuitry() {
    let case = load_case("minimal_valid_prompt");
    assert!(
        case.run_error.is_none(),
        "fixture expectation: case succeeds against Circuitry"
    );
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let result = check_for_run(&entry, &CheckOptions::default());
    assert!(result.is_ok(), "expected success, got {result:?}");
}
