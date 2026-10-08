//! Replays `electricity/scripts/generate_compiler_smoke_corpus.py`'s
//! golden cases against the real `check_for_run` -- the harness itself
//! (`tests/support/`) exercised end to end (issue #408's Scope section:
//! "Plus ONE example generator ... so the harness is exercised end to
//! end"). Lanes A-D have all landed, so every case is checked against
//! `check_for_run`'s own success/failure outcome, matching Circuitry's
//! own `run_error` (`DESIGN.md` §1/§12's "fail at the same place"
//! standard, not matching text -- the real compiler's error text for
//! these four failing cases is checked exactly by the dedicated
//! `golden_load.rs`/`golden_compile.rs` corpora, not duplicated here).

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

fn assert_check_for_run_matches_circuitry(case: &Case) {
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let result = check_for_run(&entry, &CheckOptions::default());
    assert_eq!(
        result.is_ok(),
        case.run_error.is_none(),
        "case {:?}: expected {} against Circuitry, got {result:?}",
        case.name,
        if case.run_error.is_none() {
            "success"
        } else {
            "failure"
        }
    );
}

#[test]
fn empty_document_fails_same_as_circuitry() {
    let case = load_case("empty_document");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_check_for_run_matches_circuitry(&case);
}

#[test]
fn duplicate_effect_name_fails_same_as_circuitry() {
    let case = load_case("duplicate_effect_name");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_check_for_run_matches_circuitry(&case);
}

#[test]
fn unsupported_suffix_fails_same_as_circuitry() {
    let case = load_case("unsupported_suffix");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_check_for_run_matches_circuitry(&case);
}

#[test]
fn non_mapping_root_fails_same_as_circuitry() {
    let case = load_case("non_mapping_root");
    assert!(
        case.run_error.is_some(),
        "fixture expectation: case fails against Circuitry"
    );
    assert_check_for_run_matches_circuitry(&case);
}

#[test]
fn minimal_valid_prompt_succeeds_against_circuitry() {
    let case = load_case("minimal_valid_prompt");
    assert!(
        case.run_error.is_none(),
        "fixture expectation: case succeeds against Circuitry"
    );
    assert_check_for_run_matches_circuitry(&case);
}
