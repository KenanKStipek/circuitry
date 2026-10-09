//! Signal-handling integration tests (issue #431's acceptance criteria
//! -- "Signals (CLI integration tests on Linux and macOS, `test-tools`
//! feature, temporary `HOME`, bounded time, own process group killed on
//! teardown)").
//!
//! `#![cfg(feature = "test-tools")]` below: this whole file compiles to
//! nothing without that feature, so the ordinary `cargo test --workspace`
//! gate (which never enables it) never even builds it, let alone runs
//! or skips anything here -- PR #441 review finding 11: before this
//! gate existed, un-ignoring a test in this file without it would have
//! made `cargo test --workspace` build a binary that refuses every
//! `sleep`/`fail` dispatch (`extra_allowed_providers` is empty without
//! the feature), failing for an unrelated reason.
//!
//! Run explicitly:
//! ```sh
//! cargo test -p electricity-cli --features test-tools --test signals
//! ```
//! (`.github/workflows/electricity.yml`'s own `fmt-clippy-test` job
//! runs exactly this, on both its Linux and macOS runners, as its own
//! step right after the default `cargo test --workspace`).
//!
//! Every child runs in its own process group (`process_group(0)`, so a
//! signal aimed at the group never reaches this test binary's own), and
//! [`Child::drop`]'s own `TestChild` wrapper SIGKILLs that whole group
//! on teardown -- including a panicking assertion -- so a timed-out or
//! still-sleeping child is never left behind as an orphan
//! (`LANE-CONTRACT.md`'s own rule).

#![cfg(feature = "test-tools")]

use std::fs;
use std::io::Write as _;
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
            "electricity-signals-test-{tag}-{}",
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

/// A spawned `electricity` child in its own process group, SIGKILLed
/// (the whole group, not just this one pid) on drop -- including when a
/// test panics partway through, so a sleeping child is never left
/// running after the test that started it ends.
struct TestChild {
    child: Child,
}

impl TestChild {
    fn pgid(&self) -> i32 {
        self.child.id() as i32
    }

