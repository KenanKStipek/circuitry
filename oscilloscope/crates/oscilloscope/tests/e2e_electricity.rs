//! End-to-end tests against a real `electricity`, built with
//! `--features test-tools` (issue #431's lane E2 acceptance
//! criterion: "osp's ElectricityEngine ... end-to-end tests run
//! against electricity behind a feature flag"). Mirrors
//! `e2e_cof.rs`'s own shape: success, failure, an
//! invalid document, a preview-marker refusal, all three signals,
//! `osp watch` on a finished run directory, and `--log` output.
//!
//! Gated on `OSP_E2E_ELECTRICITY=1`, with `electricity` on `PATH`
//! (`cargo build -p electricity-cli --features test-tools` from
//! `electricity/`, then that binary on `PATH` — `.github/workflows/
//! oscilloscope.yml`'s own step does exactly this). Every test gets
//! its own temporary `HOME`, a hard per-test timeout, and asserts
//! exit codes and final statuses — never exact timings. `electricity`
//! has no credentials of its own to strip (M0-H has no adapters that
//! would read any), but the four variables this repository's
//! `CLAUDE.md` names are still removed from every spawned process,
//! the same as every other e2e test here.

mod support;

use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    ELECTRICITY_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process,
    e2e_electricity_enabled, last_event, osp_electricity_command, wait_with_timeout,
    wait_with_timeout_capturing_stdout, wait_with_timeout_capturing_stdout_and_stderr, write_doc,
};

#[test]
fn a_simple_run_succeeds_and_prints_the_log() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // M0-H only runs `tool`/`json` (DESIGN.md §4.2): `shell` is out of
    // scope, so this success case uses the `json` provider where
    // `e2e_cof.rs`'s own equivalent uses `echo`.
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: json\n    params:\n      mode: parse\n      input: \"\\\"hi\\\"\"\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osp should exit 0, got {status:?}; stdout:\n{stdout}"
    );
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 0"), "stdout:\n{stdout}");
    // Proves `ElectricityEngine::detect` actually found, and passed,
    // both flags (the no-events fallback would still produce the
    // three assertions above from `state.json` alone at exit).
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
    // `doc.yml config.json` is the documented order (DESIGN.md §4.2):
    // `looks_swapped` must never fire on it.
    assert!(!stderr.contains("look swapped"), "stderr:\n{stderr}");
    assert!(run_dir.join("events.jsonl").is_file());
    assert!(run_dir.join("state.live.json").is_file());
}

#[test]
fn on_error_continue_still_exits_ok() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // `fail` (the `test-tools` feature's deterministic failure, never
    // `shell`, which M0-H doesn't run) with `on_error: continue`. A
    // declared, required `interface.inputs` entry, set with `-e`,
    // exercises the plan side of scope item 3: `oscilloscope_core::
    // plan::compile` must accept the same `-e` electricity itself
    // would check (`electricity::check_options`), and the plan must
    // still join against electricity's own live state/events for the
    // `(on_error: continue)` annotation below to appear at all.
    let doc = write_doc(
        work.path(),
        "do.yml",
        "interface:\n  inputs:\n    name:\n      type: string\n      required: true\neffects:\n  - name: flaky\n    type: tool\n    provider: fail\n    on_error: continue\n    params: {}\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("-e")
        .arg("name=World")
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osp should exit 0 (on_error: continue), got {status:?}; stdout:\n{stdout}"
    );
    assert!(stdout.contains("✗ prime.flaky"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 0"), "stdout:\n{stdout}");
    // The `(on_error: continue)` suffix (`diff.rs::on_error_suffix`)
    // only appears once `plan.match_path` finds this effect in a
    // compiled plan -- proof the `-e name=World` run really did
    // compile a plan and join it to electricity's own live path, not
    // just fall back to the no-plan "observations only" path, which
    // would still print `✗ prime.flaky`/`exit 0` without it.
    assert!(stdout.contains("(on_error: continue)"), "stdout:\n{stdout}");
    assert!(
        !stderr.contains("couldn't compile a plan"),
        "stderr:\n{stderr}"
    );
    assert!(!stderr.contains("look swapped"), "stderr:\n{stderr}");
}

#[test]
fn an_invalid_document_fails_and_still_writes_state() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // Schema-invalid (a `tool` effect needs a `provider` and
    // `params`): this fails in electricity's `fail!` macro (load
    // step), which still saves state before returning (`lib.rs`'s own
    // `state: Some(store.saved(...))`), so `--out` still gets written
    // -- unlike the preview-marker refusal below, which writes
    // nothing. electricity's own `{"ok":false,...}` JSON still
    // carries the error on stdout either way (DESIGN.md §4.1).
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: bad\n    type: tool\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(1), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run failed"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 1"), "stdout:\n{stdout}");
    assert!(run_dir.join("state.json").is_file());
    assert!(!stderr.contains("look swapped"), "stderr:\n{stderr}");
}

