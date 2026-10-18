//! End-to-end tests against a real `electricity`, built with
//! `--features test-tools` (issue #431's lane E2 acceptance
//! criterion: "osi's ElectricityEngine ... end-to-end tests run
//! against electricity behind a feature flag"). Mirrors
//! `e2e_cof.rs`'s own shape: success, failure, an
//! invalid document, a preview-marker refusal, all three signals,
//! `osi watch` on a finished run directory, and `--log` output.
//! Issue #538's B1 adds four more documents: a scripted prompt, a
//! tree loop, a `use` path child, and a decomposition run.
//!
//! Gated on `OSI_E2E_ELECTRICITY=1`, with `electricity` on `PATH`
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
    osi_electricity_command, wait_for_step_start, wait_with_timeout,
    wait_with_timeout_capturing_stdout, wait_with_timeout_capturing_stdout_and_stderr, write_doc,
};

#[test]
fn a_simple_run_succeeds_and_prints_the_log() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osi should exit 0, got {status:?}; stdout:\n{stdout}"
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
fn a_documents_reports_show_up_in_the_log_and_the_run_still_ends_ok() {
    // #610: mirrors e2e_cof.rs's own test of the same name -- an
    // unnamed report, a named warning report with data, and a report
    // inside both a chain loop pass and a tree loop pass, none of
    // which ever fires a start/end event, so the run's own final
    // summary reads exactly as it would with no reports at all.
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - type: report\n    message: \"starting up\"\n  - type: report\n    name: summary\n    level: warning\n    message: \"found an issue\"\n    data: {count: 2}\n  - type: tool\n    name: items\n    provider: json\n    params: {mode: parse, input: \"[1, 2]\"}\n  - type: loop\n    name: chain_items\n    each: {in: prime.items.value, as: item}\n    body:\n      - type: report\n        message: \"chain pass\"\n      - type: tool\n        name: nap\n        provider: json\n        params: {mode: parse, input: \"\\\"ok\\\"\"}\n  - type: loop\n    name: tree_items\n    flow: tree\n    each: {in: prime.items.value, as: item}\n    body:\n      - type: report\n        message: \"tree pass\"\n      - type: tool\n        name: nap\n        provider: json\n        params: {mode: parse, input: \"\\\"ok\\\"\"}\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("▸ starting up"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("▸ warning: found an issue  {\"count\":2}"),
        "stdout:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("▸ chain pass").count(),
        2,
        "one per chain pass; stdout:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("▸ tree pass").count(),
        2,
        "one per tree pass; stdout:\n{stdout}"
    );
    // A named report (`summary`) writes a state node (#605), but it
    // must never also grow its own step line.
    assert!(!stdout.contains("▶ prime.summary"), "stdout:\n{stdout}");
    assert!(!stdout.contains("✓ prime.summary"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
}

#[test]
fn on_error_continue_still_exits_ok() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("-e")
        .arg("name=World")
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osi should exit 0 (on_error: continue), got {status:?}; stdout:\n{stdout}"
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
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    // A `prompt` effect is schema-valid but refused with the preview
    // marker (issue #431's Goal: "prompt/loop/use/reflector/yield ...
    // refused before the run starts"): osi's own summary reads it the
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
/// window, which used to make osi's own polling loop observe both in
/// the same tick and print `prime`'s own ✗ line twice.
const LONG_SLEEP_DOC: &str = "effects:\n  - name: slow\n    type: tool\n    provider: sleep\n    params: {seconds: 30}\nfinally:\n  - name: cleanup\n    type: tool\n    provider: json\n    params:\n      mode: parse\n      input: \"\\\"done\\\"\"\n";

#[test]
fn a_single_sigint_forwards_and_osi_exits_130() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
fn sigterm_forwards_and_osi_exits_143() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
fn sighup_forwards_and_osi_exits_129() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(work.path(), "do.yml", LONG_SLEEP_DOC);
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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
fn watch_mirrors_a_run_osi_did_not_start() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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

    let mut watch = support::osi_command(&home);
    watch.arg("watch").arg(run_dir.path());
    let child = watch.spawn().expect("spawn osi watch");
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
/// through both `osi watch` on a finished run (no race: it renders
/// once, from the run's own complete final state) and a live `osi
/// <doc> --log` (issue #545: a prompt this fast can complete before
/// osi's own first state poll ever does, so the `--log`/TUI line for
/// it must hold rather than guess until that poll catches up).
#[test]
fn a_scripted_prompt_prints_tokens_sent_and_received() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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

    let mut watch = support::osi_command(&home);
    watch.arg("watch").arg(run_dir.path());
    let child = watch.spawn().expect("spawn osi watch");
    let (watch_status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(10));

    assert!(watch_status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.ask"), "stdout:\n{stdout}");
    assert!(stdout.contains("↑3 t ↓5 t"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");

    // The same scripted prompt, launched by `osi` itself rather than
    // attached to after the fact (#545's own live race).
    let run_dir2 = work.path().join("run2");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir2)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (log_status, log_stdout) =
        wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(log_status.success(), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("prime.ask"), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("↑3 t ↓5 t"), "stdout:\n{log_stdout}");
    assert!(log_stdout.contains("■ run ok"), "stdout:\n{log_stdout}");
}

/// Issue #545's own trigger case, now fixed by #465: a named `flow:
/// tree` `each` loop whose body runs a scripted prompt after a
/// staggered `sleep` (`0.1s`/`0.1s`/`1.5s`) -- the two fast passes'
/// own prompts finish and fire their `end` event well before the
/// loop's own last pass, and so well before the whole tree merges.
/// Before issue #465, electricity's `--live-state` carried no state
/// at all for a tree's in-flight branches until the merge, so a fast
/// pass's own `end` event always arrived more than #547's 1.1s hold
/// window ahead of any live-state write that could show its tokens,
/// and the `--log` line fell back to `ok` (the issue's own comment:
/// "all 9 remaining `ok` lines were tree-loop passes"). With #465's
/// overlay, the periodic (500ms) live-state flush shows a fast pass's
/// own finished prompt well inside that window.
#[test]
fn a_tree_loops_fast_pass_prints_tokens_before_the_loop_merges() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.shots.iter_0.ask:\n  - text: \"pass 0\"\n    tokens_sent: 1\n    tokens_received: 1\nprime.shots.iter_1.ask:\n  - text: \"pass 1\"\n    tokens_sent: 2\n    tokens_received: 2\nprime.shots.iter_2.ask:\n  - text: \"pass 2\"\n    tokens_sent: 3\n    tokens_received: 3\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: seed\n    type: tool\n    provider: json\n    params: {mode: parse, input: \"[0.05, 0.05, 3.0]\"}\n  - name: shots\n    type: loop\n    flow: tree\n    max_concurrency: 3\n    each: {in: \"prime.seed.value\", as: item}\n    body:\n      - name: wait\n        type: tool\n        provider: sleep\n        params: {seconds: {from: item}}\n      - name: ask\n        type: prompt\n        template: \"hi\"\n",
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &support::scripted_config(&replies),
    );

    let run_dir = work.path().join("run");
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");

    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    for line in ["↑1 t ↓1 t", "↑2 t ↓2 t", "↑3 t ↓3 t"] {
        assert!(stdout.contains(line), "stdout:\n{stdout}");
    }
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}

