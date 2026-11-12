//! End-to-end TUI test against a real `cof`, on a real pseudo-
//! terminal (issue #434's Tests section): `osi`'s own stdout/stderr
//! are a pty slave, not a pipe, so `effective_log_mode` picks the
//! interactive TUI exactly as it would on a human's own terminal.
//! Gated the same way `e2e_cof.rs` is: `OSI_E2E_COF=1` and `cof` on
//! `PATH`, a temporary `HOME`, the scripted adapter, stripped
//! credentials, a hard per-test timeout.
//!
//! No extra pty crate: `posix_openpt`/`grantpt`/`unlockpt`/`ptsname`
//! are already available through `libc`, a dependency of this crate's
//! own binary too (`main.rs`'s own `stdin_hung_up`).

mod support;

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use support::{
    ELECTRICITY_CONFIG, SCRIPTED_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process,
    drain_continuously, e2e_electricity_enabled, e2e_enabled, open_pty, osi_command,
    scripted_config, send_bytes, spawn_osi_on_pty, spawn_osi_on_pty_with_env, wait_for_screen_text,
    wait_for_screen_text_gone, wait_for_step_start, wait_with_timeout, write_doc,
};

/// Waits until `pattern` has appeared anywhere in `buf` so far, or
/// `timeout` runs out either way.
fn wait_for_pattern(buf: &Arc<Mutex<Vec<u8>>>, pattern: &[u8], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if buf
            .lock()
            .unwrap()
            .windows(pattern.len())
            .any(|w| w == pattern)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// [`wait_for_pattern`], but only looking at bytes written at or
/// after `since_len` -- some labels this screen draws (a tree loop's
/// own breadcrumbed pass containers, "pass 1 of 2") can share a
/// substring with what a *different* action is meant to produce (the
/// Time screen's own `⏎`-expanded pass rows, "pass 1"), already
/// on screen from selecting the row at all, well before the action
/// under test ever runs -- scanning the whole buffer would let that
/// stale match satisfy this wait immediately, before the real thing
/// has happened at all.
fn wait_for_pattern_since(
    buf: &Arc<Mutex<Vec<u8>>>,
    since_len: usize,
    pattern: &[u8],
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if buf.lock().unwrap()[since_len..]
            .windows(pattern.len())
            .any(|w| w == pattern)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// [`wait_for_pattern`], read straight off `master` itself rather
/// than a [`drain_continuously`] buffer -- the one test below that
/// needs to close `master` itself afterward can't hand it to
/// `drain_continuously` first (that moves it into a thread that owns
/// it for good; dropping a mere *clone* of the fd would leave the
/// real pty master still open under the thread's own copy, which is
/// exactly the one thing that test exists to not do). Non-blocking,
/// so a quiet stretch between frames can never block this past its
/// own deadline.
fn wait_for_pattern_on_master(master: &mut File, pattern: &[u8], timeout: Duration) {
    use std::io::Read;
    let fd = master.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "fcntl F_GETFL failed");
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "fcntl F_SETFL O_NONBLOCK failed"
    );
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while Instant::now() < deadline {
        match master.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(pattern.len()).any(|w| w == pattern) {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    panic!(
        "pattern {pattern:?} never appeared within {timeout:?}; got:\n{:?}",
        String::from_utf8_lossy(&buf)
    );
}

#[test]
fn a_short_run_renders_in_the_tui_and_exits_cleanly_with_no_leftovers() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    // Drained continuously from the moment the pty exists (the same
    // reasoning `wait_with_timeout_capturing_stdout` applies to a
    // plain pipe in `e2e_cof.rs`, but doubly so here: a TUI redrawing
    // at up to 10Hz can fill a pty's own small kernel buffer in well
    // under a second, and nothing draining it would make osi's own
    // writes block). Reading happens on a dup'd fd so `master` itself
    // stays free for this test to write a key press back.
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    // Review finding K1: the run finishes almost at once (an `echo`),
    // but the TUI now stays open on that final state — `q` (no
    // confirm, nothing left running) is what actually leaves it.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");

    // Finding 17: the same grace period every other test here gives
    // the draining thread to catch up on whatever osi wrote between
    // its last read and actually exiting — the alternate-screen exit
    // sequence included — before this one reads the captured buffer.
    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    // The alternate screen's own entry/exit sequences (DESIGN.md
    // §6.3's terminal safety) prove the TUI, not the plain `--log`
    // stream, actually ran -- `--log` or a non-TTY writes neither.
    assert!(
        rendered.contains("\u{1b}[?1049h"),
        "expected an alternate-screen entry sequence; got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence; got:\n{rendered:?}"
    );

    // DESIGN.md §6.3 / this repository's own lane rules: nothing osi
    // or cof started is still running, and no part of this test
    // created an `osi-*` directory under the real system temp dir in
    // the first place (`--out-dir` above kept it inside `out_dir`,
    // this test's own, already-cleaned-up tempdir).
    assert_no_leftover_process(work.path());
}

/// Issue #704: once a run has ended, `↓` moves the selection to
/// another finished row, `enter` opens it full-screen, and `]` steps
/// to the next effect in plan order -- the full-screen title changes
/// to name it, proving the key actually moved the selection rather
/// than just redrawing the same step.
#[test]
fn after_a_run_ends_down_and_enter_browse_a_row_and_bracket_steps_to_the_next() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: alpha\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: beta\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: gamma\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    let screen = wait_for_screen_text(&captured_buf, "Ended", Duration::from_secs(20));
    assert!(
        screen.contains("alpha") && screen.contains("beta") && screen.contains("gamma"),
        "expected all three rows on the ended Tape; screen:\n{screen}"
    );

    // The run's just ended: the selection starts on the last row the
    // plan ran (issue #704 rule 1's own default target, "gamma"
    // here) and the detail pane shows the run summary -- two ↑
    // actually move the selection, landing on "alpha", never back on
    // "gamma" itself (which would show the summary again, the same
    // as any other selection landing back on that exact row does).
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\x1b[A\x1b[A");
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "Tape \u{b7} ", Duration::from_secs(10));
    assert!(
        !screen.contains("Tape \u{b7} "),
        "expected enter to open the full-screen step; screen:\n{screen}"
    );
    assert!(
        screen.contains("alpha \u{b7} Selected"),
        "expected alpha's own full-screen step; screen:\n{screen}"
    );

    // `]` steps to the next effect in plan order -- "beta", never
    // "gamma" (the run's own default end target, two steps further).
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"]");
    let screen = wait_for_screen_text(
        &captured_buf,
        "beta \u{b7} Selected",
        Duration::from_secs(10),
    );
    assert!(
        screen.contains("beta \u{b7} Selected"),
        "expected stepping to reach the next effect; screen:\n{screen}"
    );

    send_bytes(&mut master, b"\x1b");
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// Issue #716: `Tab` moves focus from the Tape's own list to its
/// detail pane -- down-arrow then scrolls the pane while the list's
/// own selection stays exactly where it was, proven by the detail
/// pane's own title (`breadcrumb (dot) Selected`) still naming the
/// same row; `Tab` again gives down-arrow back to the list, which
/// now does move the selection, naming the next row in plan order
/// instead.
#[test]
fn tab_focuses_the_detail_pane_and_down_scrolls_it_while_the_list_stays() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: alpha\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: beta\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: gamma\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_screen_text(&captured_buf, "Ended", Duration::from_secs(20));

    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\x1b[A\x1b[A");
    let screen = wait_for_screen_text(
        &captured_buf,
        "alpha \u{b7} Selected",
        Duration::from_secs(10),
    );
    assert!(
        screen.contains("alpha \u{b7} Selected"),
        "expected alpha selected before Tab; screen:\n{screen}"
    );

    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\t");
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\x1b[B");
    std::thread::sleep(SIGNAL_DELAY);
    let screen = support::screen_grid(&captured_buf.lock().unwrap(), 120, 40).join("\n");
    assert!(
        screen.contains("alpha \u{b7} Selected"),
        "expected the list's own selection to stay on alpha while the pane has focus; screen:\n{screen}"
    );

    send_bytes(&mut master, b"\t");
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"\x1b[B");
    let screen = wait_for_screen_text(
        &captured_buf,
        "beta \u{b7} Selected",
        Duration::from_secs(10),
    );
    assert!(
        screen.contains("beta \u{b7} Selected"),
        "expected down-arrow to move the list's own selection again once Tab gave it back; screen:\n{screen}"
    );

    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// Issue #710: while a run is live, the Tape's own Now band sits at
/// the list's own vertical centre -- `(L - h) / 2` rows down from the
/// list's own first content row, `L` the list's own height (33, at
/// this pty's own 120x40: the Tape's inner area is 35 rows, less 2
/// for the rule and Totals row) and `h` the Now area's own (2 here:
/// the band line plus the one plain tool step actually running, no
/// preview or slots rule) -- rounding down on an uneven split, which
/// this one isn't. The window pads blank above instead of drifting
/// with the run's own progress: none of this three-step chain's own
/// past ever reaches the 15 rows 120x40 needs above the band, so the
/// band sits at the same row (absolute row 17: row 0 the header, row
/// 1 the pane's own top border, then 15 more) whichever of the three
/// steps happens to be the one running when this reads the screen.
/// Every wait here (the alternate screen opening, the first step
/// starting, the TUI drawing alpha's own row as running, `Ended`,
/// the exit wait) is its own named step with its own cap, summing
/// to 95s; the run itself finishes in about 3s.
#[test]
fn the_now_band_sits_at_the_lists_own_vertical_centre_during_a_live_run() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: alpha\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: beta\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n  - name: gamma\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"1\"]\n      allowed_commands: [\"sleep\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    const ALTERNATE_SCREEN_WAIT: Duration = Duration::from_secs(20);
    const FIRST_STEP_WAIT: Duration = Duration::from_secs(20);
    const RUNNING_ROW_WAIT: Duration = Duration::from_secs(5);
    const ENDED_WAIT: Duration = Duration::from_secs(20);
    const EXIT_WAIT: Duration = Duration::from_secs(30);
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", ALTERNATE_SCREEN_WAIT);
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.alpha",
        FIRST_STEP_WAIT,
    );
    // The engine's own start event can land before the TUI has drawn
    // the matching frame -- wait for alpha's own running row, not a
    // fixed delay, so a slow draw under load doesn't read the screen
    // one frame too early.
    wait_for_screen_text(&captured_buf, "\u{25cf} alpha", RUNNING_ROW_WAIT);

    let whole = captured_buf.lock().unwrap().clone();
    let screen = support::screen_grid(&whole, 200, 60);
    let now_row = screen
        .iter()
        .position(|line| line.contains("\u{2501}\u{2501} Now"))
        .unwrap_or_else(|| panic!("expected a Now band on screen; got:\n{}", screen.join("\n")));
    assert_eq!(
        now_row,
        2 + 15,
        "expected the Now band centred at row 17; screen:\n{}",
        screen.join("\n")
    );

    wait_for_pattern(&captured_buf, b"Ended", ENDED_WAIT);
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, EXIT_WAIT);
    assert!(status.success(), "osi should exit 0, got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// A run this `osi` launched has a registry entry while it's live --
/// named by its own document and run directory in bare `osi`'s own
/// non-TTY list too -- and that entry is gone once the run ends.
/// Every wait here (two 20s pattern waits, a 10s poll, a 10s wait for
/// `Ended`, a 30s exit wait) is its own named step with its own cap;
/// none of them is a bare sleep, and none shares a cap with any
/// other.
#[test]
fn a_launched_run_appears_in_the_registry_while_live_and_is_gone_after_it_exits() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"2\"]\n      allowed_commands: [\"sleep\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );

    let runs_dir = home.path().join("runs");
    // The entry is written once `run_start` is known, not instantly
    // at spawn -- polled rather than slept for a fixed delay, capped
    // at the 10s deadline just below (this step's own cap, not an
    // overall one shared with any other wait in this test).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut entries = Vec::new();
    while Instant::now() < deadline {
        entries = std::fs::read_dir(&runs_dir)
            .map(|rd| rd.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        if !entries.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one registry entry while the run is live, in {runs_dir:?}"
    );
    let entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entries[0].path()).unwrap()).unwrap();
    assert_eq!(entry["engine"], "cof");
    assert_eq!(
        std::fs::canonicalize(entry["run_dir"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(out_dir.path()).unwrap()
    );
    assert_eq!(
        entry["launcher"].as_str().unwrap(),
        format!("osi {}", env!("CARGO_PKG_VERSION"))
    );

    // Bare `osi`, without a TTY, lists this same run while it's
    // still live -- the content check the registry entry's own
    // assertions above can't give: a liveness or process-scan defect
    // that makes a real `cof` run look dead (or unmatched) would
    // prune the entry above right out from under this list, or never
    // list it via the scan either.
    let discover = osi_command(&home)
        .current_dir(work.path())
        .output()
        .expect("spawn bare osi");
    assert!(discover.status.success());
    let discover_stdout = String::from_utf8_lossy(&discover.stdout);
    let discover_line = discover_stdout
        .lines()
        .find(|l| l.contains("do.yml"))
        .unwrap_or_else(|| panic!("expected a line naming do.yml, got:\n{discover_stdout}"));
    assert!(discover_line.contains("cof"));
    assert!(
        discover_line.contains(&out_dir.path().display().to_string())
            || std::fs::canonicalize(out_dir.path())
                .is_ok_and(|p| discover_line.contains(&p.display().to_string())),
        "expected the run directory in: {discover_line}"
    );
    assert_eq!(
        std::fs::read_dir(&runs_dir)
            .map(|rd| rd.count())
            .unwrap_or(0),
        1,
        "discovery must not have disturbed the still-live entry"
    );

    // `q` opens a confirm-cancel dialog instead of quitting outright
    // as long as osi's own `child.try_wait()` hasn't yet observed the
    // engine exit -- waited out here on the header's own `Ended`
    // (the same fix #694 already gave the slow-streaming tests
    // above), rather than fighting that dialog from a test with no
    // plan to assert on it.
    wait_for_pattern(&captured_buf, b"Ended", Duration::from_secs(10));
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let remaining = std::fs::read_dir(&runs_dir)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(
        remaining, 0,
        "expected the registry entry to be gone once the run ended"
    );

    assert_no_leftover_process(work.path());
}

/// Issue #626 (updated by #678): `0`/`2` switch screens during a
/// live run, and the screen each one draws actually changes on
/// screen -- the Tape (its own list and Totals row) on `0`, screen
/// 2's own orchestration pane on `2`.
#[test]
fn screen_keys_switch_screens_during_a_live_run() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    // Default is the Tape; this run is still live. Checked against
    // the whole buffer so far, since nothing has been sent yet for a
    // key press to have changed anything after.
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("Run Started") && rendered.contains("Totals"),
            "expected the Tape's own list and Totals row on screen; got:\n{rendered:?}"
        );
    }

    // Every check from here on reads only what arrived *after* its
    // own key -- "flow" is in the header's own tab on every screen,
    // so searching the whole cumulative buffer again for it could
    // never actually fail, key press or not.
    let before_2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    std::thread::sleep(Duration::from_millis(500));
    {
        // A key press only redraws the cells that changed (the `· 0/2 · ▰ = 50 t` part of the title
        // keeps its columns from the Tape's own title), and a styled run is split by escapes in the raw
        // stream, so neither the bytes after the key nor the raw bytes can be searched for the title:
        // replay everything drawn so far onto a screen and read that. `Flow ·` is the rail pane's
        // own title, drawn only on the Flow screen.
        assert!(
            captured_buf.lock().unwrap().len() > before_2,
            "pressing 2 drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Flow \u{b7}"),
            "expected the Flow screen's own rail pane after pressing 2; screen:\n{screen}"
        );
    }

    let before_0 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"0");
    std::thread::sleep(Duration::from_millis(500));
    {
        // Same replay; `Tape ·` is the Tape's own pane title (the Totals row is on every screen now).
        assert!(
            captured_buf.lock().unwrap().len() > before_0,
            "pressing 0 drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Tape \u{b7}"),
            "expected the Tape's own list back after pressing 0; screen:\n{screen}"
        );
    }

    // `q` while still running asks first; confirming both cancels
    // and leaves once the engine actually exits.
    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Issue #695: `3` opens the Store screen during a live run, with
