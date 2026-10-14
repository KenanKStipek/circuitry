//! Replays the `"orchestration_structural"` cases from
//! `electricity-schema`'s own golden corpus (`electricity/scripts/
//! generate_schema_corpus.py`) against [`electricity_compiler::
//! structural_errors`] -- the near-miss-unknown-key/`group:`/
//! `interface.inputs`-type-error logic `electricity_schema::
//! orchestration_errors` alone has no knowledge of, so
//! `electricity-schema`'s own `golden_corpus.rs` skips these cases
//! rather than trying to replay them (issue #528, review finding 3:
//! the generator's own acceptance criterion names `structural_errors`,
//! not just `schema_errors`, for exactly this interleaving).
//!
//! The corpus file is shared with (and generated/checked alongside)
//! `electricity-schema/tests/golden/corpus.json`; this crate reads the
//! same committed file rather than keeping a second copy.

use electricity_compiler::structural_errors;
use electricity_schema::value_from_json;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    name: String,
    schema: String,
    document: Value,
    valid: bool,
    #[serde(default)]
    described_errors: Vec<String>,
}

#[test]
fn structural_corpus_matches_circuitry() {
    let text = include_str!("../../electricity-schema/tests/golden/corpus.json");
    let cases: Vec<Case> = serde_json::from_str(text).expect("golden corpus.json is valid JSON");
    let structural_cases: Vec<&Case> = cases
        .iter()
        .filter(|c| c.schema == "orchestration_structural")
        .collect();
    assert!(
        !structural_cases.is_empty(),
        "no \"orchestration_structural\" case found in the shared corpus"
    );

    let mut failures = Vec::new();
    for case in structural_cases {
        let document = value_from_json(&case.document);
        let errors = structural_errors(&document);
        let is_valid = errors.is_empty();
        if is_valid != case.valid {
            failures.push(format!(
                "case {:?}: expected valid={}, got valid={} (errors: {errors:?})",
                case.name, case.valid, is_valid
            ));
            continue;
        }
        if errors != case.described_errors {
            failures.push(format!(
                "case {:?}: described-error mismatch:\n  expected {:?}\n  got      {:?}",
                case.name, case.described_errors, errors
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
