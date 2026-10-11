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

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use support::{
    ELECTRICITY_CONFIG, SCRIPTED_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process,
    e2e_enabled, wait_for_step_start, wait_with_timeout, write_doc,
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

    // A freshly opened pty's own window size defaults to 0x0 until
    // something sets it -- crossterm's own `terminal::size()` reads
    // exactly this, so without it every frame a TUI draws lays out
    // into a zero-area buffer and writes nothing at all beyond the
    // alternate-screen/cursor sequences every test here already
    // checks (#494's own regression tests, further down this file,
    // need the header's actual text, not just those). `TIOCSWINSZ`
    // on the master fd alone fails ENOTTY on macOS -- it has to go
    // through an open slave fd, but the size itself belongs to the
    // pty, not that one fd, so it's still in effect once this one
    // closes again.
    {
        let slave = File::options()
            .read(true)
            .write(true)
            .open(&slave_path)
            .unwrap_or_else(|e| panic!("opening pty slave {slave_path:?} for TIOCSWINSZ: {e}"));
        let ws = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) } != 0 {
            panic!("TIOCSWINSZ failed: {}", std::io::Error::last_os_error());
        }
        // Not closed: on macOS, the slave side's open-file count
        // dropping to zero even briefly -- closing this one before
        // `spawn_osp_on_pty` opens its own three, moments later --
        // signals a hangup the master side sees as an immediate EOF,
        // which silently ended `drain_continuously`'s own read loop
        // before osp ever wrote a single byte. Leaking this one fd
        // for the rest of the test binary's own short life keeps the
        // slave's own open count never reaching zero in between.
        std::mem::forget(slave);
    }

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
    spawn_osp_on_pty_with_env(home, slave_path, args, &[])
}

/// [`spawn_osp_on_pty`], with extra environment variables set on the
/// spawned `osp` before it starts -- the final-drain regression test
/// below (#494) needs its own fake engine on `PATH` and a few
/// variables that fake engine itself reads, neither of which any
/// other caller here needs.
fn spawn_osp_on_pty_with_env(
    home: &TestHome,
    slave_path: &std::path::Path,
    args: &[&OsStr],
    extra_env: &[(&str, &OsStr)],
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
    for (key, value) in extra_env {
        cmd.env(key, value);
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

/// A fake engine standing in for a real `cof`/`electricity`, used
/// only by the two regression tests below (#494): parses out the one
/// flag `osp` always passes regardless of engine or capability
/// detection, `--out <path>`, sleeps long enough that `osp`'s own TUI
/// loop has certainly already polled this run's directory at least
/// once while it was still empty, then writes `events.jsonl`/
/// `state.live.json`/the `--out` file itself all at once and exits --
/// exactly the shape DESIGN.md §1.1 describes for a real engine
/// ("writes its last events and its final state and exits at once"),
/// with no real `cof`/`electricity` build needed on `PATH` at all.
/// Its own content comes entirely from three environment variables
/// (`OSP_DOUBLE_EVENTS`/`OSP_DOUBLE_STATE`/`OSP_DOUBLE_EXIT`) a test
/// sets before spawning `osp`, so one script serves both an ok and a
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
sleep "${OSP_DOUBLE_DELAY_S:-0.3}"
printf '%s' "$OSP_DOUBLE_EVENTS" > "$run_dir/events.jsonl"
printf '%s' "$OSP_DOUBLE_STATE" > "$run_dir/state.live.json"
printf '%s' "$OSP_DOUBLE_STATE" > "$out"
exit "${OSP_DOUBLE_EXIT:-0}"
"#;

/// Writes [`ENGINE_DOUBLE_SCRIPT`] to `bin_dir/name`, executable.
fn write_engine_double(bin_dir: &Path, name: &str) -> PathBuf {
    let path = bin_dir.join(name);
    std::fs::write(&path, ENGINE_DOUBLE_SCRIPT).expect("write engine double");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
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
/// final state only after `osp` has certainly already polled this
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
    let child = spawn_osp_on_pty_with_env(
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
            ("OSP_DOUBLE_EVENTS", OsStr::new(events)),
            ("OSP_DOUBLE_STATE", OsStr::new(state)),
            ("OSP_DOUBLE_EXIT", OsStr::new("0")),
        ],
    );

    wait_for_pattern(&captured_buf, b"\x1b[?1049h", Duration::from_secs(20));
    // The engine double sleeps 0.3s before writing anything at all
    // (comfortably more than one of the TUI's own ~30ms poll ticks),
    // then writes and exits in one burst -- a further 1.5s here gives
    // the fix's own drain every chance to have already run and the
    // TUI to have redrawn with it before this reads the pty's buffer.
    std::thread::sleep(Duration::from_millis(1500));
    send_bytes(&mut master, b"q");

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osp should exit 0, got {status:?}");

    std::thread::sleep(Duration::from_millis(200));
    let captured = captured_buf.lock().unwrap().clone();
    let rendered = String::from_utf8_lossy(&captured);
    assert!(
        !rendered.contains("aborted"),
        "the TUI showed the finished run as aborted instead of ok; got:\n{rendered:?}"
    );
    assert!(
        rendered.contains("effects 1/1"),
        "the TUI never showed every effect counted (effects 1/1); got:\n{rendered:?}"
    );

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
        "effects:\n  - name: boom\n    type: tool\n    provider: shell\n    params:\n      command: ls\n      args: [\"/nonexistent-osp-494-path\"]\n",
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
    let child = spawn_osp_on_pty_with_env(
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
            ("OSP_DOUBLE_EVENTS", OsStr::new(events)),
            ("OSP_DOUBLE_STATE", OsStr::new(state)),
            ("OSP_DOUBLE_EXIT", OsStr::new("1")),
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
    // The header's own status span (`Color::Red` for `RunStatus::
    // Failed`), the same specific check the ok test above makes.
    assert!(
        rendered.contains("\u{1b}[38;5;1;49mfailed"),
        "the TUI never showed the header's own failed status; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}