/// the finished prompt's own tile on the area diagram and its own
/// row in the Largest Stores table; `↑↓` move the table's cursor,
/// `enter` opens the selected store's own step full-screen and
/// `esc` returns.
#[test]
fn pressing_3_during_a_live_run_opens_the_store_screen() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\nprime.again:\n  - text: \"hi again\"\n    tokens_sent: 2\n    tokens_received: 4\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: again\n    type: prompt\n    template: \"hi again\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    let before_3 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"3");
    wait_for_pattern_since(&captured_buf, before_3, b"STORE", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("STORE"),
            "expected the Store screen's own chip after pressing 3; screen:\n{screen}"
        );
        assert!(
            screen.contains("Largest Stores"),
            "expected the Largest Stores table after pressing 3; screen:\n{screen}"
        );
        assert!(
            screen.contains("ask · 8 t"),
            "expected the finished prompt's own tile or row, with its own tokens, to show on the Store screen; screen:\n{screen}"
        );
    }

    // Two token-bearing stores (`ask` and `again`), so `↓`/`↑` has
    // somewhere to actually move the cursor to -- a single-row table
    // would let the cursor stay put and still pass a check that only
    // looks for *some* redraw, which the ticking clock alone already
    // guarantees.
    let before_down = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\x1b[B");
    wait_for_pattern_since(
        &captured_buf,
        before_down,
        b"again",
        Duration::from_secs(10),
    );
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        let cursor_on_again = screen
            .lines()
            .any(|l| l.contains('❯') && l.contains("again"));
        assert!(
            cursor_on_again,
            "expected ↓ to move the table's own cursor onto `again`; screen:\n{screen}"
        );
    }

    let before_up = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\x1b[A");
    wait_for_pattern_since(&captured_buf, before_up, b"ask", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        let cursor_on_ask = screen
            .lines()
            .any(|l| l.contains('❯') && l.contains("ask") && !l.contains("again"));
        assert!(
            cursor_on_ask,
            "expected ↑ to move the table's own cursor back onto `ask`; screen:\n{screen}"
        );
    }

    let before_enter = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\r");
    std::thread::sleep(Duration::from_millis(400));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_enter,
            "pressing ⏎ drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Selected"),
            "expected ⏎ to open the selected store's own step full-screen; screen:\n{screen}"
        );
    }
    send_bytes(&mut master, b"\x1b");
    std::thread::sleep(Duration::from_millis(400));

    // `q` while still running asks first; confirming both cancels
    // and leaves once the engine actually exits.
    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// 100 columns and up at 30 rows and up shows the Largest Stores
