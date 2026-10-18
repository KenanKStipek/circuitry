//! C25 re-run with a prompt (issue #538's B3): `signals.rs` and
//! `signals_loop.rs` both re-run C25 with the `sleep` test-tool, never
//! a real model call. This file is C25's prompt-effect twin --
//! `adapter: claude_code`, with a fake `claude` (the shape of
//! `tests/conformance/cases/c520-claude-code-timeout/fakes/claude`)
//! that reads stdin and then sleeps far longer than any test here
//! waits, so a signal -- never the fake's own choice -- is always
//! what ends it.
//!
//! `#![cfg(unix)]`, not `#![cfg(feature = "test-tools")]`: the
//! `claude_code` adapter is always registered (`main.rs::build_registry`
//! runs unconditionally), so `cargo test --workspace` builds and runs
//! this file with no feature flag at all, unlike `signals.rs`/
//! `signals_loop.rs`, which need the `sleep`/`fail` test-tools.
//!
//! Every fake `claude`'s own `--model` argument carries a marker
//! unique to its own test (this test binary's pid plus a counter,
//! [`unique_marker_and_sleep_secs`]), and its `sleep` itself blocks
//! for a duration drawn from that very same counter: the marker alone
//! never survives into the fake's own blocking state (`sh`'s own argv
//! is replaced the moment it execs the script's final `sleep N`, same
//! as `process_tool_signals.rs`'s own fakes), so every "no leftover
//! process"/"no new pass starts" assertion below `pgrep -f`s *both*
//! the marker and the exact `sleep N` pattern -- never a bare
//! `pgrep -f claude` or `pgrep sleep` -- issue #491 was a flake from
//! exactly that kind of unscoped match racing a concurrent run of
//! this same file. [`FakeGuard`] `pkill -f`s both patterns when a
//! test ends, including by a panicking assertion, since the fake's
//! own process group is its own session (`setsid`, same as the shell
//! tool's own child) and so is never reached by [`TestChild::drop`]'s
//! group-scoped `SIGKILL` on the tracked `electricity` process alone.
//!
//! Run explicitly:
//! ```sh
//! cargo test -p electricity-cli --test signals_prompt
//! ```
//! This file needs no feature flag, so it runs under the ordinary
//! `cargo test --workspace` step CI already runs for every other
//! unflagged test (`.github/workflows/electricity.yml`'s "cargo
//! test" step) -- it is not named individually there, unlike
//! `signals.rs`/`signals_loop.rs`, which need `--features test-tools`.

#![cfg(unix)]

use std::fs;
use std::io::Write;
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
            "electricity-signals-prompt-test-{tag}-{}",
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
/// pid (`process_tool_signals.rs`'s own [same-named helper], mirrored
/// here): every call, even from concurrently running tests in this
/// same file, gets a marker -- and a `sleep` duration derived from the
/// very same draw -- no other call, in this process or any other,
/// will ever produce.
static MARKER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A `--model` marker string and a `sleep` duration, both unique to
/// this call: the marker alone can't identify the fake once it's
/// actually blocking (see [`write_fake_claude`]), so every assertion
/// that needs to find or wait on the fake uses the duration-derived
/// pattern from [`sleep_pattern`] instead, the same shape
/// `process_tool_signals.rs`'s own `unique_sleep_secs` uses.
fn unique_marker_and_sleep_secs() -> (String, u64) {
    let n = MARKER_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id() as u64;
    (
        format!("electricity-signals-prompt-{pid}-{n}"),
        pid * 1_000 + n,
    )
}

/// A `pgrep -f`/`pkill -f` pattern that matches this exact `sleep`
/// duration and nothing else (`process_tool_signals.rs`'s own
/// `sleep_pattern`): the trailing alternation anchors the match to a
/// following space or end of line, so marker `500` can never match
/// inside marker `5001`.
fn sleep_pattern(sleep_secs: u64) -> String {
    format!("sleep {sleep_secs}( |$)")
}

