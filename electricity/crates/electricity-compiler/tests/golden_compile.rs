//! Replays `electricity/scripts/generate_compiler_compile_corpus.py`'s
//! golden cases against this lane's own `compile_document` (issue
//! #408's Test strategy section).
//!
//! Lane B's `load_document` hasn't landed yet, so each case's entry
//! file is parsed directly here (`electricity-yaml`/`electricity-json`,
//! by suffix) rather than through the pipeline's own surfaces; a
//! `DocumentOrigin::File` is built from the canonicalized case root
//! (every corpus case is a single, flat project with no nested
//! `config.json` of its own, so `document_dir` and `confinement_root`
//! coincide). A test-only projection of the compiled [`Program`]
//! ([`support::projection`]) is compared against the golden
//! `definition`; a compile error is compared, word for word, against
//! `definition_error` with its leading Python exception-class name
//! stripped (every error `compile_document` raises is Circuitry's own
//! `ValueError`-equivalent text, never third-party).

mod support;

use electricity_compiler::{DocumentOrigin, RunCheckError, compile_document, cycles, groups};
use electricity_value::Value;
use std::collections::BTreeSet;
use std::path::Path;
use support::projection::{definitions_match, project_program};
use support::reference::Case;

fn load_cases() -> Vec<Case> {
    let text = include_str!("golden/compile.json");
    serde_json::from_str(text).expect("golden/compile.json is valid JSON")
}

/// Strips a leading `SomeExceptionType: ` the corpus helper's own
/// `f"{type(exc).__name__}: {exc}"` always prefixes `definition_error`
/// with -- every error this crate's `compile_document` raises is a
/// bare message, with no Python exception-class name of its own.
fn strip_exception_prefix(message: &str) -> &str {
    match message.split_once(": ") {
        Some((prefix, rest))
            if prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            rest
        }
        _ => message,
    }
}

fn parse_document(entry: &Path) -> electricity_value::Value {
    let text = std::fs::read_to_string(entry).expect("read entry file");
    match entry.extension().and_then(|e| e.to_str()) {
        Some("json") => electricity_json::load_json(&text).expect("entry parses as JSON"),
        _ => electricity_yaml::load_yaml(&text).expect("entry parses as YAML"),
    }
}

