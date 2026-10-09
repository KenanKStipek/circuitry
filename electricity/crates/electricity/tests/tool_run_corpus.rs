//! Exercises the golden run corpus `electricity/scripts/generate_tool_
//! run_corpus.py` writes (issue #431's Test strategy section, lane C's
//! own tool-runtime-focused documents -- `success_chain`, a propagating
//! tool error, `on_error: skip`, retries exhausted against a CEL
//! `expect:`, a passing `expect:`, a raising `expect:` (the regression
//! case for this PR's own finding 2), an `enabled_tools` allowlist
//! refusal, a `params_json` merge, non-string param keys end to end,
//! and redaction surfacing in `meta.params_rendered`).
//!
//! Every case's own document root is a `dynamic` chain (Circuitry's own
//! implicit `prime` container). Lanes B2 and D have both landed, so
//! the comparison test below now actually runs each case's document
//! through `electricity::run_orchestration` and compares against the
//! committed golden, normalized the same way `_run_corpus.py` does
//! (`support::normalize`).

mod support;

use electricity::run::RunRequest;
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use serde_json::Value as Json;
use std::fs;
use std::path::PathBuf;
use support::normalize::{normalize, strip_expect_error_detail, strip_invocation_shape_fields};

use support::run_corpus::load_corpus_at;

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/tool_run_corpus.json"
);

#[test]
fn the_corpus_has_every_named_branch() {
    let cases = load_corpus_at(GOLDEN);
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for expected in [
        "success_chain",
        "tool_error_fail",
        "tool_error_skip",
        "retries_expect_exhausts",
        "retries_expect_passes",
        "cel_expect_raises",
        "allowlist_refusal",
        "params_json_merge",
        "non_string_param_keys",
        "redaction_in_params_rendered",
    ] {
        assert!(
            names.contains(&expected),
            "expected a case named {expected:?}, got {names:?}"
        );
    }
}

#[test]
fn every_case_name_is_unique() {
    let cases = load_corpus_at(GOLDEN);
    let mut names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before);
}

#[test]
fn a_propagating_tool_error_has_a_nonzero_returncode() {
    let cases = load_corpus_at(GOLDEN);
    let case = cases
        .iter()
        .find(|case| case.name == "tool_error_fail")
        .expect("tool_error_fail case");
    assert_ne!(case.result.returncode, 0);
}

#[test]
fn an_allowlist_refusal_has_a_nonzero_returncode_and_writes_no_prime_state() {
    let cases = load_corpus_at(GOLDEN);
    let case = cases
        .iter()
        .find(|case| case.name == "allowlist_refusal")
        .expect("allowlist_refusal case");
    assert_ne!(case.result.returncode, 0);
    let state = case
        .result
        .state
        .as_ref()
        .expect("a state is still written");
    assert!(
        state.get("prime").is_none(),
        "an allowlist denial fires no effect_start, so `prime` should carry no partial state"
    );
}

#[test]
fn a_raising_cel_expect_fails_with_expect_failed_text() {
    // This is this PR's own finding 2 regression, captured from the
    // real CLI rather than just `ToolRuntime.execute` directly: a CEL
    // `expect:` that raises (`value.missing` on an empty dict) must
    // still fail with `expect failed: <expr>`, not the CEL evaluator's
    // own error text.
    let cases = load_corpus_at(GOLDEN);
    let case = cases
        .iter()
        .find(|case| case.name == "cel_expect_raises")
        .expect("cel_expect_raises case");
    let state = case.result.state.as_ref().expect("a state is written");
    let error = state["prime"]["raising_expect"]["meta"]["error"]
        .as_str()
        .expect("meta.error is a string");
    assert_eq!(error, "expect failed: value.missing == 1");
}

#[test]
fn redaction_reaches_params_rendered_but_not_the_tools_own_output() {
    let cases = load_corpus_at(GOLDEN);
    let case = cases
        .iter()
        .find(|case| case.name == "redaction_in_params_rendered")
        .expect("redaction_in_params_rendered case");
    let state = case.result.state.as_ref().expect("a state is written");
    let node = &state["prime"]["has_a_secret"];
    assert_eq!(
        node["meta"]["params_rendered"]["input"]["api_key"],
        "***REDACTED***"
    );
    // The plugin itself still receives the unredacted value (core/
    // tool.py's own docstring, #238) -- `json`'s own `stringify` output
    // proves that here.
    assert!(node["value"].as_str().unwrap().contains("super-secret"));
}

// ---------------------------------------------------------------------
// Every case's own document and config, copied verbatim from
// `electricity/scripts/generate_tool_run_corpus.py` -- the golden
// corpus itself doesn't carry the document text, only `cof run`'s own
// result.
// ---------------------------------------------------------------------

