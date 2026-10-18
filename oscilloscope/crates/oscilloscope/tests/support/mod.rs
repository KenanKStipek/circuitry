//! Shared helpers for every `cof` end-to-end test (`e2e_cof.rs`,
//! `e2e_tui_pty.rs`): both need the same temporary `HOME`, stripped
//! credentials, hard timeouts and "nothing survived" check (issue
//! #424's Tests section; #434's pseudo-terminal test reuses it
//! rather than drifting its own copy).
//!
//! A `tests/support/mod.rs` (not a top-level `tests/support.rs`) so
//! Cargo never treats this as its own test binary — only `mod
//! support;` from an actual test file compiles it at all. Each test
//! binary that does gets its own copy, and no one caller uses every
//! function here, hence the blanket `dead_code` allow below rather
//! than one per unused-somewhere item.

#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub fn e2e_enabled() -> bool {
    if std::env::var_os("OSI_E2E_COF").is_none() {
        return false;
    }
    // P2-3: `OSI_E2E_COF=1` with no `cof` on `PATH` is a broken CI
    // job, not a reason to run zero tests and report green — every
    // test below used to read this the same as the env var simply
    // being unset at all and quietly skip.
    if which("cof").is_none() {
        panic!("OSI_E2E_COF=1 but `cof` is not on PATH");
    }
    true
}

/// The same shape as [`e2e_enabled`], for issue #431's lane E2: these
/// tests need an `electricity` built with `--features test-tools`
/// (`cargo build -p electricity-cli --features test-tools` from
/// `electricity/`) on `PATH`, since the `sleep`/`fail` providers below
/// only exist in that build (never in a release one).
pub fn e2e_electricity_enabled() -> bool {
    if std::env::var_os("OSI_E2E_ELECTRICITY").is_none() {
        return false;
    }
    if which("electricity").is_none() {
        panic!("OSI_E2E_ELECTRICITY=1 but `electricity` is not on PATH");
    }
    true
}

/// `default_adapter`/`default_model` are Circuitry's own config keys
/// (`cli/config.py`); a bare `"adapter": "scripted"` is an unknown key
/// the config loader silently ignores, so every one of these runs
/// used to fall back to the built-in `ollama` default instead (K1).
pub const SCRIPTED_CONFIG: &str =
    r#"{"default_adapter":"scripted","default_model":"scripted-model"}"#;

/// Every electricity e2e test that dispatches no prompt effect at all
/// (the `tool`-only and signal tests below) uses this empty config --
/// the same one `electricity/crates/electricity/tests/run_wiring.rs`'s
/// own tests already use. A test that does dispatch a prompt effect
/// (issue #538's B1) builds its own config instead, with
/// [`scripted_config`].
pub const ELECTRICITY_CONFIG: &str = "{}";

/// A `runtime.adapters.scripted.replies_file` config pointing at
/// *replies_path* (an absolute path in the test's own temp directory),
/// for an electricity e2e test whose document sets its own
/// document-level `adapter: scripted`/`model:` (issue #538's B1): no
/// `default_adapter`/`default_model` of its own, since the document
/// already names both.
pub fn scripted_config(replies_path: &Path) -> String {
    serde_json::json!({
        "runtime": {
            "adapters": {
                "scripted": {"replies_file": replies_path.to_string_lossy()}
            }
        }
    })
    .to_string()
}

/// How long every signal test waits before sending its first signal
/// (a cold process start can legitimately take longer than this on a
/// shared, busy machine; still well under a human's own first
/// Ctrl-C).
pub const SIGNAL_DELAY: Duration = Duration::from_millis(1500);

pub fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| p.is_file())
    })
}

pub struct TestHome {
    dir: tempfile::TempDir,
}

