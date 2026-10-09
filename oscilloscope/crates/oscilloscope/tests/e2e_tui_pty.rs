//! End-to-end TUI test against a real `cof`, on a real pseudo-
//! terminal (issue #434's Tests section): `osp`'s own stdout/stderr
//! are a pty slave, not a pipe, so `effective_log_mode` picks the
//! interactive TUI exactly as it would on a human's own terminal.
//! Gated the same way `e2e_cof.rs` is: `OSP_E2E_COF=1` and `cof` on
//! `PATH`, a temporary `HOME`, the scripted adapter, stripped
//! credentials, a hard per-test timeout.
//!
//! No extra pty crate: `posix_openpt`/`grantpt`/`unlockpt`/`ptsname`
//! are already available through `libc`, already a dev-dependency of
//! this crate.

mod support;

use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use support::{
    SCRIPTED_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process, e2e_enabled,
    wait_with_timeout, write_doc,
};

/// Opens a fresh pty pair, unlocked and granted, ready for a child to
/// use its slave side as its controlling stdio. Returns the master
/// (kept open for the test's own lifetime; its own `Drop` closes it)
/// and the slave device's path, re-opened fresh for each of the
/// child's three standard streams below — a single shared `File`
/// handle would make all three the same underlying OS file
/// description, which is not what three independent dup'd fds of a
/// real terminal ever are.
fn open_pty() -> (File, PathBuf) {
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    assert!(master_fd >= 0, "posix_openpt failed");
    assert_eq!(unsafe { libc::grantpt(master_fd) }, 0, "grantpt failed");
    assert_eq!(unsafe { libc::unlockpt(master_fd) }, 0, "unlockpt failed");

    let slave_name = unsafe { libc::ptsname(master_fd) };
    assert!(!slave_name.is_null(), "ptsname failed");
    let slave_path = unsafe { std::ffi::CStr::from_ptr(slave_name) }
        .to_str()
        .expect("ptsname is valid UTF-8")
        .to_string();

    let master = unsafe { File::from_raw_fd(master_fd as RawFd) };
    (master, PathBuf::from(slave_path))
}

/// Drains `master` continuously, from the moment it's called, into a
/// shared buffer a caller can inspect at any time -- started right
/// after the pty is opened, *before* the child is even spawned,
/// rather than only read from in bursts (a redrawing TUI can fill a
/// pty's own small kernel buffer in well under a second; nothing
/// draining it would make osp's own writes block, delaying its exit
/// for however long this test wasn't reading, and risking losing
/// whatever it tried to write once it finally did get to exit).
fn drain_continuously(mut master: File) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&buf);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut chunk = [0u8; 4096];
        loop {
            match master.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => shared.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });
    buf
}

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

/// Writes `bytes` to the pty's master side, i.e. exactly what a key
/// press on a real terminal would deliver to osp's own raw-mode
/// stdin — used to send `q` (review finding K1: the TUI now stays
/// open on a run's own final state until asked to leave) and a raw
/// Ctrl-C byte (finding K3: raw mode disables the kernel's own
/// `ISIG`, so `\x03` reaches osp as a plain byte, not a real SIGINT,
/// exactly as it would on a human's own terminal).
fn send_bytes(master: &mut File, bytes: &[u8]) {
    master.write_all(bytes).expect("write to pty master");
    master.flush().expect("flush pty master");
}

fn open_slave(path: &std::path::Path) -> OwnedFd {
    let file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("opening pty slave {path:?}: {e}"));
    OwnedFd::from(file)
}

fn stdio_from_slave(path: &std::path::Path) -> Stdio {
    let fd = open_slave(path);
    Stdio::from(fd)
}

/// Spawns `osp` with every one of its standard streams attached to
/// the pty's slave side, in its own process group (the same
/// isolation `osp` itself gives `cof`, and the lane rule this
/// repository's tests all follow: "signals only to processes you
/// started, in their own session").
fn spawn_osp_on_pty(
    home: &TestHome,
    slave_path: &std::path::Path,
    args: &[&OsStr],
) -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osp"));
    cmd.args(args);
    cmd.env("HOME", home.path());
    cmd.env("TERM", "xterm-256color");
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
        // `effective_log_mode` (DESIGN.md §6.2) forces the plain
        // stream whenever `CI` is set, which GitHub Actions sets for
        // every job regardless of what osp's own stdout actually is
        // -- without removing it here, this whole test file always
        // exercised the plain path on CI, pty and all, and never the
        // TUI it exists to test (caught by the real CI run, not by
        // this machine, which never had `CI` set to begin with).
        "CI",
    ] {
        cmd.env_remove(key);
    }
    cmd.stdin(stdio_from_slave(slave_path));
    cmd.stdout(stdio_from_slave(slave_path));
    cmd.stderr(stdio_from_slave(slave_path));
    // Only `setpgid` (not `setsid`): a `setsid` call from a `pre_exec`
    // closure was tried here and found to make the spawned child
    // permanently unreapable by this test's own `std::process::Child`
    // (`try_wait`/`wait` never see it exit, confirmed with a minimal
    // repro with no osp involved at all -- a macOS-specific quirk of
    // `waitpid` on a child that called `setsid`). A plain tty slave
    // fd (no explicit `TIOCSCTTY`) is still enough for
    // `crossterm::terminal::enable_raw_mode`'s own `tcgetattr`/
    // `tcsetattr`, which only need an open fd referring to some tty
    // device, not a controlling-terminal relationship -- this process
    // group isolation is here for the same reason `osp` itself gives
    // `cof` its own (DESIGN.md §4.1), so a signal sent to just this
    // pid never reaches the test harness's own process group either.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn osp on a pty")
}