/// table beside Readings; below 30 rows (still 100 columns and up)
/// Readings drops and the table keeps three rows; so does dropping
/// below 100 columns on width alone, with 30 rows to spare -- all
/// three picked up on the TUI's own next redraw, with no key needed.
#[test]
fn resizing_the_store_screen_drops_readings_below_30_rows() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    resize_pty(&slave_path, 120, 30);
    let before_3 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"3");
    wait_for_pattern_since(
        &captured_buf,
        before_3,
        b"Readings",
        Duration::from_secs(10),
    );

    let before_resize = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 120, 24);
    std::thread::sleep(Duration::from_millis(600));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_resize,
            "resizing to 120x24 drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            !screen.contains("Readings"),
            "expected the Readings pane gone below 30 rows; screen:\n{screen}"
        );
        assert!(
            screen.contains("Largest Stores"),
            "expected the table still shown below 30 rows; screen:\n{screen}"
        );
    }

    // 80-99 columns drops Readings too, on width alone -- back to 30
    // rows (plenty), but narrow enough that the chart/table pane no
    // longer has room for a second pane beside it.
    let before_narrow = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 90, 30);
    std::thread::sleep(Duration::from_millis(600));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_narrow,
            "resizing to 90x30 drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            !screen.contains("Readings"),
            "expected the Readings pane gone below 100 columns; screen:\n{screen}"
        );
        assert!(
            screen.contains("Largest Stores"),
            "expected the table still shown below 100 columns; screen:\n{screen}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// §4: below 80 columns only the Tape is offered -- shrinking the
/// terminal while the Store is on screen bounces straight back to
/// the Tape, picked up on the TUI's own next redraw with no key
/// needed.
#[test]
fn resizing_below_80_columns_bounces_store_back_to_the_tape() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    send_bytes(&mut master, b"3");
    wait_for_pattern(&captured_buf, b"STORE", Duration::from_secs(10));

    let before_60 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 60, 24);
    wait_for_pattern_since(&captured_buf, before_60, b"Tape", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Tape"),
            "expected the Tape's own panel title once the terminal is too narrow for Store; screen:\n{screen}"
        );
        assert!(
            !screen.contains("STORE"),
            "the Store must not still be offered below 80 columns; screen:\n{screen}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #691's own acceptance item: pressing `2` during a live
/// `cof` run of a document with a named `if` opens the Flow screen
/// with the decision's own `◆` row and a matching story line;
/// `→`/`←` scroll the rail and turn follow off, then back on once
/// the scroll lands Now back in view; `⏎` opens the selection
/// full-screen and back; `f` toggles follow (the title's own `·
/// Following` marker); `?` opens Flow's own Help listing §4's own
/// keys, `esc` closes it. Every check reads the replayed screen
/// (`support::screen_grid`), not the raw byte stream, since a
/// differential redraw only retransmits the cells that changed.
#[test]
fn pressing_2_during_a_live_run_opens_the_flow_screen() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: gate\n    type: if\n    if:\n      mode: cel\n      expr: \"true\"\n    then:\n      - name: slow\n        type: tool\n        provider: shell\n        params:\n          command: tail\n          args: [\"-f\", \"/dev/null\"]\n          allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.gate.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    send_bytes(&mut master, b"2");
    // The Tape's own detail pane can already show `→ then` before the
    // key lands, so the arrival of the Flow screen is read from the
    // replayed screen itself: its rail pane's title (`Flow ·`) is
    // drawn on no other screen. `meta.branch` reaches the live state
    // once cof writes it, and the TUI picks it up on its next poll, so
    // the decision row is polled for the same way rather than guessed
    // with a fixed sleep.
    let screen = wait_for_screen_text(&captured_buf, "Flow \u{b7}", Duration::from_secs(10));
    assert!(
        screen.contains("Flow \u{b7}"),
        "expected the Flow screen's own rail pane after pressing 2; screen:\n{screen}"
    );
    let screen = wait_for_screen_text(&captured_buf, "◆", Duration::from_secs(10));
    {
        assert!(
            screen.contains("◆"),
            "expected the named if's own decision row after pressing 2; screen:\n{screen}"
        );
        assert!(
            screen.contains("gate"),
            "expected gate's own row after pressing 2; screen:\n{screen}"
        );
        assert!(
            screen.contains("Story"),
            "expected the Story pane's own border title after pressing 2; screen:\n{screen}"
        );
    }

    // → scrolls the rail; this fixture's own rail (three stations)
    // fits well within the replay's own width regardless, so Now
    // never actually leaves view and follow resumes on the very
    // next redraw -- the follow-off/land-back cycle itself is a
    // buffer-level unit test (`keys::tests::flow_up_down_turns_
    // follow_off_and_back_on_the_same_way_the_tape_does`); this only
    // checks that neither key breaks the screen.
    let before_right = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\x1b[C");
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        captured_buf.lock().unwrap().len() > before_right,
        "pressing → drew nothing at all"
    );
    send_bytes(&mut master, b"\x1b[D");
    std::thread::sleep(Duration::from_millis(400));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Following"),
            "expected follow to be back on after ← lands Now back in view; screen:\n{screen}"
        );
    }

    let before_enter = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\r");
    std::thread::sleep(Duration::from_millis(500));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_enter,
            "pressing ⏎ drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Selected"),
            "expected ⏎ to open the selection full-screen; screen:\n{screen}"
        );
    }
    send_bytes(&mut master, b"\r");
    std::thread::sleep(Duration::from_millis(500));

    // Toggling follow off then back on nets back to the same
    // title (`· Following` present once more) -- a live run's own
    // continuous spinner redraws regardless of either key, so the
    // single off state in between is not a reliable thing for a
    // real PTY to observe; the off state itself is a buffer-level
    // unit test (`keys::tests::flow_left_right_scroll_and_turn_
    // follow_off`).
    let before_f = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"f");
    std::thread::sleep(Duration::from_millis(300));
    send_bytes(&mut master, b"f");
    std::thread::sleep(Duration::from_millis(500));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_f,
            "expected f to redraw the title"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Following"),
            "expected f twice to net back to the same title; screen:\n{screen}"
        );
    }

    let before_help = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"?");
    std::thread::sleep(Duration::from_millis(500));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_help,
            "pressing ? drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Help") && screen.contains("select a station"),
            "expected ? to open the Flow's own Help; screen:\n{screen}"
        );
    }
    send_bytes(&mut master, b"\x1b");
    std::thread::sleep(Duration::from_millis(500));

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Issue #691's own acceptance item: `↓`/`↑` select a lane on the
/// Flow screen over a live tree loop's own passes, turning follow
/// off and back on the same way the Tape's own land-back rule
/// does; `+`/`-` open and fold the selected fork's own lanes.
#[test]
fn flow_up_down_and_plus_minus_select_and_fold_a_live_loops_own_lanes() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "interface:\n  inputs:\n    items: {type: array, default: [\"a\", \"b\", \"c\", \"d\"]}\neffects:\n  - type: loop\n    name: fan\n    each:\n      in: input.items\n    flow: tree\n    max_concurrency: 1\n    body:\n      - name: slow\n        type: tool\n        provider: shell\n        params:\n          command: tail\n          args: [\"-f\", \"/dev/null\"]\n          allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.fan.iter_0.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    let before_2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    wait_for_pattern_since(&captured_buf, before_2, b"fan", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("fan") && screen.contains("4 lanes"),
            "expected the loop's own folded lane band after pressing 2; screen:\n{screen}"
        );
    }

    // `+` opens three lanes; the fork then draws real lane content
    // (a station per open lane) instead of the folded band alone.
    let before_plus = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"+");
    std::thread::sleep(Duration::from_millis(500));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_plus,
            "pressing + drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("slow"),
            "expected an open lane's own station after pressing +; screen:\n{screen}"
        );
    }

    // `↓` selects the next lane; `↑` selects back.
    send_bytes(&mut master, b"\x1b[B");
    std::thread::sleep(Duration::from_millis(400));
    send_bytes(&mut master, b"\x1b[A");
    std::thread::sleep(Duration::from_millis(400));

    // `-` folds the lanes back into their own band.
    let before_minus = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"-");
    std::thread::sleep(Duration::from_millis(500));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_minus,
            "pressing - drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("lanes +"),
            "expected the folded band back after pressing -; screen:\n{screen}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// §4: at 80×24 the Flow's own Story pane drops, and every loop's
/// own lanes fold into their own band regardless of lane count,
/// picked up on the TUI's own next redraw with no key needed; below
/// 80 columns the Flow isn't offered at all, bouncing to the Tape
/// the same way the Signal does.
#[test]
fn resizing_the_flow_screen_drops_the_story_pane_then_bounces_to_the_tape() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    let before_2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    wait_for_pattern_since(&captured_buf, before_2, b"Story", Duration::from_secs(10));

    let before_80 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 80, 24);
    std::thread::sleep(Duration::from_millis(600));
    {
        assert!(
            captured_buf.lock().unwrap().len() > before_80,
            "resizing to 80x24 drew nothing at all"
        );
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            !screen.contains("Story"),
            "expected the Story pane gone at 80x24; screen:\n{screen}"
        );
        assert!(
            screen.contains("slow"),
            "expected the running station's own name still shown at 80x24; screen:\n{screen}"
        );
    }

    let before_60 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 60, 24);
    wait_for_pattern_since(&captured_buf, before_60, b"Tape", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("Tape"),
            "expected the Tape's own panel title once the terminal is too narrow for Flow; screen:\n{screen}"
        );
        assert!(
            !screen.contains("Flow"),
            "Flow must not still be offered below 80 columns; screen:\n{screen}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #688: pressing `1` opens the Signal screen during a live
/// run, with the Now sweep on the trace and the running step's own
/// name below it; `←`/`→` scrub the cursor and `n` returns it to
/// Now; `+`/`-` zoom the column width; `⏎` opens the step under the
/// cursor full-screen and `esc` returns; `0` goes back to the Tape.
#[test]
fn pressing_1_during_a_live_run_opens_the_signal_screen() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, _slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    send_bytes(&mut master, b"1");
    // The replayed screen, not the bytes drawn since the key press: a
    // screen switch redraws only the cells that changed, so text that
    // sits in the same cells on both screens is never sent again.
    let screen = wait_for_screen_text(&captured_buf, "SIGNAL", Duration::from_secs(10));
    assert!(
        screen.contains("SIGNAL"),
        "expected the Signal screen's own chip after pressing 1; got:\n{screen}"
    );
    assert!(
        screen.contains('┃'),
        "expected the Now sweep on the trace; got:\n{screen}"
    );
    assert!(
        screen.contains("slow"),
        "expected the running step's own row; got:\n{screen}"
    );

    // Scrub left, then right, then zoom in and out, then back to
    // Now -- this fixture's own single, always-running step gives
    // no second step for a scrub to change the selection to, and a
    // live run's own spinner redraws regardless of any key here, so
    // this only checks that none of these keys breaks the screen;
    // the cursor's own distinct rendering, separate from the Now
    // sweep, is a buffer-level unit test (`tui.rs`'s own
    // `a_scrubbed_cursor_is_drawn_distinctly_from_the_now_sweep`).
    for key in [&b"\x1b[D"[..], b"\x1b[C", b"+", b"-", b"n"] {
        send_bytes(&mut master, key);
        std::thread::sleep(Duration::from_millis(300));
    }
    {
        let snapshot = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&snapshot);
        assert!(
            rendered.contains("Signal"),
            "the Signal screen must still be showing after scrub/zoom/n; got:\n{rendered:?}"
        );
    }

    let before_enter = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\r");
    std::thread::sleep(Duration::from_millis(300));
    send_bytes(&mut master, b"\x1b");
    std::thread::sleep(Duration::from_millis(300));
    {
        let after = captured_buf.lock().unwrap()[before_enter..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Signal"),
            "expected the Signal screen back after ⏎ then esc; got:\n{rendered:?}"
        );
    }

    let before_0 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"0");
    wait_for_pattern_since(&captured_buf, before_0, b"TAPE", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_0..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("TAPE"),
            "expected the Tape's own chip after pressing 0; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// 120×30 and up shows the Now pane with Readings beside it;
/// 80-119 drops Readings and keeps the Now pane's own running rows
/// alone -- picked up on the TUI's own next redraw, with no key of
/// its own needed.
#[test]
fn resizing_the_signal_screen_drops_readings_at_80_columns() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    send_bytes(&mut master, b"1");
    wait_for_pattern(&captured_buf, b"Readings", Duration::from_secs(10));

    let before_80 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 80, 24);
    std::thread::sleep(Duration::from_millis(600));
    {
        let after = captured_buf.lock().unwrap()[before_80..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            !rendered.contains("Readings"),
            "expected the Readings pane gone at 80x24; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("slow"),
            "expected the running row's own name still shown at 80x24; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// §4: below 80 columns only the Tape is offered -- shrinking the
/// terminal while Signal is on screen bounces straight back to the
/// Tape, picked up on the TUI's own next redraw with no key needed.
#[test]
fn resizing_below_80_columns_bounces_signal_back_to_the_tape() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    send_bytes(&mut master, b"1");
    wait_for_pattern(&captured_buf, b"SIGNAL", Duration::from_secs(10));

    let before_60 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 60, 24);
    // The header band's own chip is past the right edge at 60
    // columns and never drawn at all -- true of the Tape's own too,
    // not something this resize changes -- so the Tape's own panel
    // title is the one piece of chrome at this width that still
    // says which screen is on.
    wait_for_pattern_since(&captured_buf, before_60, b"Tape", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_60..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Tape"),
            "expected the Tape's own panel title once the terminal is too narrow for Signal; got:\n{rendered:?}"
        );
        assert!(
            !rendered.contains("Signal"),
            "Signal must not still be offered below 80 columns; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #635: the Flow screen keeps rebuilding while it's on
/// display, from the run's own state/event/log changes, not only
/// from a key press -- `2` is pressed once, immediately, well before
/// the `if` has even been reached, and every assertion after that
/// waits for the screen to update entirely on its own.
#[test]
fn the_flow_screen_updates_live_without_a_further_key_press() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: gate\n    type: if\n    if:\n      mode: cel\n      expr: \"true\"\n    then:\n      - name: quick\n        type: tool\n        provider: shell\n        params:\n          command: sleep\n          args: [\"0.3\"]\n          allowed_commands: [\"sleep\"]\n      - name: slow\n        type: tool\n        provider: shell\n        params:\n          command: tail\n          args: [\"-f\", \"/dev/null\"]\n          allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    // Pressed at once, before `gate` has even been reached (never
    // `wait_for_step_start` first, the way the ordinary Flow-screen
    // test above does) -- every assertion from here on checks that
    // the screen already on display keeps catching up by itself.
    send_bytes(&mut master, b"2");

    let before_decision = captured_buf.lock().unwrap().len();
    wait_for_pattern(&captured_buf, "→ then".as_bytes(), Duration::from_secs(20));
    {
        let after = captured_buf.lock().unwrap()[before_decision..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("◆"),
            "expected the named if's own decision row to appear with no further key press; got:\n{rendered:?}"
        );
    }

    // `quick` is the only step in this document that can ever
    // finish (`slow` never does, and `gate` stays `Running` for as
    // long as `slow` does) -- so ✓ (as its own three UTF-8 bytes,
    // never split across writes even though a differential redraw
    // can still put an SGR code on either side of it, the same
    // reason `s`'s own assertion above checks for "tate" rather
    // than "state") appearing anywhere at all is this document's own
    // one unambiguous sign that `quick` turned ✓ with no further key
    // press.
    wait_for_pattern(&captured_buf, "✓".as_bytes(), Duration::from_secs(45));
    {
        let full = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&full);
        assert!(
            rendered.contains('✓'),
            "expected the running `quick` step to turn ✓ with no further key press; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled), got {status:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Issue #535: `--no-animation` still gives a real, running TUI (the
/// same alternate-screen entry/exit and clean exit the plain run
/// above gets) -- the flag only ever stops the highlight fade and
/// the scope icon's own trace from moving, never the TUI itself.
#[test]
fn a_short_run_with_no_animation_still_renders_and_exits_cleanly() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc_arg,
            config_arg,
            OsStr::new("--out-dir"),
            out_dir_arg,
            OsStr::new("--no-animation"),
        ],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(
        status.success(),
        "osi --no-animation should exit 0, got {status:?}"
    );

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049h"),
        "expected an alternate-screen entry sequence; got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence; got:\n{rendered:?}"
    );
    assert_no_leftover_process(work.path());
}

