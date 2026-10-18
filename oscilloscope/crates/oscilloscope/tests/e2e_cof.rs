//! End-to-end tests against a real `cof` (issue #424's Tests section):
//! enabled only with `OSI_E2E_COF=1` and `cof` on `PATH`. Each test
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
    SCRIPTED_CONFIG, TestHome, assert_no_leftover_process, assert_no_new_osi_dirs, e2e_enabled,
    osi_command, scripted_config, snapshot_osi_dirs, wait_for_step_start, wait_with_timeout,
    wait_with_timeout_capturing_stdout_and_stderr, write_doc,
};

#[test]
fn a_simple_run_succeeds_and_prints_the_log() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    // Issue #433's comments: a default `--out-dir` leaves an `osi-*`
    // run directory in the real `$TMPDIR` -- every e2e test must give
    // osi its own, inside this test's own temp dir. Checked below,
    // not just given one here: comment 1 on #433 asked for an actual
    // assertion that none appears, not only the fix.
    let osi_dirs_before = snapshot_osi_dirs();
    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
    assert_no_new_osi_dirs(&osi_dirs_before);
}

#[test]
fn a_documents_reports_show_up_in_the_log_and_the_run_still_ends_ok() {
    // #610: an unnamed report, a named warning report with data, and
    // a report inside both a chain loop pass and a tree loop pass --
    // none of them ever fires a start/end event, so the run's own
    // final summary must read exactly as it would with no reports at
    // all (#605's own "never counted as work").
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - type: report\n    message: \"starting up\"\n  - type: report\n    name: summary\n    level: warning\n    message: \"found an issue\"\n    data: {count: 2}\n  - type: tool\n    name: items\n    provider: json\n    params: {mode: parse, input: \"[1, 2]\"}\n  - type: loop\n    name: chain_items\n    each: {in: prime.items.value, as: item}\n    body:\n      - type: report\n        message: \"chain pass\"\n      - type: tool\n        name: nap\n        provider: shell\n        params: {command: echo, args: [\"chain\"]}\n  - type: loop\n    name: tree_items\n    flow: tree\n    each: {in: prime.items.value, as: item}\n    body:\n      - type: report\n        message: \"tree pass\"\n      - type: tool\n        name: nap\n        provider: shell\n        params: {command: echo, args: [\"tree\"]}\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
fn events_are_detected_even_with_rich_styled_help_output() {
    // Typer forces Rich's terminal styling whenever `GITHUB_ACTIONS`
    // (set on every CI runner), `FORCE_COLOR` or `PY_COLORS` is set,
    // which splits `--events` in `cof run --help`'s own output into
    // separately-styled ANSI runs that a plain `.contains("--events")`
    // never matches -- `CofEngine::detect` used to silently read every
    // such run as "this cof has no --events" and fall back to running
    // it with the live-state file only.
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .env("FORCE_COLOR", "1")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert!(
        status.success(),
        "osi should exit 0, got {status:?}; stdout:\n{stdout}"
    );
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
    assert!(run_dir.join("events.jsonl").is_file());
}

#[test]
fn on_error_continue_still_exits_ok() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: flaky\n    type: tool\n    provider: shell\n    on_error: continue\n    params:\n      command: ls\n      args: [\"/nonexistent-osi-e2e-path\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    // See the comment in `a_simple_run_succeeds_and_prints_the_log`: a
    // default `--out-dir` leaves an `osi-*` directory in the real
    // `$TMPDIR`.
    let osi_dirs_before = snapshot_osi_dirs();
    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
        "osi should exit 0 (on_error: continue), got {status:?}; stdout:\n{stdout}"
    );
    assert!(stdout.contains("✗ prime.flaky"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 0"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
    assert_no_new_osi_dirs(&osi_dirs_before);
}

#[test]
fn an_unnamed_loops_reused_if_fails_each_pass_and_gets_one_cross_mark_per_pass() {
    // LOOP REGRESSION (issue #444's rework): an unnamed loop's body
    // writes its named `if` straight into the parent's node, so the
    // exact same path is reused every pass (no `iter_<n>` disambiguator
    // the way a *named* loop gets one) -- here a strict CEL condition
    // that always errors fails that `if` on its own, with `on_error:
    // continue` so nothing ever stops the loop. cof emits one `end`
    // event with `ok: false` per pass for it (the same shape as
    // tests/conformance/cases/on-error-if's `gate_continue`), so osi
    // must print one ✗ per pass too, not collapse every pass but the
    // first into nothing.
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - type: tool\n    name: items\n    provider: json\n    params: {mode: parse, input: \"[1, 2]\"}\n  - type: loop\n    each:\n      in: prime.items.value\n      as: item\n    body:\n      - type: if\n        name: gate\n        on_error: continue\n        if:\n          mode: cel\n          expr: \"state.input.missing > 1\"\n          strict: true\n        then:\n          - type: tool\n            name: branch\n            provider: json\n            params: {mode: stringify, input: {branch: \"then\"}}\n        else:\n          - type: tool\n            name: branch\n            provider: json\n            params: {mode: stringify, input: {branch: \"else\"}}\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
    assert_eq!(
        stdout.matches("✗ prime.gate ").count(),
        2,
        "stdout:\n{stdout}"
    );
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
}

#[test]
fn a_single_sigint_forwards_and_osi_exits_130() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let pid = child.id() as i32;

    // A signal sent after only a fixed delay can still land before
    // cof has produced any state at all under load (CI's own repro of
    // this), making the `✗ prime` count below a coin flip -- wait for
    // real proof the long-running step has started first.
    wait_for_step_start(
        &run_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");

    assert_no_leftover_process(work.path());
}

#[test]
fn a_second_sigint_during_cleanup_aborts_with_no_leftover_process() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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

    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    // The second SIGINT, landing while the `finally:` sleep is still
    // running, makes `cof` call `os._exit` at once (`cli/interrupts
    // .py`) — no final write, the same abort DESIGN.md §1.1 measures.
    // osi's own exit code is still the engine's (130); the real point
    // of this test, per issue #424's review, is that forwarding a
    // *second* signal is exercised end to end at all, and still leaves
    // nothing running regardless of which of the two paths the engine
    // took.
    assert_eq!(status.code(), Some(130), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 130"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");

    assert_no_leftover_process(work.path());
}

#[test]
fn sigterm_forwards_and_osi_exits_143() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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

    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(143), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 143"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");

    assert_no_leftover_process(work.path());
}