/// A fake `claude` (the shape of `tests/conformance/cases/
/// c520-claude-code-timeout/fakes/claude`): reads the prompt from
/// stdin so the adapter's own write never blocks, then sleeps for
/// *sleep_secs* -- always far longer than any test here waits, so a
/// signal, never the fake's own choice, is always what ends it. A
/// POSIX shell execs its own last statement in place rather than
/// forking for it (confirmed against both `/bin/bash` and
/// `/bin/dash`, the same as `process_tool_signals.rs`'s own fakes),
/// so once this script reaches `sleep`, *that* process's entire
/// command line is `sleep {sleep_secs}` -- the `--model` marker from
/// its own invocation is gone from argv by then, which is exactly why
/// *sleep_secs* -- not the marker -- has to be the thing every
/// "still running"/"now gone" check below actually looks for.
fn write_fake_claude(dir: &Path, sleep_secs: u64) -> PathBuf {
    let path = dir.join("claude");
    // Written by a short-lived child process rather than by this test
    // process: if another test thread forked while this process held
    // the file open for writing, the forked child would hold that
    // handle until its own exec, and exec'ing the script in the
    // meantime fails with ETXTBSY ("Text file busy") on Linux. A
    // child's handle is never inherited by this process's forks.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 700 \"$1\"")
        .arg("sh")
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\ncat >/dev/null\nsleep {sleep_secs}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "writing the fake failed");
    path
}

fn pgrep_matches(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false)
}

/// Polls `pgrep -f` for *pattern* until it matches at least once, up
/// to *timeout* -- firm evidence the fake `claude` itself has reached
/// its own blocking `sleep` (not merely that `electricity` has
/// started), since a signal sent any earlier could land before
/// `claude_code`'s own child even exists, or while it's still reading
/// stdin.
fn wait_for_fake_claude(pattern: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pgrep_matches(pattern) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("the fake claude matching {pattern:?} never started within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills every process still matching this run's own marker or its
/// own `sleep` pattern, unconditionally, when the test that owns it
/// ends -- including by a panicking assertion
/// (`process_tool_signals.rs`'s own `SleepGuard`, mirrored here). The
/// fake's own child gets its own session (`setsid`, same as the shell
/// tool's own child -- see `electricity-tools::process`), so it is
/// **not** a member of this test's own `electricity` child's process
/// group: [`TestChild::drop`], which `SIGKILL`s only that group, can
/// never reach it. Without this, a failed assertion before the
/// "must be gone" check runs at all would leave the fake sleeping for
/// up to an hour.
struct FakeGuard {
    patterns: Vec<String>,
}

impl FakeGuard {
    fn new(marker: &str, sleep_secs: u64) -> Self {
        FakeGuard {
            patterns: vec![marker.to_string(), sleep_pattern(sleep_secs)],
        }
    }
}

impl Drop for FakeGuard {
    fn drop(&mut self) {
        for pattern in &self.patterns {
            let _ = Command::new("pkill").args(["-f", pattern]).status();
        }
    }
}

fn live_state_path(home: &TempHome) -> PathBuf {
    home.path.join("live-state.json")
}

fn spawn(home: &TempHome, config: &Path, doc: &Path, fakes_dir: &Path, out: &Path) -> TestChild {
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            fakes_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.args([config.to_str().unwrap(), doc.to_str().unwrap()]);
    cmd.args(["--live-state", live_state_path(home).to_str().unwrap()]);
    cmd.args(["--out", out.to_str().unwrap()]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0);
    let child = cmd.spawn().expect("spawn electricity");
    TestChild { child }
}

/// Mirrors `signals.rs`'s own readiness probe: `--live-state`'s first
/// write happens well after signal handlers are armed but before
/// `execute_root` ever starts.
fn wait_until_running(home: &TempHome) {
    let path = live_state_path(home);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if Instant::now() >= deadline {
            panic!("electricity never wrote its own first --live-state within 10s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn single_prompt_doc(marker: &str) -> String {
    format!(
        "adapter: claude_code\n\
         effects:\n\
         \x20 - name: ask\n\
         \x20   type: prompt\n\
         \x20   model: \"{marker}\"\n\
         \x20   template: \"hi\"\n\
         finally:\n\
         \x20 - name: cleanup\n\
         \x20   type: tool\n\
         \x20   provider: json\n\
         \x20   params:\n\
         \x20     mode: parse\n\
         \x20     input: \"\\\"done\\\"\"\n"
    )
}

fn signal_test(tag: &str, signum: i32, want_exit: i32) {
    let (marker, sleep_secs) = unique_marker_and_sleep_secs();
    let _cleanup = FakeGuard::new(&marker, sleep_secs);
    let home = TempHome::new(tag);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &single_prompt_doc(&marker));
    write_fake_claude(&home.path, sleep_secs);
    let out = home.path.join("out.json");

    let mut child = spawn(&home, &config, &doc, &home.path, &out);
    wait_until_running(&home);
    wait_for_fake_claude(&sleep_pattern(sleep_secs), Duration::from_secs(10));
    child.send(signum);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits after the signal");
    assert_eq!(status.code(), Some(want_exit));

    assert!(out.exists());
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        !state["prime"]["cleanup"].is_null(),
        "finally: never ran: {state}"
    );
    assert!(
        !state["runtime"]["last_run"]["completed_at"].is_null(),
        "runtime.last_run.completed_at must be set: {state}"
    );

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&marker) && !pgrep_matches(&sleep_pattern(sleep_secs)),
        "the fake claude carrying marker {marker:?} must be gone after cancellation"
    );
}

