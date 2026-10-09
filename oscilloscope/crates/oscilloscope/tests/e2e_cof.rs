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

mod support;

use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    SCRIPTED_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process, e2e_enabled, osp_command,
    wait_with_timeout, wait_with_timeout_capturing_stdout, write_doc,
};

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

    let child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

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

    let child = osp_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

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

    let child = osp_command(&home)
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

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

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

    let child = osp_command(&home)
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

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

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

    let child = osp_command(&home)
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

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(143), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 143"), "stdout:\n{stdout}");

    assert_no_leftover_process(work.path());
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
    let child = watch.spawn().expect("spawn osp watch");
    let (watch_status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(10));

    assert!(watch_status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}