#[test]
fn sighup_forwards_and_osi_exits_129() {
    // Issue #433's Tests item: an end-to-end SIGHUP test against cof
    // -- `e2e_electricity.rs` already has one of its own, this crate
    // never had one against the default engine.
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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

    let (status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));

    assert_eq!(status.code(), Some(129), "stdout:\n{stdout}");
    assert!(stdout.contains("cancelling"), "stdout:\n{stdout}");
    assert!(stdout.contains("exit 129"), "stdout:\n{stdout}");
    assert_eq!(stdout.matches("✗ prime ").count(), 1, "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");

    assert_no_leftover_process(work.path());
}

#[test]
fn watch_mirrors_a_run_osi_did_not_start() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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

    let mut watch = osi_command(&home);
    watch.arg("watch").arg(run_dir.path());
    let child = watch.spawn().expect("spawn osi watch");
    let (watch_status, stdout, stderr) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(10));

    assert!(watch_status.success(), "stdout:\n{stdout}");
    assert!(stdout.contains("prime.hello"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
}

/// `cof`'s own twin of `e2e_electricity.rs`'s
/// `a_scripted_prompt_prints_tokens_sent_and_received` (issue #545):
/// comment 1 on the issue saw the same `ok` fallback with `cof`'s own
/// TUI log pane, so the fix (and this test) is not electricity-only.
#[test]
fn a_scripted_prompt_prints_tokens_sent_and_received() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
    assert!(stdout.contains("prime.ask"), "stdout:\n{stdout}");
    assert!(stdout.contains("↑3 t ↓5 t"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
}

/// #545's review finding 1: `cof`'s `--live-state` writer serialises
/// whatever the shared state dict holds *when its own debounce
/// interval next comes due*, not a copy taken at the triggering
/// effect's own completion -- so a slow leading effect can leave that
/// interval already overdue by the time it finally completes, and the
/// very next write (woken by that completion) can land while `ask`,
/// started right after, has already inserted its own running node
/// (`completed_at: null`) but not yet finished. `osi`'s `known_node`
/// must treat that running node the same as no node at all, or the
/// `end` event resolves to `ok` immediately instead of holding.
#[test]
fn a_scripted_prompt_after_a_slow_leading_effect_prints_tokens_sent_and_received() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: warmup\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: ask\n    type: prompt\n    template: \"hi\"\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));

    let run_dir = work.path().join("run");
    let child = osi_command(&home)
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
    assert!(stdout.contains("prime.ask"), "stdout:\n{stdout}");
    assert!(stdout.contains("↑3 t ↓5 t"), "stdout:\n{stdout}");
    assert!(stdout.contains("■ run ok"), "stdout:\n{stdout}");
    assert!(!stderr.contains("has no --events"), "stderr:\n{stderr}");
}

/// Issue #616: a container (the root, a `dynamic`) gets the scope's
/// effective `adapter`/`model` written into its own node by `cof`,
/// same as any leaf's. With a plan, `row_kind_for` never reaches the
/// shape guess for either (the plan already says `root`/`dynamic`),
/// so this run's own effect count is exact: three `json` steps, the
/// `dynamic`'s one `shell` child, and two more `shell` steps -- six
/// leaves, none of them a prompt. `osi watch` compiles no plan at all
/// (`PlanTree::empty()`, the same fallback this test builds by hand),
/// so it leans entirely on `RowKind::from_meta_shape`'s shape guess --
/// this is what used to miscount the `dynamic` as a prompt leaf (its
/// own node has `meta.adapter`/`meta.model` but no `meta.provider`)
/// and tag it with the configured model, on both panes.
#[test]
fn watch_counts_the_same_effects_as_a_planned_run_with_no_prompts() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
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
        "cof",
        &plan,
        &mut planned_model,
        Some(&state),
        process.clone(),
        0.0,
        "2026-01-01T00:00:10Z",
        &[],
    );
    // The root and `wrap`'s own named-dynamic container both count
    // too (issue #620): 2 + the 6 leaves (`step1`-`3`, `wrap.inner`,
    // `step4`-`5`).
    assert_eq!(planned.header.effects_planned, Some(8));
    assert_eq!(planned.header.effects_done, 8);

    let mut watched_model = oscilloscope_core::model::RunModel::new();
    let watched = oscilloscope_core::render::build(
        "do.yml",
        "cof",
        &no_plan,
        &mut watched_model,
        Some(&state),
        process,
        0.0,
        "2026-01-01T00:00:10Z",
        &[],
    );
    assert_eq!(
        watched.header.effects_planned,
        Some(8),
        "osi watch must count the same effects as a planned run"
    );
    assert_eq!(
        watched.header.effects_done, 8,
        "osi watch must count the same effects as a planned run"
    );

    for rendered in [&planned, &watched] {
        for row in rendered.rows.iter() {
            assert_ne!(
                row.dim.as_deref(),
                Some("scripted-model"),
                "orchestration row {} should show no model tag: {:?}",
                row.path,
                row.dim
            );
        }
        for row in rendered.state_rows.iter() {
            // The state pane's own dim joins the model with the
            // row's duration (`state_row_dim`), so it's never exactly
            // the model string alone -- `assert_ne!` on the full
            // string could never fail here.
            assert!(
                !row.dim.as_deref().unwrap_or("").contains("scripted-model"),
                "state row {} should show no model tag: {:?}",
                row.path,
                row.dim
            );
        }
    }
}