#[test]
fn sigint_during_a_prompt_exits_130_runs_finally_and_kills_the_fake() {
    signal_test("sigint", libc::SIGINT, 130);
}

#[test]
fn sigterm_during_a_prompt_exits_143_runs_finally_and_kills_the_fake() {
    signal_test("sigterm", libc::SIGTERM, 143);
}

/// The same prompt inside a named chain `each` loop of three passes
/// (`signals_loop.rs`'s own shape): SIGINT mid-pass-0 must stop the
/// loop before pass 1 ever starts a new `claude` of its own.
fn chain_loop_prompt_doc(marker: &str) -> String {
    format!(
        "adapter: claude_code\n\
         effects:\n\
         \x20 - name: seed\n\
         \x20   type: tool\n\
         \x20   provider: json\n\
         \x20   params: {{mode: parse, input: \"[1, 2, 3]\"}}\n\
         \x20 - name: shots\n\
         \x20   type: loop\n\
         \x20   each: {{in: \"prime.seed.value\", as: item}}\n\
         \x20   body:\n\
         \x20     - name: ask\n\
         \x20       type: prompt\n\
         \x20       model: \"{marker}\"\n\
         \x20       template: \"hi\"\n\
         finally:\n\
         \x20 - name: cleanup\n\
         \x20   type: tool\n\
         \x20   provider: json\n\
         \x20   params:\n\
         \x20     mode: parse\n\
         \x20     input: \"\\\"done\\\"\"\n"
    )
}

#[test]
fn sigint_mid_chain_loop_prompt_exits_130_runs_finally_and_starts_no_new_pass() {
    let (marker, sleep_secs) = unique_marker_and_sleep_secs();
    let _cleanup = FakeGuard::new(&marker, sleep_secs);
    let home = TempHome::new("chain");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &chain_loop_prompt_doc(&marker));
    write_fake_claude(&home.path, sleep_secs);
    let out = home.path.join("out.json");

    let mut child = spawn(&home, &config, &doc, &home.path, &out);
    wait_until_running(&home);
    wait_for_fake_claude(&sleep_pattern(sleep_secs), Duration::from_secs(10));
    child.send(libc::SIGINT);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .expect("electricity exits after SIGINT");
    assert_eq!(status.code(), Some(130));

    assert!(out.exists());
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        !state["prime"]["cleanup"].is_null(),
        "finally: never ran: {state}"
    );
    assert!(
        !state["runtime"]["last_run"]["completed_at"].is_null(),
        "runtime.last_run.completed_at must be set: {state}"
    );
    assert!(
        !state["prime"]["shots"]["iter_0"].is_null(),
        "pass 0 must actually have started under iter_0, or the next \
         assertion (no iter_1) would trivially pass with no pass ever \
         having run at all: {state}"
    );
    assert!(
        state["prime"]["shots"]["iter_0"]["ask"]["meta"]["completed_at"].is_null(),
        "pass 0's own in-flight prompt must never have completed: {state}"
    );
    assert!(
        state["prime"]["shots"]["iter_1"].is_null(),
        "a cancelled chain loop must start no new pass after the signal: {state}"
    );

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&marker) && !pgrep_matches(&sleep_pattern(sleep_secs)),
        "the fake claude carrying marker {marker:?} must be gone after cancellation"
    );
}
