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
//! [`unique_marker`]): the "no leftover process" and "no new pass
//! starts" assertions below `pgrep -f` that exact marker, never a
//! bare `pgrep -f claude` or `pgrep sleep` -- issue #491 was a flake
//! from exactly that kind of unscoped match racing a concurrent run
//! of this same file.
//!
//! Run explicitly:
//! ```sh
//! cargo test -p electricity-cli --test signals_prompt
//! ```
//! (wired into `.github/workflows/electricity.yml` alongside
//! `signals.rs`/`signals_loop.rs`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
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
/// same file, gets a marker no other call -- in this process or any
/// other -- will ever produce.
static MARKER_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_marker() -> String {
    let n = MARKER_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("electricity-signals-prompt-{}-{n}", std::process::id())
}

/// A fake `claude` (the shape of `tests/conformance/cases/
/// c520-claude-code-timeout/fakes/claude`): reads the prompt from
/// stdin so the adapter's own write never blocks, then sleeps for an
/// hour regardless of its own arguments. Every test here kills it with
/// a real signal long before that, through electricity's own
/// cancellation (`run_agent_cli`'s dropped future kills the whole
/// process group) -- never through the fake choosing to exit on its
/// own.
fn write_fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("claude");
    fs::write(&path, "#!/bin/sh\ncat >/dev/null\nsleep 3600\n").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn pgrep_matches(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false)
}

/// Polls `pgrep -f marker` until it matches at least once, up to
/// *timeout* -- firm evidence the fake `claude` itself has actually
/// started (not merely that `electricity` has), since a signal sent
/// any earlier could land before `claude_code`'s own child even
/// exists.
fn wait_for_fake_claude(marker: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if pgrep_matches(marker) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("the fake claude carrying marker {marker:?} never started within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
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
    let marker = unique_marker();
    let home = TempHome::new(tag);
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &single_prompt_doc(&marker));
    write_fake_claude(&home.path);
    let out = home.path.join("out.json");

    let mut child = spawn(&home, &config, &doc, &home.path, &out);
    wait_until_running(&home);
    wait_for_fake_claude(&marker, Duration::from_secs(10));
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
        !pgrep_matches(&marker),
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
    let marker = unique_marker();
    let home = TempHome::new("chain");
    let config = home.write("config.json", "{}");
    let doc = home.write("doc.yml", &chain_loop_prompt_doc(&marker));
    write_fake_claude(&home.path);
    let out = home.path.join("out.json");

    let mut child = spawn(&home, &config, &doc, &home.path, &out);
    wait_until_running(&home);
    wait_for_fake_claude(&marker, Duration::from_secs(10));
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
        state["prime"]["shots"]["iter_1"].is_null(),
        "a cancelled chain loop must start no new pass after the signal: {state}"
    );

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !pgrep_matches(&marker),
        "the fake claude carrying marker {marker:?} must be gone after cancellation"
    );
}