#[test]
fn a_short_run_renders_in_the_tui_and_exits_cleanly_with_no_leftovers() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
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
    // under a second, and nothing draining it would make osp's own
    // writes block). Reading happens on a dup'd fd so `master` itself
    // stays free for this test to write a key press back.
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osp_on_pty(
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
    assert!(status.success(), "osp should exit 0, got {status:?}");

    // Finding 17: the same grace period every other test here gives
    // the draining thread to catch up on whatever osp wrote between
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

    // DESIGN.md §6.3 / this repository's own lane rules: nothing osp
    // or cof started is still running, and no part of this test
    // created an `osp-*` directory under the real system temp dir in
    // the first place (`--out-dir` above kept it inside `out_dir`,
    // this test's own, already-cleaned-up tempdir).
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
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
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
    let child = spawn_osp_on_pty(
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
    // 1.5s and 3s) -- the *actual* signal osp's own render loop is
    // ready matters, not a guess at how long getting there takes.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    // `support::SIGNAL_DELAY` on top of the pattern wait: osp's own
    // TUI is ready as soon as the alternate screen appears, but `cof`
    // (a separate, independently cold-starting Python process) still
    // needs its own moment to install its own `SIGINT` handling --
    // skipping this was observed to occasionally have `cof` itself
    // exit 1 (not "Interrupted") when the signal arrived during its
    // own early start-up instead.
    std::thread::sleep(SIGNAL_DELAY);
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(130), "got {status:?}");

    // A brief grace period for the draining thread to catch up on
    // whatever osp wrote between its last read and actually exiting
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
/// the TUI's own `cancel_once` used to — osp itself still exits with
/// the POSIX `128+signum` code either way (143), but only because
/// this asserts the *right* one.
#[test]
fn a_forwarded_sigterm_in_the_tui_exits_143() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
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
    let child = spawn_osp_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
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
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
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
    let child = spawn_osp_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
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

/// Review finding K3: raw mode disables the kernel's own `ISIG`, so a
/// real Ctrl-C keypress never generates a `SIGINT` at all once the
/// TUI has entered raw mode — the byte `0x03` reaches osp as a plain
/// key, and `keys::Action::CtrlC` is the only thing that can still
/// cancel the run and make osp exit 130 the way a real forwarded
/// SIGINT otherwise would.
#[test]
fn ctrl_c_byte_in_the_tui_cancels_and_exits_130() {
    if !e2e_enabled() {
        eprintln!("skipping: OSP_E2E_COF not set or cof not on PATH");
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
    let child = spawn_osp_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
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

/// `osp watch`'s own TUI (DESIGN.md §6.3): attaches to a run `osp`
/// did not start, renders it in the alternate screen the same as a
/// live `osp <doc>` would, and exits cleanly once the run ends, with
/// no confirm needed (watch owns no engine to cancel).
#[test]
fn osp_watch_renders_in_the_tui_and_exits_cleanly() {
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
    let cof_status = wait_with_timeout(cof.spawn().expect("spawn cof"), Duration::from_secs(30));
    assert!(cof_status.success());

    let (master, slave_path) = open_pty();
    let mut master = master;
    let captured_buf = drain_continuously(master.try_clone().expect("dup the pty master"));
    let run_dir_arg = run_dir.path().as_os_str();
    let child = spawn_osp_on_pty(&home, &slave_path, &[OsStr::new("watch"), run_dir_arg]);

    // Review finding K1: the run `cof` already finished before `osp
    // watch` ever attached, so the TUI shows the finished screen
    // immediately — `q` detaches it, same as the run test above.
    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    std::thread::sleep(SIGNAL_DELAY);
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osp watch should exit 0, got {status:?}");

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
