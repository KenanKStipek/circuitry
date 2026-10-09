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
//! implicit `prime` container), so comparing a case's own `--out`
//! against `electricity::run_orchestration`'s eventual VM output needs
//! lane B2's real `exec::dynamic` chain executor *and* lane D's real
//! `run_orchestration` -- neither exists yet in this crate, so the
//! comparison tests below are `#[ignore = "needs lanes B2-D"]`,
//! mirroring `run_corpus.rs`'s own existing ones for lane A's smoke
//! corpus. This lane (C) only shape-checks the corpus end to end today.

mod support;

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

#[test]
#[ignore = "needs lanes B2-D"]
fn electricity_run_orchestration_matches_the_corpus_state() {
    unreachable!(
        "no VM wiring to run a document-level case against yet \
         (issue #431, lanes B2/D -- execute_tool itself is already \
         covered by tests/tool_execute_corpus.rs's own function-level \
         corpus)"
    )
}