/// The document's own `runtime.concurrency_groups` keys -- every golden
/// case here runs with no project `config.json` of its own
/// (`_IsolatedRun`'s isolated `HOME`/cwd), so the document's own
/// `runtime:` block is the *entire* merged runtime config `cli/
/// runtime_shim.py`'s `concurrency_limiter.group_names` would be built
/// from -- the same value [`groups::unknown_group_errors`]'s real
/// caller (`pipeline.rs`, lane B) would pass once it stops using an
/// empty placeholder set of its own.
fn known_groups_from_runtime(program: &electricity_bytecode::Program) -> BTreeSet<String> {
    let Some(Value::Dict(runtime)) = &program.runtime_block else {
        return BTreeSet::new();
    };
    let Some(Value::Dict(groups)) = runtime.get(&Value::Str("concurrency_groups".to_string()))
    else {
        return BTreeSet::new();
    };
    groups
        .keys()
        .filter_map(|k| match k {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The `run_error` lane C's own two post-compile checks
/// ([`groups::unknown_group_errors`], then [`cycles::detect_cycles`] --
/// `pipeline.rs`'s own documented order) would produce for a
/// successfully compiled *program*/*document*, formatted exactly as
/// [`RunCheckError`]'s `Display` formats them -- `None` when neither
/// check finds anything.
fn our_post_compile_run_error(
    program: &electricity_bytecode::Program,
    document: &Value,
    entry: &Path,
) -> Option<String> {
    let known_groups = known_groups_from_runtime(program);
    let group_errors = groups::unknown_group_errors(program, &known_groups);
    if !group_errors.is_empty() {
        return Some(RunCheckError::Structural(group_errors).to_string());
    }
    if let Err(err) = cycles::detect_cycles(document, Some(entry)) {
        return Some(RunCheckError::Cycle(err.0).to_string());
    }
    None
}

/// Whether *run_error* is something lane C's own group/cycle checks are
/// responsible for (and so comparable against
/// [`our_post_compile_run_error`]) -- a case whose `run_error` comes
/// from a pipeline stage this corpus's direct `compile_document` call
/// bypasses entirely (the JSON-schema structural check, lane B) is
/// left uncompared, same as the generic loop already leaves every
/// other pipeline-only error uncompared.
fn is_group_or_cycle_error(run_error: &str) -> bool {
    run_error.starts_with("Cycle: ") || run_error.contains("concurrency_groups")
}

/// Cases this generic loop skips, each compared by its own dedicated
/// test below for a documented reason.
const SPECIAL_CASED: &[&str] = &[
    "use_ref_rejected_as_electricity_divergence",
    "cel_expr_does_not_parse",
    "loop_while_cel_parse_error_has_its_own_label",
];

#[test]
fn compile_corpus_matches_circuitry() {
    for case in load_cases() {
        if SPECIAL_CASED.contains(&case.name.as_str()) {
            continue;
        }
        let temp = support::corpus::materialize(&case.name, &case.files);
        let entry = temp.entry_path(&case.entry);
        let document = parse_document(&entry);
        let root = temp.root.clone();
        let origin = DocumentOrigin::File {
            document_dir: root.clone(),
            confinement_root: root,
        };

        let result = compile_document(&document, &origin);

        match (&case.definition, &case.definition_error) {
            (Some(expected), None) => {
                let program = result.unwrap_or_else(|err| {
                    panic!(
                        "case {:?}: expected success, got error: {}",
                        case.name, err.0
                    )
                });
                let actual = project_program(&program);
                assert!(
                    definitions_match(expected, &actual),
                    "case {:?}: definition mismatch\nexpected: {}\nactual:   {}",
                    case.name,
                    serde_json::to_string_pretty(expected).unwrap(),
                    serde_json::to_string_pretty(&actual).unwrap()
                );

                let comparable = match &case.run_error {
                    None => true,
                    Some(text) => is_group_or_cycle_error(text),
                };
                if comparable {
                    let ours = our_post_compile_run_error(&program, &document, &entry)
                        .map(|text| support::reference::normalize(&text, &temp.root));
                    assert_eq!(
                        ours, case.run_error,
                        "case {:?}: group/cycle run_error mismatch",
                        case.name
                    );
                }
            }
            (None, Some(expected_error)) => {
                let err = match result {
                    Err(err) => err,
                    Ok(_) => panic!(
                        "case {:?}: expected a compile error, got success",
                        case.name
                    ),
                };
                let expected_message = strip_exception_prefix(expected_error);
                assert_eq!(
                    err.0, expected_message,
                    "case {:?}: error text mismatch",
                    case.name
                );
            }
            (None, None) => {
                // Circuitry's own compile_orchestration never reaches a
                // definition at all (e.g. the document failed to load
                // before compiling) -- nothing this crate's own
                // compile_document is responsible for reproducing.
            }
            (Some(_), Some(_)) => {
                panic!(
                    "case {:?}: corpus has both a definition and an error",
                    case.name
                )
            }
        }
    }
}

/// Documented divergence (DESIGN.md §4): a document naming `ref:` alone
/// compiles fine against Circuitry's own `compile_orchestration` (it
/// resolves at run time, outside this crate's scope) but this crate
/// rejects it outright -- electricity implements no library-name/
/// remote-library resolution.
#[test]
fn use_ref_is_rejected_even_though_circuitry_compiles_it() {
    let case = load_cases()
        .into_iter()
        .find(|c| c.name == "use_ref_rejected_as_electricity_divergence")
        .expect("case present in golden/compile.json");
    assert!(
        case.definition.is_some(),
        "fixture expectation: Circuitry's own compile_orchestration accepts ref:"
    );

    let temp = support::corpus::materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = parse_document(&entry);
    let root = temp.root.clone();
    let origin = DocumentOrigin::File {
        document_dir: root.clone(),
        confinement_root: root,
    };

    let err = compile_document(&document, &origin).unwrap_err();
    assert!(
        err.0.contains("library refs") && err.0.contains("not supported"),
        "unexpected error: {}",
        err.0
    );
}

/// Circuitry's own `validate_cel_syntax` embeds the underlying parser's
/// own error text (`lark`'s), which `cel`'s own parser never matches
/// word for word (DESIGN.md §1/§12's third-party-text carve-out) --
/// compared by location (prefix/suffix of Circuitry's own wording)
/// rather than exactly.
#[test]
fn malformed_cel_expression_fails_at_the_same_place() {
    let case = load_cases()
        .into_iter()
        .find(|c| c.name == "cel_expr_does_not_parse")
        .expect("case present in golden/compile.json");

    let temp = support::corpus::materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = parse_document(&entry);
    let root = temp.root.clone();
    let origin = DocumentOrigin::File {
        document_dir: root.clone(),
        confinement_root: root,
    };

    let err = compile_document(&document, &origin).unwrap_err();
    assert!(
        err.0
            .starts_with("CEL expression at 'prime.effects[0]': expression does not parse (")
    );
    assert!(err.0.ends_with("Expression: 'state.a =='"));
}

/// Finding 3: a `while`-loop's CEL parse error uses its own label
/// (`"Loop while CEL expression"`, not the default `"CEL expression"`),
/// wrapping the same third-party parser text
/// `malformed_cel_expression_fails_at_the_same_place` already carves
/// out by location rather than exact text.
#[test]
fn loop_while_cel_parse_error_uses_its_own_label() {
    let case = load_cases()
        .into_iter()
        .find(|c| c.name == "loop_while_cel_parse_error_has_its_own_label")
        .expect("case present in golden/compile.json");

    let temp = support::corpus::materialize(&case.name, &case.files);
    let entry = temp.entry_path(&case.entry);
    let document = parse_document(&entry);
    let root = temp.root.clone();
    let origin = DocumentOrigin::File {
        document_dir: root.clone(),
        confinement_root: root,
    };

    let err = compile_document(&document, &origin).unwrap_err();
    assert!(err.0.starts_with(
        "Loop while CEL expression at 'prime.effects[0]' (effect 'poll'): \
         expression does not parse ("
    ));
    assert!(err.0.ends_with("Expression: 'state.a =='"));
}

/// `use` cycle detection (`cycles::detect_cycles`) is not part of
/// `compile_document`/`compile_orchestration` at all -- both compile
/// successfully, cycle or not; `use_cycle_two_hop`'s own cycle is
/// pinned against Circuitry's own recorded `run_error` by
/// `compile_corpus_matches_circuitry` (which calls `detect_cycles`
/// directly, decision 1) and by `cycles.rs`'s own unit tests.
#[test]
fn use_path_children_compile_successfully_cycle_or_not() {
    for name in ["use_cycle_two_hop", "use_acyclic_chain"] {
        let case = load_cases()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("case {name:?} present in golden/compile.json"));
        assert!(case.definition.is_some(), "case {name:?} should compile");
    }
}