const SUCCESS_CHAIN_DOC: &str = "\
effects:
  - type: tool
    name: encode
    provider: json
    params:
      mode: stringify
      input: {greeting: \"hi\"}
  - type: tool
    name: decode
    provider: json
    params:
      mode: parse
      input: \"{{{prime.encode.value}}}\"
";

const TOOL_ERROR_FAIL_DOC: &str = "\
effects:
  - type: tool
    name: broken
    provider: json
    params:
      mode: parse
      input: \"not json\"
";

const TOOL_ERROR_SKIP_DOC: &str = "\
effects:
  - type: tool
    name: broken
    on_error: skip
    provider: json
    params:
      mode: parse
      input: \"not json\"
  - type: tool
    name: after
    provider: json
    params: {mode: stringify, input: {ran: true}}
";

const RETRIES_EXPECT_EXHAUSTS_DOC: &str = "\
effects:
  - type: tool
    name: never_matches
    on_error: skip
    provider: json
    retries: {max_attempts: 3, backoff_ms: 0}
    expect: \"value == 2\"
    params:
      mode: parse
      input: \"1\"
";

const RETRIES_EXPECT_PASSES_DOC: &str = "\
effects:
  - type: tool
    name: matches_first_try
    provider: json
    expect: \"value == 2\"
    params:
      mode: parse
      input: \"2\"
";

const CEL_EXPECT_RAISES_DOC: &str = "\
effects:
  - type: tool
    name: raising_expect
    on_error: skip
    provider: json
    retries: {max_attempts: 1, backoff_ms: 0}
    expect: \"value.missing == 1\"
    params:
      mode: parse
      input: \"{}\"
";

const ALLOWLIST_REFUSAL_DOC: &str = "\
effects:
  - type: tool
    name: denied
    provider: json
    params: {mode: stringify, input: {a: 1}}
";

const PARAMS_JSON_MERGE_DOC: &str = "\
effects:
  - type: tool
    name: merged
    provider: json
    params:
      mode: stringify
      input: {a: 1}
    params_json: '{\"input\": {\"b\": 2}}'
";

const NON_STRING_PARAM_KEYS_DOC: &str = "\
effects:
  - type: tool
    name: non_string_keys
    provider: json
    params:
      mode: stringify
      input:
        1: \"one-value\"
        2: \"two-value\"
";

const REDACTION_IN_PARAMS_RENDERED_DOC: &str = "\
effects:
  - type: tool
    name: has_a_secret
    provider: json
    params:
      mode: stringify
      input: {api_key: \"super-secret\", name: \"ok\"}
";

static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "electricity-tool-run-corpus-compare-{name}-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn run_case(name: &str, orchestration: &str, config: &str) -> Json {
    let dir = temp_dir(name);
    let config_path = dir.join("config.json");
    fs::write(&config_path, config).unwrap();
    let doc_path = dir.join("orchestration.yml");
    fs::write(&doc_path, orchestration).unwrap();
    let out_path = dir.join("out.json");

    let req = RunRequest {
        config_path,
        orchestration_path: doc_path,
        inputs: IndexMap::new(),
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
    let state_text = electricity::out::render_state(&state, false);

    let root = dir.to_str().unwrap();
    let state_json: Json = serde_json::from_str(&state_text).unwrap();
    let state_json = normalize(None, &state_json, root);
    let state_json = strip_invocation_shape_fields(&state_json);
    strip_expect_error_detail(&state_json)
}

#[tokio::test]
async fn electricity_run_orchestration_matches_the_corpus_state() {
    let cases = load_corpus_at(GOLDEN);
    for (name, doc, config) in [
        ("success_chain", SUCCESS_CHAIN_DOC, "{}"),
        ("tool_error_fail", TOOL_ERROR_FAIL_DOC, "{}"),
        ("tool_error_skip", TOOL_ERROR_SKIP_DOC, "{}"),
        ("retries_expect_exhausts", RETRIES_EXPECT_EXHAUSTS_DOC, "{}"),
        ("retries_expect_passes", RETRIES_EXPECT_PASSES_DOC, "{}"),
        ("cel_expect_raises", CEL_EXPECT_RAISES_DOC, "{}"),
        (
            "allowlist_refusal",
            ALLOWLIST_REFUSAL_DOC,
            "{\"enabled_tools\": []}",
        ),
        ("params_json_merge", PARAMS_JSON_MERGE_DOC, "{}"),
        ("non_string_param_keys", NON_STRING_PARAM_KEYS_DOC, "{}"),
        (
            "redaction_in_params_rendered",
            REDACTION_IN_PARAMS_RENDERED_DOC,
            "{}",
        ),
    ] {
        let case = cases
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name}: no such case"));
        let expected = case
            .result
            .state
            .clone()
            .unwrap_or_else(|| panic!("{name}: corpus case has no state"));
        let expected = strip_invocation_shape_fields(&expected);
        let expected = strip_expect_error_detail(&expected);
        let actual = run_case(name, doc, config).await;
        assert_eq!(
            actual, expected,
            "{name}: state diverges from the golden corpus"
        );
    }
}
