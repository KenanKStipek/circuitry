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

fn temp_home(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("electricity-cli-test-{tag}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp HOME");
    dir
}

fn command(tag: &str) -> Command {
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", temp_home(tag));
    cmd.env("PATH", env::var("PATH").unwrap_or_default());
    cmd
}

#[test]
fn version_prints_preview_notice_and_exits_zero() {
    let output = command("version").arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("electricity"));
    assert!(stdout.contains("(preview)"));
}

#[test]
fn help_prints_usage_and_preview_notice_and_exits_zero() {
    let output = command("help").arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: electricity"));
    assert!(stdout.contains("preview"));
}

#[test]
fn run_request_fails_with_preview_message_and_exit_code_one() {
    let output = command("run")
        .args(["config.json", "orchestration.yml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot run orchestrations yet"));
    assert!(stderr.contains("cof run"));
}

#[test]
fn no_arguments_is_a_usage_error_with_exit_code_two() {
    let output = command("usage-error").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage: electricity"));
}

#[test]
fn unknown_flag_is_a_usage_error_with_exit_code_two() {
    let output = command("bogus-flag").arg("--bogus").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