/// A SIGTERM must always
/// be honoured, including on the finished screen (DESIGN.md §6.3: the
/// run already ended on its own, and the TUI is just showing its
/// final state until the user leaves it) -- not only while the engine
/// is still going, the only shape `a_forwarded_sigterm_in_the_tui_
/// exits_143` exercises. The engine already exited with its own
/// `code`, so there is nothing left to cancel; osi's own exit code is
/// that same one, not 143 -- SIGTERM here means "stop showing me the
/// finished screen", not "the run failed after all".
#[test]
fn a_forwarded_sigterm_on_the_finished_screen_still_exits_promptly() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let captured_buf = drain_continuously(master);

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    // The `echo` finishes almost at once, well inside `SIGNAL_DELAY`
    // -- by the time this sends SIGTERM, the TUI is on the finished
    // screen already, never having been asked to leave it yet.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(
        status.success(),
        "the run itself already succeeded; a SIGTERM on the finished screen must not turn that \
         into a failure, got {status:?}"
    );

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence after the forwarded signal; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// The terminal-restore half of issue #434's own acceptance criterion
/// ("a test proves raw mode and the alternate screen are left on ...
/// a forwarded signal, through ... a pseudo-terminal test"): a
/// SIGINT delivered mid-run still leaves the alternate-screen exit
/// sequence on the pty, the same as a clean exit, because `do_run`'s
/// signal-forwarding loop only ever returns normally (dropping
/// `TerminalGuard`) -- it never `exit`s out from under it.
#[test]
fn a_forwarded_sigint_still_restores_the_terminal() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let captured_buf = drain_continuously(master);

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    // Waits for the TUI to have actually entered the alternate screen
    // before sending SIGINT, rather than a fixed sleep: a fixed delay
    // long enough to cover a `cof run --help` probe plus a plan
    // compile plus raw-mode/pty setup on a cold, loaded machine was
    // observed to still occasionally lose the race (flaking at both
    // 1.5s and 3s) -- the *actual* signal osi's own render loop is
    // ready matters, not a guess at how long getting there takes.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    // Waits for cof's own start event on the running step, rather
    // than a fixed delay after the pattern wait above (issue #433's
    // comments: the same flakiness #444 already removed from
    // e2e_cof.rs/e2e_electricity.rs this way) -- a fixed delay long
    // enough to cover `cof`'s own cold start installing its own
    // `SIGINT` handling was still observed to occasionally lose the
    // race under load, with `cof` exiting 1 (not "Interrupted") when
    // the signal arrived during its own early start-up instead.
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(130), "got {status:?}");

    // A brief grace period for the draining thread to catch up on
    // whatever osi wrote between its last read and actually exiting
    // (DESIGN.md §6.3: the alternate-screen exit sequence is part of
    // that).
    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049h"),
        "expected an alternate-screen entry sequence; got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence after the forwarded signal; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Review finding K2: a forwarded `SIGTERM` must reach the engine as
/// a real `SIGTERM`, not be silently turned into a `SIGINT` the way
/// the TUI's own `cancel_once` used to — osi itself still exits with
/// the POSIX `128+signum` code either way (143), but only because
/// this asserts the *right* one.
#[test]
fn a_forwarded_sigterm_in_the_tui_exits_143() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let captured_buf = drain_continuously(master);

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(143), "got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence after the forwarded signal; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Review finding K2, `SIGHUP`'s own exit code (129) — and finding
/// K1's "a `SIGHUP` must never wait for a key": the terminal is gone
/// by definition, so this must leave the instant the engine does,
/// never pausing on the finished screen the way a run that simply
/// completes on its own now does.
#[test]
fn a_forwarded_sighup_in_the_tui_exits_129() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let captured_buf = drain_continuously(master);

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    unsafe {
        libc::kill(pid, libc::SIGHUP);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(129), "got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence after the forwarded signal; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// A driver that merely closes its
/// end of the pty and exits, with no signal sent at all, leaves osi
/// without even a SIGHUP -- the slave was never its own *controlling*
/// terminal (`spawn_osi_on_pty`'s own `setsid`, with no `TIOCSCTTY`
/// and, on top, `open_slave`'s own `O_NOCTTY`), so nothing tells the
/// kernel to send one. Before the fix, `event::poll`/`read` never
/// actually errored at all:
/// crossterm's own event source checks only `POLLIN` on the tty fd,
/// never `POLLHUP`, and a hung-up pty slave sets both at once (EOF
/// counts as "readable") -- so crossterm just kept calling a 0-byte
/// `read()` in a tight inner loop until its own poll timeout ran out,
/// then returned cleanly with nothing to show for it, over and over.
/// A whole CPU core spun for as long as the engine (here, a `tail -f`
/// that never finishes on its own) kept running, and every signal
/// sent at it, SIGTERM included, went unanswered -- not because the
/// loop ever stopped reaching its own top, but because nothing in it
/// ever noticed anything was wrong. osi must now notice the dead
/// terminal on its own (`stdin_hung_up`'s own raw, non-blocking
/// `poll(2)`, checked before ever calling into crossterm at all):
/// cancel the engine, and exit promptly -- in practice within a
/// couple of redraw ticks, never minutes.
#[test]
fn a_closed_pty_master_mid_run_with_no_signal_still_exits_promptly_with_no_leftovers() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (mut master, slave_path) = open_pty();

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern_on_master(&mut master, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );

    // Close the master without
    // ever sending osi a signal of any kind -- no clone of this fd
    // survives anywhere else (`wait_for_pattern_on_master` read
    // straight off this same `File`, never a `drain_continuously`
    // clone), so this is the whole pty master actually going away.
    let closed_at = Instant::now();
    drop(master);

    // A generous hard cap (the real incident spun for 13 minutes); the
    // point of this assertion is that it panics long before reaching
    // it if the bug ever came back, not that it needs to be this
    // loose.
    let status = wait_with_timeout(child, Duration::from_secs(15));
    let elapsed = closed_at.elapsed();
    assert!(
        status.code().is_some(),
        "osi should have exited on its own, got {status:?}"
    );
    // 101 is a Rust panic: ratatui's own `Terminal::drop` reporting a
    // failed cursor restore with `eprintln!` on the dead terminal.
    assert_ne!(
        status.code(),
        Some(101),
        "osi panicked on its way out of a lost terminal"
    );
    // The acceptance bound ("within 3s"), with a
    // little slack for a loaded CI runner.
    assert!(
        elapsed < Duration::from_secs(4),
        "osi took {elapsed:?} to notice its terminal was gone -- it should be a couple of \
         redraw ticks, not spinning"
    );

    assert_no_leftover_process(work.path());
}

/// [`a_closed_pty_master_mid_run_with_no_signal_still_exits_promptly_with_no_leftovers`],
/// on the *finished* screen instead -- the run already ended on its
/// own (DESIGN.md §6.3's "the engine exiting does not, on its own,
/// end this loop"), so there is no engine left to cancel at all, only
/// the TUI's own loop still polling a long-gone terminal for a `q`
/// that will now never come. The second
/// combination (`mid`/`finished`), and the one case `cancel_for_lost_
/// terminal`'s own `exit_status.is_none()` check has nothing to do.
#[test]
fn a_closed_pty_master_on_the_finished_screen_with_no_signal_still_exits_promptly() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (mut master, slave_path) = open_pty();

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern_on_master(&mut master, b"\x1b[?1049h", Duration::from_secs(20));
    // The `echo` finishes almost at once -- well inside `SIGNAL_DELAY`,
    // the TUI is on the finished screen already, never having been
    // asked to leave it yet.
    std::thread::sleep(SIGNAL_DELAY);

    let closed_at = Instant::now();
    drop(master);

    let status = wait_with_timeout(child, Duration::from_secs(15));
    let elapsed = closed_at.elapsed();
    // The run itself finished fine before the terminal went away, so
    // osi ends with the run's own exit code: 0, never a panic's 101.
    assert_eq!(
        status.code(),
        Some(0),
        "osi should exit 0 after a successful run when its terminal is gone, got {status:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "osi took {elapsed:?} to notice its terminal was gone on the finished screen -- it \
         should be a couple of redraw ticks, not spinning"
    );

    assert_no_leftover_process(work.path());
}