/// Issue #620: a finished run's header must read `effects_run` from
/// `runtime.last_run.totals` exactly, on a document combining every
/// shape that leaves effects planned but never run -- an `if` whose
/// condition is false (its `else` branch's own effect is the only one
/// that runs), an `each` over an empty list, and a `while` that stops
/// before its `max_iterations`.
#[test]
fn a_finished_runs_effects_count_matches_effects_run() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        r#"effects:
  - name: items
    type: tool
    provider: json
    params: {mode: parse, input: "[]"}
  - name: gate
    type: if
    if: {mode: cel, expr: "false"}
    then:
      - name: taken
        type: tool
        provider: shell
        params: {command: echo, args: ["taken"]}
    else:
      - name: not_taken
        type: tool
        provider: shell
        params: {command: echo, args: ["not_taken"]}
  - name: empty_each
    type: loop
    each: {in: prime.items.value, as: item}
    body:
      - name: never
        type: tool
        provider: shell
        params: {command: echo, args: ["never"]}
  - name: short_while
    type: loop
    while: {mode: cel, expr: "state.iter.index < 2"}
    max_iterations: 10
    body:
      - name: tick
        type: tool
        provider: shell
        params: {command: echo, args: ["tick"]}
"#,
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let mut cof = Command::new("cof");
    cof.arg("run")
        .arg(&doc)
        .arg("--config")
        .arg(&config)
        .arg("--quiet")
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

    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.path().join("state.json")).expect("read state.json"),
    )
    .expect("parse state.json");
    let effects_run = state
        .pointer("/runtime/last_run/totals/effects_run")
        .and_then(serde_json::Value::as_u64)
        .expect("effects_run in state.json");

    let inputs = oscilloscope_core::plan::parse_inputs(&[]).expect("no -e inputs");
    let program = oscilloscope_core::plan::compile(&doc, Some(&config), &inputs)
        .expect("the document compiles");
    let plan = oscilloscope_core::plan::PlanTree::from_program(&program);
    let process = oscilloscope_core::model::ProcessState::Exited {
        interrupted: false,
        likely_failed: false,
        has_failure_reason: false,
        exited_at: None,
    };

    let mut model = oscilloscope_core::model::RunModel::new();
    let rendered = oscilloscope_core::render::build(
        "do.yml",
        "cof",
        &plan,
        &mut model,
        Some(&state),
        process,
        0.0,
        "2026-01-01T00:00:10Z",
        &[],
    );
    assert_eq!(rendered.header.effects_planned, Some(effects_run));
    assert_eq!(rendered.header.effects_done, effects_run);
}
