//! A `use` effect carries the trust of the document it is compiled in
//! (`UseDefinition.parent_trusted`, issue #578): only a document run by path
//! is trusted, so only its path children are.

use electricity_compiler::{DocumentOrigin, compile::compile_document};
use std::path::PathBuf;

const DOCUMENT: &str = "effects:\n  - type: use\n    name: child\n    path: child.yaml\n";

fn parent_trusted_in(origin: &DocumentOrigin) -> bool {
    let document = electricity_yaml::load_yaml_last_key_wins(DOCUMENT).unwrap();
    let program = compile_document(&document, origin).unwrap();
    let json = serde_json::to_string(&program).unwrap();
    if json.contains("\"parent_trusted\":true") {
        true
    } else {
        assert!(json.contains("\"parent_trusted\":false"), "{json}");
        false
    }
}

fn dir() -> PathBuf {
    PathBuf::from("/project")
}

#[test]
fn a_document_run_by_path_makes_its_use_effects_trusted() {
    assert!(parent_trusted_in(&DocumentOrigin::File {
        document_dir: dir(),
        confinement_root: dir(),
    }));
}

#[test]
fn a_limited_document_makes_its_use_effects_limited() {
    assert!(!parent_trusted_in(&DocumentOrigin::LimitedFile {
        document_dir: dir(),
        confinement_root: dir(),
    }));
}

#[test]
fn a_generated_document_makes_its_use_effects_limited() {
    assert!(!parent_trusted_in(&DocumentOrigin::Generated));
}
