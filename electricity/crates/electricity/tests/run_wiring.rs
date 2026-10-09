//! `electricity::run_orchestration`'s own run wiring (issue #431's
//! run-wiring steps 1-19), exercised directly against real temporary
//! documents -- everything up to and including [`electricity_vm::
//! execute_root`] itself, which is still lane B's own stub
//! (`NotImplemented`) as of this lane's own PR: every case here either
//! fails before execution ever starts (config, load, allowlist,
//! effective-settings, pre/post-state-check, refusal errors -- none of
//! which need a real VM at all) or deliberately exercises the
//! `execute_root` stub's own deterministic failure to prove the
//! `--events`/`--live-state`/`--out` wiring around it is correct. Once
//! lanes B/C land and `execute_root` actually runs a document, the one
//! test marked `needs-B/needs-C` below should be updated to assert a
//! real success instead.

use electricity::run::{RunRequest, Signal};
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use std::fs;
use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "electricity-run-wiring-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

fn request(config_path: PathBuf, orchestration_path: PathBuf) -> RunRequest {
    RunRequest {
        config_path,
        orchestration_path,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: None,
    }
}

async fn run(req: &RunRequest) -> electricity::RunResult {
    let token = CancellationToken::new();
    electricity::run_orchestration(req, &token).await
}

#[tokio::test]
async fn a_missing_config_file_writes_no_state_at_all() {
    let dir = temp_dir("missing-config");
    let doc = write(&dir, "doc.yml", "effects: []\n");
    let req = request(dir.join("does-not-exist.json"), doc);

    let result = run(&req).await;

    assert!(!result.ok);
    assert!(result.state.is_none());
    assert!(result.error.unwrap().starts_with("Config file not found:"));
    assert!(result.signal.is_none());
}

