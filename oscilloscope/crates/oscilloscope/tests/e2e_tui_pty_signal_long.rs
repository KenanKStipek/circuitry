//! A PTY test for issue #734: on a run whose own `run_start` was an
//! hour ago, `osi watch`'s own single-run Signal still follows Now
//! (the sweep stays on screen, and its marks move with real time),
//! and reads its second marks in the shared elapsed format
//! (`1h00m..s`), never a raw second count. The run itself is
//! synthetic -- a fake engine process standing in for `cof`, and
//! hand-written `events.jsonl`/`state.live.json` -- so the hour
//! doesn't have to actually pass; only the few seconds between the
//! test's own two screen reads do.

mod support;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    TestHome, assert_no_leftover_process, drain_continuously, open_pty, screen_grid, send_bytes,
    spawn_osi_on_pty, wait_for_screen_text, wait_with_timeout,
};

/// A fake engine standing in for `cof`, named `cof` itself so a
/// `ps`-based liveness check elsewhere in this epic's own tests
/// would see it as one too -- bounded the way issue #661 requires
/// of every fake: it ends the moment it's reparented (this test
/// process going away) or after 60s either way, never an unbounded
/// `while True: sleep` loop.
const FAKE_COF_SCRIPT: &str = r#"#!/usr/bin/env python3
import os, time
parent = os.getppid()
end = time.time() + 60
while time.time() < end and os.getppid() == parent:
    time.sleep(0.2)
"#;

/// Writes [`FAKE_COF_SCRIPT`] to `bin_dir/cof`, executable. A short-
/// lived child process writes it, so this test process never holds
/// it open for writing (the same ETXTBSY concern `e2e_tui_pty.rs`'s
/// own `write_engine_double` documents).
fn write_fake_cof(bin_dir: &Path) -> PathBuf {
    let path = bin_dir.join("cof");
    let mut writer = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(&path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("start the fake cof's writer");
    std::io::Write::write_all(
        &mut writer.stdin.take().unwrap(),
        FAKE_COF_SCRIPT.as_bytes(),
    )
    .expect("write the fake cof");
    assert!(
        writer.wait().unwrap().success(),
        "writing the fake cof failed"
    );
    path
}

fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S.%3fZ").to_string()
}

#[test]
fn osi_watch_signal_follows_now_an_hour_into_a_run() {
    let home = TestHome::new();
    let run_dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let fake_cof = write_fake_cof(bin_dir.path());
    let mut fake = Command::new(&fake_cof)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the fake cof");
    let fake_pid = fake.id();

    let now = chrono::Utc::now();
    let run_start = now - chrono::Duration::seconds(3600);
    let ask_start = run_start + chrono::Duration::seconds(1);
    let ask_done = now - chrono::Duration::seconds(6);

    let events = format!(
        concat!(
            "{{\"v\":1,\"seq\":0,\"ts\":\"{run_start}\",\"ev\":\"run_start\",\"run_id\":\"r1\",",
            "\"orchestration\":\"pipelines/nightly-report.yml\",\"config\":null,",
            "\"engine\":\"cof 0.1.0\",\"pid\":{fake_pid}}}\n",
            "{{\"v\":1,\"seq\":1,\"ts\":\"{ask_start}\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.ask\"}}\n",
            "{{\"v\":1,\"seq\":2,\"ts\":\"{ask_done}\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.ask\",\"ok\":true,\"ms\":1}}\n",
            "{{\"v\":1,\"seq\":3,\"ts\":\"{ask_done}\",\"ev\":\"start\",\"id\":2,\"path\":\"prime.wait\"}}\n",
        ),
        run_start = rfc3339(run_start),
        ask_start = rfc3339(ask_start),
        ask_done = rfc3339(ask_done),
        fake_pid = fake_pid,
    );
    std::fs::write(run_dir.path().join("events.jsonl"), events).unwrap();

    let state = format!(
        concat!(
            "{{\"input\":{{}},\"runtime\":{{\"last_run\":{{\"orchestration_path\":",
            "\"pipelines/nightly-report.yml\"}}}},\"_run_id\":\"r1\",\"_timestamp\":\"{now}\",",
            "\"prime\":{{\"ask\":{{\"value\":\"ok\",\"meta\":{{\"type\":\"prompt\",",
            "\"created_at\":\"{ask_start}\",\"completed_at\":\"{ask_done}\",",
            "\"tokens_sent\":1200,\"tokens_received\":40,\"adapter\":\"scripted\",",
            "\"model\":\"m\"}}}},",
            "\"wait\":{{\"meta\":{{\"type\":\"tool\",\"provider\":\"shell\",",
            "\"created_at\":\"{ask_done}\"}}}}}}}}",
        ),
        now = rfc3339(now),
        ask_start = rfc3339(ask_start),
        ask_done = rfc3339(ask_done),
    );
    std::fs::write(run_dir.path().join("state.live.json"), state).unwrap();

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let run_dir_arg = run_dir.path().as_os_str();
    let child = spawn_osi_on_pty(&home, &slave_path, &[OsStr::new("watch"), run_dir_arg]);

    wait_for_screen_text(&captured_buf, "nightly-report.yml", Duration::from_secs(20));
    send_bytes(&mut master, b"1");
    wait_for_screen_text(&captured_buf, "SIGNAL", Duration::from_secs(10));
    // The chip switches the instant "SIGNAL" lands, a redraw tick or
    // two before the chart's own cells (sent well after it in the
    // same differential frame) are certain to have arrived too.
    std::thread::sleep(Duration::from_millis(300));
    let screen_a = {
        let whole = captured_buf.lock().unwrap().clone();
        screen_grid(&whole, 200, 60).join("\n")
    };
    assert!(
        screen_a.contains('\u{2503}'),
        "expected the Now sweep on screen an hour into the run; screen:\n{screen_a}"
    );
    assert!(
        screen_a.contains("1h00m"),
        "expected a second mark or the Now label in the elapsed format; screen:\n{screen_a}"
    );
    assert!(
        !screen_a.contains("3600s") && !screen_a.contains("3601s"),
        "a mark must never read as a bare second count; screen:\n{screen_a}"
    );

    std::thread::sleep(Duration::from_secs(2));
    let screen_b = {
        let whole = captured_buf.lock().unwrap().clone();
        screen_grid(&whole, 200, 60).join("\n")
    };
    assert_ne!(
        screen_a, screen_b,
        "expected the marks to have moved between two screens two seconds apart"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi watch should exit 0, got {status:?}");

    let _ = fake.kill();
    let _ = fake.wait();

    assert_no_leftover_process(run_dir.path());
}
