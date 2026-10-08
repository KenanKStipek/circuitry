//! End-to-end tests against a real `cof` (issue #424's Tests section):
//! enabled only with `OSP_E2E_COF=1` and `cof` on `PATH`. Each test
//! gets its own temporary `HOME`, the scripted adapter (no network, no
//! credentials), a hard per-test timeout, and asserts exit codes and
//! final statuses — never exact timings.
//!
//! `cof` is never run with the owner's real credentials: the four
//! variables this repository's `CLAUDE.md` names are stripped from
//! every spawned process's environment regardless of what the test
//! runner's own environment carries.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var_os("OSP_E2E_COF").is_some() && which("cof").is_some()
}

/// `default_adapter`/`default_model` are Circuitry's own config keys
/// (`cli/config.py`); a bare `"adapter": "scripted"` is an unknown key
/// the config loader silently ignores, so every one of these runs
/// used to fall back to the built-in `ollama` default instead (K1).
/// None of today's e2e docs prompt at all, so this makes no observable
/// difference yet — it is here so a future prompt-effect e2e test
/// doesn't inherit the same silent miss.
const SCRIPTED_CONFIG: &str = r#"{"default_adapter":"scripted","default_model":"scripted-model"}"#;

/// How long every signal test waits before sending its first signal.
/// `osp` registers its signal handlers before anything else in
/// `do_run` (F13), but that is a guarantee about *osp's own code*, not
/// about how long the OS takes to finish loading and starting the
/// process at all — a cold page cache under heavy memory pressure (a
/// real, observed condition on a shared, multi-tenant dev machine) can
/// push that past what used to be a merely-generous 500ms, and a
/// signal arriving before `main` even runs always hits the OS default
/// disposition no matter how early application code registers a
/// handler. 1.5s is still well under what a human's own first Ctrl-C
/// takes in practice.
const SIGNAL_DELAY: Duration = Duration::from_millis(1500);

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| p.is_file())
    })
}

struct TestHome {
    dir: tempfile::TempDir,
}

impl TestHome {
    fn new() -> Self {
        TestHome {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

fn osp_command(home: &TestHome) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osp"));
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

/// Waits for `child` with a hard timeout, killing it (and logging a
/// panic) rather than ever hanging a CI job.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("e2e test exceeded its {timeout:?} hard timeout; osp was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_doc(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn a_simple_run_succeeds_and_prints_the_log() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = wait_with_timeout(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osp should exit 0, got {status:?}; stdout:\n{stdout}"
    );
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 0"), "stdout:\n{stdout}");
}

#[test]
fn on_error_continue_still_exits_ok() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: flaky\n    type: tool\n    provider: shell\n    on_error: continue\n    params:\n      command: ls\n      args: [\"/nonexistent-osp-e2e-path\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = wait_with_timeout(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osp should exit 0 (on_error: continue), got {status:?}; stdout:\n{stdout}"
    );
    assert!(stdout.contains("✗ prime.flaky"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 0"), "stdout:\n{stdout}");
}

#[test]
fn a_single_sigint_forwards_and_osp_exits_130() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let pid = child.id() as i32;

    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = wait_with_timeout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");

    assert_no_leftover_process(work.path());
}

#[test]
fn a_second_sigint_during_cleanup_aborts_with_no_leftover_process() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // A `finally:` sleep gives the second SIGINT a real, generous
    // window to land *during* cleanup — without one, `tail -f
    // /dev/null` dies so fast from the first SIGINT alone that the
    // second has nothing left to interrupt, making the exact landing
    // moment a coin flip under whatever load the test runner is under
    // (this is the same shape issue #424's review flagged for the
    // golden fixtures, F12, fixed there with the same kind of sleep).
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\nfinally:\n  - name: cleanup\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"10\"]\n      allowed_commands: [\"sleep\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let pid = child.id() as i32;

    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    std::thread::sleep(Duration::from_millis(500));
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = wait_with_timeout(child, Duration::from_secs(30));

    // The second SIGINT, landing while the `finally:` sleep is still
    // running, makes `cof` call `os._exit` at once (`cli/interrupts
    // .py`) — no final write, the same abort DESIGN.md §1.1 measures.
    // osp's own exit code is still the engine's (130); the real point
    // of this test, per issue #424's review, is that forwarding a
    // *second* signal is exercised end to end at all, and still leaves
    // nothing running regardless of which of the two paths the engine
    // took.
    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");

    assert_no_leftover_process(work.path());
}

#[test]
fn sigterm_forwards_and_osp_exits_143() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let pid = child.id() as i32;

    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let status = wait_with_timeout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(143), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 143"), "stdout:\n{stdout}");

    assert_no_leftover_process(work.path());
}

/// No process whose own argv names this test's unique work directory
/// (the doc/config paths `cof`, and anything it spawned in turn, were
/// invoked with) should still exist once osp has exited. `home.path()`
/// doesn't work as the needle here: it is only ever set as an
/// environment variable, which never appears in a process's own argv,
/// so a `pgrep -f` against it can never match anything regardless of
/// whether a process actually survived (#424 review finding F10).
fn assert_no_leftover_process(work: &Path) {
    std::thread::sleep(Duration::from_millis(300));
    let ps = Command::new("pgrep")
        .arg("-f")
        .arg(work.display().to_string())
        .output()
        .expect("pgrep should be available");
    assert!(
        ps.stdout.is_empty(),
        "a process matching this test's own work dir survived osp's exit: {}",
        String::from_utf8_lossy(&ps.stdout)
    );
}

#[test]
fn watch_mirrors_a_run_osp_did_not_start() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut cof = Command::new("cof");
    cof.arg("run")
        .arg(&doc)
        .arg("--config")
        .arg(&config)
        .arg("--quiet")
        .arg("--live-state")
        .arg(run_dir.path().join("state.live.json"))
        .arg("--out")
        .arg(run_dir.path().join("state.json"))
        .env("HOME", home.path())
        .current_dir(work.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
    ] {
        cof.env_remove(key);
    }
    let status = wait_with_timeout(cof.spawn().expect("spawn cof"), Duration::from_secs(30));
    assert!(status.success());

    let mut watch = osp_command(&home);
    watch.arg("watch").arg(run_dir.path());
    let mut child = watch.spawn().expect("spawn osp watch");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let watch_status = wait_with_timeout(child, Duration::from_secs(10));

    assert!(watch_status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}