/// Issue #538's B1, item 2 ("a tree loop"): a named `flow: tree` `each`
/// loop, `max_concurrency: 2`, three items, a `json` body. Asserts
/// only the finished run's own lines -- a mid-run assertion belongs to
/// `a_tree_loops_fast_pass_prints_tokens_before_the_loop_merges`
/// below, which exercises issue #465's own overlay directly; this
/// test only needs the run to merge correctly in the end.
#[test]
fn a_tree_loop_merges_every_pass() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");

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
/// child, which osi grafts into its own compiled plan
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
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));

    assert!(status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.step.inner"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("▶ prime.step.inner  tool"),
        "the plan's own `tool` label must appear on the child's start line, \
         or it was never actually grafted into osi's compiled plan: stdout:\n{stdout}"
    );
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
}

/// Issue #538's B1, item 4 ("decomposition"): the `decompose-success`
/// conformance case's document, config and scripted replies
/// (`tests/conformance/cases/decompose-success/`), inlined. The
/// decomposition machinery's own planner-check steps (`prime.big.plan`,
/// `.check`, `.result_path`, `.done`) and its chunk effects
/// (`.a`, `.b`, `.merge`) are injected at runtime -- neither is
/// declared anywhere in the document -- so none of them is in osi's
/// own compiled plan; every one renders as an observed-only row.
/// This asserts a subset of those observed-only rows
/// (`prime.big.plan`, `.a`, `.b`, `.merge`), plus the two effects the
/// document does declare and osi's plan does compile
/// (`prime.big`, `prime.downstream`).
#[test]
fn decomposition_renders_the_planner_and_chunk_rows() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
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
    let child = osi_electricity_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
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