#[test]
fn unsupported_content_is_refused_as_a_pre_execution_failure() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // A `prompt` effect is schema-valid but refused with the preview
    // marker (issue #431's Goal: "prompt/loop/use/reflector/yield ...
    // refused before the run starts"): osp's own summary reads it the
    // same way as the invalid document above (`■ run failed`, exit
    // 1), but the refusal itself is the case that writes *nothing* at
    // all under the run directory -- electricity never even reaches
    // the `fail!` macro that saves state for the invalid-document
    // case, since the refusal check runs earlier still.
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(1), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run failed"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("is a preview and cannot run orchestrations yet"),
        "stdout:\n{stdout}"
    );
    assert!(stdout.contains("exit 1"), "stdout:\n{stdout}");
    assert!(!run_dir.join("state.json").is_file());
    assert!(!stderr.contains("look swapped"), "stderr:\n{stderr}");
}

/// A document whose root blocks on the `test-tools` feature's own
/// `sleep` provider, long enough for every signal test below to send
/// a signal mid-flight and still have time left to observe its
/// effect (the same shape `electricity-cli`'s own `signals.rs` uses).
const LONG_SLEEP_DOC: &str =
    "effects:\n  - name: slow\n    type: tool\n    provider: sleep\n    params: {seconds: 30}\n";

#[test]
fn a_single_sigint_forwards_and_osp_exits_130() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
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

    // A single SIGINT still writes a final snapshot (DESIGN.md §4.1's
    // own signal table): `events.jsonl`'s own last line is this run's
    // `run_end`, naming the signal that actually ended it.
    assert!(run_dir.join("events.jsonl").is_file());
    assert!(run_dir.join("state.live.json").is_file());
    let event = last_event(&run_dir.join("events.jsonl"));
    assert_eq!(event["ev"], "run_end", "{event:?}");
    assert_eq!(event["signal"], "SIGINT", "{event:?}");

    assert_no_leftover_process(work.path());
}

#[test]
fn a_second_sigint_during_cleanup_aborts_with_no_leftover_process() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // A `finally:` sleep gives the second SIGINT a real window to
    // land *during* cleanup (the same shape `e2e_cof.rs`'s own
    // equivalent test uses, and `electricity-cli`'s own
    // `signals.rs`).
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: sleep\n    params: {seconds: 30}\nfinally:\n  - name: cleanup\n    type: tool\n    provider: sleep\n    params: {seconds: 10}\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
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

    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");

    assert_no_leftover_process(work.path());
}

#[test]
fn sigterm_forwards_and_osp_exits_143() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
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

    assert!(run_dir.join("events.jsonl").is_file());
    assert!(run_dir.join("state.live.json").is_file());
    let event = last_event(&run_dir.join("events.jsonl"));
    assert_eq!(event["ev"], "run_end", "{event:?}");
    assert_eq!(event["signal"], "SIGTERM", "{event:?}");

    assert_no_leftover_process(work.path());
}

#[test]
fn sighup_forwards_and_osp_exits_129() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let pid = child.id() as i32;

    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGHUP);
    }

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(129), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 129"), "stdout:\n{stdout}");

    assert!(run_dir.join("events.jsonl").is_file());
    assert!(run_dir.join("state.live.json").is_file());
    let event = last_event(&run_dir.join("events.jsonl"));
    assert_eq!(event["ev"], "run_end", "{event:?}");
    assert_eq!(event["signal"], "SIGHUP", "{event:?}");

    assert_no_leftover_process(work.path());
}

#[test]
fn watch_mirrors_a_run_osp_did_not_start() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: json\n    params:\n      mode: parse\n      input: \"\\\"hi\\\"\"\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let mut electricity = Command::new("electricity");
    electricity
        .arg(&config)
        .arg(&doc)
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
        electricity.env_remove(key);
    }
    let status = wait_with_timeout(
        electricity.spawn().expect("spawn electricity"),
        Duration::from_secs(30),
    );
    assert!(status.success());

    let mut watch = support::osp_command(&home);
    watch.arg("watch").arg(run_dir.path());
    let child = watch.spawn().expect("spawn osp watch");
    let (watch_status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(10));

    assert!(watch_status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}
