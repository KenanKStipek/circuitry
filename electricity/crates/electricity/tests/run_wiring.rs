//! `electricity::run_orchestration`'s own run wiring (issue #431's
//! run-wiring steps 1-19), exercised directly against real temporary
//! documents -- most cases here fail before execution ever starts
//! (config, load, allowlist, effective-settings, pre/post-state-check,
//! refusal errors -- none of which need a real VM at all); a few now
//! exercise [`electricity_vm::execute_root`]'s own real interpreter
//! (lane B2, merged) end to end.

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
async fn a_structural_check_failure_still_seeds_input_from_e_values() {
    // PR #441 review finding 3: `-e` values reach `state["input"]`
    // at the seed step (run-wiring step 4), *before* the document is
    // even loaded -- so a structural failure (step 14, long after)
    // still carries them, exactly as `cof run`'s own `_load_state`
    // does.
    let dir = temp_dir("structural-failure-with-e");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", "{}\n");
    let mut req = request(config, doc);
    req.inputs.insert("name".to_string(), "World".to_string());
    req.inputs.insert("count".to_string(), "5".to_string());

    let result = run(&req).await;

    assert!(!result.ok);
    let state = result
        .state
        .expect("a structural failure still writes state");
    let input = state
        .as_dict()
        .unwrap()
        .get(&electricity_value::Value::Str("input".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        input.get(&electricity_value::Value::Str("name".to_string())),
        Some(&electricity_value::Value::Str("World".to_string()))
    );
    assert_eq!(
        input.get(&electricity_value::Value::Str("count".to_string())),
        Some(&electricity_value::Value::Int(5.into()))
    );
}

#[tokio::test]
async fn a_missing_required_input_keeps_an_earlier_input_keys_own_default() {
    // PR #441 review finding 3's second half: a failure *inside*
    // `check_interface_inputs` itself still mirrors Python's own
    // in-place mutation of `state["input"]` as far as it got.
    let dir = temp_dir("partial-interface-failure");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "interface:\n  inputs:\n    a:\n      type: integer\n      default: 3\n    b:\n      type: string\n      required: true\neffects: []\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("missing required input 'b' declared in orchestration interface.")
    );
    let state = result.state.expect("a step-10 failure still writes state");
    let input = state
        .as_dict()
        .unwrap()
        .get(&electricity_value::Value::Str("input".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        input.get(&electricity_value::Value::Str("a".to_string())),
        Some(&electricity_value::Value::Int(3.into()))
    );
    assert!(!input.contains_key(&electricity_value::Value::Str("b".to_string())));
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
    assert!(!result.error.unwrap().starts_with("Config file"));
    // `runtime.effective_settings`/`sources` are written at step 13,
    // strictly *before* the structural check this document fails at
    // (step 14) -- so this structural failure's own state already
    // carries the config file's `default_model`, not just proves the
    // file parsed (PR #441 review finding 9: this test's own former
    // comment, claiming the opposite order, was stale).
    let state = result
        .state
        .expect("a pre-step-14 failure still writes state");
    let dict = state.as_dict().unwrap();
    let runtime = dict
        .get(&electricity_value::Value::Str("runtime".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    let effective_settings = runtime
        .get(&electricity_value::Value::Str(
            "effective_settings".to_string(),
        ))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        effective_settings.get(&electricity_value::Value::Str("model".to_string())),
        Some(&electricity_value::Value::Str("custom-model".to_string()))
    );
    let sources = effective_settings
        .get(&electricity_value::Value::Str("sources".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        sources.get(&electricity_value::Value::Str("model".to_string())),
        Some(&electricity_value::Value::Str("config".to_string()))
    );
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

/// PR #441 review finding 4: a document that turns a (valid) `runtime.
/// persistence` block on is refused with the preview marker right
/// after that block's own validation, no state written -- a malformed
/// one still fails the ordinary way (`validate_persistence`'s own job,
/// inside `pre_state_checks`, run first).
#[tokio::test]
async fn a_configured_persistence_backend_is_refused_with_no_state_written() {
    let dir = temp_dir("persistence-refusal");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "runtime:\n  persistence:\n    enabled: true\n    backend: sqlite\n    db_path: a.db\neffects: []\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    let error = result.error.unwrap();
    assert!(error.contains("is a preview and cannot run orchestrations yet"));
    assert!(error.contains("runtime.persistence"));
}

/// PR #441 review finding 2: `enabled: 1` (or any other Python-truthy
/// non-`true` value) must refuse exactly like `enabled: true` -- a
/// literal-`true`-only check would let this run and silently ignore
/// the persistence block, where `cof run` would write snapshots.
#[tokio::test]
async fn a_persistence_block_enabled_with_a_truthy_non_boolean_is_still_refused() {
    let dir = temp_dir("persistence-truthy-enabled");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "runtime:\n  persistence:\n    enabled: 1\n    backend: jsonl-file\n    path: runs.jsonl\neffects: []\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    let error = result.error.unwrap();
    assert!(error.contains("is a preview and cannot run orchestrations yet"));
    assert!(error.contains("runtime.persistence"));
}

/// A malformed persistence block still fails the ordinary way (the
/// validation itself, not the refusal) -- `pre_state_checks` catches
/// it before the refusal check this lane added ever runs.
#[tokio::test]
async fn a_malformed_persistence_backend_fails_validation_not_the_refusal() {
    let dir = temp_dir("persistence-malformed");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "runtime:\n  persistence:\n    enabled: true\n    backend: sqlite\neffects: []\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    let error = result.error.unwrap();
    assert!(!error.contains("is a preview and cannot run orchestrations yet"));
    assert!(error.contains("requires runtime.persistence.db_path"));
    // A validation failure (unlike the refusal) still writes state --
    // it's an ordinary pre_state_checks error.
    assert!(result.state.is_some());
}

/// A document that declares a runtime plugin by name is refused the
/// same way, naming it in the refusal text.
#[tokio::test]
async fn a_declared_runtime_plugin_is_refused_with_no_state_written() {
    let dir = temp_dir("plugins-refusal");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", "plugins: [my-plugin]\neffects: []\n");
    let req = request(config, doc);

    let result = run(&req).await;
    assert!(!result.ok);
    assert!(result.state.is_none());
    let error = result.error.unwrap();
    assert!(error.contains("is a preview and cannot run orchestrations yet"));
    assert!(error.contains("my-plugin"));
}

/// Lane B2 has landed: a document that passes every check now really
/// runs, through `execute_root`'s own real interpreter -- this
/// exercises the full `--events`/`--live-state`/`--out` pipeline
/// around a genuine success (run_start, the success's own run_end, the
/// final live-state write equalling `--out`).
#[tokio::test]
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
    assert!(result.ok, "{:?}", result.error);
    assert!(result.signal.is_none());

    let expected_out_text = electricity::out::render_state(result.state.as_ref().unwrap(), false);
    let live_state_text = fs::read_to_string(&live_state_path).unwrap();
    assert_eq!(expected_out_text, live_state_text);

    let events_text = fs::read_to_string(&events_path).unwrap();
    let lines: Vec<&str> = events_text.lines().collect();
    assert!(lines.first().unwrap().contains("\"run_start\""));
    assert!(lines.last().unwrap().contains("\"run_end\""));
}

/// PR #441 review finding 17's own "missing test": a token already
/// cancelled *before* `execute_root` ever starts -- possible today
/// against the real interpreter (lane B2 has landed), unlike when this
/// finding was first written against the stub. The interrupt text,
/// `RunResult::signal`, and `--events`' own `run_end.signal` all come
/// from the same place (finding 13: keyed off `VmError::Cancelled`
/// specifically).
#[tokio::test]
async fn a_token_cancelled_before_execute_root_starts_is_an_interrupted_run() {
    let dir = temp_dir("pre-cancelled");
    let config = write(&dir, "config.json", "{}");
    // At least one real effect: an empty chain has nothing to dispatch
    // at all, so there would be no per-effect cancellation check for
    // an already-cancelled token to ever hit, and the run would
    // succeed regardless.
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: json\n    params: {mode: stringify, input: {}}\n",
    );
    let events_path = dir.join("events.jsonl");
    let req = RunRequest {
        config_path: config,
        orchestration_path: doc,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: Some(events_path.clone()),
    };

    let token = CancellationToken::new();
    assert!(token.request(2)); // SIGINT -- stable POSIX number, no libc dep needed here
    let result = electricity::run_orchestration(&req, &token).await;

    assert!(!result.ok);
    assert_eq!(result.signal, Some(Signal::Sigint));
    assert_eq!(
        result.error.as_deref(),
        Some(Signal::Sigint.interrupt_text())
    );

    let events_text = fs::read_to_string(&events_path).unwrap();
    let run_end = events_text.lines().last().unwrap();
    assert!(run_end.contains("\"run_end\""));
    assert!(run_end.contains("\"SIGINT\""));

    // PR #441 review finding 9: a cancelled run's own totals are
    // untested -- `effects_run` is 1 (not 0, even though the token was
    // already cancelled before the document's one declared tool effect
    // ever dispatched): `RunObserver::effect_complete` fires once for
    // the implicit root chain container itself (`Totals::observe_
    // complete`'s own doc comment: "every completed node, root and
    // containers included"), even an interrupted one -- `core/dynamic.
    // py`'s own interrupted-chain path fires its container's `on_
    // complete` the same way (~:603-606), so the two engines' counts
    // agree here, confirmed directly.
    use electricity_value::Value;
    let dict = result.state.unwrap();
    let dict = dict.as_dict().unwrap();
    let runtime = dict
        .get(&Value::Str("runtime".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    let last_run = runtime
        .get(&Value::Str("last_run".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    let totals = last_run
        .get(&Value::Str("totals".to_string()))
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(
        totals.get(&Value::Str("effects_run".to_string())),
        Some(&Value::from(1i64))
    );
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

/// PR #441 review finding 8: a signal already pending when a pre-
/// execution check fails (here, the same structural check [`signal_
/// is_none_for_an_uncancelled_failure`] uses) is reported as an
/// interrupted run -- the check's own real error text and exit 1, not
/// the interrupt text and the signal's own exit code -- matching
/// `cli/interrupts.py`'s own handler, which raises `KeyboardInterrupt`
/// in the main thread the instant a signal arrives, at whatever Python
/// bytecode boundary that happens to be, including mid-check; `run()`'s
/// own broad `except (Exception, KeyboardInterrupt)` never distinguishes
/// where that boundary fell.
#[tokio::test]
async fn a_signal_pending_when_a_pre_execution_check_fails_is_reported_as_interrupted() {
    let dir = temp_dir("signal-during-check");
    let config = write(&dir, "config.json", "{}");
    let doc = write(&dir, "doc.yml", "{}\n");
    let req = request(config, doc);

    let token = CancellationToken::new();
    assert!(token.request(2)); // SIGINT -- stable POSIX number
    let result = electricity::run_orchestration(&req, &token).await;

    assert!(!result.ok);
    assert_eq!(result.signal, Some(Signal::Sigint));
    assert_eq!(result.error.as_deref(), Some("Interrupted (Ctrl-C/SIGINT)"));
    assert!(result.state.is_some());
}

#[test]
fn signal_exit_codes_match_the_posix_convention() {
    assert_eq!(Signal::Sigint.exit_code(), 130);
    assert_eq!(Signal::Sigterm.exit_code(), 143);
    assert_eq!(Signal::Sighup.exit_code(), 129);
}

/// PR #441 review finding 10: without `--live-state`, `run_orchestration`
/// never needs a timer driver at all -- built on a bare `rt` Tokio
/// runtime (no `enable_time`/`enable_all`), the select-against-a-sleep-
/// tick loop this test's own document would otherwise panic inside
/// (`tokio::time::sleep` requires one) must never even be reached.
#[test]
fn a_run_with_no_live_state_never_touches_tokios_timer_driver() {
    let dir = temp_dir("no-live-state-no-timer");
    let config = write(&dir, "config.json", "{}");
    let doc = write(
        &dir,
        "doc.yml",
        "effects:\n  - name: a\n    type: tool\n    provider: json\n    params: {mode: stringify, input: {}}\n",
    );
    let req = RunRequest {
        config_path: config,
        orchestration_path: doc,
        inputs: IndexMap::new(),
        out_path: None,
        pretty: false,
        live_state_path: None,
        events_path: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a bare rt runtime with no timer driver");
    let result = runtime.block_on(async {
        let token = CancellationToken::new();
        electricity::run_orchestration(&req, &token).await
    });
    assert!(result.ok, "{:?}", result.error);
}
