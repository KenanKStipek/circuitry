//! Issue #478's own leftover-replies export (`electricity/docs/spec/
//! scripted-replies.md` §7): `electricity::write_leftover_export`'s own
//! call site in `main` runs on every exit path. The three undecorated
//! tests below (no `test-tools`, the plain `cargo test --workspace`
//! gate) cover exit 0, 1 and 2 for an ordinary document that never
//! loads a `scripted` adapter at all -- the export must stay absent on
//! every one of those, the common case this hook must never affect.
//!
//! No document can make the adapter actually load until lane G flips
//! `prompt`, so proving the hook carries *real* content on every exit
//! path needs a different instance to register -- the `test-tools`-
//! only hook `register_test_only_scripted_adapter` (`main.rs`) builds
//! one directly and calls `check()` on it, the same lazy-load-then-
//! register path `cof run`'s own preflight does for a document whose
//! top-level `adapter: scripted` is never actually dispatched (no
//! prompt effect ever calls `generate()`) -- a full, untouched count,
//! never consumed. The `test-tools`-gated tests below drive that hook
//! for exit 0 and 1; `tests/signals.rs` drives the same hook for the
//! three signal exits (130/143/129) and the second-signal absence
//! case, reusing its own process-group signal-sending
//! infrastructure.
//!
//! The expected counts these tests assert on were verified against
//! Circuitry's own `circuitry.adapters.scripted.ScriptedAdapter.check`/
//! `_merge_leftover_replies` through `generate_scripted_corpus.py`'s
//! own `a_single_preflight_only_instance_reports_every_path_at_its_
//! full_count` merge case (identical shape: one `check()`-only
//! instance, full queues, nothing consumed) -- the exact functions
//! `cof run`'s own preflight and `atexit` hook call.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = env::temp_dir().join(format!(
            "electricity-leftover-export-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp HOME");
        Self { path }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn command(tag: &str, export_path: &PathBuf) -> (Command, TempHome) {
    let home = TempHome::new(tag);
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd.env("CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE", export_path);
    (cmd, home)
}

#[test]
fn an_ordinary_successful_run_exits_zero_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-ok-{}.json",
        std::process::id()
    ));
    let (mut cmd, home) = command("ok", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", "effects: []\n");
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !export.exists(),
        "no scripted adapter ever loaded, so the export must stay absent"
    );
    let _ = fs::remove_file(&export);
}

#[test]
fn an_ordinary_run_failure_exits_one_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-fail-{}.json",
        std::process::id()
    ));
    let (mut cmd, home) = command("fail", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n  - name: broken\n    type: tool\n    provider: json\n    params: {mode: parse, input: 123}\n",
    );
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !export.exists(),
        "no scripted adapter ever loaded, so the export must stay absent"
    );
    let _ = fs::remove_file(&export);
}

#[test]
fn a_usage_error_exits_two_and_writes_no_export() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-usage-{}.json",
        std::process::id()
    ));
    let (mut cmd, _home) = command("usage", &export);
    let output = cmd.arg("--bogus").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !export.exists(),
        "a usage error runs the export hook too, but nothing ever registers"
    );
    let _ = fs::remove_file(&export);
}

/// `prime.x`: 2 replies; `prime.y`: 1 -- fixed counts the
/// `test-tools`-only register hook's own `check()` call leaves fully
/// untouched (it never dispatches `generate()`), so every test below
/// expects exactly this map back from the export, regardless of exit
/// path.
#[cfg(feature = "test-tools")]
const LEFTOVER_EXPORT_FIXTURE: &str =
    "prime.x:\n  - text: one\n  - text: two\nprime.y:\n  - text: only\n";

#[cfg(feature = "test-tools")]
fn expected_leftover_export_counts() -> serde_json::Value {
    serde_json::json!({"prime.x": 2, "prime.y": 1})
}

#[cfg(feature = "test-tools")]
fn command_with_register_hook(tag: &str, export_path: &PathBuf) -> (Command, TempHome, PathBuf) {
    let (mut cmd, home) = command(tag, export_path);
    let replies = home.write("replies.yaml", LEFTOVER_EXPORT_FIXTURE);
    cmd.env("ELECTRICITY_TEST_SCRIPTED_REGISTER_REPLIES_FILE", &replies);
    (cmd, home, replies)
}

#[cfg(feature = "test-tools")]
fn assert_export_has_the_fixtures_full_counts(export: &PathBuf) {
    let text = fs::read_to_string(export)
        .unwrap_or_else(|e| panic!("export file {} should exist: {e}", export.display()));
    let got: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(got, expected_leftover_export_counts());
}

#[cfg(feature = "test-tools")]
#[test]
fn an_ordinary_successful_run_exits_zero_and_writes_the_registered_full_counts() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-ok-content-{}.json",
        std::process::id()
    ));
    let (mut cmd, home, _replies) = command_with_register_hook("ok-content", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", "effects: []\n");
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_export_has_the_fixtures_full_counts(&export);
    let _ = fs::remove_file(&export);
}

#[cfg(feature = "test-tools")]
#[test]
fn an_ordinary_run_failure_exits_one_and_still_writes_the_registered_full_counts() {
    let export = env::temp_dir().join(format!(
        "electricity-leftover-export-fail-content-{}.json",
        std::process::id()
    ));
    let (mut cmd, home, _replies) = command_with_register_hook("fail-content", &export);
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n  - name: broken\n    type: tool\n    provider: json\n    params: {mode: parse, input: 123}\n",
    );
    let output = cmd
        .args([config.to_str().unwrap(), doc.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    // The export hook runs regardless of the run's own outcome
    // (spec §7: "whether the run itself succeeded or failed") -- an
    // adapter registered before the run started is unaffected by a
    // later, unrelated tool failure.
    assert_export_has_the_fixtures_full_counts(&export);
    let _ = fs::remove_file(&export);
}
