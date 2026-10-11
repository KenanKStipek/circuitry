//! CLI-level signal/cancellation tests for a *process-backed* tool
//! (`shell`), as issue #459's own test plan asks for and review
//! finding 8 on PR #467 names as missing: `electricity-cli/tests/
//! signals.rs` only ever exercises the `sleep` test-tool (a plain
//! async sleep, no real child process of its own), so none of those
//! tests could ever catch a process-runtime regression -- a leaked
//! grandchild, a timeout that doesn't kill the whole group, or a
//! `finally:` that can no longer spawn after the run it's cleaning up
//! from was cancelled.
//!
//! Every document here runs `sh -c "sleep N & sleep N"`: the foreground
//! `sleep N` is the tracked child itself, the backgrounded one is a
//! grandchild in the *same* process group (a non-interactive `sh -c`
//! never does its own job-control pgrp separation) -- exactly the
//! shape `core/cancellation.py::kill_process_group`'s own grandchild
//! case needs. Every duration here is unique across this file (and
//! distinct from every other test file's own sleep durations in this
//! crate) so `pgrep -f` can never match a sibling test's still-running
//! child when tests run in parallel.

#![cfg(unix)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "electricity-process-signals-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempHome { path }
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

struct TestChild {
    child: Child,
}

impl TestChild {
    fn pgid(&self) -> i32 {
        self.child.id() as i32
    }

    fn send(&self, signum: i32) {
        unsafe {
            libc::kill(-self.pgid(), signum);
        }
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pgid(), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn pgrep_matches(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn live_state_path(home: &TempHome) -> PathBuf {
    home.path.join("live-state.json")
}

fn spawn(home: &TempHome, config: &Path, doc: &Path, extra_args: &[&str]) -> TestChild {
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.args([config.to_str().unwrap(), doc.to_str().unwrap()]);
    cmd.args(["--live-state", live_state_path(home).to_str().unwrap()]);
    cmd.args(extra_args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    let child = cmd.spawn().expect("spawn electricity");
    TestChild { child }
}

/// Mirrors `signals.rs`'s own readiness probe: `--live-state`'s first
/// write happens well after signal handlers are armed but before any
/// effect dispatches.
fn wait_until_running(home: &TempHome) {
    let path = live_state_path(home);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            panic!("electricity never wrote --live-state within 10s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shell_sleep_doc(sleep_secs: u32) -> String {
    format!(
        "effects:\n\
         \x20 - name: s\n\
         \x20   type: tool\n\
         \x20   provider: shell\n\
         \x20   timeout_ms: 30000\n\
         \x20   params:\n\
         \x20     command: sh\n\
         \x20     allowed_commands: [sh]\n\
         \x20     args: [\"-c\", \"sleep {sleep_secs} & sleep {sleep_secs}\"]\n"
    )
}

fn signal_test(tag: &str, sleep_secs: u32, signum: i32, want_exit: i32) {
    let home = TempHome::new(tag);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &shell_sleep_doc(sleep_secs));
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    assert!(
        pgrep_matches(&format!("sleep {sleep_secs}")),
        "the shell step's own child (and grandchild) must be running before the signal"
    );
    child.send(signum);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits after the signal");
    assert_eq!(status.code(), Some(want_exit));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&format!("sleep {sleep_secs}")),
        "both the tracked child and its grandchild must be gone after cancellation"
    );
}

#[test]
fn sigint_during_a_shell_steps_grandchild_kills_both_exit_130() {
    signal_test("sigint", 8491, libc::SIGINT, 130);
}

#[test]
fn sigterm_during_a_shell_steps_grandchild_kills_both_exit_143() {
    signal_test("sigterm", 8492, libc::SIGTERM, 143);
}

#[test]
fn sighup_during_a_shell_steps_grandchild_kills_both_exit_129() {
    signal_test("sighup", 8493, libc::SIGHUP, 129);
}

#[test]
fn a_shell_timeout_kills_a_grandchild_too() {
    let home = TempHome::new("timeout");
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n\
         \x20 - name: s\n\
         \x20   type: tool\n\
         \x20   provider: shell\n\
         \x20   timeout_ms: 500\n\
         \x20   on_error: continue\n\
         \x20   params:\n\
         \x20     command: sh\n\
         \x20     allowed_commands: [sh]\n\
         \x20     args: [\"-c\", \"sleep 8494 & sleep 8494\"]\n",
    );
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits once the step times out");
    assert!(status.success(), "on_error: continue must still exit 0");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches("sleep 8494"),
        "a timed-out shell step must not leak the grandchild it forked"
    );
}

#[test]
fn a_finally_shell_step_still_spawns_after_the_run_is_cancelled() {
    let home = TempHome::new("finally-spawns");
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n\
         \x20 - name: s\n\
         \x20   type: tool\n\
         \x20   provider: shell\n\
         \x20   timeout_ms: 30000\n\
         \x20   params:\n\
         \x20     command: sleep\n\
         \x20     allowed_commands: [sleep]\n\
         \x20     args: [\"30\"]\n\
         finally:\n\
         \x20 - name: cleanup\n\
         \x20   type: tool\n\
         \x20   provider: shell\n\
         \x20   params:\n\
         \x20     command: echo\n\
         \x20     allowed_commands: [echo]\n\
         \x20     args: [\"cleaned up\"]\n",
    );
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits after SIGINT");
    assert_eq!(status.code(), Some(130));
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        state["prime"]["cleanup"]["value"],
        serde_json::Value::String("cleaned up\n".to_string()),
        "a finally: shell step must still actually run (and produce a real \
         result) after the run it's cleaning up from was cancelled: {state}"
    );
}
