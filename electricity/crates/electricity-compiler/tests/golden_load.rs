//! Replays `electricity/scripts/generate_compiler_load_corpus.py`'s golden
//! cases (issue #408's lane B section): `check_report` against Circuitry's
//! `validate(...)`, and `check_for_run`'s text against `run(...)`'s own
//! `RunResult.error`. Every case here fails before `compile_orchestration`
//! ever runs (a load, structural, or concurrency-configuration error), so
//! every one is fully checkable against this lane alone -- with one
//! exception: `validate()`'s own concurrency-configuration check runs
//! *after* a document compiles, so the `check_report` side of the two
//! `*_concurrency_config_error` cases still needs a real compile (lane C);
//! `Case::check_report_needs` marks that, and [`assert_check_report`] only
//! asserts `ok: false` plus the lane C marker for those two, while still
//! fully checking `check_for_run` (whose own concurrency check runs
//! *before* structural checks even start, needing nothing further).

mod support;

use electricity_compiler::{check_for_run, check_report};
use support::corpus::materialize;
use support::reference::{Case, matches, normalize};

fn load_case(name: &str) -> Case {
    let text = include_str!("golden/load.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden/load.json is valid JSON");
    cases
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no case named {name:?} in golden/load.json"))
}

fn assert_check_report(case: &Case, root: &std::path::Path, entry: &std::path::Path) {
    let options = case.check_options();
    let report = check_report(entry, &options);

    assert_eq!(
        report.warnings, case.validate.warnings,
        "case {:?}: check_report warnings",
        case.name
    );

    if let Some(lane) = &case.check_report_needs {
        assert!(
            !report.ok,
            "case {:?}: expected ok: false, got {report:?}",
            case.name
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("not implemented in lane")),
            "case {:?}: expected a lane {lane} stub marker, got {:?}",
            case.name,
            report.errors
        );
        return;
    }

    assert_eq!(
        report.ok, case.validate.ok,
        "case {:?}: check_report ok",
        case.name
    );
    assert_eq!(
        report.errors.len(),
        case.validate.errors.len(),
        "case {:?}: check_report error count -- actual: {:?}",
        case.name,
        report.errors
    );
    for (index, (actual, expected)) in report
        .errors
        .iter()
        .zip(case.validate.errors.iter())
        .enumerate()
    {
        let mode = case
            .comparison
            .validate_errors
            .get(index)
            .map(String::as_str)
            .unwrap_or("exact");
        let normalized = normalize(actual, root);
        assert!(
            matches(mode, expected, &normalized),
            "case {:?} error[{index}] ({mode}): expected {expected:?}, got {normalized:?}",
            case.name
        );
    }
}

fn assert_check_for_run(case: &Case, root: &std::path::Path, entry: &std::path::Path) {
    let options = case.check_options();
    let result = check_for_run(entry, &options);

    let Some(expected) = &case.run_error else {
        assert!(
            result.is_ok(),
            "case {:?}: expected check_for_run to succeed, got {result:?}",
            case.name
        );
        return;
    };
    let actual = result
        .err()
        .unwrap_or_else(|| panic!("case {:?}: expected check_for_run to fail", case.name))
        .to_string();
    let normalized = normalize(&actual, root);
    let mode = case.comparison.run_error.as_deref().unwrap_or("exact");
    assert!(
        matches(mode, expected, &normalized),
        "case {:?} run_error ({mode}): expected {expected:?}, got {normalized:?}",
        case.name
    );
}

fn run_case(name: &str) {
    let case = load_case(name);
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    assert_check_report(&case, &temp.root, &entry);
    assert_check_for_run(&case, &temp.root, &entry);
}

#[test]
fn unsupported_suffix_md() {
    run_case("unsupported_suffix_md");
}

#[test]
fn toon_refused() {
    let case = load_case("toon_refused");
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    // Deliberately not compared against Circuitry's own message (a
    // documented divergence, issue #408's Scope section): checked
    // directly against electricity's own text instead of the shared
    // `assert_check_report`/`assert_check_for_run` helpers.
    let report = check_report(&entry, &case.check_options());
    assert!(!report.ok);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("TOON documents are not supported"));
    let result = check_for_run(&entry, &case.check_options());
    let message = result.unwrap_err().to_string();
    assert!(message.contains("TOON documents are not supported"));
}

#[test]
fn empty_yaml_file() {
    run_case("empty_yaml_file");
}