/// real Ctrl-C keypress never generates a `SIGINT` at all once the
/// TUI has entered raw mode — the byte `0x03` reaches osi as a plain
/// key, and `keys::Action::CtrlC` is the only thing that can still
/// cancel the run and make osi exit 130 the way a real forwarded
/// SIGINT otherwise would.
#[test]
fn ctrl_c_byte_in_the_tui_cancels_and_exits_130() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    send_bytes(&mut master, b"\x03");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(130), "got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence after Ctrl-C; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// `osi watch`'s own TUI (DESIGN.md §6.3): attaches to a run `osi`
/// did not start, renders it in the alternate screen the same as a
/// live `osi <doc>` would, and exits cleanly once the run ends, with
/// no confirm needed (watch owns no engine to cancel).
#[test]
fn osi_watch_renders_in_the_tui_and_exits_cleanly() {
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
    let cof_status = wait_with_timeout(cof.spawn().expect("spawn cof"), Duration::from_secs(30));
    assert!(cof_status.success());

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let run_dir_arg = run_dir.path().as_os_str();
    let child = spawn_osi_on_pty(&home, &slave_path, &[OsStr::new("watch"), run_dir_arg]);

    // Review finding K1: the run `cof` already finished before `osi
    // watch` ever attached, so the TUI shows the finished screen
    // immediately — `q` detaches it, same as the run test above.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi watch should exit 0, got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        rendered.contains("\u{1b}[?1049h"),
        "expected an alternate-screen entry sequence; got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("\u{1b}[?1049l"),
        "expected an alternate-screen exit sequence; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Issue #695: `3` opens the Store screen under `osi watch` too,
/// not only a live `osi <doc>` run -- `osi watch` owns no engine of
/// its own, so the run this attaches to is a separate, still-
/// running `cof` process this test spawns and cleans up itself.
#[test]
fn pressing_3_during_osi_watch_of_a_live_run_opens_the_store_screen() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));
    let events_path = run_dir.path().join("events.jsonl");

    let mut cof = Command::new("cof");
    cof.arg("run")
        .arg(&doc)
        .arg("--config")
        .arg(&config)
        .arg("--quiet")
        .arg("--live-state")
        .arg(run_dir.path().join("state.live.json"))
        .arg("--events")
        .arg(&events_path)
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
    let mut cof_child = cof.spawn().expect("spawn cof");

    wait_for_step_start(&events_path, "prime.slow", Duration::from_secs(20));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let run_dir_arg = run_dir.path().as_os_str();
    let child = spawn_osi_on_pty(&home, &slave_path, &[OsStr::new("watch"), run_dir_arg]);

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);

    let before_3 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"3");
    wait_for_pattern_since(&captured_buf, before_3, b"STORE", Duration::from_secs(10));
    {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        assert!(
            screen.contains("STORE"),
            "expected the Store screen's own chip after pressing 3 under osi watch; screen:\n{screen}"
        );
        assert!(
            screen.contains("Largest Stores"),
            "expected the Largest Stores table after pressing 3 under osi watch; screen:\n{screen}"
        );
        assert!(
            screen.contains("ask · 8 t"),
            "expected the finished prompt's own store, with its own tokens, to show on the Store screen; screen:\n{screen}"
        );
    }

    // `q` under `osi watch` detaches immediately, no confirm needed
    // (watch owns no engine to cancel) -- the still-running `cof`
    // process is this test's own, cleaned up after.
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi watch should exit 0, got {status:?}");

    let _ = cof_child.kill();
    let _ = cof_child.wait();

    assert_no_leftover_process(work.path());
}

/// `osi watch` writes its own registry entry for a run it did not
/// launch (a separate, still-running `cof` process this test spawns
/// directly), and removes it again on detach -- while the watched
/// run itself keeps running underneath.
#[test]
fn osi_watch_writes_its_own_registry_entry_and_removes_it_on_detach() {
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
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work.path(), "config.json", SCRIPTED_CONFIG);
    let events_path = run_dir.path().join("events.jsonl");

    let mut cof = Command::new("cof");
    cof.arg("run")
        .arg(&doc)
        .arg("--config")
        .arg(&config)
        .arg("--quiet")
        .arg("--live-state")
        .arg(run_dir.path().join("state.live.json"))
        .arg("--events")
        .arg(&events_path)
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
    let mut cof_child = cof.spawn().expect("spawn cof");

    wait_for_step_start(&events_path, "prime.slow", Duration::from_secs(20));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let run_dir_arg = run_dir.path().as_os_str();
    let child = spawn_osi_on_pty(&home, &slave_path, &[OsStr::new("watch"), run_dir_arg]);

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));

    let runs_dir = home.path().join("runs");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut entries = Vec::new();
    while Instant::now() < deadline {
        entries = std::fs::read_dir(&runs_dir)
            .map(|rd| rd.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        if !entries.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one registry entry while the watch is live, in {runs_dir:?}"
    );
    let entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entries[0].path()).unwrap()).unwrap();
    assert_eq!(
        entry["launcher"].as_str().unwrap(),
        format!("osi {} watch", env!("CARGO_PKG_VERSION"))
    );

    // `q` under `osi watch` detaches immediately -- the watched
    // `cof` process keeps running underneath, this test's own to
    // kill and clean up after.
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi watch should exit 0, got {status:?}");

    let remaining = std::fs::read_dir(&runs_dir)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(
        remaining, 0,
        "expected the watch's own registry entry to be gone once it detached"
    );

    let _ = cof_child.kill();
    let _ = cof_child.wait();

    assert_no_leftover_process(work.path());
}

// Issue #698 (design v1 part 5): the Select screen's own PTY tests
// (two live runs listed, exactly one attaching straight away, no
// live runs showing the empty Select's Recent Runs table, and the
// `esc` round trip back to Select) live in their own file,
// `e2e_tui_pty_select.rs` -- every one of them reads `discover_runs`'s
// own machine-wide `ps` scan, which would also see any `cof`/
// `electricity` process another test in *this* file's own thread
// pool happens to have live at the same moment (`cargo test` runs
// each test binary in turn, but a single binary's own `#[test]`
// functions run concurrently across threads by default); kept to a
// file with nothing else in it, serialised among themselves with a
// shared lock, they see a clean process table instead.

/// A fake engine standing in for a real `cof`/`electricity`, used
/// only by the two regression tests below (#494): parses out the one
/// flag `osi` always passes regardless of engine or capability
/// detection, `--out <path>`, sleeps long enough that `osi`'s own TUI
/// loop has certainly already polled this run's directory at least
/// once while it was still empty, then writes `events.jsonl`/
/// `state.live.json`/the `--out` file itself all at once and exits --
/// exactly the shape DESIGN.md §1.1 describes for a real engine
/// ("writes its last events and its final state and exits at once"),
/// with no real `cof`/`electricity` build needed on `PATH` at all.
/// Its own content comes entirely from three environment variables
/// (`OSI_DOUBLE_EVENTS`/`OSI_DOUBLE_STATE`/`OSI_DOUBLE_EXIT`) a test
/// sets before spawning `osi`, so one script serves both an ok and a
/// failed run.
const ENGINE_DOUBLE_SCRIPT: &str = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' "usage: --events PATH --live-state PATH"
  exit 0
fi
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--out" ]; then
    out="$arg"
  fi
  prev="$arg"
done
if [ -z "$out" ]; then
  exit 2
fi
run_dir=$(dirname "$out")
sleep "${OSI_DOUBLE_DELAY_S:-0.3}"
printf '%s' "$OSI_DOUBLE_EVENTS" > "$run_dir/events.jsonl"
printf '%s' "$OSI_DOUBLE_STATE" > "$run_dir/state.live.json"
printf '%s' "$OSI_DOUBLE_STATE" > "$out"
exit "${OSI_DOUBLE_EXIT:-0}"
"#;

/// Writes [`ENGINE_DOUBLE_SCRIPT`] to `bin_dir/name`, executable. A
/// short-lived child process writes it, so this test process never
/// holds it open for writing: a fork from another test thread would
/// keep such a handle until its own exec, and starting the double
/// meanwhile fails with ETXTBSY on Linux (#623).
fn write_engine_double(bin_dir: &Path, name: &str) -> PathBuf {
    let path = bin_dir.join(name);
    let mut writer = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("start the engine double's writer");
    std::io::Write::write_all(
        &mut writer.stdin.take().unwrap(),
        ENGINE_DOUBLE_SCRIPT.as_bytes(),
    )
    .expect("write engine double");
    assert!(
        writer.wait().unwrap().success(),
        "writing the engine double failed"
    );
    path
}

/// `PATH`, with `bin_dir` prepended -- so `ElectricityEngine::detect`/
/// `command` find the engine double in `bin_dir` ahead of any real
/// `electricity` build that happens to also be on this machine's own
/// `PATH`.
fn path_with(bin_dir: &Path) -> OsString {
    let mut paths = vec![bin_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).expect("join PATH")
}

