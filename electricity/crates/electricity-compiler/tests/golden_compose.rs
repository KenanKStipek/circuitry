//! Replays `electricity/scripts/generate_compiler_compose_corpus.py`'s
//! golden cases: every message in `core/prompt_files.py` and in
//! `core/prompt_compose.py`'s compile-time composition checks.
//!
//! `check_for_run` always fails at `load_document` (lane B's own stub)
//! before reaching any of this lane's own code, so every case here
//! calls this crate's public [`compile_document`] directly instead --
//! parsing the entry file with `electricity-yaml` (a lane A/merged-crate
//! dependency, unaffected by lane B's stub) the same way `load_document`
//! eventually will, and building a [`DocumentOrigin::File`] from the
//! entry's own parent directory (every *now-testable* case's project
//! root -- none of them sit under a nested `circuitry.config.json`/
//! `config.json`). [`compile_document`]'s error carries the exact same
//! text `RunCheckError::Compile` would wrap verbatim once lane B lands,
//! since lane D's own `prompt_files::compile_declared_prompts`/
//! `compose::check_prompt_composition` run first inside it, before lane
//! C's own effect-by-effect compile step.
//!
//! A case whose document fails at the declared-prompts or composition
//! step is fully testable now. A case whose composition *succeeds*
//! reaches lane C's own unconditional "not implemented" gap next, so
//! its real (`run_error: None`) outcome can't be reproduced yet --
//! `#[ignore]`d, naming lane C, exactly like `golden_smoke.rs`'s own
//! precedent. The digest is independent of that gap, though
//! (`document_content_digest` runs against the parsed document
//! directly, never through `compile_document`), so
//! [`every_case_digest_matches_circuitry`] below verifies every case's
//! recorded `digest` right now, success or failure alike.

mod support;

use electricity_compiler::{DocumentOrigin, compile_document, document_content_digest};
use std::path::{Path, PathBuf};
use support::corpus::materialize;
use support::reference::{Case, matches, normalize};

/// The ``circuitry.config.json``/``config.json`` filenames
/// `default_project_root` looks for, in that order -- `core/
/// prompt_files.py`'s own `_CONFIG_FILENAMES`.
const CONFIG_FILENAMES: [&str; 2] = ["circuitry.config.json", "config.json"];

/// `core/prompt_files.py::default_project_root`, ported here for the
/// test harness: `_compiler_corpus.py::run_case` computes the
/// `confinement_root` it passes to `compile_orchestration`/
/// `document_content_digest` the same way, so a test that hardcoded
/// `confinement_root = document_dir` instead would disagree with the
/// golden file on any case whose project root sits above the
/// document's own directory (`digest_includes_a_parent_directory_
/// prompt_file`'s `docs/doc.yml` under a project-root `config.json`,
/// say).
fn default_project_root(document_dir: &Path) -> PathBuf {
    let mut current = document_dir.to_path_buf();
    loop {
        for name in CONFIG_FILENAMES {
            if current.join(name).is_file() {
                return current;
            }
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return document_dir.to_path_buf(),
        }
    }
}

fn load_case(name: &str) -> Case {
    let text = include_str!("golden/compose.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden/compose.json is valid JSON");
    cases
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no case named {name:?} in golden/compose.json"))
}

/// Parses *entry* the way `load_document` eventually will (lane B),
/// without depending on its still-stubbed implementation.
fn load_value(entry: &Path) -> electricity_value::Value {
    let text = std::fs::read_to_string(entry).expect("read entry file");
    electricity_yaml::load_yaml(&text).expect("entry file parses as YAML")
}

/// *definition_error*'s own raw message, minus `_compiler_corpus.py`'s
/// `f"{type(exc).__name__}: {exc}"` prefix -- the exception class name
/// is always a plain identifier with no `:` of its own, so splitting on
/// the first `": "` reliably separates it from the message even when
/// the message itself goes on to contain more colons.
fn strip_exception_class_name(definition_error: &str) -> &str {
    definition_error
        .split_once(": ")
        .map(|(_, message)| message)
        .unwrap_or(definition_error)
}

