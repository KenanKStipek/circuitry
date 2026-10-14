//! End-to-end tests against a real `electricity`, built with
//! `--features test-tools` (issue #431's lane E2 acceptance
//! criterion: "osp's ElectricityEngine ... end-to-end tests run
//! against electricity behind a feature flag"). Mirrors
//! `e2e_cof.rs`'s own shape: success, failure, an
//! invalid document, a preview-marker refusal, all three signals,
//! `osp watch` on a finished run directory, and `--log` output.
//! Issue #538's B1 adds four more documents: a scripted prompt, a
//! tree loop, a `use` path child, and a decomposition run.
//!
//! Gated on `OSP_E2E_ELECTRICITY=1`, with `electricity` on `PATH`
//! (`cargo build -p electricity-cli --features test-tools` from
//! `electricity/`, then that binary on `PATH` — `.github/workflows/
//! oscilloscope.yml`'s own step does exactly this). Every test gets
//! its own temporary `HOME`, a hard per-test timeout, and asserts
//! exit codes and final statuses — never exact timings. The
//! `scripted` adapter the prompt tests below use reads no real
//! credentials, but the four variables this repository's `CLAUDE.md`
//! names are still removed from every spawned process, the same as
//! every other e2e test here.

mod support;

use electricity_bytecode::PREVIEW_MARKER;
use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    ELECTRICITY_CONFIG, TestHome, assert_no_leftover_process, e2e_electricity_enabled, last_event,
    osp_electricity_command, wait_for_step_start, wait_with_timeout,
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
    // This success case uses the `json` provider, where `e2e_cof.rs`'s
    // own equivalent uses `echo`, so this suite never starts a real
    // process.
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
    // `fail` (the `test-tools` feature's deterministic failure, chosen
    // so this test never runs a real process) with `on_error: continue`. A
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
    // case, since the refusal check runs earlier still. `litellm` (a
    // Python-SDK-only adapter, out of scope for lane F2, issue #503)
    // stays the unbuildable example here -- the config's own default
    // adapter, `ollama`, became a real, buildable adapter in that same
    // lane, so a bare `prompt` with no `provider:` no longer hits this
    // refusal at all.
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: ask\n    type: prompt\n    provider: litellm\n    template: \"hi\"\n",
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
    assert!(stdout.contains(PREVIEW_MARKER), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 1"), "stdout:\n{stdout}");
    assert!(!run_dir.join("state.json").is_file());
    assert!(!stderr.contains("look swapped"), "stderr:\n{stderr}");
}

/// A document whose root blocks on the `test-tools` feature's own
/// `sleep` provider, long enough for every signal test below to send
/// a signal mid-flight and still have time left to observe its
/// effect (the same shape `electricity-cli`'s own `signals.rs` uses).
/// The `finally: json` step is a duplicate-✗
/// repro shape: electricity's final `end` event for `prime` and its
/// final `state.live.json` write land within the same sub-millisecond
/// window, which used to make osp's own polling loop observe both in
/// the same tick and print `prime`'s own ✗ line twice.
const LONG_SLEEP_DOC: &str = "effects:\n  - name: slow\n    type: tool\n    provider: sleep\n    params: {seconds: 30}\nfinally:\n  - name: cleanup\n    type: tool\n    provider: json\n    params:\n      mode: parse\n      input: \"\\\"done\\\"\"\n";

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

    wait_for_step_start(
        &run_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");

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

    wait_for_step_start(
        &run_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
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

    wait_for_step_start(
        &run_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(143), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 143"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");

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

    wait_for_step_start(
        &run_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGHUP);
    }

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(129), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 129"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");

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

