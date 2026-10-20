//! End-to-end tests for run history (issue #649) against a real
//! `cof`: enabled only with `OSI_E2E_COF=1` and `cof` on `PATH`, same
//! as `e2e_cof.rs`. Every spawn below points `OSI_HISTORY_DIR` at
//! this test's own temp directory, on top of the usual isolated
//! `HOME` (`support::TestHome`) -- belt and suspenders against ever
//! touching the real `~/.local/state/circuitry/osi/history`.

mod support;

use std::time::Duration;

use oscilloscope_core::history;
use support::{
    SCRIPTED_CONFIG, TestHome, e2e_enabled, osi_command, wait_with_timeout_capturing_stdout,
    wait_with_timeout_capturing_stdout_and_stderr, write_doc,
};

fn run_once(
    home: &TestHome,
    history_dir: &std::path::Path,
    doc: &std::path::Path,
    config: &std::path::Path,
    work: &std::path::Path,
    run_dir_name: &str,
) -> (std::process::ExitStatus, String) {
    let run_dir = work.join(run_dir_name);
    let child = osi_command(home)
        .arg(doc)
        .arg(config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--log")
        .env("OSI_HISTORY_DIR", history_dir)
        .current_dir(work)
        .spawn()
        .expect("spawn osi");
    wait_with_timeout_capturing_stdout(child, Duration::from_secs(30))
}

#[test]
fn two_runs_of_one_document_leave_two_records_with_the_same_document_hash() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let history_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (status1, stdout1) = run_once(
        &home,
        history_dir.path(),
        &doc,
        &config,
        work.path(),
        "run1",
    );
    assert!(status1.success(), "stdout:\n{stdout1}");
    let (status2, stdout2) = run_once(
        &home,
        history_dir.path(),
        &doc,
        &config,
        work.path(),
        "run2",
    );
    assert!(status2.success(), "stdout:\n{stdout2}");

    let document = std::fs::canonicalize(&doc)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let records = history::read_records(history_dir.path(), &document);
    assert_eq!(records.len(), 2, "expected two records, got {records:?}");
    assert_eq!(records[0].document_hash, records[1].document_hash);
    assert!(records[0].document_hash.is_some());
    assert_ne!(records[0].run_id, records[1].run_id);
    for record in &records {
        assert_eq!(record.outcome, history::Outcome::Ok);
        assert_eq!(record.engine.as_deref(), Some("cof"));
    }
}

#[test]
fn a_failed_run_is_recorded_as_failed_and_never_feeds_the_statistics() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let history_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: boom\n    type: tool\n    provider: shell\n    params:\n      command: \"false\"\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (status, stdout) = run_once(
        &home,
        history_dir.path(),
        &doc,
        &config,
        work.path(),
        "run1",
    );
    assert!(!status.success(), "stdout:\n{stdout}");

    let document = std::fs::canonicalize(&doc)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let records = history::read_records(history_dir.path(), &document);
    assert_eq!(records.len(), 1, "expected one record, got {records:?}");
    assert_eq!(records[0].outcome, history::Outcome::Failed);

    let stats = history::compute_stats(&records, records[0].document_hash.as_deref());
    assert_eq!(
        stats.same_hash_ok_count, 0,
        "a failed run must not count as ok"
    );
    assert!(stats.whole_run.is_none());
}

#[test]
fn a_fourth_runs_glance_line_shows_the_history_eta_while_earlier_ones_show_the_loop_eta_or_none() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let history_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    for i in 0..3 {
        let (status, stdout) = run_once(
            &home,
            history_dir.path(),
            &doc,
            &config,
            work.path(),
            &format!("run{i}"),
        );
        assert!(status.success(), "stdout:\n{stdout}");
    }

    let run_dir = work.path().join("run-glance");
    let child = osi_command(&home)
        .arg(&doc)
        .arg(&config)
        .arg("--out-dir")
        .arg(&run_dir)
        .arg("--glance")
        .env("OSI_HISTORY_DIR", history_dir.path())
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi --glance");
    let (status, stdout) = wait_with_timeout_capturing_stdout(child, Duration::from_secs(30));
    assert!(status.success(), "stdout:\n{stdout}");
    // Three prior ok runs of the same document hash meet the p50
    // threshold (issue #649): the fourth run's own final `--glance`
    // line shows the history ETA, not the innermost loop's own.
    assert!(
        stdout.contains("ETA ~") && !stdout.contains("loop ETA ~"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn watch_on_a_finished_run_directory_adds_one_record_and_a_second_watch_adds_none() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let history_dir = tempfile::tempdir().unwrap();
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
        .env("OSI_HISTORY", "off")
        .current_dir(work.path())
        .spawn()
        .expect("spawn osi");
    let (status, stdout, _) =
        wait_with_timeout_capturing_stdout_and_stderr(child, Duration::from_secs(30));
    assert!(status.success(), "stdout:\n{stdout}");

    let document = std::fs::canonicalize(&doc)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let watch1 = osi_command(&home)
        .arg("watch")
        .arg(&run_dir)
        .arg("--plan")
        .arg(&doc)
        .arg("--log")
        .env("OSI_HISTORY_DIR", history_dir.path())
        .spawn()
        .expect("spawn osi watch");
    let (status1, stdout1, _) =
        wait_with_timeout_capturing_stdout_and_stderr(watch1, Duration::from_secs(30));
    assert!(status1.success(), "stdout:\n{stdout1}");
    let records_after_first_watch = history::read_records(history_dir.path(), &document);
    assert_eq!(
        records_after_first_watch.len(),
        1,
        "expected one record, got {records_after_first_watch:?}"
    );

    let watch2 = osi_command(&home)
        .arg("watch")
        .arg(&run_dir)
        .arg("--plan")
        .arg(&doc)
        .arg("--log")
        .env("OSI_HISTORY_DIR", history_dir.path())
        .spawn()
        .expect("spawn osi watch (second time)");
    let (status2, stdout2, _) =
        wait_with_timeout_capturing_stdout_and_stderr(watch2, Duration::from_secs(30));
    assert!(status2.success(), "stdout:\n{stdout2}");
    let records_after_second_watch = history::read_records(history_dir.path(), &document);
    assert_eq!(
        records_after_second_watch.len(),
        1,
        "the second watch of the same finished run must add no record: {records_after_second_watch:?}"
    );
    assert_eq!(
        records_after_first_watch[0].run_id,
        records_after_second_watch[0].run_id
    );
}