impl TestHome {
    pub fn new() -> Self {
        TestHome {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub fn osi_command(home: &TestHome) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osi"));
    cmd.env("HOME", home.path());
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
    ] {
        cmd.env_remove(key);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

/// [`osi_command`], with `--engine electricity` already appended
/// (issue #431's lane E2) -- every one of this crate's electricity
/// e2e tests opts into it the same way, so each one doesn't repeat
/// the same two-argument tail.
pub fn osi_electricity_command(home: &TestHome) -> Command {
    let mut cmd = osi_command(home);
    cmd.arg("--engine").arg("electricity");
    cmd
}

/// Waits for `child` with a hard timeout, killing it (and panicking)
/// rather than ever hanging a CI job.
pub fn wait_with_timeout(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("e2e test exceeded its {timeout:?} hard timeout; osi was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `wait_with_timeout`, with `child`'s own stdout read on its own
/// thread rather than in this one (P2-3): reading concurrently with
/// the wait means a kill on timeout closes the pipe (EOF) and
/// unblocks the read too, instead of hanging on it with no timeout of
/// its own at all.
pub fn wait_with_timeout_capturing_stdout(
    mut child: Child,
    timeout: Duration,
) -> (std::process::ExitStatus, String) {
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    // Issue #433's comments: every caller of this helper spawns an
    // `osi` whose own stderr is piped too (`osi_command`), and this
    // helper otherwise never reads it -- a child that writes more
    // than a pipe buffer's worth to stderr would block on the write
    // end forever, with no timeout of its own. Drained on its own
    // thread and discarded; a caller that needs the text itself uses
    // `wait_with_timeout_capturing_stdout_and_stderr` instead.
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (stderr_done_tx, stderr_done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        let _ = stderr_done_tx.send(());
    });
    let status = wait_with_timeout(child, timeout);
    let stdout = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stdout reader thread did not finish>"));
    let _ = stderr_done_rx.recv_timeout(Duration::from_secs(5));
    (status, stdout)
}

/// [`wait_with_timeout_capturing_stdout`], with `child`'s stderr
/// captured the same way alongside it -- the electricity e2e tests
/// (issue #431's lane E2, scope item 1) need stderr to assert the
/// "this electricity has no --events" notice did *not* print, which
/// `wait_with_timeout_capturing_stdout` alone can't see (it leaves
/// stderr piped but never read).
pub fn wait_with_timeout_capturing_stdout_and_stderr(
    mut child: Child,
    timeout: Duration,
) -> (std::process::ExitStatus, String, String) {
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        let _ = stderr_tx.send(buf);
    });
    let status = wait_with_timeout(child, timeout);
    let stdout = stdout_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stdout reader thread did not finish>"));
    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stderr reader thread did not finish>"));
    (status, stdout, stderr)
}

/// Polls *events_path* (an `events.jsonl`, possibly not created yet)
/// until it carries a `start` event for *step_path*, bounded at
/// *timeout*, panicking with a clear message if it never does -- a
/// signal test must send its signal only once the long-running step
/// it means to interrupt has actually started. In CI a signal sent
/// after only a fixed delay could still land before the engine had
/// produced any state at all, making the test's own assertions (e.g.
/// `✗ prime` appearing exactly once) a coin flip rather than
/// deterministic.
pub fn wait_for_step_start(events_path: &Path, step_path: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(events_path) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("ev").and_then(|v| v.as_str()) == Some("start")
                    && value.get("path").and_then(|v| v.as_str()) == Some(step_path)
                {
                    return;
                }
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "{events_path:?} never carried a start event for {step_path:?} within {timeout:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The last non-empty line of *path* (an `events.jsonl`), parsed as
/// JSON -- every electricity signal e2e test uses this to check the
/// stream's own final `run_end` line carries the signal that actually
/// ended the run, not just that osi's own summary line did.
pub fn last_event(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("couldn't read {}: {e}", path.display()));
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_else(|| panic!("{} has no event lines", path.display()));
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{} last line is not JSON: {e}", path.display()))
}

pub fn write_doc(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// No process whose own argv names `needle` (a test's unique work
/// directory, or run directory, that the doc/config paths `cof` and
/// anything it spawned in turn were invoked with) should still exist
/// once osi has exited. A plain `HOME` path doesn't work as the
/// needle: it is only ever set as an environment variable, which
/// never appears in a process's own argv, so a `pgrep -f` against it
/// can never match anything regardless of whether a process actually
/// survived (#424 review finding F10).
/// Every `osi-<pid>-<nanos>` entry directly under the real temp
/// directory right now (`unique_run_dir`'s own default-`--out-dir`
/// naming) -- a before/after diff around one test's own osi spawn(s)
/// (comment 1 on #433), rather than a bare "is it empty" check on the
/// whole temp directory, since other processes on the same machine
/// may have their own `osi-*` directories there at the same time.
pub fn snapshot_osi_dirs() -> std::collections::HashSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return std::collections::HashSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("osi-"))
        })
        .collect()
}

/// Asserts this test's own osi spawn(s) left no *new* `osi-*` run
/// directory behind in the real temp directory, compared to a
/// [`snapshot_osi_dirs`] taken before they ran -- every e2e test that
/// spawns `osi run` passes its own `--out-dir` precisely so this never
/// finds one; it exists to catch a future test that forgets to.
pub fn assert_no_new_osi_dirs(before: &std::collections::HashSet<PathBuf>) {
    let after = snapshot_osi_dirs();
    let new: Vec<&PathBuf> = after.difference(before).collect();
    assert!(
        new.is_empty(),
        "this test left new osi-* run director{} behind in {:?}: {new:?} \
         -- every osi run in an e2e test must pass its own --out-dir",
        if new.len() == 1 { "y" } else { "ies" },
        std::env::temp_dir()
    );
}

pub fn assert_no_leftover_process(needle: &Path) {
    std::thread::sleep(Duration::from_millis(300));
    let ps = Command::new("pgrep")
        .arg("-f")
        .arg(needle.display().to_string())
        .output()
        .expect("pgrep should be available");
    assert!(
        ps.stdout.is_empty(),
        "a process matching {needle:?} survived osi's exit: {}",
        String::from_utf8_lossy(&ps.stdout)
    );
}
