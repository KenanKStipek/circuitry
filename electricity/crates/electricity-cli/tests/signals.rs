//! Signal-handling integration tests (issue #431's acceptance criteria
//! -- "Signals (CLI integration tests on Linux and macOS, `test-tools`
//! feature, temporary `HOME`, bounded time, own process group killed on
//! teardown)").
//!
//! `#![cfg(feature = "test-tools")]` below: this whole file compiles to
//! nothing without that feature, so the ordinary `cargo test --workspace`
//! gate (which never enables it) never even builds it, let alone runs
//! or skips anything here -- PR #441: before this
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
/// -- confirmed directly during this test's own review (PR #441):
/// a 200ms fixed sleep here was intermittently too short
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

/// *out*'s own `prime.cleanup` node (the `finally:` step [`LONG_SLEEP_DOC`]
/// declares) is present with a real value -- `finally:` actually ran,
/// not merely that the run exited with the right code (PR #441:
/// no signal test checked this before).
fn assert_finally_ran(out: &Path) {
    let state: serde_json::Value = serde_json::from_str(&fs::read_to_string(out).unwrap()).unwrap();
    assert!(
        !state["prime"]["cleanup"].is_null(),
        "finally: never ran: {state}"
    );
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
    assert_finally_ran(&out);
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
    child.send(libc::SIGTERM);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGTERM");
    assert_eq!(status.code(), Some(143));
    assert!(out.exists());
    assert_finally_ran(&out);
    // SIGTERM/SIGHUP's own run_end.signal was
    // untested (neither test passed --events before).
    let events_text = fs::read_to_string(&events).unwrap();
    let run_end = events_text.lines().last().unwrap();
    assert!(run_end.contains("\"run_end\""));
    assert!(run_end.contains("\"SIGTERM\""));
}

#[test]
fn sighup_exits_129() {
    let home = TempHome::new("sighup");
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
    child.send(libc::SIGHUP);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGHUP");
    assert_eq!(status.code(), Some(129));
    assert!(out.exists());
    assert_finally_ran(&out);
    let events_text = fs::read_to_string(&events).unwrap();
    let run_end = events_text.lines().last().unwrap();
    assert!(run_end.contains("\"run_end\""));
    assert!(run_end.contains("\"SIGHUP\""));
}

