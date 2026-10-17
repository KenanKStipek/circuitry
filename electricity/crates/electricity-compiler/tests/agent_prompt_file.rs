//! The `agent` tool's `params.prompt: {file: {from: ...}, within: ...}`
//! (issue #591) under each document origin: compiled with the same trust
//! rules as a prompt's `template` of that form. The messages are
//! `compile_orchestration`'s own; `golden_compile.rs` covers a document run
//! by path.

use electricity_compiler::{DocumentOrigin, compile::compile_document};
use std::path::PathBuf;

fn document(within: &str) -> electricity_value::Value {
    electricity_yaml::load_yaml_last_key_wins(&format!(
        "effects:\n  - type: tool\n    name: implement\n    provider: agent\n    params:\n      \
         prompt: {{file: {{from: input.f}}, within: {within}}}\n"
    ))
    .unwrap()
}

fn project() -> PathBuf {
    PathBuf::from("/project")
}

fn limited() -> DocumentOrigin {
    DocumentOrigin::LimitedFile {
        document_dir: project(),
        confinement_root: project(),
    }
}

#[test]
fn a_limited_documents_within_must_stay_inside_its_project() {
    let err = compile_document(&document("/elsewhere"), &limited()).unwrap_err();
    assert_eq!(
        err.0,
        "prime.effects[0].params.prompt: 'within' '/elsewhere' resolves outside the project \
         (/project) — a limited document's 'within' must stay inside it."
    );
    let program = compile_document(&document("/project/data"), &limited()).unwrap();
    let json = serde_json::to_string(&program).unwrap();
    assert!(
        json.contains(
            "\"prompt_file\":{\"path_from\":\"input.f\",\"within\":\"/project/data\",\
             \"within_from\":null,\"confine_to\":\"/project\"}"
        ),
        "{json}"
    );
}

#[test]
fn a_document_run_by_path_may_name_any_within() {
    let origin = DocumentOrigin::File {
        document_dir: project(),
        confinement_root: project(),
    };
    assert!(compile_document(&document("/elsewhere"), &origin).is_ok());
}

#[test]
fn a_generated_document_cannot_use_it() {
    let err = compile_document(&document("/project"), &DocumentOrigin::Generated).unwrap_err();
    assert_eq!(
        err.0,
        "prime.effects[0].params.prompt: 'file:' cannot be used here — this document has no \
         file of its own (it was generated at run time, or has no path: stdin, an SDK string, \
         a 'use: inline' child)."
    );
}
