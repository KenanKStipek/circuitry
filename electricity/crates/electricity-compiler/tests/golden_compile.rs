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

use electricity_compiler::{DocumentOrigin, compile_document};
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

/// Cases this generic loop skips, each compared by its own dedicated
/// test below for a documented reason.
const SPECIAL_CASED: &[&str] = &[
    "use_ref_rejected_as_electricity_divergence",
    "cel_expr_does_not_parse",
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

/// `use` cycle detection (`cycles::detect_cycles`) is not part of
/// `compile_document`/`compile_orchestration` at all -- both compile
/// successfully, cycle or not; `use_cycle_two_hop`'s own cycle is
/// pinned instead by `cycles.rs`'s own unit tests, which call
/// `detect_cycles` directly.
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