/// Issue #494: `run_tui`'s own loop used to stop polling the instant
/// `try_wait` first reported the engine's exit, so a fast-exiting
/// engine's last events and final write -- already sitting on disk
/// by the time `try_wait` can possibly see the exit at all, since a
/// process's own writes always happen-before its own exit -- were
/// simply never read again. The header kept showing the run
/// `aborted`, with `effects 0/1`, forever (nothing left running to
/// ever change it) -- reproduced here with a fake engine (no real
/// `cof`/`electricity` needed) that writes a complete, successful
/// final state only after `osi` has certainly already polled this
/// run's own, still-empty directory at least once.
#[test]
fn a_fast_exiting_engine_shows_the_real_ok_ending_not_aborted() {
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_engine_double(bin_dir.path(), "electricity");

    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: hello\n    type: tool\n    provider: shell\n    params:\n      command: echo\n      args: [\"hi\"]\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let events = concat!(
        "{\"v\":1,\"seq\":0,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n",
        "{\"v\":1,\"seq\":1,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n",
        "{\"v\":1,\"seq\":2,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.hello\",\"ok\":true,\"ms\":1}\n",
        "{\"v\":1,\"seq\":3,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"run_end\",\"ok\":true}\n",
    );
    let state = concat!(
        "{\"runtime\":{\"last_run\":{\"completed_at\":\"2024-01-01T00:00:00Z\",",
        "\"totals\":{\"wall_time_s\":0.01,\"effects_run\":2,\"tokens_sent\":0,\"tokens_received\":0,\"cost_usd\":0.0}}},",
        "\"prime\":{\"value\":true,\"meta\":{\"created_at\":\"2024-01-01T00:00:00Z\",\"completed_at\":\"2024-01-01T00:00:00Z\",\"error\":null,\"flow\":\"chain\"},",
        "\"hello\":{\"value\":\"hi\\n\",\"meta\":{\"created_at\":\"2024-01-01T00:00:00Z\",\"completed_at\":\"2024-01-01T00:00:00Z\",\"exit_code\":0,\"provider\":\"shell\",\"stdout\":\"hi\\n\",\"stderr\":\"\",\"error\":null}}}}",
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let path_env = path_with(bin_dir.path());
    let child = spawn_osi_on_pty_with_env(
        &home,
        &slave_path,
        &[
            doc_arg,
            config_arg,
            OsStr::new("--engine"),
            OsStr::new("electricity"),
            OsStr::new("--out-dir"),
            out_dir_arg,
        ],
        &[
            ("PATH", path_env.as_os_str()),
            ("OSI_DOUBLE_EVENTS", OsStr::new(events)),
            ("OSI_DOUBLE_STATE", OsStr::new(state)),
            ("OSI_DOUBLE_EXIT", OsStr::new("0")),
        ],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    // The engine double sleeps 0.3s before writing anything at all
    // (comfortably more than one of the TUI's own ~30ms poll ticks),
    // then writes and exits in one burst -- a further 1.5s here gives
    // the fix's own drain every chance to have already run and the
    // TUI to have redrawn with it before this reads the pty's buffer.
    std::thread::sleep(Duration::from_millis(1500));
    let rendered_before_switch = captured_buf.lock().unwrap().clone();
    assert!(
        !String::from_utf8_lossy(&rendered_before_switch).contains("aborted"),
        "the TUI showed the finished run as aborted instead of ok; got:\n{:?}",
        String::from_utf8_lossy(&rendered_before_switch)
    );

    // The effects count moved to screen 2's own orchestration pane
    // title (issue #626): switching onto it repaints essentially
    // every cell (a different screen entirely, not a small diff off
    // the overview still on screen), so the final `2/2` lands in one
    // contiguous burst a substring search can actually see -- unlike
    // searching the overview's own already-drawn `done` digit, which
    // a differential redraw only ever retransmits as a single cell
    // (`\x1b[28;42H2` here), never the surrounding `/2 effects` text
    // it sits inside.
    let before_len = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    std::thread::sleep(Duration::from_millis(300));
    let after_switch = {
        let buf = captured_buf.lock().unwrap();
        buf[before_len..].to_vec()
    };
    let rendered_after_switch = String::from_utf8_lossy(&after_switch);
    assert!(
        rendered_after_switch.contains("2/2"),
        "screen 2's own orchestration pane title never showed the real final count; got:\n{rendered_after_switch:?}"
    );

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");

    assert_no_leftover_process(work.path());
}

/// [`a_fast_exiting_engine_shows_the_real_ok_ending_not_aborted`], for
/// a run that failed instead -- the same gap must not turn a real
/// `failed` ending into `aborted` either.
#[test]
fn a_fast_exiting_engine_shows_the_real_failed_ending_not_aborted() {
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    write_engine_double(bin_dir.path(), "electricity");

    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: boom\n    type: tool\n    provider: shell\n    params:\n      command: ls\n      args: [\"/nonexistent-osi-494-path\"]\n",
    );
    let config = write_doc(work.path(), "config.json", ELECTRICITY_CONFIG);

    let events = concat!(
        "{\"v\":1,\"seq\":0,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n",
        "{\"v\":1,\"seq\":1,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.boom\"}\n",
        "{\"v\":1,\"seq\":2,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.boom\",\"ok\":false,\"ms\":1,\"error\":\"/bin/ls failed (exit 1): ls: cannot access\"}\n",
        "{\"v\":1,\"seq\":3,\"ts\":\"2024-01-01T00:00:00Z\",\"ev\":\"run_end\",\"ok\":false,\"error\":\"boom: /bin/ls failed (exit 1)\"}\n",
    );
    let state = concat!(
        "{\"runtime\":{\"last_run\":{\"completed_at\":\"2024-01-01T00:00:00Z\",",
        "\"totals\":{\"wall_time_s\":0.01,\"effects_run\":1,\"tokens_sent\":0,\"tokens_received\":0,\"cost_usd\":0.0}}},",
        "\"prime\":{\"value\":false,\"meta\":{\"created_at\":\"2024-01-01T00:00:00Z\",\"completed_at\":\"2024-01-01T00:00:00Z\",\"error\":\"boom: /bin/ls failed (exit 1)\"},",
        "\"boom\":{\"value\":null,\"meta\":{\"created_at\":\"2024-01-01T00:00:00Z\",\"completed_at\":\"2024-01-01T00:00:00Z\",\"exit_code\":null,\"provider\":\"shell\",\"stdout\":null,\"stderr\":null,\"error\":\"/bin/ls failed (exit 1)\"}}}}",
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let path_env = path_with(bin_dir.path());
    let child = spawn_osi_on_pty_with_env(
        &home,
        &slave_path,
        &[
            doc_arg,
            config_arg,
            OsStr::new("--engine"),
            OsStr::new("electricity"),
            OsStr::new("--out-dir"),
            out_dir_arg,
        ],
        &[
            ("PATH", path_env.as_os_str()),
            ("OSI_DOUBLE_EVENTS", OsStr::new(events)),
            ("OSI_DOUBLE_STATE", OsStr::new(state)),
            ("OSI_DOUBLE_EXIT", OsStr::new("1")),
        ],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(Duration::from_millis(1500));
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(1), "got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        !rendered.contains("aborted"),
        "the TUI showed the finished run as aborted instead of failed; got:\n{rendered:?}"
    );
    // The Tape's own header band (issue #678) carries the status
    // glyph and word together, `✗ Failed` -- the icon trace just
    // ahead of it is in the same failed colour now (§9's settled
    // icon-in-the-word-osi rule), so ratatui's own diff can reuse
    // that run's SGR code across the cursor jump between them
    // rather than emitting a fresh one right before the glyph.
    assert!(
        rendered.contains("✗ Failed"),
        "the TUI never showed the header's own failed status; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}

/// Resizes an already-open pty's own window (issue #678's four size
/// classes): `TIOCSWINSZ` on a freshly opened slave fd, the same way
/// `open_pty` sets the initial size -- the kernel delivers `SIGWINCH`
/// to the pty's own foreground process group on its own, which is
/// what tells crossterm's `event::read()` (osi's own event loop) a
/// resize happened at all, with no signal of this test's own to send.
fn resize_pty(slave_path: &Path, cols: u16, rows: u16) {
    let slave = File::options()
        .read(true)
        .write(true)
        .open(slave_path)
        .unwrap_or_else(|e| panic!("opening pty slave {slave_path:?} for TIOCSWINSZ: {e}"));
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) } != 0 {
        panic!("TIOCSWINSZ failed: {}", std::io::Error::last_os_error());
    }
}

fn spawn_scripted_run_on_tape(
    home: &TestHome,
    work: &Path,
    out_dir: &Path,
) -> (File, PathBuf, std::process::Child, Arc<Mutex<Vec<u8>>>) {
    let doc = write_doc(
        work,
        "do.yml",
        "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let config = write_doc(work, "config.json", SCRIPTED_CONFIG);
    let (master, slave_path) = open_pty();
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.as_os_str();
    let child = spawn_osi_on_pty(
        home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);
    (master, slave_path, child, captured_buf)
}

/// Issue #678's own key map: `p` toggles the Tape's prompt previews
/// off and back on -- a finished prompt's own preview line (`›`)
/// disappears, then reappears.
#[test]
fn pressing_p_on_the_tape_toggles_previews() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));
    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--out-dir"),
            out_dir.path().as_os_str(),
        ],
    );
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains('\u{203a}'),
            "expected a preview line (\u{203a}) with previews on by default; got:\n{rendered:?}"
        );
    }

    let before_p = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"p");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_p..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            !rendered.contains('\u{203a}'),
            "expected no preview line after turning previews off; got:\n{rendered:?}"
        );
    }

    let before_p2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"p");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_p2..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains('\u{203a}'),
            "expected the preview line back after pressing p again; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #678's own key map: `t` on the Tape swaps the token column
/// for the deviation-from-usual view -- a finished row's own bar
/// (`\u{25b0}`) gives way to a `\u{2014}` (no history yet), and `t` again
/// brings the bar back.
#[test]
fn pressing_t_on_the_tape_toggles_the_tokens_view() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));
    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--out-dir"),
            out_dir.path().as_os_str(),
        ],
    );
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains('\u{25b0}'),
            "expected the fixed-scale bar by default; got:\n{rendered:?}"
        );
    }

    let before_t = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"t");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_t..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("vs Usual") || rendered.contains('\u{2014}'),
            "expected the deviation view's own title or dash after pressing t; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #738: the Help overlay opens over the Tape on its Key page
