//! Exercises the golden corpus `electricity/scripts/generate_vm_run_
//! seeding.py` writes (PR #441 review finding 1a/1d): `-e` seeding's
//! own success-path corner cases -- an optional, `null`-valued input
//! dropped on success (1a), and a root-level `-e` key that is never
//! lifted under `input` at all (a namespace name, an `_`-prefixed key,
//! or a sibling of an `-e input=...` that already wins outright -- 1d)
//! -- each compared against a real `cof run`'s own normalized `--out`
//! state. Every case's own document and `-e` inputs live in the golden
//! file itself (`CorpusCase::orchestration`/`inputs`).

mod support;

use electricity::run::RunRequest;
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::fs;
use std::path::PathBuf;
use support::normalize::{normalize, strip_invocation_shape_fields};
use support::run_corpus::{CorpusCase, load_corpus_at};

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/run_seeding.json"
);

static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "electricity-run-seeding-compare-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn run_case(case: &CorpusCase) -> Json {
    let dir = temp_dir(&case.name);
    let orchestration = case
        .orchestration
        .as_deref()
        .unwrap_or_else(|| panic!("{}: corpus case has no 'orchestration' text", case.name));
    let doc_path = dir.join("orchestration.yml");
    fs::write(&doc_path, orchestration).unwrap();
    let config_path = dir.join("config.json");
    fs::write(&config_path, "{}").unwrap();

    let inputs: IndexMap<String, String> = case
        .inputs
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .unwrap_or_else(|| panic!("{}: -e input {k} isn't a string", case.name))
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let out_path = dir.join("expected.out.json");
    let req = RunRequest {
        config_path,
        orchestration_path: doc_path,
        inputs,
        out_path: Some(out_path.clone()),
        pretty: false,
        live_state_path: None,
        events_path: None,
    };
    let token = CancellationToken::new();
    let result = electricity::run_orchestration(&req, &token).await;
    assert!(
        result.ok,
        "{}: expected a successful run, got error {:?}",
        case.name, result.error
    );
    let state = result.state.unwrap();
    let state_text = electricity::out::render_state(&state, false);

    let root = dir.to_str().unwrap();
    let state_json: Json = serde_json::from_str(&state_text).unwrap();
    let state_json = normalize(None, &state_json, root);
    strip_invocation_shape_fields(&state_json)
}

fn expected_state(case: &CorpusCase) -> Json {
    let state = case
        .result
        .state
        .clone()
        .unwrap_or_else(|| panic!("{}: corpus case has no state", case.name));
    strip_invocation_shape_fields(&state)
}

#[tokio::test]
async fn electricity_matches_the_golden_run_seeding_corpus() {
    let cases = load_corpus_at(GOLDEN);
    assert!(!cases.is_empty(), "expected at least one seeding case");
    for case in &cases {
        let actual = run_case(case).await;
        assert_eq!(
            actual,
            expected_state(case),
            "{}: state diverges from the golden corpus",
            case.name
        );
    }
}

#[test]
fn every_golden_seeding_case_actually_succeeded() {
    let cases = load_corpus_at(GOLDEN);
    for case in &cases {
        assert_eq!(
            case.result.returncode, 0,
            "{}: golden case has a nonzero returncode",
            case.name
        );
    }
}