/// Issue #616: a container (the root, a `dynamic`) gets the scope's
/// effective `adapter`/`model` written into its own node by
/// electricity, same as any leaf's -- the default model, `llama3.1:8b`,
/// with no `adapter:`/`model:` of its own in the document and an empty
/// config. With a plan, `row_kind_for` never reaches the shape guess
/// for either (the plan already says `root`/`dynamic`), so this run's
/// own effect count is exact: three `json` steps, the `dynamic`'s one
/// `shell` child, and two more `shell` steps -- six leaves, none of
/// them a prompt. `osi watch` compiles no plan at all (`PlanTree::
/// empty()`, the same fallback this test builds by hand), so it leans
/// entirely on `RowKind::from_meta_shape`'s shape guess -- this is
/// what used to miscount the `dynamic` as a prompt leaf (its own node
/// has `meta.adapter`/`meta.model` but no `meta.provider`) and tag it
/// with the default model, on both panes.
#[test]
fn watch_counts_the_same_effects_as_a_planned_run_with_no_prompts() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        r#"effects:
  - name: step1
    type: tool
    provider: json
    params: {mode: parse, input: "1"}
  - name: step2
    type: tool
    provider: json
    params: {mode: parse, input: "2"}
  - name: step3
    type: tool
    provider: json
    params: {mode: parse, input: "3"}
  - name: wrap
    type: dynamic
    effects:
      - name: inner
        type: tool
        provider: shell
        params: {command: echo, args: ["hi"]}
  - name: step4
    type: tool
    provider: shell
    params: {command: echo, args: ["s4"]}
  - name: step5
    type: tool
    provider: shell
    params: {command: echo, args: ["s5"]}
"#,
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

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.path().join("state.json")).expect("read state.json"),
    )
    .expect("parse state.json");

    let inputs = oscilloscope_core::plan::parse_inputs(&[]).expect("no -e inputs");
    let program = oscilloscope_core::plan::compile(&doc, Some(&config), &inputs)
        .expect("the document compiles");
    let plan = oscilloscope_core::plan::PlanTree::from_program(&program);
    let no_plan = oscilloscope_core::plan::PlanTree::empty();
    let process = oscilloscope_core::model::ProcessState::Exited {
        interrupted: false,
        likely_failed: false,
        has_failure_reason: false,
        exited_at: None,
    };

    let mut planned_model = oscilloscope_core::model::RunModel::new();
    let planned = oscilloscope_core::render::build(
        "do.yml",
        "electricity",
        &plan,
        &mut planned_model,
        Some(&state),
        process.clone(),
        0.0,
        "2026-01-01T00:00:10Z",
        &[],
    );
    assert_eq!(planned.header.effects_planned, Some(6));
    assert_eq!(planned.header.effects_done, 6);

    let mut watched_model = oscilloscope_core::model::RunModel::new();
    let watched = oscilloscope_core::render::build(
        "do.yml",
        "electricity",
        &no_plan,
        &mut watched_model,
        Some(&state),
        process,
        0.0,
        "2026-01-01T00:00:10Z",
        &[],
    );
    assert_eq!(
        watched.header.effects_done, 6,
        "osi watch must count the same effects as a planned run"
    );

    for rendered in [&planned, &watched] {
        for row in rendered.rows.iter() {
            assert_ne!(
                row.dim.as_deref(),
                Some("llama3.1:8b"),
                "orchestration row {} should show no model tag: {:?}",
                row.path,
                row.dim
            );
        }
        for row in rendered.state_rows.iter() {
            assert_ne!(
                row.dim.as_deref(),
                Some("llama3.1:8b"),
                "state row {} should show no model tag: {:?}",
                row.path,
                row.dim
            );
        }
    }
}
