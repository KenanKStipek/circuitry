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
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use support::{
    ELECTRICITY_CONFIG, SCRIPTED_CONFIG, SIGNAL_DELAY, TestHome, assert_no_leftover_process,
    e2e_enabled, scripted_config, wait_for_step_start, wait_with_timeout, write_doc,
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
    // Without `FD_CLOEXEC`, `spawn_osi_on_pty`'s own `Command::spawn`
    // (fork, then exec) would hand osi itself a surviving duplicate
    // of this exact fd -- a real bug this file's own tests never hit
    // before, since none of them ever needed the master to actually,
    // fully close while osi was still alive: osi would then still
    // hold the pty's master open all on its own even after this
    // test's own copy is dropped, so the slave side this test cares
    // about (osi's own stdin/stdout/stderr) would never see the EOF/
    // write-error a closed master is supposed to cause at all -- the
    // bug `a_closed_pty_master_with_no_signal_still_exits_promptly_
    // with_no_leftovers` below exists to catch, found by that exact
    // test hanging on this exact gap.
    assert_eq!(
        unsafe { libc::fcntl(master_fd, libc::F_SETFD, libc::FD_CLOEXEC) },
        0,
        "fcntl F_SETFD FD_CLOEXEC failed"
    );

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
        // `spawn_osi_on_pty` opens its own three, moments later --
        // signals a hangup the master side sees as an immediate EOF,
        // which silently ended `drain_continuously`'s own read loop
        // before osi ever wrote a single byte. Leaking this one fd
        // for the rest of the test binary's own short life keeps the
        // slave's own open count never reaching zero in between.
        //
        // Review finding 5 on PR #496: the cost of that is paid by
        // every test in this file, not just this one -- this leaked
        // fd stays open for the whole binary's life, so the real
        // slave fds `spawn_osi_on_pty` opens next are never the last
        // ones left, and `master` (this pty's own read side) can
        // never see a real EOF once osi's own three close on exit.
        // `drain_continuously`'s loop below only ever stops reading
        // because its read errors (the master itself closing when
        // this function's own `File` drops at the end of a test),
        // never because the pipe quietly ran dry -- no test here can
        // be written the other way, waiting on `master` for EOF to
        // know osi is done, while this leak stands.
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
/// draining it would make osi's own writes block, delaying its exit
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

/// Writes `bytes` to the pty's master side, i.e. exactly what a key
/// press on a real terminal would deliver to osi's own raw-mode
/// stdin — used to send `q` (review finding K1: the TUI now stays
/// open on a run's own final state until asked to leave) and a raw
/// Ctrl-C byte (finding K3: raw mode disables the kernel's own
/// `ISIG`, so `\x03` reaches osi as a plain byte, not a real SIGINT,
/// exactly as it would on a human's own terminal).
fn send_bytes(master: &mut File, bytes: &[u8]) {
    master.write_all(bytes).expect("write to pty master");
    master.flush().expect("flush pty master");
}

fn open_slave(path: &std::path::Path) -> OwnedFd {
    // On Linux, a process that has
    // called `setsid()` (no controlling terminal of its own yet) and
    // then opens a tty device *by path* auto-acquires it as its
    // controlling terminal, same as a bare `open()` + no `O_NOCTTY`
    // always has on historical SysV ttys -- `osi` itself never asks
    // for one (`TerminalGuard::enter` only `tcgetattr`/`tcsetattr`s an
    // already-open fd), so the three slave opens below, each handed
    // to the spawned `osi` as a *fresh* stdio fd rather than a dup of
    // one already-open one, must say so explicitly: a real SIGHUP
    // test (`a_forwarded_sighup_in_the_tui_exits_129`) sends the
    // signal directly to osi's own pid regardless, but the whole
    // *point* of the lost-terminal tests below is a slave that was
    // never, ever a controlling terminal for anyone.
    let file = File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)
        .unwrap_or_else(|e| panic!("opening pty slave {path:?}: {e}"));
    OwnedFd::from(file)
}

fn stdio_from_slave(path: &std::path::Path) -> Stdio {
    let fd = open_slave(path);
    Stdio::from(fd)
}

/// Spawns `osi` with every one of its standard streams attached to
/// the pty's slave side, in its own process group (the same
/// isolation `osi` itself gives `cof`, and the lane rule this
/// repository's tests all follow: "signals only to processes you
/// started, in their own session").
fn spawn_osi_on_pty(
    home: &TestHome,
    slave_path: &std::path::Path,
    args: &[&OsStr],
) -> std::process::Child {
    spawn_osi_on_pty_with_env(home, slave_path, args, &[])
}

