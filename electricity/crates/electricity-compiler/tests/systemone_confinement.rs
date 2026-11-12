//! The `systemone` tool's `params.state_file`/`within` under each
//! document origin (issue #724): a literal `within` must be an absolute
//! path or `{from: input.<name>}`, a document with no file of its own
//! cannot name either at all, and a limited document's literal `within`
//! must stay inside its own project -- the same trust rules as the
//! `agent` tool's `prompt.file` (`agent_prompt_file.rs`).

use electricity_compiler::{DocumentOrigin, compile::compile_document};
use std::path::PathBuf;

fn document(within: &str) -> electricity_value::Value {
    electricity_yaml::load_yaml_last_key_wins(&format!(
        "effects:\n  - type: tool\n    name: decide\n    provider: systemone\n    params:\n      \
         model: m\n      questions: {{q: {{type: noul, instructions: i}}}}\n      \
         state_file: /project/state.json\n      within: {within}\n"
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

fn trusted() -> DocumentOrigin {
    DocumentOrigin::File {
        document_dir: project(),
        confinement_root: project(),
    }
}

#[test]
fn a_limited_documents_within_must_stay_inside_its_project() {
    let err = compile_document(&document("/elsewhere"), &limited()).unwrap_err();
    assert_eq!(
        err.0,
        "Tool effect 'decide' (systemone) at 'prime.effects[0]': params.within: must lie \
         inside the document's own project (a document the caller did not name by path is \
         confined to it)"
    );
    assert!(compile_document(&document("/project/data"), &limited()).is_ok());
}

#[test]
fn a_document_run_by_path_may_name_any_within() {
    assert!(compile_document(&document("/elsewhere"), &trusted()).is_ok());
}

#[test]
fn a_generated_document_cannot_use_state_file_or_within() {
    let err = compile_document(&document("/project"), &DocumentOrigin::Generated).unwrap_err();
    assert_eq!(
        err.0,
        "Tool effect 'decide' (systemone) at 'prime.effects[0]': params.state_file: 'file:' \
         cannot be used here — this document has no file of its own (it was generated at run \
         time, or has no path: stdin, an SDK string, a 'use: inline' child); give the text in \
         params.state.; params.within: 'file:' cannot be used here — this document has no \
         file of its own (it was generated at run time, or has no path: stdin, an SDK string, \
         a 'use: inline' child); give the text in params.state."
    );
}

#[test]
fn a_templated_within_is_refused_as_literal() {
    let err = compile_document(&document("/project/{{input.x}}"), &trusted()).unwrap_err();
    assert_eq!(
        err.0,
        "Tool effect 'decide' (systemone) at 'prime.effects[0]': params.within must be a \
         literal absolute path or {from: input.<name>}"
    );
}

#[test]
fn a_within_from_input_is_accepted() {
    let yaml = "effects:\n  - type: tool\n    name: decide\n    provider: systemone\n    \
                params:\n      model: m\n      questions: {q: {type: noul, instructions: i}}\n      \
                state_file: /project/state.json\n      within: {from: input.dir}\n";
    let doc = electricity_yaml::load_yaml_last_key_wins(yaml).unwrap();
    assert!(compile_document(&doc, &limited()).is_ok());
    assert!(compile_document(&doc, &trusted()).is_ok());
}
