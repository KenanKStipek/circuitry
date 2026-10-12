//! Integration tests for the `electricity` binary. Every invocation runs
//! with a temporary `HOME` and no credential environment variables, per the
//! repository's rule for subprocess tests (never touch the real `HOME`).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

/// A document that fails at run time regardless of whether
/// `electricity_vm::execute_root` is still lane B's own stub or the
/// real interpreter (issue #431's golden run corpus's own
/// `json_parse_failure` case: `prime.broken: JsonPlugin: parse mode
/// requires params['input'] as a string.`) -- unlike `effects: []`
/// (which the stub also fails today, but will *succeed* once lane B2
/// lands), every test below that only needs an ordinary ``--out``-
/// writing failure, not any particular error text, uses this instead
/// (PR #441 review finding 10).
const FAILURE_DOC: &str = "\
effects:\n  - name: broken\n    type: tool\n    provider: json\n    params: {mode: parse, input: 123}\n";

/// A temporary `HOME` directory, removed when the test ends.
struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path =
            env::temp_dir().join(format!("electricity-cli-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp HOME");
        Self { path }
    }

    /// `config.json` under this home, written with *contents* (`"{}"`
    /// for every test that doesn't care what's in it).
    fn config(&self, contents: &str) -> PathBuf {
        let path = self.path.join("config.json");
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn command(tag: &str) -> (Command, TempHome) {
    let home = TempHome::new(tag);
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    (cmd, home)
}

#[test]
fn version_prints_exact_preview_string_and_exits_zero() {
    let (mut cmd, _home) = command("version");
    let output = cmd.arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        format!("electricity {} (preview)", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_usage_and_preview_notice_and_exits_zero() {
    let (mut cmd, _home) = command("help");
    let output = cmd.arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: electricity"));
    assert!(stdout.contains("preview"));
}

/// `electricity <config.json> <doc>` runs the document check first
/// (issue #431's run-wiring table). An empty document is one of the
/// few checks fully resolvable without a real VM, so its exact text
/// -- not just a marker naming an unimplemented lane -- is a stable
/// thing to assert here. An ordinary run failure's own error lives in
/// the stdout JSON payload, never on stderr (PR #441 review finding 4,
/// K1: `cli/app.py::run`'s own non-TTY failure path prints no `Error:`
/// line on stderr at all, only `Warning:` ones).
#[test]
fn run_request_fails_with_the_checks_own_error_text_and_exit_code_one() {
    let (mut cmd, home) = command("run");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "").unwrap();
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = payload["error"].as_str().unwrap();
    // The "required property" text past the location is the Rust
    // `jsonschema` crate's own (third-party) wording, not required to
    // match Circuitry's Python `jsonschema` text word for word
    // (DESIGN.md §1/§12) -- only the "Orchestration validation failed:"
    // wrapper and the location are Circuitry's own.
    assert!(error.starts_with("Orchestration validation failed:\n  - top level: "));
    assert!(error.contains("required property"), "{error}");
}

#[test]
fn a_missing_config_file_fails_with_circuitrys_own_text_and_writes_no_out() {
    let (mut cmd, home) = command("missing-config");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let out = home.path.join("out.json");
    let output = cmd
        .args([
            "no-such-config.json",
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Error: Config file not found:"),
        "{stderr}"
    );
    assert!(!out.exists());
}

/// A missing orchestration file prints `cof`'s own "Orchestration not
/// found" text on *stdout* (not stderr, and not the ordinary `{"ok":
/// false, ...}` JSON payload either -- `cof` resolves the document in
/// the CLI layer, before `run()`'s own JSON-output logic is ever
/// reached, same as a config error -- PR #441 review finding 5).
#[test]
fn a_missing_orchestration_file_fails_with_cofs_own_text_and_writes_no_out() {
    let (mut cmd, home) = command("missing-orchestration");
    let config = home.config("{}");
    let out = home.path.join("out.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            "no-such-doc.yml",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Error: Orchestration not found: no-such-doc.yml\n");
    assert!(!out.exists());
}

/// The missing-document check runs before `-e` is ever parsed (PR #441
/// review finding 5): a malformed `-e` alongside a missing document
/// still exits 1 (the missing-document text), not 2 (the `-e` usage
/// error) -- matching `cof`'s own order (`_resolve_orchestration`
/// before `_parse_env_vars`).
#[test]
fn a_missing_orchestration_file_wins_over_a_malformed_e_value() {
    let (mut cmd, home) = command("missing-orchestration-bad-e");
    let config = home.config("{}");
    let output = cmd
        .args([config.to_str().unwrap(), "no-such-doc.yml", "-e", "badtext"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Error: Orchestration not found: no-such-doc.yml\n");
}

/// A config-resolution warning (`electricity-config`'s own "Unknown
/// environment" `log::warn!`, issue #442) fires at `cof run`'s own
/// point in its resolution order -- config first, before the missing-
/// orchestration check -- so it must still print even though the
/// orchestration-not-found error then ends the run before
/// `run_orchestration` is ever reached (PR #452 review finding 2: the
/// warning must not be lost just because this invocation never runs
/// anything).
#[test]
fn a_config_warning_still_prints_when_the_orchestration_is_missing() {
    let (mut cmd, home) = command("missing-orchestration-config-warning");
    let config = home.config("{\"environment\": \"staging\"}");
    let output = cmd
        .args([config.to_str().unwrap(), "no-such-doc.yml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr,
        "WARNING: Unknown environment 'staging'; falling back to 'dev'. Valid: ('dev', 'prod', 'test')\n"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Error: Orchestration not found: no-such-doc.yml\n");
}

#[test]
fn known_run_flag_in_first_position_is_a_run_request() {
    let (mut cmd, home) = command("run-flag-first");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args([
            "--profile",
            "p",
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    // `--profile` is now a real path (issue #495, lane R): `p` doesn't
    // exist relative to this process's own cwd, so the run fails with
    // electricity's own not-found text (DESIGN §6.11) -- exit 1, the
    // ordinary run-failure JSON payload on stdout, not stderr (PR #441
    // review finding 4, K1).
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("Profile 'p' not found")
    );
}

#[test]
fn no_arguments_is_a_usage_error_with_exit_code_two() {
    let (mut cmd, _home) = command("usage-error");
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: electricity"));
}

#[test]
fn unknown_flag_is_a_usage_error_with_exit_code_two() {
    let (mut cmd, _home) = command("bogus-flag");
    let output = cmd.arg("--bogus").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// `--dump-ir` wiring: `effects: []` has no structural, concurrency-
/// configuration, compile, group, or cycle error, so the document check
/// succeeds and `--dump-ir` prints the resulting `Program` as the
/// documented `{"ir_version": "unstable", "program": ...}` wrapper.
/// `--dump-ir` never runs anything, so it has no VM/refusal of its own
/// either.
#[test]
fn dump_ir_prints_the_program_json_and_exits_zero() {
    let (mut cmd, home) = command("dump-ir");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args(["config.json", doc.to_str().unwrap(), "--dump-ir"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON on stdout");
    assert_eq!(value["ir_version"], "unstable");
    assert!(value["program"].is_object());
}

#[test]
fn dump_ir_flag_before_positionals_is_also_recognized() {
    let (mut cmd, home) = command("dump-ir-first");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args(["--dump-ir", "config.json", doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn help_mentions_dump_ir_is_unstable() {
    let (mut cmd, _home) = command("help-dump-ir");
    let output = cmd.arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--dump-ir"));
    assert!(stdout.contains("Unstable"));
}

/// A document with a required input: `--dump-ir -e` reaches the IR
/// (issue #429 -- `--dump-ir` is otherwise unusable on any document
/// that declares one).
#[test]
fn dump_ir_with_e_on_a_required_input_exits_zero_with_the_program_json() {
    let (mut cmd, home) = command("dump-ir-e");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects: []\n",
    )
    .unwrap();
    let output = cmd
        .args([
            "config.json",
            doc.to_str().unwrap(),
            "--dump-ir",
            "-e",
            "name=World",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON on stdout");
    assert_eq!(value["ir_version"], "unstable");
    assert!(value["program"].is_object());
}

/// The same document, without `-e`: the exact check's own message a
/// plain run of it would report, not a generic failure.
#[test]
fn dump_ir_without_e_on_a_required_input_fails_with_the_missing_input_message() {
    let (mut cmd, home) = command("dump-ir-no-e");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects: []\n",
    )
    .unwrap();
    let output = cmd
        .args(["config.json", doc.to_str().unwrap(), "--dump-ir"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.trim_end(),
        "missing required input 'name' declared in orchestration interface."
    );
}

/// A document that only declares content the M0-H VM actually runs is
/// never refused -- [`FAILURE_DOC`] fails at run time regardless of
/// whether `execute_root` is still lane B's own stub or the real
/// interpreter, so this is an *ordinary* failure, not the preview
/// refusal: `--out`/`--events`/`--live-state` are written, same as any
/// other failed run (issue #431's run-wiring step 20).
#[test]
fn run_request_with_every_new_flag_writes_out_events_and_live_state_on_an_ordinary_failure() {
    let (mut cmd, home) = command("run-new-flags");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, FAILURE_DOC).unwrap();
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--pretty",
            "--events",
            events.to_str().unwrap(),
            "--live-state",
            live_state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    // On failure, stdout carries the `{"ok": false, ...}` payload
    // regardless of `--out` (issue #431's "CLI output" decision).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["state_out"], out.to_str().unwrap());

    assert!(out.exists());
    assert!(events.exists());
    assert!(live_state.exists());
    // `--pretty` governs `--out`'s own file -- sorted keys, 2-space
    // indent.
    let out_text = fs::read_to_string(&out).unwrap();
    assert!(out_text.starts_with("{\n  \"_run_id\""));
    let events_text = fs::read_to_string(&events).unwrap();
    assert!(
        events_text
            .lines()
            .next()
            .unwrap()
            .contains("\"run_start\"")
    );
    assert!(events_text.lines().last().unwrap().contains("\"run_end\""));
    // `--live-state` is always written compact (never `--pretty`,
    // matching `cli/live_state.py`'s own `dumps_saved_state(state)`
    // with no `pretty=` kwarg) -- so this compares parsed content, not
    // bytes, when `--pretty` was given for `--out`.
    let live_state_text = fs::read_to_string(&live_state).unwrap();
    let out_value: serde_json::Value = serde_json::from_str(&out_text).unwrap();
    let live_state_value: serde_json::Value = serde_json::from_str(&live_state_text).unwrap();
    assert_eq!(out_value, live_state_value);
}

/// Without `--pretty`, `--live-state`'s final write is byte-identical
/// to `--out` (issue #431's acceptance criteria).
#[test]
fn live_state_is_byte_identical_to_out_without_pretty() {
    let (mut cmd, home) = command("live-state-byte-identical");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, FAILURE_DOC).unwrap();
    let out = home.path.join("out.json");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--live-state",
            live_state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(&out).unwrap(),
        fs::read_to_string(&live_state).unwrap()
    );
}

/// The same four flags, but written `--flag=value` instead of `--flag
/// value` -- `cof`'s own Click parser accepts both forms for a value
/// flag (orchestrator ruling on PR #432's review).
#[test]
fn run_request_with_every_new_flag_in_equals_form_also_writes_out() {
    let (mut cmd, home) = command("run-new-flags-equals");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, FAILURE_DOC).unwrap();
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap().to_string(),
            doc.to_str().unwrap().to_string(),
            format!("--out={}", out.to_str().unwrap()),
            format!("--events={}", events.to_str().unwrap()),
            format!("--live-state={}", live_state.to_str().unwrap()),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(out.exists());
    assert!(events.exists());
    assert!(live_state.exists());
}

/// A document naming unsupported content (here, a `prompt` effect
/// against `litellm`, a Circuitry-known adapter with no electricity
/// builder) is refused with the preview marker before any file is
/// written at all -- even when `--out`/`--events`/`--live-state` are
/// all given.
#[test]
fn an_unsupported_effect_is_refused_with_no_files_written() {
    let (mut cmd, home) = command("refusal");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "effects:\n  - name: ask\n    type: prompt\n    provider: litellm\n    template: \"hi\"\n",
    )
    .unwrap();
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
            "--live-state",
            live_state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains(electricity_bytecode::PREVIEW_MARKER),
        "{error}"
    );
    assert!(!out.exists());
    assert!(!events.exists());
    assert!(!live_state.exists());
}

/// A `loop`'s own `while:` in `mode: model` -- the compiler's own
/// default (`while_config.get("mode") or "model"`) whenever `mode:` is
/// absent, though cof's own schema requires the key explicitly, so
/// every real document spells it out -- is refused the same way as any
/// other unsupported content, even though `loop_` itself is supported
/// (lane H, #461): the walker recurses into a supported container's own
/// condition and refuses `RefusalReason::ModelCondition` before the run
/// ever starts (`model_condition` stays `false` until lane G2 lands).
/// A real document-level, CLI-driven companion of `electricity-bytecode`'s
/// own hand-built-`Op` unit test,
/// `a_while_loops_model_mode_condition_is_refused_even_once_loop_is_supported`.
#[test]
fn a_model_mode_while_condition_is_refused_with_no_files_written() {
    let (mut cmd, home) = command("while-model-mode");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "effects:\n  - name: poll\n    type: loop\n    while: {mode: model, template: \"done?\"}\n    \
         body:\n      - name: step\n        type: tool\n        provider: json\n        \
         params: {mode: stringify, input: {x: 1}}\n",
    )
    .unwrap();
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
            "--live-state",
            live_state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains(electricity_bytecode::PREVIEW_MARKER),
        "{error}"
    );
    assert!(!out.exists());
    assert!(!events.exists());
    assert!(!live_state.exists());
}

/// A document naming its own top-level `adapter:` is refused the same
/// way, with no file written at all, even though the rest of the
/// document (`json`/`dynamic` only) is otherwise fully supported --
/// `litellm` (a Python-SDK-only adapter, out of scope for lane F2,
/// issue #503) has no electricity-side adapter build at all, so
/// settled ruling 3's own stateless refusal (`unbuildable_known_adapter`)
/// fires before preflight (issue #495, lane R) is ever reached, not
/// because preflight itself is unported. `openai` used to be this
/// test's own example; it became a real, buildable adapter in lane F2,
/// so it now reaches a real preflight failure instead (see the Python
/// conformance case `document-adapter-preflight-not-ready`).
#[test]
fn a_document_level_adapter_is_refused_with_no_files_written() {
    let (mut cmd, home) = command("document-adapter-refusal");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "adapter: litellm\neffects:\n  - name: parse\n    type: tool\n    provider: json\n    params: {mode: parse, input: '{\"a\": 1}'}\n",
    )
    .unwrap();
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let live_state = home.path.join("live.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
            "--live-state",
            live_state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains(electricity_bytecode::PREVIEW_MARKER),
        "{error}"
    );
    assert!(error.contains("litellm"), "{error}");
    assert!(!out.exists());
    assert!(!events.exists());
    assert!(!live_state.exists());
}

/// A boolean flag given `=value` is a usage error, not silently
/// accepted or ignored.
#[test]
fn a_boolean_flag_with_an_equals_value_is_a_usage_error() {
    let (mut cmd, home) = command("pretty-equals");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args(["config.json", doc.to_str().unwrap(), "--pretty=true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// A malformed `-e` (no `=`) gets Circuitry's own exact
/// `cli/app.py::_parse_env_vars` message and this preview's usage-error
/// exit code.
#[test]
fn malformed_e_value_is_a_usage_error_with_circuitrys_own_message() {
    let (mut cmd, home) = command("run-e-malformed");
    // A real (if empty) config file: config resolution is step 1 of
    // issue #431's run-wiring table, strictly before `-e` parsing
    // (step 2, `cli/app.py`'s own `resolve_config` at line ~1127 vs.
    // `_parse_env_vars` at ~1261/1270) -- a missing config file would
    // otherwise mask this test's own bad-`-e` assertion behind "Config
    // file not found" instead.
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "-e",
            "badtext",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid -e format: 'badtext' (expected KEY=VALUE)"),
        "{stderr}"
    );
}

/// Same ordering as
/// [`a_config_warning_still_prints_when_the_orchestration_is_missing`],
/// for the other early-exit path: a malformed `-e` still ends the run
/// (exit 2) after config resolution's own warning has already printed
/// (PR #452 review finding 2).
#[test]
fn a_config_warning_still_prints_before_a_malformed_e_value_error() {
    let (mut cmd, home) = command("run-e-malformed-config-warning");
    let config = home.config("{\"environment\": \"staging\"}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "-e",
            "badtext",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines = stderr.lines();
    assert_eq!(
        lines.next(),
        Some(
            "WARNING: Unknown environment 'staging'; falling back to 'dev'. Valid: ('dev', 'prod', 'test')"
        )
    );
    assert!(
        stderr.contains("Invalid -e format: 'badtext' (expected KEY=VALUE)"),
        "{stderr}"
    );
}

/// A `CIRCUITRY_MODEL` environment overlay outranks both the config
/// file's own `default_model` and `SANE_DEFAULTS` (issue #431's run-
/// wiring step 1: `SANE_DEFAULTS`, the named file deep-merged on top,
/// then `CIRCUITRY_*` env overlays) -- observed through `runtime.
/// effective_settings.model` in `--out`, written even for this
/// document's own structural failure (which happens after that field
/// is set, run-wiring step 14).
#[test]
fn a_circuitry_env_overlay_outranks_the_config_files_own_default_model() {
    let home = TempHome::new("env-overlay");
    let config = home.config(r#"{"default_model": "from-config-file"}"#);
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "{}\n").unwrap();
    let out = home.path.join("out.json");
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("CIRCUITRY_MODEL", "from-env-overlay");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        state["runtime"]["effective_settings"]["model"],
        "from-env-overlay"
    );
    assert_eq!(
        state["runtime"]["effective_settings"]["sources"]["model"],
        "config"
    );
}

/// A run that succeeds its document check but fails at run time
/// ([`FAILURE_DOC`]) with no `--out` at all: on failure, stdout always
/// carries the JSON payload regardless of `--out` (issue #431's "CLI
/// output" decision) -- `state_out` is `null` since none was given.
#[test]
fn a_failure_with_no_out_flag_still_prints_the_json_payload_on_stdout() {
    let (mut cmd, home) = command("no-out-failure");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, FAILURE_DOC).unwrap();
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["state_out"], serde_json::Value::Null);
    assert!(!payload["error"].as_str().unwrap().is_empty());
}

/// A profile's own `out:` (DESIGN §6.11), with no `--out` flag at all,
/// is where `cof run` actually writes the state (`main.rs:710`'s
/// `effective_out_path` falls back to `RunResult::out_path`) -- and
/// `runtime.effective_settings.out` in that written file must name the
/// same path, not `null` (PR #497 review finding 1): Python's own
/// `effective_settings` always records whichever of `--out`/profile/
/// default actually applies (`cli/runtime_shim.py:708`), never only a
/// literal `--out` flag. Checked directly against a real `cof run
/// --profile p` with no `--out`, a profile naming the same `out:` path,
/// and an equivalent failing document: it writes its state to that
/// path and records `"sources": {"out": "profile", ...}` with
/// `"effective_settings": {"out": "<that path>", ...}` there -- never
/// `null` -- exactly this test's own two assertions below.
#[test]
fn a_profiles_out_is_both_written_to_and_recorded_with_no_out_flag() {
    let (mut cmd, home) = command("profile-out-no-flag");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, FAILURE_DOC).unwrap();
    let profile_out = home.path.join("profile-out.json");
    let profile = home.path.join("p.yml");
    fs::write(
        &profile,
        format!("out: {}\n", profile_out.to_str().unwrap()),
    )
    .unwrap();
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--profile",
            profile.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(
        payload["state_out"].as_str().unwrap(),
        profile_out.to_str().unwrap()
    );
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&profile_out).unwrap()).unwrap();
    assert_eq!(
        state["runtime"]["effective_settings"]["out"]
            .as_str()
            .unwrap(),
        profile_out.to_str().unwrap()
    );
    assert_eq!(
        state["runtime"]["effective_settings"]["sources"]["out"],
        "profile"
    );
}

/// A profile's own `persistence:` block (DESIGN §6.11) reaches the
/// same preview refusal as a document/config-level `runtime.
/// persistence` (issue #431's "Out of scope": persistence isn't built
/// in this milestone at all) -- the profile-layered merge
/// (`effective_settings_with_profile`'s own `persistence` tier)
/// reaches `persistence_is_configured` exactly like every other
/// source, so this refuses up front, no files written, the same as
/// the unconditional (no-profile) case.
#[test]
fn a_profiles_persistence_block_is_refused_with_no_files_written() {
    let (mut cmd, home) = command("profile-persistence-refusal");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let persisted = home.path.join("state.jsonl");
    let profile = home.path.join("p.yml");
    fs::write(
        &profile,
        format!(
            "persistence:\n  backend: jsonl-file\n  path: {}\n",
            persisted.to_str().unwrap()
        ),
    )
    .unwrap();
    let out = home.path.join("out.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--profile",
            profile.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(
        error.contains(electricity_bytecode::PREVIEW_MARKER),
        "{error}"
    );
    assert!(error.contains("runtime.persistence"), "{error}");
    assert!(!out.exists());
    assert!(!persisted.exists());
}

/// A failure *before* effective settings is ever resolved (the
/// document's own structural check, step 5 -- document loading, then
/// the allowlist check, both run before a `--profile` is even parsed)
/// reports only the plain `--out`: a loaded profile's own `out:`
/// (DESIGN §6.11) is irrelevant here because the profile is never
/// reached at all, matching DESIGN §6.11's "a profile failure's `--out`
/// is the plain `--out` only" (`lib.rs`'s own `resolved_out_path` doc
/// comment) -- this is the same rule one step earlier, for a failure
/// that never even gets as far as loading the profile.
#[test]
fn a_failure_before_the_profile_is_ever_loaded_writes_only_the_plain_out_flag() {
    let (mut cmd, home) = command("failure-before-profile-load");
    let config = home.config("{}");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "").unwrap();
    let profile_out = home.path.join("profile-out.json");
    let profile = home.path.join("p.yml");
    fs::write(
        &profile,
        format!("out: {}\n", profile_out.to_str().unwrap()),
    )
    .unwrap();
    let plain_out = home.path.join("plain-out.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc.to_str().unwrap(),
            "--profile",
            profile.to_str().unwrap(),
            "--out",
            plain_out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload["state_out"].as_str().unwrap(),
        plain_out.to_str().unwrap()
    );
    assert!(plain_out.exists());
    assert!(!profile_out.exists());
}

/// Round-7 fix (issue #498's own orchestrator follow-up): a `$defs`
/// chain of 10000 single-hop `$ref`s in a `prompt` effect's own
/// `schema:` used to overflow electricity's real main-thread native
/// stack -- `thread 'main' has overflowed its stack`, process abort,
/// no `--out` written at all -- well under `electricity_schema::engine`'s
/// own hop cap (4096). This is the end-to-end proof the fix covers the
/// real binary's own main thread, not just a test harness thread with
/// an explicit stack size.
#[test]
fn a_ten_thousand_hop_ref_chain_in_a_prompt_schema_never_overflows_the_main_thread() {
    let (mut cmd, home) = command("ref-chain-10000");
    let mut defs = serde_json::Map::new();
    for i in 0..10000 {
        defs.insert(
            format!("L{i}"),
            serde_json::json!({"$ref": format!("#/$defs/L{}", i + 1)}),
        );
    }
    defs.insert("L10000".to_string(), serde_json::json!({"type": "object"}));
    let schema = serde_json::json!({"$defs": defs, "$ref": "#/$defs/L0"});
    let doc = serde_json::json!({
        "name": "chain",
        "adapter": "scripted",
        "model": "m",
        "effects": [{
            "type": "prompt",
            "name": "c0",
            "prompt_type": "json",
            "on_error": "continue",
            "schema": schema,
            "template": "c0",
        }],
    });
    let doc_path = home.path.join("doc.yml");
    fs::write(&doc_path, serde_json::to_string(&doc).unwrap()).unwrap();
    let replies_path = home.path.join("replies.yml");
    let replies = serde_json::json!({"prime.c0": [{"text": "{}"}]});
    fs::write(&replies_path, serde_json::to_string(&replies).unwrap()).unwrap();
    let config = home.config(
        &serde_json::json!({
            "runtime": {"adapters": {"scripted": {"replies_file": replies_path.to_str().unwrap()}}},
        })
        .to_string(),
    );
    let out = home.path.join("out.json");
    let output = cmd
        .args([
            config.to_str().unwrap(),
            doc_path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(out.exists());
    let out_value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let meta = &out_value["prime"]["c0"]["meta"];
    let attempt = &meta["fallback_attempts"][0];
    assert_eq!(attempt["status"].as_str().unwrap(), "failed");
    assert_eq!(
        attempt["error"].as_str().unwrap(),
        "maximum recursion depth exceeded while calling a Python object"
    );
}
