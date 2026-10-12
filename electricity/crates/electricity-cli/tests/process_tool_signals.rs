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
//! case needs.
//!
//! [`unique_sleep_secs`] derives every `sleep` duration used as a
//! marker from this test *process*'s own pid plus a per-call counter,
//! so two concurrent runs of this file -- two checkouts, or a CI
//! retry racing a still-finishing previous run -- can never pick the
//! same number and therefore can never see, signal or wait on each
//! other's processes via `pgrep -f`/`pkill -f` (issue #491).
//!
//! The shell step's own child gets `setsid()` (its own session, not
//! merely its own process group -- see `electricity-tools::process`),
//! so it and its background grandchild are **not** members of this
//! test's own `electricity` child's process group: [`TestChild::drop`]
//! below, which `SIGKILL`s only that group, can never reach them. A
//! panicking assertion that skips a test's normal cancellation
//! assertions would otherwise leave them running, re-parented to
//! init, where every later run (including from another checkout)
//! would see them and misread its own "must be gone" assertion.
//! [`SleepGuard`] is the fix: a `Drop` guard, scoped to this run's own
//! unique marker, that `pkill -f`s it unconditionally.

#![cfg(unix)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
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

/// A process-wide counter, combined with this test *process*'s own
/// pid, so every call across every test in this file -- even ones
/// running concurrently on separate threads, as `cargo test` does by
/// default -- gets a number no other call, in this process or any
/// other, will ever produce. The counter never exceeds 3 digits in
/// practice (this file has five tests), so multiplying the pid by
/// 1000 leaves every process's own range disjoint from every other
/// process's.
static MARKER_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_sleep_secs() -> u64 {
    let n = MARKER_COUNTER.fetch_add(1, Ordering::SeqCst);
    (std::process::id() as u64) * 1_000 + n
}

/// A `pgrep -f`/`pkill -f` pattern that matches this exact marker and
/// nothing else: a plain `"sleep {sleep_secs}"` substring would also
/// match any other marker whose digits happen to start with this
/// one's (e.g. marker `500` inside marker `5001`). The trailing
/// alternation anchors the match to either a following space (the
/// `sh -c "sleep N & sleep N"` foreground occurrence) or end of the
/// command line (the backgrounded occurrence, or a bare `sleep N`).
/// Used for cleanup (`SleepGuard`) and the "must be gone" assertions,
/// where every process carrying this marker needs to be caught,
/// including a leftover `sh -c ...` itself.
fn sleep_pattern(sleep_secs: u64) -> String {
    format!("sleep {sleep_secs}( |$)")
}

/// A `pgrep -f` pattern that matches *only* a process whose entire
/// command line is the bare `sleep {sleep_secs}`, used for the
/// readiness check in [`signal_test`]. `sh -c "sleep N & sleep N"`'s
/// own process count there depends on the shell: a forking `sh`
/// (bash, macOS's own `/bin/sh`) keeps itself alive alongside two
/// forked `sleep N` children, three processes whose command line
/// contains `sleep N`. dash (`ubuntu-latest`'s `/bin/sh`) execs the
/// last command of a `-c` string instead of forking for it, so the
/// `sh` process itself *becomes* the foreground `sleep N` and only
/// two processes remain. Either way, exactly two processes end up
/// with a command line that is *exactly* `sleep {sleep_secs}` (the
/// backgrounded one, forked in both shells; the foreground one,
/// forked under bash or exec'd-into under dash) -- confirmed directly
/// against both `/bin/bash` and `/bin/dash` -- so waiting for 2 here
/// is shell-independent, unlike waiting for 3.
fn exact_sleep_pattern(sleep_secs: u64) -> String {
    format!("^sleep {sleep_secs}$")
}