/// (§3.7's glyph/line/colour legend), and `←→` reaches its Keys
/// page (the key bar's own keys, including the movement aliases and
/// `/`/`e` the key bar itself does not show). Closing it (any key,
/// `esc` included) is checked directly against `keys::App::
/// handle_key`, with no pty involved, by `keys::tests::esc_closes_
/// the_help_overlay`.
#[test]
fn the_help_overlay_lists_the_tapes_own_key_and_pages_to_its_keys() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, _slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    send_bytes(&mut master, b"?");
    let screen = wait_for_screen_text(&captured_buf, "Help · Tape · Key", Duration::from_secs(10));
    assert!(
        screen.contains("done · failed") && screen.contains("running · ahead"),
        "expected the Key page's own glyph legend; got:\n{screen}"
    );

    send_bytes(&mut master, "\x1b[C".as_bytes());
    let screen = wait_for_screen_text(&captured_buf, "Help · Tape · Keys", Duration::from_secs(10));
    assert!(
        screen.contains("tokens:") && screen.contains("usual"),
        "expected the Tape's own t key listed; got:\n{screen}"
    );
    assert!(
        screen.contains("filter rows") && screen.contains("errors only"),
        "expected the movement aliases' own / and e rows; got:\n{screen}"
    );

    send_bytes(&mut master, "\x1b[D".as_bytes());
    wait_for_screen_text(&captured_buf, "Help · Tape · Key", Duration::from_secs(10));

    send_bytes(&mut master, b"\x1b");
    std::thread::sleep(Duration::from_millis(300));

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #678's own size classes: 120×30 (list beside its detail
/// pane) → 80×24 (list alone, no detail pane) → 60×24 (glyph, name
/// and the token column only) → 60×16 (too small) and back up again
/// -- each resize picked up on the TUI's own next redraw, with no key
/// of its own needed.
#[test]
fn resizing_through_the_tapes_four_size_classes_and_back() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("Selected"),
            "expected the detail pane's own title at 120x30; got:\n{rendered:?}"
        );
    }

    let before_80 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 80, 24);
    std::thread::sleep(Duration::from_millis(600));
    {
        let after = captured_buf.lock().unwrap()[before_80..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            !rendered.contains("Selected"),
            "expected the detail pane gone at 80x24; got:\n{rendered:?}"
        );
    }

    let before_60 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 60, 24);
    std::thread::sleep(Duration::from_millis(600));
    {
        let after = captured_buf.lock().unwrap()[before_60..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("slow"),
            "expected the running row's own name at 60x24; got:\n{rendered:?}"
        );
    }

    let before_too_small = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 60, 16);
    wait_for_pattern_since(
        &captured_buf,
        before_too_small,
        b"needs",
        Duration::from_secs(10),
    );
    {
        let after = captured_buf.lock().unwrap()[before_too_small..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("needs") && rendered.contains("log"),
            "expected the too-small notice naming the minimum and --log; got:\n{rendered:?}"
        );
    }

    let before_back = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 120, 30);
    wait_for_pattern_since(
        &captured_buf,
        before_back,
        b"Selected",
        Duration::from_secs(10),
    );

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #678's own key map: `↑`/`↓` move the Tape's own selection
/// (off the running row `follow` picked by default, onto the
/// finished `ask` prompt above it), `enter` opens that row's own
/// step full-screen (the list's own bordered title, always `Tape
/// · …`, disappears -- `draw_tape` skips `draw_tape_list` entirely
/// while full-screen), and `esc` closes it back to the normal
/// side-by-side layout.
#[test]
fn arrow_keys_enter_and_esc_move_and_open_a_step_on_the_tape() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: ask\n    type: prompt\n    template: \"hi\"\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
    );
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.ask:\n  - text: \"hello back\"\n    tokens_sent: 3\n    tokens_received: 5\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));
    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--out-dir"),
            out_dir.path().as_os_str(),
        ],
    );
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.slow",
        Duration::from_secs(20),
    );
    std::thread::sleep(SIGNAL_DELAY);

    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("Tape \u{b7}"),
            "expected the list's own bordered title before any key; got:\n{rendered:?}"
        );
    }

    let before_up = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\x1b[A");
    wait_for_pattern_since(&captured_buf, before_up, b"ask", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_up..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("ask"),
            "expected the detail pane to follow the selection up onto ask; got:\n{rendered:?}"
        );
    }

    let before_enter = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\r");
    wait_for_pattern_since(
        &captured_buf,
        before_enter,
        b"Prompt Sent",
        Duration::from_secs(10),
    );
    {
        let after = captured_buf.lock().unwrap()[before_enter..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Prompt Sent"),
            "expected the full-screen step view's own content; got:\n{rendered:?}"
        );
        assert!(
            !rendered.contains("Tape \u{b7}"),
            "expected the list's own bordered title gone while full-screen; got:\n{rendered:?}"
        );
    }

    let before_esc = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\x1b");
    wait_for_pattern_since(&captured_buf, before_esc, b"Tape", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_esc..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Tape \u{b7}"),
            "expected the list's own bordered title back after esc; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #678's own key map: `c` cancels the run after a
/// confirmation, the same as `q` does, but never quits osi itself --
/// the TUI stays up afterward, now showing the run's own Ended state,
/// until a later `q` actually exits it.
#[test]
fn pressing_c_cancels_the_run_without_quitting_the_tui() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, _slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    let before_c = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"c");
    wait_for_pattern_since(&captured_buf, before_c, b"ancel", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_c..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.to_lowercase().contains("cancel"),
            "expected the cancel confirmation dialog; got:\n{rendered:?}"
        );
    }

    let before_y = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"y");
    wait_for_pattern_since(&captured_buf, before_y, b"Ended", Duration::from_secs(20));
    {
        let after = captured_buf.lock().unwrap()[before_y..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Ended"),
            "expected the Tape to show the run's own Ended state after cancelling; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should still exit 130 (the run itself was cancelled); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #678's own §5: the detail pane stays down to 100 columns,
/// not just 120 -- only below 100 does the Tape drop to one pane.
#[test]
fn the_tapes_detail_pane_still_shows_at_100x30() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let (mut master, slave_path, child, captured_buf) =
        spawn_scripted_run_on_tape(&home, work.path(), out_dir.path());

    let before_100 = captured_buf.lock().unwrap().len();
    resize_pty(&slave_path, 100, 30);
    wait_for_pattern_since(
        &captured_buf,
        before_100,
        b"Selected",
        Duration::from_secs(10),
    );
    {
        let after = captured_buf.lock().unwrap()[before_100..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("Selected"),
            "expected the detail pane's own title at 100x30; got:\n{rendered:?}"
        );
    }

    send_bytes(&mut master, b"q");
    std::thread::sleep(Duration::from_millis(200));
    send_bytes(&mut master, b"y");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(130),
        "osi should exit 130 (cancelled, the \"slow\" tool was still running); got {status:?}"
    );
    assert_no_leftover_process(work.path());
}

/// Issue #675's acceptance: a slow-streaming prompt's running row
/// shows its own live token figures growing, then settles to the
/// exact ones once it finishes. A raw loopback server stands in for
/// a real `ollama`, answering with newline-delimited JSON deltas on
/// its own real delay -- the scripted adapter's own `chunks` deliver
/// every delta at once, with no delay between them to observe at
/// all.
fn spawn_slow_ollama_mock() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        // Nothing streamed yet -- the running row reads "waiting for
        // the first token" for as long as this lasts.
        std::thread::sleep(Duration::from_millis(1500));
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
        );
        let _ = stream.write_all(b"{\"model\":\"demo\",\"response\":\"Thanks \",\"done\":false}\n");
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(1200));
        let _ = stream
            .write_all(b"{\"model\":\"demo\",\"response\":\"for the report.\",\"done\":false}\n");
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(300));
        let _ = stream.write_all(
            b"{\"model\":\"demo\",\"response\":\"\",\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":8,\"eval_count\":4}\n",
        );
        let _ = stream.flush();
    });
    addr
}

#[test]
fn a_slow_streaming_prompt_shows_live_token_figures_then_settles() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let addr = spawn_slow_ollama_mock();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: ask\n    type: prompt\n    template: \"Say hi.\"\n",
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &format!(
            "{{\"default_adapter\":\"ollama\",\"default_model\":\"demo\",\"runtime\":{{\"adapters\":{{\"ollama\":{{\"base_url\":\"http://{addr}\"}}}}}}}}"
        ),
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_pattern(&captured_buf, "t/s".as_bytes(), Duration::from_secs(10));
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("\u{2193}\u{2248}"),
            "expected the running row's own received estimate marker while streaming; got:\n{rendered:?}"
        );
    }

    wait_for_pattern(
        &captured_buf,
        "\u{2191}8 \u{2193}4".as_bytes(),
        Duration::from_secs(20),
    );
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("\u{2191}8 \u{2193}4"),
            "expected the finished prompt's own exact token figures (prompt_eval_count 8, eval_count 4); got:\n{rendered:?}"
        );
    }

    // The prompt's final figures land a moment before the run itself
    // ends, and `q` on a run that is still live only asks for a
    // confirmation this test never answers: wait for the header's own
    // `Ended` first. Every wait is its own named step, so a slow runner
    // fails on the step that lagged rather than on the exit.
    wait_for_pattern(&captured_buf, "Ended".as_bytes(), Duration::from_secs(30));
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// [`a_slow_streaming_prompt_shows_live_token_figures_then_settles`],
/// against a real `electricity` build instead of `cof`: the same
/// mock, the same document, the same figures, since both engines
/// write the identical `meta.live` shape (issue #674's PR #683) and
/// drive `on_progress` from the same NDJSON deltas (`run_curl_
/// streaming`'s own `on_line`).
#[test]
fn a_slow_streaming_prompt_shows_live_token_figures_then_settles_on_electricity() {
    if !e2e_electricity_enabled() {
        eprintln!("skipping: OSI_E2E_ELECTRICITY not set or electricity not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let addr = spawn_slow_ollama_mock();
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: ask\n    type: prompt\n    template: \"Say hi.\"\n",
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &format!(
            "{{\"default_adapter\":\"ollama\",\"default_model\":\"demo\",\"runtime\":{{\"adapters\":{{\"ollama\":{{\"base_url\":\"http://{addr}\"}}}}}}}}"
        ),
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc_arg,
            config_arg,
            OsStr::new("--engine"),
            OsStr::new("electricity"),
            OsStr::new("--out-dir"),
            out_dir_arg,
        ],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_pattern(&captured_buf, "t/s".as_bytes(), Duration::from_secs(10));
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("\u{2193}\u{2248}"),
            "expected the running row's own received estimate marker while streaming; got:\n{rendered:?}"
        );
    }

    wait_for_pattern(
        &captured_buf,
        "\u{2191}8 \u{2193}4".as_bytes(),
        Duration::from_secs(20),
    );
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("\u{2191}8 \u{2193}4"),
            "expected the finished prompt's own exact token figures (prompt_eval_count 8, eval_count 4); got:\n{rendered:?}"
        );
    }

    // The prompt's final figures land a moment before the run itself
    // ends, and `q` on a run that is still live only asks for a
    // confirmation this test never answers: wait for the header's own
    // `Ended` first. Every wait is its own named step, so a slow runner
    // fails on the step that lagged rather than on the exit.
    wait_for_pattern(&captured_buf, "Ended".as_bytes(), Duration::from_secs(30));
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// Issue #703's own PTY acceptance item: a running agent session
/// (the reference `fake_pi.py`, issue #637) shows its turn and
/// current tool right on the Tape's own row while it runs, and
/// `enter` opens the one shared Agent block with the transcript tail
/// growing as further tool calls start. Every wait below is its own
/// named step with its own cap, never a bare sleep.
#[test]
fn a_running_agent_shows_its_turn_and_tool_live_and_the_block_grows_on_enter() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let fake_pi = std::fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/agent_live_fakes/fake_pi.py"),
    )
    .expect("fake_pi.py exists");
    // An absolute transcript path -- the agent tool writes it
    // relative to its own `cwd`, which this test's own `cof`
    // process never sets to `work.path()` itself, so a relative name
    // here would otherwise land wherever the test binary's own
    // process happened to start from.
    let transcript_path = work.path().join("agent_transcript.jsonl");
    let doc = write_doc(
        work.path(),
        "do.yml",
        &format!(
            "effects:\n  - name: fixer\n    type: tool\n    provider: agent\n    params:\n      engine: pi\n      prompt: \"Fix the bug.\"\n      tools: [read, bash, edit]\n      transcript: {}\n",
            transcript_path.display()
        ),
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &serde_json::json!({
            "default_adapter": "scripted",
            "default_model": "scripted-model",
            "runtime": {
                "adapters": {"scripted": {}},
                "plugins": {"agent": {"pi": {"binary": fake_pi.to_string_lossy()}}},
            },
        })
        .to_string(),
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(
        &out_dir.path().join("events.jsonl"),
        "prime.fixer",
        Duration::from_secs(20),
    );

    // The fake's own first tool call (`bash`) starts right after its
    // first turn's `message_end`, well before that call's own 2.5s
    // sleep ends -- the Tape row must show the turn and that running
    // tool while it's still in flight, not just the session's own
    // elapsed time.
    let screen = wait_for_screen_text(&captured_buf, "turn 1", Duration::from_secs(15));
    assert!(
        screen.contains("turn 1"),
        "expected the running turn on the Tape row; screen:\n{screen}"
    );
    assert!(
        screen.contains("bash"),
        "expected the running tool on the Tape row; screen:\n{screen}"
    );

    // At this width (120 columns) the Tape's own list and its
    // detail pane already share the screen while following, so the
    // list's own title ("Tape \u{b7} ") is the one thing `enter`'s own
    // full-screen step view removes -- its own detail pane already
    // drew Asked/Turn beside the list, so checking for those alone
    // after `enter` would prove nothing `enter` itself did.
    assert!(
        screen.contains("Tape \u{b7} "),
        "expected the Tape's own list before \u{23ce}; screen:\n{screen}"
    );
    send_bytes(&mut master, b"\r");
    let screen = wait_for_screen_text_gone(&captured_buf, "Tape \u{b7} ", Duration::from_secs(10));
    assert!(
        !screen.contains("Tape \u{b7} "),
        "expected \u{23ce} to open the full-screen step, dropping the list's own title; screen:\n{screen}"
    );
    assert!(
        screen.contains("Asked"),
        "expected the Agent block's own Asked field after \u{23ce}; screen:\n{screen}"
    );
    assert!(
        screen.contains("Turn"),
        "expected the Agent block's own Turn field after \u{23ce}; screen:\n{screen}"
    );
    assert!(
        screen.contains("\u{203a} bash"),
        "expected the first tool call in the transcript tail; screen:\n{screen}"
    );

    // The transcript tail grows: the second tool call (`edit`)
    // appears once its own turn starts, well after the first screen
    // above was captured -- the tail's own `\u{203a}`-prefixed line,
    // never the Tools field's own `edit \u{d7}1`, which would already
    // match without the tail growing at all.
    let screen = wait_for_screen_text(&captured_buf, "\u{203a} edit", Duration::from_secs(15));
    assert!(
        screen.contains("\u{203a} edit"),
        "expected the transcript tail to grow with the second tool call; screen:\n{screen}"
    );

    // The run ends on its own (the fake needs no signal); `q` leaves
    // the now-finished screen the same way every other test here
    // does.
    wait_for_screen_text(&captured_buf, "Ended", Duration::from_secs(30));
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}
/// The Totals row's own token figure, parsed out of a replayed
/// screen -- `" ↑<sent> ↓<received> · <total> t "`
/// (`tui::draw_totals_row`), read as the figure right after the
/// row's own `·`, the same one a human reads as the run's tokens so
/// far. `None` before the Totals row has drawn at all.
fn totals_tokens(screen: &str) -> Option<u64> {
    let line = screen.lines().find(|l| l.contains("Totals"))?;
    let after_dot = line.split('·').nth(1)?;
    let digits: String = after_dot
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect();
    digits.replace(',', "").parse().ok()
}

