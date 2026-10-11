//! A pty-based regression test for review finding 1 on PR #467: a
//! process-backed tool's child must get `setsid()`, not merely its own
//! process group (`setpgid`) -- otherwise a child that touches the
//! inherited controlling terminal (`ffmpeg`'s own startup `tcsetattr`,
//! or plain `stty`) is stopped by `SIGTTOU` until the step's own
//! timeout kills it. Reproduced directly (orchestrator ruling, PR #467
//! fix pass): `script -q /dev/null electricity ...` stops `stty sane`
//! for the whole 6s timeout before the fix, succeeds in well under 1s
//! after it.
//!
//! Builds its own pty with `posix_openpt`/`grantpt`/`unlockpt`/
//! `ptsname` rather than shelling out to `script`: BSD (macOS) and
//! util-linux (Linux) `script` take different flags, while these four
//! POSIX calls are the same on both -- the one thing this test needs
//! to run unmodified on both of `electricity.yml`'s own runners.

#![cfg(unix)]

use std::ffi::CStr;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_electricity"))
}

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "electricity-pty-setsid-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        TempHome { path }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A pty master/slave pair, opened via plain POSIX calls (no external
/// `script`/`tty` dependency -- see this file's own module doc).
struct Pty {
    master: OwnedFd,
    slave_path: PathBuf,
}

impl Pty {
    fn open() -> Self {
        // SAFETY: a plain POSIX pty-allocation sequence; `master_fd` is
        // a freshly-opened fd this function exclusively owns from here
        // on (wrapped in `OwnedFd` immediately below).
        let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(master_fd >= 0, "posix_openpt failed");
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        unsafe {
            assert_eq!(libc::grantpt(master.as_raw_fd()), 0, "grantpt failed");
            assert_eq!(libc::unlockpt(master.as_raw_fd()), 0, "unlockpt failed");
        }
        // `ptsname` (not the `_r` variant): this test is single-
        // threaded with respect to pty allocation, and `ptsname_r`
        // isn't available on every platform this needs to run on.
        let slave_path = unsafe {
            let ptr = libc::ptsname(master.as_raw_fd());
            assert!(!ptr.is_null(), "ptsname failed");
            PathBuf::from(CStr::from_ptr(ptr).to_string_lossy().into_owned())
        };
        Pty { master, slave_path }
    }
}

/// Spawns *bin* with the pty's slave wired up as fd 0/1/2 **and**, via
/// `pre_exec`, made the new session's controlling terminal
/// (`setsid()` then `TIOCSCTTY`) -- the same thing a real terminal
/// emulator/login session does for whatever it launches, so *bin*
/// inherits a real controlling terminal exactly like `cof run`/
/// `electricity` run interactively.
fn spawn_under_pty(pty: &Pty, mut cmd: Command) -> std::process::Child {
    let slave_cstr = std::ffi::CString::new(pty.slave_path.to_str().unwrap()).unwrap();
    // SAFETY: opened fresh for this one child; dup'd onto 0/1/2 below,
    // then the original is closed in `pre_exec` (running post-fork, in
    // the child only) once it's no longer needed there either.
    let slave_fd = unsafe { libc::open(slave_cstr.as_ptr(), libc::O_RDWR) };
    assert!(slave_fd >= 0, "open(slave) failed");
    unsafe {
        cmd.stdin(Stdio::from_raw_fd(libc::dup(slave_fd)));
        cmd.stdout(Stdio::from_raw_fd(libc::dup(slave_fd)));
        cmd.stderr(Stdio::from_raw_fd(libc::dup(slave_fd)));
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(slave_fd);
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn electricity under a pty");
    unsafe {
        libc::close(slave_fd);
    }
    child
}

/// Reads *pty*'s master side to EOF on a background thread for as long
/// as the returned `JoinHandle` is kept -- the terminal driver's own
/// output buffer can otherwise fill and block a foreground writer
/// (electricity's own stdout/stderr) once nothing drains it.
fn drain_master(pty: &Pty) -> std::thread::JoinHandle<()> {
    let fd = pty.master.as_raw_fd();
    // SAFETY: a `File` wrapper for reading only; `pty`'s own `OwnedFd`
    // still owns and closes the underlying fd on drop, not this `File`
    // (built from a borrowed raw fd via `dup`, never taking ownership).
    let dup_fd = unsafe { libc::dup(fd) };
    std::thread::spawn(move || {
        let mut file = unsafe { fs::File::from_raw_fd(dup_fd) };
        let mut buf = [0u8; 4096];
        loop {
            match file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
}

#[test]
fn a_shell_steps_child_gets_its_own_session_not_merely_a_process_group() {
    let home = TempHome::new("setsid");
    let config = home.write("config.json", "{}");
    let doc = home.write(
        "doc.yml",
        "effects:\n\
         \x20 - name: s\n\
         \x20   type: tool\n\
         \x20   provider: shell\n\
         \x20   timeout_ms: 5000\n\
         \x20   params:\n\
         \x20     command: stty\n\
         \x20     args: [\"sane\"]\n\
         \x20     allowed_commands: [stty]\n",
    );
    let out = home.path.join("out.json");

    let pty = Pty::open();
    let _drain = drain_master(&pty);

    let mut cmd = Command::new(bin());
    cmd.env_clear();
    cmd.env("HOME", &home.path);
    cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd.args([
        config.to_str().unwrap(),
        doc.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);

    let started = Instant::now();
    let mut child = spawn_under_pty(&pty, cmd);
    let status = child.wait().expect("electricity under a pty exits");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "a setsid child must finish almost at once, not stall until the \
         5s timeout on SIGTTOU (pre-fix: stopped until the timeout, \
         elapsed={elapsed:?})"
    );
    assert!(
        status.success(),
        "electricity under a pty must exit 0, not time out on SIGTTOU"
    );

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        state["prime"]["s"]["value"],
        serde_json::Value::String(String::new()),
        "stty sane must have actually run (empty stdout), not timed out: {state}"
    );
}
