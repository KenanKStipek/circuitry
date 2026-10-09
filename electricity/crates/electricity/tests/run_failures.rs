//! Exercises the golden `--out`/stdout corpus `electricity/scripts/
//! generate_vm_run_failures.py` writes (PR #441 review finding 9):
//! every one of `electricity::run_orchestration`'s own pre-execution
//! failure paths (load, effective-settings shape, complexity,
//! concurrency, persistence validation, a missing required input,
//! structural, compile, an unknown concurrency group, two of PR #441
//! review finding 1's own -e-seeding corner cases), each compared
//! against a real `cof run`'s own normalized `--out` state and full
//! stdout bytes. Every case's own document (and config/`-e` inputs,
//! for the two that have one) lives in the golden file itself
//! (`CorpusCase::orchestration`/`config`/`inputs`) -- not copy-pasted
//! into this file too, the one place either could ever drift from
//! what `cof` actually ran.

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
    "/tests/golden/run_failures.json"
);

static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "electricity-run-failures-compare-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Ran {
    state: Json,
    /// The full stdout payload (`{"ok": false, "error": ..., "warnings":
    /// [...], "state_out": ...}`), with *this test's own* temporary
    /// directory replaced by `<root>` -- the same placeholder the
    /// golden corpus's own normalizer used, so `state_out`'s own path
    /// compares byte for byte too (both sides write to a file named
    /// `expected.out.json`, directly alongside the document).
    stdout: Json,
}

/// Runs *case*'s own document (and config/`-e` inputs, if it has one)
/// through `electricity::run_orchestration` directly, writing `--out`
/// and building the stdout failure payload exactly as `electricity-cli`'s
/// own `run_action` does, so this test can compare both against a real
/// `cof run`'s own normalized output.
async fn run_case(case: &CorpusCase) -> Ran {
    let dir = temp_dir(&case.name);
    let orchestration = case
        .orchestration
        .as_deref()
        .unwrap_or_else(|| panic!("{}: corpus case has no 'orchestration' text", case.name));
    let doc_path = dir.join("orchestration.yml");
    fs::write(&doc_path, orchestration).unwrap();

    let config_path = dir.join("config.json");
    let config_text = match &case.config {
        Some(config) => serde_json::to_string(config).unwrap(),
        None => "{}".to_string(),
    };
    fs::write(&config_path, config_text).unwrap();

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
    let state = result
        .state
        .expect("every case in this corpus still writes state");
    electricity::out::write_out(&out_path, &state, false).unwrap();
    let state_text = electricity::out::render_state(&state, false);

    let root = dir.to_str().unwrap();
    let state_json: Json = serde_json::from_str(&state_text).unwrap();
    let state_json = normalize(None, &state_json, root);
    let state_json = strip_invocation_shape_fields(&state_json);

    let stdout_text = electricity::out::failure_payload(
        result.error.as_deref().unwrap_or(""),
        &result.warnings,
        Some(&out_path),
    );
    let stdout_json: Json = serde_json::from_str(&stdout_text).unwrap();
    let mut stdout_json = normalize(None, &stdout_json, root);
    // `run_orchestration`'s own error text embeds `RunRequest::
    // orchestration_path` exactly as given, which this test always
    // builds as an absolute path (changing the process's own `cwd` to
    // get a relative one instead, the way `cof run orchestration.yml`
    // itself does, would race every other test in this same binary);
    // `cof`'s own text embeds the bare relative name it was actually
    // invoked with. `normalize`'s own root substitution above already
    // turned the absolute prefix into `<root>/`; strip that one
    // remaining `<root>/` the golden has no placeholder for at all,
    // leaving every other occurrence (there is none in this corpus)
    // alone.
    if let Some(error) = stdout_json.get("error").and_then(Json::as_str) {
        let stripped = error.replace("<root>/", "");
        stdout_json["error"] = Json::String(stripped);
    }

    Ran {
        state: state_json,
        stdout: stdout_json,
    }
}

fn corpus_case<'a>(cases: &'a [CorpusCase], name: &str) -> &'a CorpusCase {
    cases
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}: no such case"))
}

fn expected_stdout(case: &CorpusCase) -> Json {
    serde_json::from_str(&case.result.stdout)
        .unwrap_or_else(|err| panic!("{}: stdout isn't JSON: {err}", case.name))
}