/// Whether events_path already carries an end event for step_path --
/// used to confirm a growth the Totals row showed was actually
/// observed while the step was still running, not merely after it
/// had already finished (a slow poll could otherwise still pass the
/// "it grew" assertion with nothing left running at all).
fn step_has_ended(events_path: &Path, step_path: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(events_path) else {
        return false;
    };
    text.lines().any(|line| {
        if line.trim().is_empty() {
            return false;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("ev").and_then(|v| v.as_str()) == Some("end")
            && value.get("path").and_then(|v| v.as_str()) == Some(step_path)
    })
}

/// Polls the replayed screen until the Totals row's own token figure
/// reads strictly above `above`, bounded at `timeout` -- panics with
/// the last screen it saw if it never does, same shape as
/// `wait_for_screen_text`.
fn wait_for_totals_tokens_above(
    captured_buf: &Arc<Mutex<Vec<u8>>>,
    above: u64,
    timeout: Duration,
) -> (String, u64) {
    let deadline = Instant::now() + timeout;
    loop {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = support::screen_grid(&whole, 200, 60).join("\n");
        if let Some(tokens) = totals_tokens(&screen) {
            if tokens > above {
                return (screen, tokens);
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "the Totals row never grew past {above} within {timeout:?}; last screen:\n{screen}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Issue #702's acceptance item: a PTY test with the fake `pi` used
/// by the recorded `agent_live_pi` fixture -- the Totals row's own
/// token figure grows while the agent tool runs (its own two tool
/// calls, ~5s total), not only once the step finishes.
#[test]
fn the_totals_row_grows_while_an_agent_session_runs() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let fake_pi = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/agent_live_fakes/fake_pi.py")
        .canonicalize()
        .expect("fake_pi.py must exist");
    let doc = write_doc(
        work.path(),
        "do.yml",
        "effects:\n  - name: fixer\n    type: tool\n    provider: agent\n    params:\n      engine: pi\n      prompt: \"Fix the bug.\"\n      tools: [read, bash, edit]\n",
    );
    let config = write_doc(
        work.path(),
        "config.json",
        &format!(
            "{{\"default_adapter\":\"scripted\",\"default_model\":\"scripted-model\",\"runtime\":{{\"plugins\":{{\"agent\":{{\"pi\":{{\"binary\":{:?}}}}}}}}}}}",
            fake_pi.to_string_lossy()
        ),
    );

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    let events_path = out_dir.path().join("events.jsonl");
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_step_start(&events_path, "prime.fixer", Duration::from_secs(20));
    // Named, polling waits capped by a generous deadline, never a
    // fixed sleep: the fake's own two tool calls together run for
    // only ~5s, so 20s per wait leaves ample room under load while
    // still failing well short of the outer per-test timeout.
    let (early, early_tokens) =
        wait_for_totals_tokens_above(&captured_buf, 0, Duration::from_secs(20));
    assert!(
        !step_has_ended(&events_path, "prime.fixer"),
        "the agent should still be running the moment the Totals row first counts it:\n{early}"
    );

    let (later, later_tokens) =
        wait_for_totals_tokens_above(&captured_buf, early_tokens, Duration::from_secs(20));
    assert!(
        !step_has_ended(&events_path, "prime.fixer"),
        "the agent should still be running when the Totals row grows further: {early_tokens} then {later_tokens}:\n{later}"
    );

    wait_for_pattern(&captured_buf, "Ended".as_bytes(), Duration::from_secs(30));
    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// Issue #715: a count of 1,000 or more never shows a thousands
/// separator, on any screen -- three scripted prompts total 1,234,
/// 35,000,000 and 1,200,000,000 tokens between sent and received, so
/// the Tape, Signal, Flow and Store all draw at least one figure deep
/// in `kt`/`Mt`/`Gt` territory.
#[test]
fn large_token_figures_never_show_thousands_separators_on_any_screen() {
    if !e2e_enabled() {
        eprintln!("skipping: OSI_E2E_COF not set or cof not on PATH");
        return;
    }
    let home = TestHome::new();
    let work = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let replies = write_doc(
        work.path(),
        "replies.yaml",
        "prime.small:\n  - text: \"ok\"\n    tokens_sent: 234\n    tokens_received: 1000\nprime.million:\n  - text: \"ok\"\n    tokens_sent: 5000000\n    tokens_received: 30000000\nprime.billion:\n  - text: \"ok\"\n    tokens_sent: 200000000\n    tokens_received: 1000000000\n",
    );
    let doc = write_doc(
        work.path(),
        "do.yml",
        "adapter: scripted\nmodel: scripted-model\neffects:\n  - name: small\n    type: prompt\n    template: \"hi\"\n  - name: million\n    type: prompt\n    template: \"hi\"\n  - name: billion\n    type: prompt\n    template: \"hi\"\n",
    );
    let config = write_doc(work.path(), "config.json", &scripted_config(&replies));

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let child = spawn_osi_on_pty(
        &home,
        &slave_path,
        &[
            doc.as_os_str(),
            config.as_os_str(),
            OsStr::new("--out-dir"),
            out_dir.path().as_os_str(),
        ],
    );
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    wait_for_pattern(&captured_buf, "Ended".as_bytes(), Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);

    let tape = wait_for_screen_text(&captured_buf, "TAPE", Duration::from_secs(10));
    assert_no_thousands_separator(&tape, "Tape");

    send_bytes(&mut master, b"1");
    let signal = wait_for_screen_text(&captured_buf, "SIGNAL", Duration::from_secs(10));
    assert_no_thousands_separator(&signal, "Signal");

    send_bytes(&mut master, b"2");
    let flow = wait_for_screen_text(&captured_buf, "FLOW", Duration::from_secs(10));
    assert_no_thousands_separator(&flow, "Flow");

    send_bytes(&mut master, b"3");
    let store = wait_for_screen_text(&captured_buf, "STORE", Duration::from_secs(10));
    assert_no_thousands_separator(&store, "Store");

    send_bytes(&mut master, b"q");
    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osi should exit 0, got {status:?}");
    std::thread::sleep(Duration::from_millis(200));
    assert_no_leftover_process(work.path());
}

/// A comma with a digit on both sides is a thousands separator in
/// every token figure this screen ever draws -- nothing else in the
/// chrome puts a digit directly against a comma, and no axis label
/// ever carries one either (`format_shared_axis_label` only ever
/// prints a plain count or a `k`-scaled one, never a comma).
fn assert_no_thousands_separator(screen: &str, screen_name: &str) {
    let chars: Vec<char> = screen.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != ',' || i == 0 || i + 1 >= chars.len() {
            continue;
        }
        assert!(
            !(chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit()),
            "expected no thousands-separated token figure on the {screen_name} screen; screen:\n{screen}"
        );
    }
}