    fn send(&self, signum: i32) {
        // Negative pid: signal the whole process group, not just the
        // one pid -- `process_group(0)` below made this child its own
        // group leader, so this never reaches the test binary itself.
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

/// A document whose root blocks for a long time (`sleep`'s own
/// `params.seconds`) with a `finally:` that itself blocks briefly --
/// long enough for every test here to send a signal mid-flight and
/// still have time left to observe its effect, short enough that a
/// test that *doesn't* get interrupted (a bug) still fails on its own
/// timeout rather than hanging the suite.
const LONG_SLEEP_DOC: &str = "\
effects:
  - name: slow
    type: tool
    provider: sleep
    params: {seconds: 30}
finally:
  - name: cleanup
    type: tool
    provider: sleep
    params: {seconds: 5}
";

/// *extra_args* should not itself include `--live-state`: this
/// function always adds its own, to *home*'s own `live-state.json`,
/// since [`wait_until_running`] needs it as a readiness probe
/// regardless of whether a given test cares about its contents.
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

fn live_state_path(home: &TempHome) -> PathBuf {
    home.path.join("live-state.json")
}

/// Waits for *home*'s own `--live-state` file to exist -- its first
/// write is synchronous, and happens well after signals are armed (the
/// very first thing `run_action` does) but before `execute_root` ever
/// starts, so its existence is firm evidence the signal handler is
/// already installed, unlike a fixed sleep: on a quiet machine a few
/// milliseconds is enough, but a busy one sharing this machine with
/// other work (`LANE-CONTRACT.md`'s own warning) can stretch process
/// startup well past what looked like a safe fixed margin in testing
/// -- confirmed directly during this test's own review (PR #441
/// finding 5): a 200ms fixed sleep here was intermittently too short
/// under load, sending SIGINT before the handler was installed and
/// letting the OS's own default disposition kill the child instead.
fn wait_until_running(home: &TempHome) {
    let path = live_state_path(home);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            panic!(
                "electricity never wrote its own first --live-state at {} within 10s",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sigint_exits_130_runs_finally_and_writes_out() {
    let home = TempHome::new("sigint");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let events = home.path.join("events.jsonl");
    let mut child = spawn(
        &home,
        &config,
        &doc,
        &[
            "--out",
            out.to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
        ],
    );
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
    assert!(out.exists());
    let events_text = fs::read_to_string(&events).unwrap();
    let run_end = events_text.lines().last().unwrap();
    assert!(run_end.contains("\"run_end\""));
    assert!(run_end.contains("\"SIGINT\""));
}

#[test]
fn sigterm_exits_143() {
    let home = TempHome::new("sigterm");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGTERM);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGTERM");
    assert_eq!(status.code(), Some(143));
    assert!(out.exists());
}

#[test]
fn sighup_exits_129() {
    let home = TempHome::new("sighup");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGHUP);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGHUP");
    assert_eq!(status.code(), Some(129));
    assert!(out.exists());
}

#[test]
fn a_double_sighup_still_exits_129_cleanly() {
    let home = TempHome::new("double-sighup");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(10));
    // SIGHUP is never a second signal (issue #431's Signals section) --
    // a second one here must not race the first's own cleanup to an
    // early/different exit.
    child.send(libc::SIGHUP);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after a double SIGHUP");
    assert_eq!(status.code(), Some(129));
    assert!(out.exists());
}

#[test]
fn a_second_sigint_during_a_blocking_finally_exits_immediately_with_no_out() {
    let home = TempHome::new("double-sigint");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    // The first SIGINT cancels the root `sleep` and starts `finally:`
    // (its own 5-second sleep) -- a second SIGINT shortly after, while
    // that `finally:` is still blocking, must exit at once rather than
    // waiting for it.
    child.send(libc::SIGINT);
    std::thread::sleep(Duration::from_millis(2000));
    let before_second_signal = std::time::Instant::now();
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(2))
        .expect("a second SIGINT exits immediately, not after finally's own 5s sleep");
    assert!(before_second_signal.elapsed() < Duration::from_secs(2));
    assert_eq!(status.code(), Some(130));
    // No `--out` at all for a second-signal exit (issue #431's Signals
    // section: "no `--out`, no `run_end`").
    assert!(!out.exists());
}

#[test]
fn a_queued_tree_branch_never_starts() {
    // A tree `dynamic` with `max_concurrency: 1` and two branches, the
    // first a long sleep and the second a `fail` tool that would
    // otherwise prove it ran by writing an error into the final state --
    // cancelling before the first branch finishes must mean the second
    // (queued) branch is never dispatched at all, so the final state
    // has no trace of it (DESIGN.md §6.5/§6.9's own "queued tree
    // branches never start" rule).
    const DOC: &str = "\
effects:
  - name: fanout
    type: dynamic
    flow: tree
    max_concurrency: 1
    effects:
      - name: first
        type: tool
        provider: sleep
        params: {seconds: 30}
      - name: second
        type: tool
        provider: fail
        params: {}
";
    let home = TempHome::new("queued-branch");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        state["prime"]["fanout"]["second"].is_null(),
        "a queued branch must never have started: {state}"
    );
}

/// Not `#[ignore]`: proves the test harness itself (process group,
/// signal delivery, teardown) works today, independent of lanes B/C --
/// a `--help` child isn't long-running, so SIGINT just has to reach it
/// (or it finishes before the signal does, either is fine) without
/// hanging or leaking a process.
#[test]
fn the_test_harness_can_spawn_and_signal_a_child_without_hanging() {
    let home = TempHome::new("harness-smoke");
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.arg("--help");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    let mut child = TestChild {
        child: cmd.spawn().unwrap(),
    };
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(5))
        .expect("a --help child exits promptly");
    // `--help` has no run to interrupt, so it never arms any signal
    // handler of its own -- either it already finished (exit 0) or
    // SIGINT's own default disposition ended it before that (a
    // signal-terminated `ExitStatus` reports via `.signal()`, not
    // `.code()`); either is a pass for this smoke test, which only
    // cares that nothing hung and the child is accounted for.
    use std::os::unix::process::ExitStatusExt as _;
    assert!(status.code().is_some() || status.signal().is_some());
    let _ = std::io::stdout().flush();
}
