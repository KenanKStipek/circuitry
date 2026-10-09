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

/// `electricity <config.json> <doc>` runs `check_for_run` first (issue
/// #408's CLI section). An empty document is one of the few checks
/// fully resolvable without lane C's own compiler, so its exact text
/// -- not just a marker naming an unimplemented lane -- is a stable
/// thing to assert here.
#[test]
fn run_request_fails_with_the_checks_own_error_text_and_exit_code_one() {
    let (mut cmd, home) = command("run");
    let config = home.path.join("config.json");
    let doc = home.path.join("doc.yml");
    fs::write(&config, "{}").unwrap();
    fs::write(&doc, "").unwrap();
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The "required property" text past the location is the Rust
    // `jsonschema` crate's own (third-party) wording, not required to
    // match Circuitry's Python `jsonschema` text word for word
    // (DESIGN.md §1/§12) -- only the "Orchestration validation failed:"
    // wrapper and the location are Circuitry's own.
    assert!(stderr.starts_with("Orchestration validation failed:\n  - top level: "));
    assert!(stderr.contains("required property"), "{stderr}");
}

#[test]
fn known_run_flag_in_first_position_is_a_run_request() {
    let (mut cmd, _home) = command("run-flag-first");
    let output = cmd
        .args(["--profile", "p", "config.json", "orchestration.yml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
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

/// `--dump-ir` wiring (issue #408's CLI section): `effects: []` has no
/// structural, concurrency-configuration, compile, group, or cycle
/// error, so `check_for_run` succeeds and `--dump-ir` prints the
/// resulting `Program` as the documented `{"ir_version": "unstable",
/// "program": ...}` wrapper.
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

/// The same document, without `-e`: the exact `check_for_run` message a
/// plain `cof run` of it would report, not a generic failure.
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

/// The run path with valid `-e` inputs reaches the same preview
/// refusal a document with no inputs at all does (issue #429).
#[test]
fn run_request_with_valid_e_inputs_reaches_the_preview_refusal() {
    let (mut cmd, home) = command("run-e-valid");
    let doc = home.path.join("doc.yml");
    fs::write(
        &doc,
        "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects: []\n",
    )
    .unwrap();
    let output = cmd
        .args(["config.json", doc.to_str().unwrap(), "-e", "name=World"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is a preview and cannot run orchestrations yet"),
        "{stderr}"
    );
}

/// A malformed `-e` (no `=`) gets Circuitry's own exact
/// `cli/app.py::_parse_env_vars` message and this preview's usage-error
/// exit code.
#[test]
fn malformed_e_value_is_a_usage_error_with_circuitrys_own_message() {
    let (mut cmd, home) = command("run-e-malformed");
    let doc = home.path.join("doc.yml");
    fs::write(&doc, "effects: []\n").unwrap();
    let output = cmd
        .args(["config.json", doc.to_str().unwrap(), "-e", "badtext"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid -e format: 'badtext' (expected KEY=VALUE)"),
        "{stderr}"
    );
}