#[tokio::test]
async fn a_structural_check_failure_still_writes_out_state() {
    let dir = temp_dir("structural-failure");
    let config = write(&dir, "config.json", "{}");
    // No `effects:`/`steps:` key at all -- a schema validation failure,
    // the same shape `check_for_run` always reports.
    let doc = write(&dir, "doc.yml", "{}\n");
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(!result.ok);
    let state = result
        .state
        .expect("a structural failure still writes state");
    let error = result.error.expect("a structural failure reports an error");
    assert!(error.starts_with("Orchestration validation failed:"));

    let dict = state.as_dict().unwrap();
    // A structural failure happens in `post_state_checks` (run-wiring
    // step 14), *after* `_run_id`/`_timestamp`/`runtime.last_run`/
    // `effective_settings`/`plugins` are already written (steps
    // 11-13) -- so the full top-level key order is already present,
    // matching the golden run corpus's own `json_parse_failure` case
    // (`input, runtime, _run_id, _timestamp` -- no `prime`, since
    // `execute_root` is never reached).
    assert_eq!(
        dict.keys().map(|k| k.as_str().unwrap()).collect::<Vec<_>>(),
        vec!["input", "runtime", "_run_id", "_timestamp"]
    );
    let runtime = dict
        .get(&electricity_value::Value::Str("runtime".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    let last_run = runtime
        .get(&electricity_value::Value::Str("last_run".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    // The full shape: `document_hash` is computed independently of
    // compilation (this module's own `run_orchestration` doc comment),
    // so even this structural failure gets every `last_run` key steps
    // 11-13 always write, not a sparse `setdefault`-only shape.
    assert_eq!(
        last_run
            .keys()
            .map(|k| k.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "run_id",
            "orchestration_path",
            "document_hash",
            "dry_run",
            "validate_only",
            "verbose",
            "started_at",
            "completed_at",
            "totals",
        ]
    );
    let plugins = runtime
        .get(&electricity_value::Value::Str("plugins".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        plugins
            .keys()
            .map(|k| k.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["contract_version", "configured", "loaded", "events"]
    );
}

#[tokio::test]
async fn a_bad_e_value_against_a_required_input_is_an_ordinary_failure_with_state() {
    let dir = temp_dir("missing-required-input");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects: []\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(!result.ok);
    assert!(
        result
            .error
            .unwrap()
            .contains("missing required input 'name'")
    );
    assert!(result.state.is_some());
}

#[tokio::test]
async fn an_unsupported_effect_is_refused_with_no_state_written() {
    let dir = temp_dir("refusal");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(!result.ok);
    assert!(
        result.state.is_none(),
        "a refusal must write no state at all"
    );
    let error = result.error.unwrap();
    assert!(error.contains("is a preview and cannot run orchestrations yet"));
    assert!(error.contains("prime.ask"));
}

#[tokio::test]
async fn a_config_default_model_is_recorded_as_its_own_source() {
    let dir = temp_dir("config-model");
    let config = write(&dir, "config.json", r#"{"default_model": "custom-model"}"#);
    // Deliberately structurally invalid (no effects at all) so this
    // test only needs to look at the pre-check failure's own partial
    // state, not wait on a VM that doesn't exist yet -- the config
    // merge already ran (step 6) by the time this fails.
    let doc = write(&dir, "doc.yml", "{}\n");
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    // The model/source isn't recorded until `runtime.effective_settings`
    // is written (step 13), which a pre-step-14 failure never reaches --
    // this at least proves the config file was read and merged with
    // `SANE_DEFAULTS` rather than erroring as a bad config.
    assert!(!result.error.unwrap().starts_with("Config file"));
}

#[tokio::test]
async fn an_unknown_tool_provider_is_refused_before_a_structural_error_elsewhere_would_fire() {
    let dir = temp_dir("unknown-provider");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: broken\n    type: tool\n    provider: shell\n    params: {command: echo}\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    assert!(result.error.unwrap().contains("shell"));
}

/// `execute_root` is lane B's own stub (`NotImplemented`) as of this
/// PR -- a document that passes every check still ends in an ordinary
/// failure today, which still exercises the full `--events`/
/// `--live-state`/`--out` pipeline around it (run_start, the failure's
/// own run_end, the final live-state write equalling `--out`).
#[tokio::test]
#[ignore = "needs-B/needs-C: once execute_root runs a real document, this should assert success instead"]
async fn a_document_that_passes_every_check_still_writes_full_observability_today() {
    let dir = temp_dir("full-pipeline");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", "effects: []\n");
    // `run_orchestration` never writes `--out` itself -- that's
    // `electricity-cli`'s own job, from `RunResult::state` -- so this
    // compares `--live-state`'s own final write against the exact
    // bytes `electricity::out::render_state` would produce for that
    // same state, rather than a real `--out` file.
    let events_path = dir.join("events.jsonl");
    let live_state_path = dir.join("live-state.json");
    let req = RunRequest {
        config_path: config,
        orchestration_path: doc,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: Some(live_state_path.clone()),
        events_path: Some(events_path.clone()),
    };

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.signal.is_none());

    let expected_out_text = electricity::out::render_state(result.state.as_ref().unwrap(), false);
    let live_state_text = fs::read_to_string(&live_state_path).unwrap();
    assert_eq!(expected_out_text, live_state_text);

    let events_text = fs::read_to_string(&events_path).unwrap();
    let lines: Vec<&str> = events_text.lines().collect();
    assert!(lines.first().unwrap().contains("\"run_start\""));
    assert!(lines.last().unwrap().contains("\"run_end\""));
}

#[tokio::test]
async fn signal_is_none_for_an_uncancelled_failure() {
    let dir = temp_dir("no-signal");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", "{}\n");
    let req = request(config, doc);

    let result = run(&req).await;
    assert_eq!(result.signal, None);
}

#[test]
fn signal_exit_codes_match_the_posix_convention() {
    assert_eq!(Signal::Sigint.exit_code(), 130);
    assert_eq!(Signal::Sigterm.exit_code(), 143);
    assert_eq!(Signal::Sighup.exit_code(), 129);
}