fn pgrep_count(pattern: &str) -> usize {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn pgrep_matches(pattern: &str) -> bool {
    pgrep_count(pattern) > 0
}

/// Polls `pgrep -f` for *pattern* until at least *min_count* processes
/// match, up to *timeout* -- rather than asserting once, right after
/// `--live-state` first appears, which can run well before the shell
/// step has actually forked anything under load (issue #491).
fn wait_for_pgrep_count(pattern: &str, min_count: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let count = pgrep_count(pattern);
        if count >= min_count {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "pattern {pattern:?} matched only {count} process(es), \
                 wanted at least {min_count}, within {timeout:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills every process still matching this run's own unique marker,
/// unconditionally, when the test that owns it ends -- including by a
/// panicking assertion. The shell step's child is its own session
/// (`setsid`), so [`TestChild::drop`]'s process-group `SIGKILL` on the
/// *electricity* process never reaches it or its grandchild; only a
/// marker-scoped `pkill -f` does.
struct SleepGuard {
    pattern: String,
}

impl SleepGuard {
    fn new(sleep_secs: u64) -> Self {
        SleepGuard {
            pattern: sleep_pattern(sleep_secs),
        }
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        let _ = Command::new("pkill").args(["-f", &self.pattern]).status();
    }
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
/// effect dispatches. Firm evidence `electricity` itself is running --
/// not that the shell step has forked anything yet; callers that care
/// about the shell step's own child/grandchild still need
/// [`wait_for_pgrep_count`] after this.
fn wait_until_running(home: &TempHome) {
    let path = live_state_path(home);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if Instant::now() >= deadline {
            panic!("electricity never wrote --live-state within 10s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shell_sleep_doc(sleep_secs: u64) -> String {
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

fn signal_test(tag: &str, signum: i32, want_exit: i32) {
    let sleep_secs = unique_sleep_secs();
    let _cleanup = SleepGuard::new(sleep_secs);
    let pattern = sleep_pattern(sleep_secs);

    let home = TempHome::new(tag);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &shell_sleep_doc(sleep_secs));
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    // Two processes whose command line is exactly `sleep {sleep_secs}`
    // means both the child and the grandchild are up, on either a
    // forking `sh` (bash, macOS) or an exec-on-last-command `sh`
    // (dash, ubuntu-latest) -- see `exact_sleep_pattern`.
    wait_for_pgrep_count(&exact_sleep_pattern(sleep_secs), 2, Duration::from_secs(10));
    child.send(signum);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits after the signal");
    assert_eq!(status.code(), Some(want_exit));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&pattern),
        "both the tracked child and its grandchild must be gone after cancellation"
    );
}

#[test]
fn sigint_during_a_shell_steps_grandchild_kills_both_exit_130() {
    signal_test("sigint", libc::SIGINT, 130);
}

#[test]
fn sigterm_during_a_shell_steps_grandchild_kills_both_exit_143() {
    signal_test("sigterm", libc::SIGTERM, 143);
}

#[test]
fn sighup_during_a_shell_steps_grandchild_kills_both_exit_129() {
    signal_test("sighup", libc::SIGHUP, 129);
}

#[test]
fn a_shell_timeout_kills_a_grandchild_too() {
    let sleep_secs = unique_sleep_secs();
    let _cleanup = SleepGuard::new(sleep_secs);
    let pattern = sleep_pattern(sleep_secs);

    let home = TempHome::new("timeout");
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        &format!(
            "effects:\n\
             \x20 - name: s\n\
             \x20   type: tool\n\
             \x20   provider: shell\n\
             \x20   timeout_ms: 500\n\
             \x20   on_error: continue\n\
             \x20   params:\n\
             \x20     command: sh\n\
             \x20     allowed_commands: [sh]\n\
             \x20     args: [\"-c\", \"sleep {sleep_secs} & sleep {sleep_secs}\"]\n"
        ),
    );
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits once the step times out");
    assert!(status.success(), "on_error: continue must still exit 0");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&pattern),
        "a timed-out shell step must not leak the grandchild it forked"
    );
}

#[test]
fn a_finally_shell_step_still_spawns_after_the_run_is_cancelled() {
    let sleep_secs = unique_sleep_secs();
    let _cleanup = SleepGuard::new(sleep_secs);
    let pattern = sleep_pattern(sleep_secs);

    let home = TempHome::new("finally-spawns");
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        &format!(
            "effects:\n\
             \x20 - name: s\n\
             \x20   type: tool\n\
             \x20   provider: shell\n\
             \x20   timeout_ms: 30000\n\
             \x20   params:\n\
             \x20     command: sleep\n\
             \x20     allowed_commands: [sleep]\n\
             \x20     args: [\"{sleep_secs}\"]\n\
             finally:\n\
             \x20 - name: cleanup\n\
             \x20   type: tool\n\
             \x20   provider: shell\n\
             \x20   params:\n\
             \x20     command: echo\n\
             \x20     allowed_commands: [echo]\n\
             \x20     args: [\"cleaned up\"]\n"
        ),
    );
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    wait_for_pgrep_count(&pattern, 1, Duration::from_secs(10));
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