#[test]
fn empty_json_file() {
    run_case("empty_json_file");
}

#[test]
fn whitespace_only_file() {
    run_case("whitespace_only_file");
}

#[test]
fn non_mapping_root_yaml_truthy() {
    run_case("non_mapping_root_yaml_truthy");
}

#[test]
fn non_mapping_root_yaml_falsy_becomes_empty_dict() {
    run_case("non_mapping_root_yaml_falsy_becomes_empty_dict");
}

#[test]
fn non_mapping_root_json() {
    run_case("non_mapping_root_json");
}

#[test]
fn duplicate_key_yaml() {
    run_case("duplicate_key_yaml");
}

#[test]
fn duplicate_key_yaml_crlf() {
    run_case("duplicate_key_yaml_crlf");
}

#[test]
fn duplicate_key_json() {
    run_case("duplicate_key_json");
}

#[test]
fn lone_cr_near_miss_key() {
    run_case("lone_cr_near_miss_key");
}

#[test]
fn near_miss_key_error_and_unrelated_key_warning() {
    run_case("near_miss_key_error_and_unrelated_key_warning");
}

#[test]
fn schema_violation_loop_needs_exactly_one_of_while_or_each() {
    run_case("schema_violation_loop_needs_exactly_one_of_while_or_each");
}

#[test]
fn group_field_on_a_container_effect() {
    run_case("group_field_on_a_container_effect");
}

#[test]
fn interface_inputs_unknown_type() {
    run_case("interface_inputs_unknown_type");
}

#[test]
fn interface_inputs_default_type_mismatch_with_unquote_hint() {
    run_case("interface_inputs_default_type_mismatch_with_unquote_hint");
}

#[test]
fn negative_max_concurrency_config_error() {
    run_case("negative_max_concurrency_config_error");
}

#[test]
fn invalid_concurrency_groups_config_error() {
    run_case("invalid_concurrency_groups_config_error");
}

#[test]
fn runtime_not_an_object_is_a_run_only_error() {
    run_case("runtime_not_an_object_is_a_run_only_error");
}

#[test]
fn plugins_not_a_list_is_a_run_only_error() {
    run_case("plugins_not_a_list_is_a_run_only_error");
}

#[test]
fn plugins_entry_not_a_string_is_a_run_only_error() {
    run_case("plugins_entry_not_a_string_is_a_run_only_error");
}

#[test]
fn missing_required_interface_input_is_a_run_only_error() {
    run_case("missing_required_interface_input_is_a_run_only_error");
}

#[test]
fn missing_entry_file_with_an_unsupported_suffix() {
    run_case("missing_entry_file_with_an_unsupported_suffix");
}

#[test]
fn non_utf8_file_with_an_unsupported_suffix() {
    run_case("non_utf8_file_with_an_unsupported_suffix");
}

#[test]
fn every_case_in_the_corpus_has_a_test() {
    let text = include_str!("golden/load.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden/load.json is valid JSON");
    let covered = [
        "unsupported_suffix_md",
        "toon_refused",
        "empty_yaml_file",
        "empty_json_file",
        "whitespace_only_file",
        "non_mapping_root_yaml_truthy",
        "non_mapping_root_yaml_falsy_becomes_empty_dict",
        "non_mapping_root_json",
        "duplicate_key_yaml",
        "duplicate_key_yaml_crlf",
        "duplicate_key_json",
        "lone_cr_near_miss_key",
        "near_miss_key_error_and_unrelated_key_warning",
        "schema_violation_loop_needs_exactly_one_of_while_or_each",
        "group_field_on_a_container_effect",
        "interface_inputs_unknown_type",
        "interface_inputs_default_type_mismatch_with_unquote_hint",
        "negative_max_concurrency_config_error",
        "invalid_concurrency_groups_config_error",
        "runtime_not_an_object_is_a_run_only_error",
        "plugins_not_a_list_is_a_run_only_error",
        "plugins_entry_not_a_string_is_a_run_only_error",
        "missing_required_interface_input_is_a_run_only_error",
        "missing_entry_file_with_an_unsupported_suffix",
        "non_utf8_file_with_an_unsupported_suffix",
    ];
    for case in &cases {
        assert!(
            covered.contains(&case.name.as_str()),
            "case {:?} has no dedicated #[test] in golden_load.rs",
            case.name
        );
    }
    assert_eq!(
        covered.len(),
        cases.len(),
        "a covered name no longer matches any case"
    );
}
