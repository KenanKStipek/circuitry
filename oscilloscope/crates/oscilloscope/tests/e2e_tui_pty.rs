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
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use support::{
    SCRIPTED_CONFIG, TestHome, assert_no_leftover_process, e2e_enabled, wait_with_timeout,
    write_doc,
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
    ] {
        cmd.env_remove(key);
    }
    cmd.stdin(stdio_from_slave(slave_path));
    cmd.stdout(stdio_from_slave(slave_path));
    cmd.stderr(stdio_from_slave(slave_path));
    // `setsid` (a new session, detached from the test harness's own
    // controlling terminal) plus `TIOCSCTTY` on fd 0 (already the pty
    // slave, Rust's own stdio redirection having already run by the
    // time a `pre_exec` closure fires) is what makes osp a real
    // foreground process of a real controlling terminal — without
    // one, raw-mode's own `/dev/tty` open fails, `TerminalGuard::enter`
    // falls back to a plain `child.wait()` with no signal-forwarding
    // loop at all, and a signal sent straight to osp's pid hits the
    // OS default disposition instead of osp's own handler (observed:
    // `kill`ed outright by SIGINT rather than exiting 130). A bare
    // `setpgid` alone, enough for `cof`'s own process-group isolation
    // (DESIGN.md §4.1), is not enough here — osp itself needs the
    // controlling terminal `cof` never does.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) != 0 {
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

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osp_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    // The pty's own master side is read on its own thread so the
    // child can never block on a full pty buffer waiting for a
    // reader that only ever showed up at the very end (the same
    // `wait_with_timeout_capturing_stdout` reasoning `e2e_cof.rs`
    // already applies to a plain pipe).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut master = master;
        let _ = master.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert!(status.success(), "osp should exit 0, got {status:?}");

    let captured = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
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

    let doc_arg = doc.as_os_str();
    let config_arg = config.as_os_str();
    let out_dir_arg = out_dir.path().as_os_str();
    let child = spawn_osp_on_pty(
        &home,
        &slave_path,
        &[doc_arg, config_arg, OsStr::new("--out-dir"), out_dir_arg],
    );
    let pid = child.id() as i32;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut master = master;
        let _ = master.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    // Longer than `support::SIGNAL_DELAY`: the TUI's own startup does
    // everything the plain path's does (the `cof run --help` probe, the
    // plan compile) *plus* entering raw mode and spawning the
    // terminal before its own signal-forwarding loop is ever reached,
    // so it needs more margin against a cold start on a shared,
    // loaded machine -- observed flaking at 1.5s, never at 3s.
    std::thread::sleep(Duration::from_secs(3));
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    let status = wait_with_timeout(child, Duration::from_secs(30));
    assert_eq!(status.code(), Some(130), "got {status:?}");

    let captured = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
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