/// Replays *name*, asserting [`compile_document`]'s error text matches
/// the golden case's own `definition_error` (which must be `Some`) --
/// `_compiler_corpus.py`'s own record of calling `compile_orchestration`
/// directly, the same call this test makes, unlike `run_error`/
/// `validate`'s `errors` (the *full* `cof check`/`cof run` pipeline,
/// which catches a schema-shaped violation -- `prompts: nope`, say --
/// in its own structural/JSON-Schema pass, lane B's own code, before
/// `compile_orchestration`/`compile_document` is ever reached).
fn assert_compile_error_matches(name: &str) {
    let case = load_case(name);
    let definition_error = case
        .definition_error
        .as_deref()
        .unwrap_or_else(|| panic!("case {name:?} has no definition_error to compare against"));
    let expected = strip_exception_class_name(definition_error);
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = load_value(&entry);
    let document_dir = entry.parent().unwrap().to_path_buf();
    let confinement_root = default_project_root(&document_dir);
    let origin = DocumentOrigin::File {
        document_dir,
        confinement_root,
    };

    let result = compile_document(&document, &origin);
    let actual = result
        .as_ref()
        .err()
        .unwrap_or_else(|| panic!("case {name:?}: expected an error, got {result:?}"))
        .to_string();
    let actual = normalize(&actual, &temp.root);
    let mode = case.comparison.run_error.as_deref().unwrap_or("exact");
    assert!(
        matches(mode, expected, &actual),
        "case {name:?} ({mode}): expected {expected:?}, got {actual:?}"
    );
}

macro_rules! run_error_case {
    ($test_name:ident, $case_name:literal) => {
        #[test]
        fn $test_name() {
            assert_compile_error_matches($case_name);
        }
    };
}

// -- core/prompt_files.py -----------------------------------------------------

run_error_case!(prompts_not_a_mapping, "prompts_not_a_mapping");
run_error_case!(prompts_key_dotted, "prompts_key_dotted");
run_error_case!(prompts_key_non_string, "prompts_key_non_string");
run_error_case!(prompts_value_wrong_shape, "prompts_value_wrong_shape");
run_error_case!(prompts_file_non_string, "prompts_file_non_string");
run_error_case!(prompts_file_absolute_path, "prompts_file_absolute_path");
run_error_case!(prompts_file_template_tag, "prompts_file_template_tag");
run_error_case!(prompts_file_missing, "prompts_file_missing");
run_error_case!(
    prompts_file_not_a_regular_file,
    "prompts_file_not_a_regular_file"
);
run_error_case!(prompts_file_over_size_limit, "prompts_file_over_size_limit");
run_error_case!(prompts_file_not_utf8, "prompts_file_not_utf8");
run_error_case!(
    prompts_file_symlink_escapes_the_project,
    "prompts_file_symlink_escapes_the_project"
);

/// `prompts_file_unreadable`'s own golden `run_error` is only compared
/// by `"location"` (the OS error text after "could not be read: " is
/// Rust's `io::Error` `Display`, which never agrees with Python's own
/// `OSError.__str__` word for word, DESIGN.md §1/§12) -- but the
/// Circuitry-owned prefix in front of it must still match exactly, not
/// just "non-empty". The case's `secret.md` is written with `mode:
/// "000"` (`tests/support`'s own additive field, applied with `chmod`
/// -- see `support::corpus::apply_mode`), so this only runs where Unix
/// permission bits are meaningful.
#[test]
#[cfg(unix)]
fn prompts_file_unreadable_matches_circuitrys_own_prefix() {
    let case = load_case("prompts_file_unreadable");
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = load_value(&entry);
    let document_dir = entry.parent().unwrap().to_path_buf();
    let confinement_root = default_project_root(&document_dir);
    let origin = DocumentOrigin::File {
        document_dir,
        confinement_root,
    };

    let result = compile_document(&document, &origin);
    let actual = result
        .as_ref()
        .err()
        .unwrap_or_else(|| panic!("expected an error, got {result:?}"))
        .to_string();
    let actual = normalize(&actual, &temp.root);

    let prefix = "prompts.voice: 'file' 'secret.md' could not be read: ";
    assert!(
        actual.starts_with(prefix),
        "expected prefix {prefix:?}, got {actual:?}"
    );
}

// -- core/prompt_compose.py: check_prompt_composition -------------------------

run_error_case!(unknown_partial_name, "unknown_partial_name");
run_error_case!(
    declared_prompt_and_effect_name_collide,
    "declared_prompt_and_effect_name_collide"
);
run_error_case!(
    reference_to_a_non_text_effect,
    "reference_to_a_non_text_effect"
);
run_error_case!(
    reference_to_a_json_prompt_is_not_text_producing,
    "reference_to_a_json_prompt_is_not_text_producing"
);
run_error_case!(cycle_among_declared_prompts, "cycle_among_declared_prompts");
run_error_case!(invalid_partial_name_shape, "invalid_partial_name_shape");
run_error_case!(
    set_delimiter_inside_a_declared_prompt,
    "set_delimiter_inside_a_declared_prompt"
);
run_error_case!(malformed_declared_prompt, "malformed_declared_prompt");
run_error_case!(
    bare_reference_does_not_cross_a_named_if_boundary,
    "bare_reference_does_not_cross_a_named_if_boundary"
);
run_error_case!(
    dotted_reference_inside_a_named_if_to_a_non_root_name,
    "dotted_reference_inside_a_named_if_to_a_non_root_name"
);
run_error_case!(
    tool_params_partial_reference_is_checked,
    "tool_params_partial_reference_is_checked"
);

