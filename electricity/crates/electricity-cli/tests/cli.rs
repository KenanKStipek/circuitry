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

#[test]
fn run_request_fails_with_preview_message_and_exit_code_one() {
    let (mut cmd, _home) = command("run");
    let output = cmd
        .args(["config.json", "orchestration.yml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot run orchestrations yet"));
    assert!(stderr.contains("cof run"));
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