/// `nohup electricity ...` (SIGHUP already
/// `SIG_IGN` on entry) must stay ignored -- a SIGHUP sent to the child
/// does nothing at all, not even cancel the run; a *different* signal
/// (SIGINT) still cancels it normally afterwards.
#[test]
fn a_sighup_ignored_on_entry_nohup_never_cancels_the_run() {
    let home = TempHome::new("nohup");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.args([config.to_str().unwrap(), doc.to_str().unwrap()]);
    cmd.args(["--live-state", live_state_path(&home).to_str().unwrap()]);
    cmd.args(["--out", out.to_str().unwrap()]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    // SIG_IGN on SIGHUP before exec -- exactly what `nohup` itself does,
    // and what `sighup_is_already_ignored` (`main.rs`) checks for; a
    // child's own disposition set this way survives `exec` unchanged.
    unsafe {
        cmd.pre_exec(|| {
            if libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = TestChild {
        child: cmd
            .spawn()
            .expect("spawn electricity under a SIG_IGN SIGHUP"),
    };
    wait_until_running(&home);
    child.send(libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.child.try_wait().unwrap().is_none(),
        "a SIGHUP ignored on entry must never cancel the run"
    );
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
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
    // The events file got at least one write
    // (run_start, and likely a start for the root `sleep`) before the
    // second signal cut the process off -- but never a run_end line,
    // which only `EventLog::run_end` (called after `execute_root`
    // returns) would ever add.
    if events.exists() {
        let events_text = fs::read_to_string(&events).unwrap();
        assert!(
            !events_text.contains("\"run_end\""),
            "a second-signal exit must never write run_end: {events_text}"
        );
    }
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

/// Lane I (issue #469), ruling R3 on PR #472's review:
/// a SIGINT while a `use` child is running must exit 130 and leave no
/// trace of the child's own effect having run -- `VmError::Cancelled`
/// bypasses `execute_use`'s own output-mapping/retry bookkeeping
/// entirely (the same rule every other container follows), so the use
/// node itself never gets a `completed_at`, and the child's own
/// full-passthrough keys (which only ever land on a *successful*
/// child run) never appear at all -- not even a stale mid-run graft,
/// since [`run_isolated_inner`] always clears it before returning,
/// cancelled or not.
#[test]
fn sigint_during_a_use_child_exits_130_with_no_leftover_child_effects() {
    const DOC: &str = "\
effects:
  - name: wrapper
    type: use
    inline: |
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
    let home = TempHome::new("sigint-use-child");
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
    assert!(out.exists());
    assert_finally_ran(&out);
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let wrapper = &state["prime"]["wrapper"];
    assert!(
        wrapper["meta"]["completed_at"].is_null(),
        "a use node cancelled mid-child must never complete: {state}"
    );
    assert!(
        wrapper.get("slow").is_none(),
        "the child's own effect must never be merged onto the use node \
         when the run was cancelled before it finished: {state}"
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

/// `prime.x`: 2 replies; `prime.y`: 1 -- fixed counts
/// `electricity-cli/tests/leftover_export.rs`'s own
/// `LEFTOVER_EXPORT_FIXTURE` also uses, so a reader comparing the two
/// files sees the identical fixture and expected result. The
/// `test-tools`-only register hook's own `check()` call leaves these
/// fully untouched (it never dispatches `generate()`) -- the exact
/// shape `generate_scripted_corpus.py`'s own
/// `a_single_preflight_only_instance_reports_every_path_at_its_full_count`
/// merge case already verifies against Circuitry's own
/// `ScriptedAdapter.check`/`_merge_leftover_replies`, the functions
/// `cof run`'s own preflight and `atexit` hook call for a document
/// whose top-level `adapter: scripted` is never actually dispatched.
const LEFTOVER_EXPORT_FIXTURE: &str =
    "prime.x:\n  - text: one\n  - text: two\nprime.y:\n  - text: only\n";

fn expected_leftover_export_counts() -> serde_json::Value {
    serde_json::json!({"prime.x": 2, "prime.y": 1})
}

/// [`spawn`], plus the `test-tools`-only register hook
/// (`ELECTRICITY_TEST_SCRIPTED_REGISTER_REPLIES_FILE`) and the export
/// target (`CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE`) issue
/// #478's own exit-path tests below need -- *home*'s own
/// `replies.yaml` is always [`LEFTOVER_EXPORT_FIXTURE`].
fn spawn_with_leftover_export_hook(
    home: &TempHome,
    config: &Path,
    doc: &Path,
    extra_args: &[&str],
    export: &Path,
) -> TestChild {
    let replies = home.write("replies.yaml", LEFTOVER_EXPORT_FIXTURE);
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.env("ELECTRICITY_TEST_SCRIPTED_REGISTER_REPLIES_FILE", &replies);
    cmd.env("CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE", export);
    cmd.args([config.to_str().unwrap(), doc.to_str().unwrap()]);
    cmd.args(["--live-state", live_state_path(home).to_str().unwrap()]);
    cmd.args(extra_args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    TestChild {
        child: cmd
            .spawn()
            .expect("spawn electricity with the leftover-export register hook"),
    }
}

fn assert_export_has_the_fixtures_full_counts(export: &Path) {
    let text = fs::read_to_string(export)
        .unwrap_or_else(|e| panic!("export file {} should exist: {e}", export.display()));
    let got: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(got, expected_leftover_export_counts());
}

/// Issue #478's own exit-path wiring: `electricity::write_leftover_export`
/// runs on a *first* SIGINT's own handled exit (130), writing the
/// register hook's own registered full counts -- exactly as it would
/// on an ordinary exit 0/1 (`tests/leftover_export.rs`'s own
/// `test-tools`-gated coverage).
#[test]
fn sigint_exit_writes_the_registered_full_counts() {
    let home = TempHome::new("sigint-export-hook");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let export = home.path.join("leftover-export.json");
    let mut child = spawn_with_leftover_export_hook(
        &home,
        &config,
        &doc,
        &["--out", out.to_str().unwrap()],
        &export,
    );
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
    assert!(out.exists());
    assert_export_has_the_fixtures_full_counts(&export);
}

/// The same proof as above, for SIGTERM's own handled exit (143).
#[test]
fn sigterm_exit_writes_the_registered_full_counts() {
    let home = TempHome::new("sigterm-export-hook");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let export = home.path.join("leftover-export.json");
    let mut child = spawn_with_leftover_export_hook(
        &home,
        &config,
        &doc,
        &["--out", out.to_str().unwrap()],
        &export,
    );
    wait_until_running(&home);
    child.send(libc::SIGTERM);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGTERM");
    assert_eq!(status.code(), Some(143));
    assert!(out.exists());
    assert_export_has_the_fixtures_full_counts(&export);
}

/// The same proof as above, for SIGHUP's own handled exit (129).
#[test]
fn sighup_exit_writes_the_registered_full_counts() {
    let home = TempHome::new("sighup-export-hook");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let export = home.path.join("leftover-export.json");
    let mut child = spawn_with_leftover_export_hook(
        &home,
        &config,
        &doc,
        &["--out", out.to_str().unwrap()],
        &export,
    );
    wait_until_running(&home);
    child.send(libc::SIGHUP);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGHUP");
    assert_eq!(status.code(), Some(129));
    assert!(out.exists());
    assert_export_has_the_fixtures_full_counts(&export);
}

/// A *second* SIGINT's own hard `std::process::exit` skips every
/// destructor, `write_leftover_export`'s own call site included
/// (issue #431's Signals section: "no `--out`, no `run_end`" for a
/// second-signal exit; the same `arm_signals`-armed hard exit never
/// reaches `main`'s own tail at all) -- the export must stay absent
/// even though the register hook ran and an instance is sitting in
/// the registry, same as Python's own `os._exit` skipping `atexit`.
#[test]
fn a_second_sigint_still_writes_no_export_even_though_an_instance_is_registered() {
    let home = TempHome::new("double-sigint-export-hook");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", LONG_SLEEP_DOC);
    let out = home.path.join("out.json");
    let export = home.path.join("leftover-export.json");
    let mut child = spawn_with_leftover_export_hook(
        &home,
        &config,
        &doc,
        &["--out", out.to_str().unwrap()],
        &export,
    );
    wait_until_running(&home);
    child.send(libc::SIGINT);
    std::thread::sleep(Duration::from_millis(2000));
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(2))
        .expect("electricity exited after a second SIGINT");
    assert_eq!(status.code(), Some(130));
    assert!(!out.exists());
    assert!(
        !export.exists(),
        "a second signal's own hard exit must never write the export, \
         even though the register hook already ran"
    );
}