/// [`spawn_osi_on_pty`], with extra environment variables set on the
/// spawned `osi` before it starts -- the final-drain regression test
/// below (#494) needs its own fake engine on `PATH` and a few
/// variables that fake engine itself reads, neither of which any
/// other caller here needs.
fn spawn_osi_on_pty_with_env(
    home: &TestHome,
    slave_path: &std::path::Path,
    args: &[&OsStr],
    extra_env: &[(&str, &OsStr)],
) -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osi"));
    cmd.args(args);
    cmd.env("HOME", home.path());
    cmd.env("TERM", "xterm-256color");
    // Issue #649: a bare `HOME` override isn't enough on its own --
    // an inherited `XDG_STATE_HOME` or `OSI_HISTORY_DIR` would still
    // point history at the real directory.
    cmd.env("OSI_HISTORY_DIR", home.path().join("history"));
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
        "XDG_STATE_HOME",
        "OSI_HISTORY",
        // `effective_log_mode` (DESIGN.md §6.2) forces the plain
        // stream whenever `CI` is set, which GitHub Actions sets for
        // every job regardless of what osi's own stdout actually is
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
    // `setsid()`, not merely
    // `setpgid(0, 0)` -- matching how a real terminal-launching
    // driver spawns its own child (Python's own `start_new_session=
    // True`), and the only
    // way osi ends up with *no controlling terminal at all* rather
    // than merely its own process group within the test harness's
    // own session. A `setsid` call from a `pre_exec` closure was once
    // suspected of making the spawned child permanently unreapable by
    // this test's own `std::process::Child` on macOS; re-checked
    // directly (a minimal repro spawning `/bin/sleep` this same way,
    // `try_wait`-polled to completion) and found to reap normally --
    // every pty test in this file below still reaps `osi` the same
    // way regardless. A plain tty slave fd (no explicit `TIOCSCTTY`,
    // and now `O_NOCTTY` on top, `open_slave`'s own doc comment) is
    // still enough for `crossterm::terminal::enable_raw_mode`'s own
    // `tcgetattr`/`tcsetattr`, which only need an open fd referring
    // to some tty device, not a controlling-terminal relationship --
    // this isolation is here for the same reason `osi` itself gives
    // `cof` its own (DESIGN.md §4.1), so a signal sent to just this
    // pid never reaches the test harness's own session either.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn osi on a pty")
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

/// Issue #626: `0`/`2` switch screens during a live run, and the
/// screen each one draws actually changes on screen -- the overview
/// (its own `now`/`time`/`flow`/`cost` panes) on `0`, screen 2's own
/// orchestration pane on `2`.
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

    // Default is the overview; this run is still live. Checked
    // against the whole buffer so far, since nothing has been sent
    // yet for a key press to have changed anything after.
    {
        let captured = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&captured);
        assert!(
            rendered.contains("now") && rendered.contains("time") && rendered.contains("cost"),
            "expected the overview's own panes on screen; got:\n{rendered:?}"
        );
    }

    // Every check from here on reads only what arrived *after* its
    // own key -- "flow" is in the header's own tab on every screen,
    // and "cost"/"now"/"time" were already drawn once by the overview
    // above, so searching the whole cumulative buffer again could
    // never actually fail, key press or not.
    let before_2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_2..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("flow ("),
            "expected the Flow screen's own flow pane after pressing 2; got:\n{rendered:?}"
        );
    }

    let before_0 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"0");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_0..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("flow") && rendered.contains("cost"),
            "expected the overview's own cards back after pressing 0; got:\n{rendered:?}"
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

/// Issue #632: `3` opens the Cost screen during a live run, and its
/// own treemap and largest-stores table actually show the prompt
/// that already finished.
#[test]
fn pressing_3_during_a_live_run_opens_the_cost_screen() {
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

    let before_3 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"3");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_3..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("tokens by store"),
            "expected the Cost screen's own treemap after pressing 3; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("largest stores"),
            "expected the Cost screen's own table after pressing 3; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("ask"),
            "expected the finished prompt's own store to show somewhere on the Cost screen; got:\n{rendered:?}"
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