/// Issue #538's B1, item 1 ("a scripted prompt"): `adapter: scripted`,
/// one prompt, one reply with tokens. Asserts the `prime.<name>` line
/// carries the `↑<sent> ↓<received>` suffix `oscilloscope-core::diff`'s
/// own `result_summary` prints for a prompt whose meta has
/// `tokens_sent`/`tokens_received` (never the plain `"ok"` fallback) --
/// through both `osp watch` on a finished run (no race: it renders
/// once, from the run's own complete final state) and a live `osp
/// <doc> --log` (issue #545: a prompt this fast can complete before
/// osp's own first state poll ever does, so the `--log`/TUI line for
/// it must hold rather than guess until that poll catches up).
#[test]
fn a_scripted_prompt_prints_tokens_sent_and_received() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n",
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &support::scripted_config(&replies),
    );
    let run_dir = tempfile::tempdir().unwrap();

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
    assert!(stdout.contains("prime.ask"), "stdout:\n{stdout}");
    assert!(stdout.contains("↑3 ↓5"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");

    // The same scripted prompt, launched by `osp` itself rather than
    // attached to after the fact (#545's own live race).
    let run_dir2 = work.path().join("run2");
    let child = osp_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir2)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osp");
    let (log_status, log_stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(log_status.success(), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("prime.ask"), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("↑3 ↓5"), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("■ run ok"), "stdout:\n{log_stdout}");
}

/// Issue #538's B1, item 2 ("a tree loop"): a named `flow: tree` `each`
/// loop, `max_concurrency: 2`, three items, a `json` body. Asserts
/// only the finished run's own lines -- nothing mid-run, since
/// issue #465 (electricity's own `--live-state` shows none of a
/// tree's in-flight branches before the merge, unlike `cof`'s) is out
/// of scope for this issue and a test that asserted that gap directly
/// would start failing the moment #465 is fixed.
#[test]
fn a_tree_loop_merges_every_pass() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: seed\n    type: tool\n    provider: json\n    params: {mode: parse, input: \"[1, 2, 3]\"}\n  - name: shots\n    type: loop\n    flow: tree\n    max_concurrency: 2\n    each: {in: \"prime.seed.value\", as: item}\n    body:\n      - name: step\n        type: tool\n        provider: json\n        params: {mode: parse, input: \"\\\"ok\\\"\"}\n",
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

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    for n in 0..3 {
        assert!(
            stdout.contains(&format!("prime.shots.iter_{n}.step")),
            "stdout:\n{stdout}"
        );
    }
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}

/// Issue #538's B1, item 3 ("a `use`"): a `use: {path: child.yml}`
/// child, which osp grafts into its own compiled plan
/// (`oscilloscope_core::plan::try_graft_use`). `prime.step.inner`
/// printing at all doesn't by itself prove the graft happened: an
/// observed-only child (one `try_graft_use` failed to resolve) prints
/// the exact same line from events/state alone. What only a successful
/// graft produces is the *label* on its `▶` line: `summary_for`'s own
/// `▶` fallback (`diff.rs`) reads the plan's `LeafEffectKind::label`
/// (`"tool"`) for a path the plan recognises, and the generic
/// `"effect"` fallback otherwise (`plan_kind_label`) -- confirmed
/// directly against both outcomes (a resolvable and an unresolvable
/// child) before writing this assertion.
#[test]
fn a_use_path_child_grafts_into_the_plan() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    write_doc(
        work.path(),
        "child.yml",
        "effects:\n  - name: inner\n    type: tool\n    provider: json\n    params:\n      mode: stringify\n      input: from-child\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: step\n    type: use\n    path: child.yml\n",
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
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.step.inner"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("▶ prime.step.inner  tool"),
        "the plan's own `tool` label must appear on the child's start line, \
         or it was never actually grafted into osp's compiled plan: stdout:\n{stdout}"
    );
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}

/// Issue #538's B1, item 4 ("decomposition"): the `decompose-success`
/// conformance case's document, config and scripted replies
/// (`tests/conformance/cases/decompose-success/`), inlined. The
/// decomposition machinery's own planner-check steps (`prime.big.plan`,
/// `.check`, `.result_path`, `.done`) and its chunk effects
/// (`.a`, `.b`, `.merge`) are injected at runtime -- neither is
/// declared anywhere in the document -- so none of them is in osp's
/// own compiled plan; every one renders as an observed-only row.
/// This asserts a subset of those observed-only rows
/// (`prime.big.plan`, `.a`, `.b`, `.merge`), plus the two effects the
/// document does declare and osp's plan does compile
/// (`prime.big`, `prime.downstream`).
#[test]
fn decomposition_renders_the_planner_and_chunk_rows() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSP_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        r#"prime.big.plan:
- text: '{"say": "Split into two chunks plus a merge step.", "chunks": [{"name": "a", "job": "handle the first half"}, {"name": "b", "job": "handle the second half"}], "yaml": "effects:\n  - type: prompt\n    name: a\n    template: \"Chunk A instructions.\"\n  - type: prompt\n    name: b\n    template: \"Chunk B instructions.\"\n  - type: prompt\n    name: merge\n    template: \"Combine the chunks.\"\n"}'
prime.big.a:
- text: chunk a result
  tokens_sent: 11
  tokens_received: 13
prime.big.b:
- text: chunk b result
  tokens_sent: 17
  tokens_received: 19
prime.big.merge:
- text: merged result
  tokens_sent: 23
  tokens_received: 29
"#,
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        r#"name: decompose-success
adapter: scripted
model: m
effects:
  - type: prompt
    name: big
    template: >-
      Analyze and critique the following in great depth, cross-reference
      every claim, and deduce the underlying causes, evaluate alternatives
      and justify your conclusion with explicit reasoning. Analyze and
      critique the following in great depth, cross-reference every claim,
      and deduce the underlying causes, evaluate alternatives and justify
      your conclusion with explicit reasoning. Analyze and critique the
      following in great depth, cross-reference every claim, and deduce
      the underlying causes.
  - type: tool
    name: downstream
    provider: json
    params:
      mode: stringify
      input: "{{prime.big.value}}"
"#,
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &serde_json::json!({
            "runtime": {
                "complexity": {
                    "scoring": {"enabled": true},
                    "decomposition": {
                        "enabled": true,
                        "threshold": 10,
                        "max_depth": 1,
                        "max_chunks": 5
                    }
                },
                "adapters": {
                    "scripted": {"replies_file": replies.to_string_lossy()}
                }
            }
        })
        .to_string(),
    );

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
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    for path in [
        "prime.big",
        "prime.big.plan",
        "prime.big.a",
        "prime.big.b",
        "prime.big.merge",
        "prime.downstream",
    ] {
        assert!(stdout.contains(path), "missing {path} in stdout:\n{stdout}");
    }
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}
