//! Lane H (#461) follow-up to `signals.rs` (issue #431's own acceptance
//! criteria): SIGINT mid-`loop`, chain and tree flow -- the two
//! "Cancellation tests" sub-section cases #461's own test plan called
//! for and the writer's PR left unbuilt (#466 review finding 9). A
//! fresh file, not an edit to `signals.rs` itself (lane B/D's own file,
//! `docs/spec/vm-lanes.md`): same helpers, duplicated rather than
//! shared across `tests/` binaries, same convention `signals.rs`'s own
//! module doc comment explains (process group per child, SIGKILLed on
//! `Drop`, a `--live-state` readiness probe instead of a fixed sleep).
//!
//! Run explicitly:
//! ```sh
//! cargo test -p electricity-cli --features test-tools --test signals_loop
//! ```
//! (wired into `.github/workflows/electricity.yml` alongside `signals.rs`).

#![cfg(feature = "test-tools")]

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
            "electricity-signals-loop-test-{tag}-{}",
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

/// Same readiness probe as `signals.rs`'s own `wait_until_running`:
/// `--live-state`'s first write happens well after signals are armed
/// but before `execute_root` ever starts, so its existence is firm
/// evidence the signal handler is already installed.
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

/// A named chain `each` loop of 3 sleeping passes: SIGINT mid-pass-0
/// must stop the loop before pass 1 ever starts (chain flow runs one
/// pass at a time, so "no new pass starts" here just means the loop
/// never reaches `iter_1`), exit 130, still run `finally:`, and write
/// `--out` with cof's own "cancellation bypasses `on_error`, with no
/// final `value`/`meta` write at all" shape (`core/loop.py`'s own
/// `except Exception`, never `BaseException` -- `exec::loop_::is_fatal`
/// mirrors it). The loop's own node and pass 0's own `iter_0` both
/// exist already (both are created up front, before that pass's body
/// ever dispatches), but neither ever gets a `completed_at`/`value`
/// write of its own, since cancellation during pass 0's own in-flight
/// `sleep` skips even that pass's own completion.
const CHAIN_SLEEP_LOOP_DOC: &str = "\
effects:
  - name: seed
    type: tool
    provider: json
    params: {mode: parse, input: \"[1, 2, 3]\"}
  - name: shots
    type: loop
    each: {in: \"prime.seed.value\", as: item}
    body:
      - name: step
        type: tool
        provider: sleep
        params: {seconds: 30}
finally:
  - name: cleanup
    type: tool
    provider: sleep
    params: {seconds: 5}
";

#[test]
fn sigint_mid_chain_loop_pass_exits_130_runs_finally_and_starts_no_new_pass() {
    let home = TempHome::new("chain");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", CHAIN_SLEEP_LOOP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
    assert!(out.exists());
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        !state["prime"]["cleanup"].is_null(),
        "finally: never ran: {state}"
    );
    let shots = &state["prime"]["shots"];
    assert!(
        shots["iter_0"]["step"]["meta"]["completed_at"].is_null(),
        "pass 0's own in-flight sleep must never have completed: {state}"
    );
    assert!(
        shots["iter_1"].is_null(),
        "a cancelled chain loop must start no new pass after the signal: {state}"
    );
    assert!(
        shots["value"].is_null(),
        "cancellation bypasses the loop's own final value/meta write entirely: {state}"
    );
    assert!(
        shots["meta"]["completed_at"].is_null(),
        "cancellation bypasses the loop's own final meta write entirely: {state}"
    );
}

/// A named tree `each` loop of 3 sleeping branches, `max_concurrency: 2`:
/// SIGINT while two are already running must mean the third (queued)
/// branch never starts at all (DESIGN.md's own "queued tree branches
/// never start" rule, already proven for `dynamic` in `signals.rs`'s own
/// `a_queued_tree_branch_never_starts` -- this is its `loop`-flavoured
/// twin), no merge happens (no `iter_<N>` node at all, since isolated
/// per-pass stores are only ever merged back once every pass settles
/// and nothing is cancelled), exit 130, and `finally:` still runs.
const TREE_SLEEP_LOOP_DOC: &str = "\
effects:
  - name: seed
    type: tool
    provider: json
    params: {mode: parse, input: \"[1, 2, 3]\"}
  - name: shots
    type: loop
    flow: tree
    max_concurrency: 2
    each: {in: \"prime.seed.value\", as: item}
    body:
      - name: step
        type: tool
        provider: sleep
        params: {seconds: 30}
finally:
  - name: cleanup
    type: tool
    provider: sleep
    params: {seconds: 5}
";

#[test]
fn sigint_mid_tree_loop_dispatch_exits_130_runs_finally_and_never_starts_the_queued_branch() {
    let home = TempHome::new("tree");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", TREE_SLEEP_LOOP_DOC);
    let out = home.path.join("out.json");
    let mut child = spawn(&home, &config, &doc, &["--out", out.to_str().unwrap()]);
    wait_until_running(&home);
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exited after SIGINT");
    assert_eq!(status.code(), Some(130));
    assert!(out.exists());
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        !state["prime"]["cleanup"].is_null(),
        "finally: never ran: {state}"
    );
    let shots = &state["prime"]["shots"];
    assert!(
        shots["iter_2"].is_null(),
        "the third (queued) branch must never have started: {state}"
    );
    assert!(
        shots["iter_0"].is_null() && shots["iter_1"].is_null(),
        "no merge happens at all once the run is cancelled, even for a branch that did \
         finish before the signal: {state}"
    );
    assert!(
        shots["value"].is_null(),
        "cancellation skips the merge and the loop's own final value/meta write entirely: {state}"
    );
}