/// Issue #635's own acceptance item: pressing `2` during a live `cof`
/// run of a document with a named `if` opens the Flow screen with
/// the decision's own `◆` row and a matching story line, and `s`
/// swaps the selected pane for the state tree.
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

    let before_2 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"2");
    // `meta.branch` lands on the live state once cof writes it (ahead
    // of `slow` itself starting, DESIGN.md §1.2), but the TUI only
    // picks it up on its own next live-state poll -- waited for
    // directly, rather than a fixed sleep guessing how long that
    // takes.
    wait_for_pattern(&captured_buf, "→ then".as_bytes(), Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_2..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("◆"),
            "expected the named if's own decision row after pressing 2; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("gate"),
            "expected gate's own row after pressing 2; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("story"),
            "expected the story pane's own border title after pressing 2; got:\n{rendered:?}"
        );
    }

    let before_s = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"s");
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_s..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        // A differential redraw only retransmits changed cells
        // (`a_fast_exiting_engine_shows_the_real_ok_ending_not_
        // aborted`'s own comment, below): the border title's own
        // leading " s" can coincidentally already match the
        // previous frame's " selected " title at the same cell and
        // style, leaving only "tate" actually retransmitted --
        // checked for instead of the full word for exactly that
        // reason.
        assert!(
            rendered.contains("tate"),
            "expected the state tree's own border title in the selected pane's own place after pressing s; got:\n{rendered:?}"
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

/// Issue #650: pressing `1` opens the Time screen during a live
/// run, which shows a running row's own bar and the now line;
/// `⏎` on a finished loop row expands it into one row per pass;
/// `+` and `=` zoom the axis and fit it back without the screen
/// breaking.
#[test]
fn pressing_1_during_a_live_run_opens_the_time_screen() {
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
        "effects:\n  - type: tool\n    name: items\n    provider: json\n    params: {mode: parse, input: \"[1, 2]\"}\n  - type: loop\n    name: fan\n    each: {in: prime.items.value, as: item}\n    body:\n      - type: tool\n        name: nap\n        provider: shell\n        params:\n          command: echo\n          args: [\"x\"]\n          allowed_commands: [\"echo\"]\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: tail\n      args: [\"-f\", \"/dev/null\"]\n      allowed_commands: [\"tail\"]\n",
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

    let before_1 = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"1");
    wait_for_pattern(&captured_buf, b"timeline", Duration::from_secs(10));
    {
        let after = captured_buf.lock().unwrap()[before_1..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("timeline"),
            "expected the timeline pane's own border title after pressing 1; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains('┃'),
            "expected the now line on the running row's own bar; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("slow"),
            "expected the running step's own row; got:\n{rendered:?}"
        );
    }

    // Turns follow off, then jumps to the first row (`prime`) and
    // down twice to land on `fan` deterministically (`items` is the
    // one step before it) -- more robust than counting rows up from
    // the running step, since whether `fan`'s own two passes fold
    // into one "quick steps" summary or stay separate rows isn't
    // this test's concern. Each key is its own write, with enough of
    // a pause after for a redraw to land before the next.
    for key in [&b"f"[..], b"g", b"j", b"j"] {
        send_bytes(&mut master, key);
        std::thread::sleep(Duration::from_millis(400));
    }

    let before_enter = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"\r");
    wait_for_pattern_since(
        &captured_buf,
        before_enter,
        b"pass 1",
        Duration::from_secs(10),
    );
    {
        let after = captured_buf.lock().unwrap()[before_enter..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("pass 1") && rendered.contains("pass 2"),
            "expected one row per pass after ⏎ on the loop row; got:\n{rendered:?}"
        );
    }

    // `ratatui` only ever writes bytes for cells that actually
    // changed -- a keypress whose own effect, if any, never touches
    // what's on screen (the whole buffer already contained
    // "timeline" from the very first redraw) would otherwise pass
    // this check for doing nothing at all. Checking each keypress's
    // own incremental output is non-empty instead proves each one
    // really did redraw something.
    let before_plus = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"+");
    std::thread::sleep(Duration::from_millis(300));
    let after_plus = captured_buf.lock().unwrap()[before_plus..].to_vec();
    assert!(
        !after_plus.is_empty(),
        "+ should zoom the timeline in and redraw it; got no output at all"
    );

    let before_equals = captured_buf.lock().unwrap().len();
    send_bytes(&mut master, b"=");
    std::thread::sleep(Duration::from_millis(300));
    let after_equals = captured_buf.lock().unwrap()[before_equals..].to_vec();
    assert!(
        !after_equals.is_empty(),
        "= should fit the timeline back to the whole run and redraw it; got no output at all"
    );
    {
        let snapshot = captured_buf.lock().unwrap().clone();
        let rendered = String::from_utf8_lossy(&snapshot);
        assert!(
            rendered.contains("timeline"),
            "the timeline pane must still be showing after + and =; got:\n{rendered:?}"
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

/// Issue #632: `3` opens the Cost screen under `osi watch` too, not
/// only a live `osi <doc>` run -- `osi watch` owns no engine of its
/// own, so the run this attaches to is a separate, still-running
/// `cof` process this test spawns and cleans up itself.
#[test]
fn pressing_3_during_osi_watch_of_a_live_run_opens_the_cost_screen() {
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
    std::thread::sleep(Duration::from_millis(500));
    {
        let after = captured_buf.lock().unwrap()[before_3..].to_vec();
        let rendered = String::from_utf8_lossy(&after);
        assert!(
            rendered.contains("tokens by store"),
            "expected the Cost screen's own treemap after pressing 3 under osi watch; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("largest stores"),
            "expected the Cost screen's own table after pressing 3 under osi watch; got:\n{rendered:?}"
        );
        assert!(
            rendered.contains("ask"),
            "expected the finished prompt's own store to show on the Cost screen; got:\n{rendered:?}"
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
    // The header's own status span starts a fresh styled run right
    // at the word `failed` -- an SGR sequence (ending `m`) directly
    // followed by the literal text, rather than every other place
    // `failed` appears in this buffer (the plan/log rows and the
    // error text print it mid-line, after a cursor-position escape
    // or a plain space, never straight out of an `m`). Checking for
    // that shape, rather than crossterm's own exact SGR encoding for
    // `Color::Red` (review finding 3 on PR #496), survives a
    // dependency bump that changes how a color is encoded without
    // losing what this assertion is actually for.
    assert!(
        rendered.contains("mfailed"),
        "the TUI never showed the header's own failed status; got:\n{rendered:?}"
    );

    assert_no_leftover_process(work.path());
}
