//! `claude_code`/`pi` end to end (issue #520 part B) -- a real document,
//! through `electricity::run_orchestration`'s own public API, against a
//! fake `claude`/`pi` binary this test writes itself (never a real CLI --
//! the issue's own settled question 15). The conformance suite
//! (`tests/conformance/cases/c520-*`) pins the byte-for-byte text against
//! `cof run`; this file only exercises the Rust wiring (registry,
//! preflight, the real `run_tracked` process, cost totals) through the
//! same public entry point every other adapter's own run-wiring test
//! uses (`run_wiring.rs`).

use electricity::run::RunRequest;
use electricity_vm::CancellationToken;
use indexmap::IndexMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "electricity-agent-cli-adapters-test-{name}-{}",
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

fn write_executable(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = write(dir, name, script);
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&path, perms).unwrap();
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
        profile_path: None,
        armed: true,
    }
}

async fn run(req: &RunRequest) -> electricity::RunResult {
    let token = CancellationToken::new();
    electricity::run_orchestration(req, &token).await
}

fn get<'a>(
    dict: &'a indexmap::IndexMap<electricity_value::Value, electricity_value::Value>,
    key: &str,
) -> &'a electricity_value::Value {
    dict.get(&electricity_value::Value::Str(key.to_string()))
        .unwrap_or_else(|| panic!("missing key {key:?} in {dict:?}"))
}

fn as_dict(
    value: &electricity_value::Value,
) -> &indexmap::IndexMap<electricity_value::Value, electricity_value::Value> {
    match value {
        electricity_value::Value::Dict(d) => d,
        other => panic!("expected a dict, got {other:?}"),
    }
}

/// A fake `claude`: always reads (and discards) stdin, then writes
/// Claude Code's own `--output-format json` result shape, with the
/// cost selected by `--model` (`m1`/`m2`) so a two-prompt chain can
/// check `runtime.last_run.totals.cost_usd` sums both calls.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
cat >/dev/null
model=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--model" ]; then
    model="$2"
  fi
  shift
done
case "$model" in
  m1) cost=0.5 ;;
  m2) cost=0.25 ;;
  *) cost=0.5 ;;
esac
printf '{"result": "claude says hi", "is_error": false, "usage": {"input_tokens": 3, "output_tokens": 5}, "total_cost_usd": %s}\n' "$cost"
"#;

#[tokio::test]
async fn claude_code_runs_end_to_end_through_a_fake_binary() {
    let dir = temp_dir("claude-code");
    let binary = write_executable(&dir, "claude", FAKE_CLAUDE);
    let config = write(
        &dir,
        "config.json",
        &format!(
            r#"{{"runtime": {{"adapters": {{"claude_code": {{"binary": "{}"}}}}}}}}"#,
            binary.display()
        ),
    );
    let doc = write(
        &dir,
        "doc.yml",
        "adapter: claude_code\nmodel: m1\neffects:\n  - type: prompt\n    name: greeting\n    template: \"say hello\"\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(result.ok, "expected success, got: {:?}", result.error);
    let state = result.state.unwrap();
    let prime = as_dict(get(as_dict(&state), "prime"));
    let greeting = as_dict(get(prime, "greeting"));
    assert_eq!(
        get(greeting, "value"),
        &electricity_value::Value::Str("claude says hi".to_string())
    );
    let meta = as_dict(get(greeting, "meta"));
    assert_eq!(get(meta, "cost_usd"), &electricity_value::Value::from(0.5));
    assert_eq!(
        get(meta, "tokens_sent"),
        &electricity_value::Value::from(3i64)
    );
    assert_eq!(
        get(meta, "tokens_received"),
        &electricity_value::Value::from(5i64)
    );

    let runtime = as_dict(get(as_dict(&state), "runtime"));
    let last_run = as_dict(get(runtime, "last_run"));
    let totals = as_dict(get(last_run, "totals"));
    assert_eq!(
        get(totals, "cost_usd"),
        &electricity_value::Value::from(0.5)
    );
}

#[tokio::test]
async fn claude_code_sums_cost_across_a_chain_of_two_prompts() {
    let dir = temp_dir("claude-code-chain");
    let binary = write_executable(&dir, "claude", FAKE_CLAUDE);
    let config = write(
        &dir,
        "config.json",
        &format!(
            r#"{{"runtime": {{"adapters": {{"claude_code": {{"binary": "{}"}}}}}}}}"#,
            binary.display()
        ),
    );
    let doc = write(
        &dir,
        "doc.yml",
        "adapter: claude_code\neffects:\n  \
         - type: prompt\n    name: first\n    model: m1\n    template: \"one\"\n  \
         - type: prompt\n    name: second\n    model: m2\n    template: \"two\"\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(result.ok, "expected success, got: {:?}", result.error);
    let state = result.state.unwrap();
    let runtime = as_dict(get(as_dict(&state), "runtime"));
    let last_run = as_dict(get(runtime, "last_run"));
    let totals = as_dict(get(last_run, "totals"));
    assert_eq!(
        get(totals, "cost_usd"),
        &electricity_value::Value::from(0.75)
    );
}

/// A fake `pi`: ignores argv entirely and prints one assistant
/// `message_end` event (pi's own `--mode json` event stream), stdin
/// unread -- pi never reads a prompt from stdin (the attached `@file`
/// carries it).
const FAKE_PI: &str = r#"#!/bin/sh
printf '{"type": "message_end", "message": {"role": "assistant", "content": "pi says hi", "stopReason": "end_turn", "usage": {"input": 4, "output": 6, "cost": 0.25}}}\n'
"#;

#[tokio::test]
async fn pi_runs_end_to_end_through_a_fake_binary() {
    let dir = temp_dir("pi");
    let binary = write_executable(&dir, "pi", FAKE_PI);
    let config = write(
        &dir,
        "config.json",
        &format!(
            r#"{{"runtime": {{"adapters": {{"pi": {{"binary": "{}"}}}}}}}}"#,
            binary.display()
        ),
    );
    let doc = write(
        &dir,
        "doc.yml",
        "adapter: pi\neffects:\n  - type: prompt\n    name: greeting\n    template: \"say hello\"\n",
    );
    let req = request(config, doc);

    let result = run(&req).await;

    assert!(result.ok, "expected success, got: {:?}", result.error);
    let state = result.state.unwrap();
    let prime = as_dict(get(as_dict(&state), "prime"));
    let greeting = as_dict(get(prime, "greeting"));
    assert_eq!(
        get(greeting, "value"),
        &electricity_value::Value::Str("pi says hi".to_string())
    );
    let meta = as_dict(get(greeting, "meta"));
    assert_eq!(get(meta, "cost_usd"), &electricity_value::Value::from(0.25));
    assert_eq!(
        get(meta, "finish_reason"),
        &electricity_value::Value::Str("end_turn".to_string())
    );
}
