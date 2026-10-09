//! Exercises the golden `--out`/stdout corpus `electricity/scripts/
//! generate_vm_run_failures.py` writes (PR #441 review finding 9):
//! every one of `electricity::run_orchestration`'s own pre-execution
//! failure paths (load, effective-settings shape, complexity,
//! concurrency, persistence validation, a missing required input,
//! structural, compile, an unknown concurrency group), each compared
//! against a real `cof run`'s own normalized `--out` state and stdout.

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

// ---------------------------------------------------------------------
// Every case's own document, copied verbatim from
// `electricity/scripts/generate_vm_run_failures.py` -- the golden
// corpus itself doesn't carry the document text, only `cof run`'s own
// result.
// ---------------------------------------------------------------------

const LOAD_ERROR_DOC: &str = "effects: []\neffects: []\n";

const EFFECTIVE_SETTINGS_SHAPE_ERROR_DOC: &str = "runtime: \"not an object\"\neffects: []\n";

const COMPLEXITY_ERROR_DOC: &str = "\
runtime:
  complexity:
    routing:
      enabled: true
effects: []
";

const CONCURRENCY_ERROR_DOC: &str = "\
runtime:
  max_concurrency: -1
effects: []
";

const PERSISTENCE_VALIDATION_ERROR_DOC: &str = "\
runtime:
  persistence:
    enabled: true
    backend: sqlite
effects: []
";

const MISSING_REQUIRED_INPUT_DOC: &str = "\
interface:
  inputs:
    name:
      type: string
      required: true
effects: []
";

const STRUCTURAL_ERROR_DOC: &str = "{}\n";

const DUPLICATE_EFFECT_NAME_DOC: &str = "\
effects:
  - name: dup
    type: tool
    provider: json
    params: {mode: stringify, input: {}}
  - name: dup
    type: tool
    provider: json
    params: {mode: stringify, input: {}}
";

const UNKNOWN_GROUP_DOC: &str = "\
effects:
  - name: a
    type: tool
    provider: json
    group: no-such-group
    params: {mode: stringify, input: {}}
";

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
    /// With *this test's own* temporary directory (plus the trailing
    /// path separator) stripped from the front of any occurrence --
    /// `run_orchestration`'s own error text embeds `RunRequest::
    /// orchestration_path` exactly as given, which this test always
    /// builds as an absolute path (changing the process's own `cwd` to
    /// get a relative one instead, the way `cof run orchestration.yml`
    /// itself does, would race every other test in this same binary);
    /// `cof`'s own text embeds the bare relative name it was actually
    /// invoked with, so this is the one piece of `--out`-shaped
    /// invocation-specific text [`support::normalize`]'s own root
    /// replacement can't reach (that one substitutes `<root>` back in;
    /// the golden has no such placeholder to match here at all, since
    /// `cof`'s own text never contained an absolute path to begin
    /// with).
    error: Option<String>,
}

async fn run_case(name: &str, orchestration: &str) -> Ran {
    let dir = temp_dir(name);
    let config_path = dir.join("config.json");
    fs::write(&config_path, "{}").unwrap();
    let doc_path = dir.join("orchestration.yml");
    fs::write(&doc_path, orchestration).unwrap();

    let req = RunRequest {
        config_path,
        orchestration_path: doc_path,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: None,
    };
    let token = CancellationToken::new();
    let result = electricity::run_orchestration(&req, &token).await;
    let state = result
        .state
        .expect("every case in this corpus still writes state");
    let state_text = electricity::out::render_state(&state, false);

    let root = dir.to_str().unwrap();
    let state_json: Json = serde_json::from_str(&state_text).unwrap();
    let state_json = normalize(None, &state_json, root);
    let state_json = strip_invocation_shape_fields(&state_json);

    let error = result
        .error
        .map(|text| text.replace(&format!("{root}/"), ""));

    Ran {
        state: state_json,
        error,
    }
}

fn corpus_case<'a>(cases: &'a [CorpusCase], name: &str) -> &'a CorpusCase {
    cases
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}: no such case"))
}

fn expected_error(case: &CorpusCase) -> String {
    let payload: Json = serde_json::from_str(&case.result.stdout)
        .unwrap_or_else(|err| panic!("{}: stdout isn't JSON: {err}", case.name));
    payload["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{}: stdout has no string 'error'", case.name))
        .to_string()
}

fn expected_state(case: &CorpusCase) -> Json {
    let state = case
        .result
        .state
        .clone()
        .unwrap_or_else(|| panic!("{}: corpus case has no state", case.name));
    strip_invocation_shape_fields(&state)
}

/// Whether a case's own error text is Circuitry's own (compared byte
/// for byte) or a third-party library's (compared only up to and
/// including the location prefix, DESIGN.md §1/§12 -- the Rust
/// `jsonschema` crate quotes a missing-property name in double quotes,
/// Python's own in single quotes, for `structural_error` here).
enum ErrorCompare {
    Exact,
    LocationPrefix(&'static str),
}

#[tokio::test]
async fn electricity_matches_the_golden_run_failures_corpus() {
    let cases = load_corpus_at(GOLDEN);
    for (name, doc, compare) in [
        ("load_error", LOAD_ERROR_DOC, ErrorCompare::Exact),
        (
            "effective_settings_shape_error",
            EFFECTIVE_SETTINGS_SHAPE_ERROR_DOC,
            ErrorCompare::Exact,
        ),
        (
            "complexity_error",
            COMPLEXITY_ERROR_DOC,
            ErrorCompare::Exact,
        ),
        (
            "concurrency_error",
            CONCURRENCY_ERROR_DOC,
            ErrorCompare::Exact,
        ),
        (
            "persistence_validation_error",
            PERSISTENCE_VALIDATION_ERROR_DOC,
            ErrorCompare::Exact,
        ),
        (
            "missing_required_input",
            MISSING_REQUIRED_INPUT_DOC,
            ErrorCompare::Exact,
        ),
        (
            "structural_error",
            STRUCTURAL_ERROR_DOC,
            ErrorCompare::LocationPrefix("Orchestration validation failed:\n  - top level: "),
        ),
        (
            "duplicate_effect_name",
            DUPLICATE_EFFECT_NAME_DOC,
            ErrorCompare::Exact,
        ),
        ("unknown_group", UNKNOWN_GROUP_DOC, ErrorCompare::Exact),
    ] {
        let case = corpus_case(&cases, name);
        let ran = run_case(name, doc).await;
        let expected = expected_error(case);
        match compare {
            ErrorCompare::Exact => assert_eq!(
                ran.error.as_deref(),
                Some(expected.as_str()),
                "{name}: error text diverges from the golden corpus"
            ),
            ErrorCompare::LocationPrefix(prefix) => {
                let actual = ran.error.as_deref().unwrap_or("");
                assert!(
                    actual.starts_with(prefix) && expected.starts_with(prefix),
                    "{name}: expected both to start with {prefix:?}\n  actual:   {actual:?}\n  expected: {expected:?}"
                );
            }
        }
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