// -- cases whose document passes declared-prompts and composition, so they
// -- reach lane C's own unconditional gap next (`compile_document`'s
// -- "not implemented" stub) rather than Circuitry's own real outcome.

fn assert_compile_succeeds(name: &str) {
    let case = load_case(name);
    assert!(case.run_error.is_none());
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = load_value(&entry);
    let document_dir = entry.parent().unwrap().to_path_buf();
    let confinement_root = default_project_root(&document_dir);
    let origin = DocumentOrigin::File {
        document_dir,
        confinement_root,
    };
    let result = compile_document(&document, &origin);
    assert!(result.is_ok(), "expected success, got {result:?}");
}

#[test]
#[ignore = "passes composition and reaches lane C's own compile gap; \
            un-ignore once lane C's compile_document lands"]
fn prompts_file_crlf_translated_succeeds_against_circuitry() {
    assert_compile_succeeds("prompts_file_crlf_translated");
}

#[test]
#[ignore = "passes composition and reaches lane C's own compile gap; \
            un-ignore once lane C's compile_document lands"]
fn dotted_reference_reaches_into_a_named_if_from_anywhere_succeeds_against_circuitry() {
    assert_compile_succeeds("dotted_reference_reaches_into_a_named_if_from_anywhere");
}

#[test]
#[ignore = "passes composition and reaches lane C's own compile gap; \
            un-ignore once lane C's compile_document lands"]
fn dotted_self_reference_from_inside_the_same_named_if_succeeds_against_circuitry() {
    assert_compile_succeeds("dotted_self_reference_from_inside_the_same_named_if");
}

#[test]
#[ignore = "passes composition and reaches lane C's own compile gap; \
            un-ignore once lane C's compile_document lands"]
fn dotted_self_reference_from_inside_a_named_dynamic_succeeds_against_circuitry() {
    assert_compile_succeeds("dotted_self_reference_from_inside_a_named_dynamic");
}

/// The digest itself can't be reached through `compile_document` either
/// (`pipeline.rs` only attaches it to a `Program` that already compiled
/// successfully) -- see [`every_case_digest_matches_circuitry`] below
/// for where it *is* verified right now, independent of compile
/// succeeding.
#[test]
#[ignore = "compile_document itself is lane C's gap; the digest is \
            verified independently by every_case_digest_matches_circuitry"]
fn digest_with_no_prompt_files_succeeds_against_circuitry() {
    assert_compile_succeeds("digest_with_no_prompt_files");
}

#[test]
#[ignore = "compile_document itself is lane C's gap; the digest is \
            verified independently by every_case_digest_matches_circuitry"]
fn digest_includes_a_referenced_prompt_file_succeeds_against_circuitry() {
    assert_compile_succeeds("digest_includes_a_referenced_prompt_file");
}

#[test]
#[ignore = "compile_document itself is lane C's gap; the digest is \
            verified independently by every_case_digest_matches_circuitry"]
fn digest_includes_a_parent_directory_prompt_file_succeeds_against_circuitry() {
    assert_compile_succeeds("digest_includes_a_parent_directory_prompt_file");
}

// -- the document digest: verified against every case, independent of
// -- whether composition/compile succeeds (`document_content_digest`
// -- runs against the parsed document directly, not through
// -- `compile_document`).

/// *case*'s own recorded `digest`, or an error string naming the
/// mismatch -- collected rather than asserted immediately, so one
/// failing case's message doesn't hide any other.
fn digest_mismatch(case: &Case) -> Option<String> {
    let Some(expected) = case.digest.as_deref() else {
        return Some(format!(
            "case {:?} has no digest to compare against",
            case.name
        ));
    };
    let temp = materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = load_value(&entry);
    let document_dir = entry.parent().unwrap().to_path_buf();
    let confinement_root = default_project_root(&document_dir);

    match document_content_digest(&entry, &document, &confinement_root) {
        Ok(actual) if actual == expected => None,
        Ok(actual) => Some(format!(
            "case {:?}: digest mismatch: expected {expected:?}, got {actual:?}",
            case.name
        )),
        Err(err) => Some(format!(
            "case {:?}: digest computation failed: {err:?}",
            case.name
        )),
    }
}

#[test]
fn every_case_digest_matches_circuitry() {
    let text = include_str!("golden/compose.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden/compose.json is valid JSON");
    let failures: Vec<String> = cases.iter().filter_map(digest_mismatch).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
