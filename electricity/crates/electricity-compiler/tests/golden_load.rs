//! Replays `electricity/scripts/generate_compiler_load_corpus.py`'s golden
//! cases (issue #408's lane B section): `check_report` against Circuitry's
//! `validate(...)`, and `check_for_run`'s text against `run(...)`'s own
//! `RunResult.error`. Every case here fails before `compile_orchestration`
//! ever runs (a load, structural, or concurrency-configuration error), so
//! every one is fully checkable against this lane alone.

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

    // `case.validate.warnings` minus `case.lint_warnings`: `check_report`
    // does not reproduce Circuitry's lint advisories (issue #428), only
    // `unknown_key_warnings` and the host-settings notice.
    assert_eq!(
        report.warnings,
        case.non_lint_warnings(),
        "case {:?}: check_report warnings",
        case.name
    );

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
fn if_threshold_huge_int_is_a_maximum_error() {
    run_case("if_threshold_huge_int_is_a_maximum_error");
}

#[test]
fn tree_loop_max_concurrency_huge_negative_int_is_a_minimum_error() {
    run_case("tree_loop_max_concurrency_huge_negative_int_is_a_minimum_error");
}

#[test]
fn prompt_timeout_ms_huge_int_is_valid() {
    run_case("prompt_timeout_ms_huge_int_is_valid");
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
fn interface_input_e_string_value_int_shaped_text_is_recovered() {
    run_case("interface_input_e_string_value_int_shaped_text_is_recovered");
}

#[test]
fn interface_input_e_string_value_boolean_shaped_text_is_recovered() {
    run_case("interface_input_e_string_value_boolean_shaped_text_is_recovered");
}

#[test]
fn interface_input_e_string_value_exponent_shaped_text_is_recovered() {
    run_case("interface_input_e_string_value_exponent_shaped_text_is_recovered");
}

#[test]
fn interface_input_e_string_value_null_text_stays_present_via_restore() {
    run_case("interface_input_e_string_value_null_text_stays_present_via_restore");
}

#[test]
fn interface_input_e_string_value_array_shaped_text_passes_only_via_restore() {
    run_case("interface_input_e_string_value_array_shaped_text_passes_only_via_restore");
}

#[test]
fn interface_input_e_required_non_string_input_given_null_is_still_missing() {
    run_case("interface_input_e_required_non_string_input_given_null_is_still_missing");
}

#[test]
fn interface_input_e_boolean_word_that_json_sniffs_to_an_int() {
    run_case("interface_input_e_boolean_word_that_json_sniffs_to_an_int");
}

#[test]
fn interface_input_e_number_word_that_json_sniffs_to_a_bool() {
    run_case("interface_input_e_number_word_that_json_sniffs_to_a_bool");
}

#[test]
fn interface_input_e_array_value_that_json_sniffs_to_an_object() {
    run_case("interface_input_e_array_value_that_json_sniffs_to_an_object");
}

#[test]
fn interface_input_e_underscore_prefixed_key_is_never_lifted() {
    run_case("interface_input_e_underscore_prefixed_key_is_never_lifted");
}

#[test]
fn interface_input_e_input_namespace_key_satisfies_required_input() {
    run_case("interface_input_e_input_namespace_key_satisfies_required_input");
}

#[test]
fn interface_input_e_non_dict_input_key_value_becomes_an_empty_namespace() {
    run_case("interface_input_e_non_dict_input_key_value_becomes_an_empty_namespace");
}

#[test]
fn interface_input_e_input_key_present_other_e_keys_are_not_lifted() {
    run_case("interface_input_e_input_key_present_other_e_keys_are_not_lifted");
}

#[test]
fn interface_input_e_declared_input_named_prime_cannot_be_satisfied() {
    run_case("interface_input_e_declared_input_named_prime_cannot_be_satisfied");
}

#[test]
fn interface_input_e_number_valid() {
    run_case("interface_input_e_number_valid");
}

#[test]
fn interface_input_e_number_invalid() {
    run_case("interface_input_e_number_invalid");
}

#[test]
fn interface_input_e_integer_with_underscores() {
    run_case("interface_input_e_integer_with_underscores");
}

#[test]
fn interface_input_e_integer_invalid() {
    run_case("interface_input_e_integer_invalid");
}

#[test]
fn interface_input_e_boolean_word_valid() {
    run_case("interface_input_e_boolean_word_valid");
}

#[test]
fn interface_input_e_boolean_invalid() {
    run_case("interface_input_e_boolean_invalid");
}

#[test]
fn interface_input_e_array_valid() {
    run_case("interface_input_e_array_valid");
}

#[test]
fn interface_input_e_array_invalid() {
    run_case("interface_input_e_array_invalid");
}

#[test]
fn interface_input_e_object_valid() {
    run_case("interface_input_e_object_valid");
}

#[test]
fn interface_input_e_object_invalid() {
    run_case("interface_input_e_object_invalid");
}

#[test]
fn required_interface_input_supplied_via_e_run_succeeds() {
    run_case("required_interface_input_supplied_via_e_run_succeeds");
}

#[test]
fn default_overridden_by_provided_e_value() {
    run_case("default_overridden_by_provided_e_value");
}

#[test]
fn undeclared_extra_e_input_is_allowed() {
    run_case("undeclared_extra_e_input_is_allowed");
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
        "if_threshold_huge_int_is_a_maximum_error",
        "tree_loop_max_concurrency_huge_negative_int_is_a_minimum_error",
        "prompt_timeout_ms_huge_int_is_valid",
        "runtime_not_an_object_is_a_run_only_error",
        "plugins_not_a_list_is_a_run_only_error",
        "plugins_entry_not_a_string_is_a_run_only_error",
        "missing_required_interface_input_is_a_run_only_error",
        "interface_input_e_string_value_int_shaped_text_is_recovered",
        "interface_input_e_string_value_boolean_shaped_text_is_recovered",
        "interface_input_e_string_value_exponent_shaped_text_is_recovered",
        "interface_input_e_string_value_null_text_stays_present_via_restore",
        "interface_input_e_string_value_array_shaped_text_passes_only_via_restore",
        "interface_input_e_required_non_string_input_given_null_is_still_missing",
        "interface_input_e_boolean_word_that_json_sniffs_to_an_int",
        "interface_input_e_number_word_that_json_sniffs_to_a_bool",
        "interface_input_e_array_value_that_json_sniffs_to_an_object",
        "interface_input_e_underscore_prefixed_key_is_never_lifted",
        "interface_input_e_input_namespace_key_satisfies_required_input",
        "interface_input_e_non_dict_input_key_value_becomes_an_empty_namespace",
        "interface_input_e_input_key_present_other_e_keys_are_not_lifted",
        "interface_input_e_declared_input_named_prime_cannot_be_satisfied",
        "interface_input_e_number_valid",
        "interface_input_e_number_invalid",
        "interface_input_e_integer_with_underscores",
        "interface_input_e_integer_invalid",
        "interface_input_e_boolean_word_valid",
        "interface_input_e_boolean_invalid",
        "interface_input_e_array_valid",
        "interface_input_e_array_invalid",
        "interface_input_e_object_valid",
        "interface_input_e_object_invalid",
        "required_interface_input_supplied_via_e_run_succeeds",
        "default_overridden_by_provided_e_value",
        "undeclared_extra_e_input_is_allowed",
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