fn expected_state(case: &CorpusCase) -> Json {
    let state = case
        .result
        .state
        .clone()
        .unwrap_or_else(|| panic!("{}: corpus case has no state", case.name));
    strip_invocation_shape_fields(&state)
}

/// Normalizes ASCII double quotes to single quotes -- the one
/// difference between the Rust `jsonschema` crate's own missing-
/// property message and Python `jsonschema`'s (DESIGN.md §1/§12:
/// third-party text only has to fail at the same place, never match
/// word for word) -- so [`ErrorCompare::LocationPrefix`] can still
/// assert full equality rather than merely a shared prefix (PR #441
/// review finding 7).
fn normalize_quotes(s: &str) -> String {
    s.replace('"', "'")
}

/// Whether a case's own error text is Circuitry's own (compared byte
/// for byte) or a third-party library's (compared after normalizing
/// quote style, and asserting there is exactly one error line --
/// `structural_error` here, DESIGN.md §1/§12).
enum ErrorCompare {
    Exact,
    LocationPrefix(&'static str),
}

#[tokio::test]
async fn electricity_matches_the_golden_run_failures_corpus() {
    let cases = load_corpus_at(GOLDEN);
    for (name, compare) in [
        ("load_error", ErrorCompare::Exact),
        ("effective_settings_shape_error", ErrorCompare::Exact),
        ("complexity_error", ErrorCompare::Exact),
        ("concurrency_error", ErrorCompare::Exact),
        ("persistence_validation_error", ErrorCompare::Exact),
        ("missing_required_input", ErrorCompare::Exact),
        (
            "structural_error",
            ErrorCompare::LocationPrefix("Orchestration validation failed:\n  - top level: "),
        ),
        ("duplicate_effect_name", ErrorCompare::Exact),
        ("unknown_group", ErrorCompare::Exact),
        ("best_effort_stops_before_step_10", ErrorCompare::Exact),
        (
            "allowlist_failure_restores_raw_text_input",
            ErrorCompare::Exact,
        ),
    ] {
        let case = corpus_case(&cases, name);
        let ran = run_case(case).await;
        let mut expected_stdout = expected_stdout(case);
        let mut actual_stdout = ran.stdout.clone();
        let expected_error = expected_stdout["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: golden stdout has no string 'error'"))
            .to_string();
        let actual_error = actual_stdout["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: actual stdout has no string 'error'"))
            .to_string();
        match compare {
            ErrorCompare::Exact => assert_eq!(
                actual_error, expected_error,
                "{name}: error text diverges from the golden corpus"
            ),
            ErrorCompare::LocationPrefix(prefix) => {
                assert!(
                    actual_error.starts_with(prefix) && expected_error.starts_with(prefix),
                    "{name}: expected both to start with {prefix:?}\n  actual:   {actual_error:?}\n  expected: {expected_error:?}"
                );
                assert_eq!(
                    actual_error.lines().count(),
                    2,
                    "{name}: expected exactly one error line after the prefix, got {actual_error:?}"
                );
                assert_eq!(
                    expected_error.lines().count(),
                    2,
                    "{name}: expected exactly one golden error line after the prefix, got {expected_error:?}"
                );
                assert_eq!(
                    normalize_quotes(&actual_error),
                    normalize_quotes(&expected_error),
                    "{name}: error text diverges beyond quote style\n  actual:   {actual_error:?}\n  expected: {expected_error:?}"
                );
                // Quote style is the one difference this case tolerates
                // -- blank both sides' own `error` field the same way
                // before comparing the rest of the stdout payload byte
                // for byte below.
                expected_stdout["error"] = Json::String("<ERROR>".to_string());
                actual_stdout["error"] = Json::String("<ERROR>".to_string());
            }
        }
        assert_eq!(
            actual_stdout, expected_stdout,
            "{name}: stdout diverges from the golden corpus"
        );
        assert_eq!(
            ran.state,
            expected_state(case),
            "{name}: state diverges from the golden corpus"
        );
    }
}

#[test]
fn every_golden_failure_case_actually_failed() {
    let cases = load_corpus_at(GOLDEN);
    for case in &cases {
        assert_ne!(
            case.result.returncode, 0,
            "{}: golden case has a zero returncode",
            case.name
        );
    }
}
