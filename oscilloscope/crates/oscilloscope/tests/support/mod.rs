//! Shared helpers for every `cof` end-to-end test (`e2e_cof.rs`,
//! `e2e_tui_pty.rs`): both need the same temporary `HOME`, stripped
//! credentials, hard timeouts and "nothing survived" check (issue
//! #424's Tests section; #434's pseudo-terminal test reuses it
//! rather than drifting its own copy).
//!
//! A `tests/support/mod.rs` (not a top-level `tests/support.rs`) so
//! Cargo never treats this as its own test binary — only `mod
//! support;` from an actual test file compiles it at all. Each test
//! binary that does gets its own copy, and no one caller uses every
//! function here, hence the blanket `dead_code` allow below rather
//! than one per unused-somewhere item.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn e2e_enabled() -> bool {
    if std::env::var_os("OSI_E2E_COF").is_none() {
        return false;
    }
    // P2-3: `OSI_E2E_COF=1` with no `cof` on `PATH` is a broken CI
    // job, not a reason to run zero tests and report green — every
    // test below used to read this the same as the env var simply
    // being unset at all and quietly skip.
    if which("cof").is_none() {
        panic!("OSI_E2E_COF=1 but `cof` is not on PATH");
    }
    true
}

/// The same shape as [`e2e_enabled`], for issue #431's lane E2: these
/// tests need an `electricity` built with `--features test-tools`
/// (`cargo build -p electricity-cli --features test-tools` from
/// `electricity/`) on `PATH`, since the `sleep`/`fail` providers below
/// only exist in that build (never in a release one).
pub fn e2e_electricity_enabled() -> bool {
    if std::env::var_os("OSI_E2E_ELECTRICITY").is_none() {
        return false;
    }
    if which("electricity").is_none() {
        panic!("OSI_E2E_ELECTRICITY=1 but `electricity` is not on PATH");
    }
    true
}

/// `default_adapter`/`default_model` are Circuitry's own config keys
/// (`cli/config.py`); a bare `"adapter": "scripted"` is an unknown key
/// the config loader silently ignores, so every one of these runs
/// used to fall back to the built-in `ollama` default instead (K1).
pub const SCRIPTED_CONFIG: &str =
    r#"{"default_adapter":"scripted","default_model":"scripted-model"}"#;

/// Every electricity e2e test that dispatches no prompt effect at all
/// (the `tool`-only and signal tests below) uses this empty config --
/// the same one `electricity/crates/electricity/tests/run_wiring.rs`'s
/// own tests already use. A test that does dispatch a prompt effect
/// (issue #538's B1) builds its own config instead, with
/// [`scripted_config`].
pub const ELECTRICITY_CONFIG: &str = "{}";

/// A `runtime.adapters.scripted.replies_file` config pointing at
/// *replies_path* (an absolute path in the test's own temp directory),
/// for an electricity e2e test whose document sets its own
/// document-level `adapter: scripted`/`model:` (issue #538's B1): no
/// `default_adapter`/`default_model` of its own, since the document
/// already names both.
pub fn scripted_config(replies_path: &Path) -> String {
    serde_json::json!({
        "runtime": {
            "adapters": {
                "scripted": {"replies_file": replies_path.to_string_lossy()}
            }
        }
    })
    .to_string()
}

/// How long every signal test waits before sending its first signal
/// (a cold process start can legitimately take longer than this on a
/// shared, busy machine; still well under a human's own first
/// Ctrl-C).
pub const SIGNAL_DELAY: Duration = Duration::from_millis(1500);

pub fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|p| p.is_file())
    })
}

pub struct TestHome {
    dir: tempfile::TempDir,
}

impl TestHome {
    pub fn new() -> Self {
        TestHome {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub fn osi_command(home: &TestHome) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osi"));
    cmd.env("HOME", home.path());
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
    ] {
        cmd.env_remove(key);
    }
    // Issue #649: a bare `HOME` override isn't enough on its own --
    // an inherited `XDG_STATE_HOME` or `OSI_HISTORY_DIR` would still
    // point history at the real directory.
    cmd.env("OSI_HISTORY_DIR", home.path().join("history"));
    cmd.env_remove("XDG_STATE_HOME");
    cmd.env_remove("OSI_HISTORY");
    // Issue #698: same discipline for the run registry -- an
    // inherited `XDG_RUNTIME_DIR`/`XDG_STATE_HOME` would otherwise
    // still point it at a real, shared directory.
    cmd.env("OSI_RUNS_DIR", home.path().join("runs"));
    cmd.env_remove("XDG_RUNTIME_DIR");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd
}

/// [`osi_command`], with `--engine electricity` already appended
/// (issue #431's lane E2) -- every one of this crate's electricity
/// e2e tests opts into it the same way, so each one doesn't repeat
/// the same two-argument tail.
pub fn osi_electricity_command(home: &TestHome) -> Command {
    let mut cmd = osi_command(home);
    cmd.arg("--engine").arg("electricity");
    cmd
}

/// Waits for `child` with a hard timeout, killing it (and panicking)
/// rather than ever hanging a CI job.
pub fn wait_with_timeout(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("e2e test exceeded its {timeout:?} hard timeout; osi was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `wait_with_timeout`, with `child`'s own stdout read on its own
/// thread rather than in this one (P2-3): reading concurrently with
/// the wait means a kill on timeout closes the pipe (EOF) and
/// unblocks the read too, instead of hanging on it with no timeout of
/// its own at all.
pub fn wait_with_timeout_capturing_stdout(
    mut child: Child,
    timeout: Duration,
) -> (std::process::ExitStatus, String) {
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    // Issue #433's comments: every caller of this helper spawns an
    // `osi` whose own stderr is piped too (`osi_command`), and this
    // helper otherwise never reads it -- a child that writes more
    // than a pipe buffer's worth to stderr would block on the write
    // end forever, with no timeout of its own. Drained on its own
    // thread and discarded; a caller that needs the text itself uses
    // `wait_with_timeout_capturing_stdout_and_stderr` instead.
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (stderr_done_tx, stderr_done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        let _ = stderr_done_tx.send(());
    });
    let status = wait_with_timeout(child, timeout);
    let stdout = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stdout reader thread did not finish>"));
    let _ = stderr_done_rx.recv_timeout(Duration::from_secs(5));
    (status, stdout)
}

/// [`wait_with_timeout_capturing_stdout`], with `child`'s stderr
/// captured the same way alongside it -- the electricity e2e tests
/// (issue #431's lane E2, scope item 1) need stderr to assert the
/// "this electricity has no --events" notice did *not* print, which
/// `wait_with_timeout_capturing_stdout` alone can't see (it leaves
/// stderr piped but never read).
pub fn wait_with_timeout_capturing_stdout_and_stderr(
    mut child: Child,
    timeout: Duration,
) -> (std::process::ExitStatus, String, String) {
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        let _ = stderr_tx.send(buf);
    });
    let status = wait_with_timeout(child, timeout);
    let stdout = stdout_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stdout reader thread did not finish>"));
    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::from("<stderr reader thread did not finish>"));
    (status, stdout, stderr)
}

/// Polls *events_path* (an `events.jsonl`, possibly not created yet)
/// until it carries a `start` event for *step_path*, bounded at
/// *timeout*, panicking with a clear message if it never does -- a
/// signal test must send its signal only once the long-running step
/// it means to interrupt has actually started. In CI a signal sent
/// after only a fixed delay could still land before the engine had
/// produced any state at all, making the test's own assertions (e.g.
/// `✗ prime` appearing exactly once) a coin flip rather than
/// deterministic.
pub fn wait_for_step_start(events_path: &Path, step_path: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(events_path) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("ev").and_then(|v| v.as_str()) == Some("start")
                    && value.get("path").and_then(|v| v.as_str()) == Some(step_path)
                {
                    return;
                }
            }
        }
        if start.elapsed() > timeout {
            panic!(
                "{events_path:?} never carried a start event for {step_path:?} within {timeout:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The last non-empty line of *path* (an `events.jsonl`), parsed as
/// JSON -- every electricity signal e2e test uses this to check the
/// stream's own final `run_end` line carries the signal that actually
/// ended the run, not just that osi's own summary line did.
pub fn last_event(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("couldn't read {}: {e}", path.display()));
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_else(|| panic!("{} has no event lines", path.display()));
    serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("{} last line is not JSON: {e}", path.display()))
}

pub fn write_doc(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// No process whose own argv names `needle` (a test's unique work
/// directory, or run directory, that the doc/config paths `cof` and
/// anything it spawned in turn were invoked with) should still exist
/// once osi has exited. A plain `HOME` path doesn't work as the
/// needle: it is only ever set as an environment variable, which
/// never appears in a process's own argv, so a `pgrep -f` against it
/// can never match anything regardless of whether a process actually
/// survived (#424 review finding F10).
/// Every `osi-<pid>-<nanos>` entry directly under the real temp
/// directory right now (`unique_run_dir`'s own default-`--out-dir`
/// naming) -- a before/after diff around one test's own osi spawn(s)
/// (comment 1 on #433), rather than a bare "is it empty" check on the
/// whole temp directory, since other processes on the same machine
/// may have their own `osi-*` directories there at the same time.
pub fn snapshot_osi_dirs() -> std::collections::HashSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return std::collections::HashSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("osi-"))
        })
        .collect()
}

/// Asserts this test's own osi spawn(s) left no *new* `osi-*` run
/// directory behind in the real temp directory, compared to a
/// [`snapshot_osi_dirs`] taken before they ran -- every e2e test that
/// spawns `osi run` passes its own `--out-dir` precisely so this never
/// finds one; it exists to catch a future test that forgets to.
pub fn assert_no_new_osi_dirs(before: &std::collections::HashSet<PathBuf>) {
    let after = snapshot_osi_dirs();
    let new: Vec<&PathBuf> = after.difference(before).collect();
    assert!(
        new.is_empty(),
        "this test left new osi-* run director{} behind in {:?}: {new:?} \
         -- every osi run in an e2e test must pass its own --out-dir",
        if new.len() == 1 { "y" } else { "ies" },
        std::env::temp_dir()
    );
}

pub fn assert_no_leftover_process(needle: &Path) {
    std::thread::sleep(Duration::from_millis(300));
    let ps = Command::new("pgrep")
        .arg("-f")
        .arg(needle.display().to_string())
        .output()
        .expect("pgrep should be available");
    assert!(
        ps.stdout.is_empty(),
        "a process matching {needle:?} survived osi's exit: {}",
        String::from_utf8_lossy(&ps.stdout)
    );
}

/// Replays raw terminal output onto a blank character grid: cursor positioning (`CSI r;cH` / `f`,
/// `CSI nA/B/C/D`), carriage return and line feed move the cursor, every other escape sequence
/// (colours, attributes, clears, cursor visibility, alternate screen) is skipped, and printable text
/// lands at the cursor. Each returned line is one row, trailing blanks trimmed, so a test can look for
/// the text a screen actually shows rather than for bytes that a styled run may split with escapes.
pub fn screen_grid(bytes: &[u8], cols: usize, rows: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut grid = vec![vec![' '; cols]; rows];
    let (mut row, mut col) = (0usize, 0usize);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' {
            i += 1;
            if i < chars.len() && chars[i] == '[' {
                i += 1;
                let start = i;
                while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i >= chars.len() {
                    break;
                }
                let params: String = chars[start..i].iter().collect();
                let fin = chars[i];
                i += 1;
                let nums: Vec<usize> = params
                    .trim_start_matches('?')
                    .split(';')
                    .filter_map(|p| p.parse().ok())
                    .collect();
                match fin {
                    'H' | 'f' => {
                        row = nums.first().copied().unwrap_or(1).saturating_sub(1);
                        col = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
                    }
                    'A' => row = row.saturating_sub(nums.first().copied().unwrap_or(1)),
                    'B' => row += nums.first().copied().unwrap_or(1),
                    'C' => col += nums.first().copied().unwrap_or(1),
                    'D' => col = col.saturating_sub(nums.first().copied().unwrap_or(1)),
                    // Erase in display/line (`\x1b[2J`, `\x1b[K`, ...)
                    // -- a real terminal drops the erased cells for
                    // good; a resize mid-stream (ratatui clears and
                    // redraws from scratch at the new size) leaves a
                    // wider frame's own stale cells bleeding through
                    // underneath a narrower one otherwise, since this
                    // replay buffer never actually shrinks.
                    'J' => {
                        let mode = nums.first().copied().unwrap_or(0);
                        match mode {
                            0 => {
                                if row < rows {
                                    for c in grid[row].iter_mut().skip(col) {
                                        *c = ' ';
                                    }
                                    for line in grid.iter_mut().skip(row + 1) {
                                        line.iter_mut().for_each(|c| *c = ' ');
                                    }
                                }
                            }
                            1 => {
                                for line in grid.iter_mut().take(row) {
                                    line.iter_mut().for_each(|c| *c = ' ');
                                }
                                if row < rows {
                                    for c in grid[row].iter_mut().take(col.min(cols)) {
                                        *c = ' ';
                                    }
                                }
                            }
                            _ => {
                                for line in grid.iter_mut() {
                                    line.iter_mut().for_each(|c| *c = ' ');
                                }
                            }
                        }
                    }
                    'K' => {
                        let mode = nums.first().copied().unwrap_or(0);
                        if row < rows {
                            match mode {
                                0 => {
                                    for c in grid[row].iter_mut().skip(col) {
                                        *c = ' ';
                                    }
                                }
                                1 => {
                                    for c in grid[row].iter_mut().take(col.min(cols)) {
                                        *c = ' ';
                                    }
                                }
                                _ => {
                                    grid[row].iter_mut().for_each(|c| *c = ' ');
                                }
                            }
                        }
                    }
                    _ => {}
                }
            } else if i < chars.len() {
                // a two-character escape (`ESC 7`, `ESC =`, ...)
                i += 1;
            }
            continue;
        }
        match c {
            '\r' => col = 0,
            '\n' => {
                row += 1;
                col = 0;
            }
            '\u{7}' | '\u{8}' | '\u{f}' | '\u{e}' => {}
            _ => {
                if !c.is_control() {
                    if row < rows && col < cols {
                        grid[row][col] = c;
                    }
                    col += 1;
                }
            }
        }
        i += 1;
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect()
}

/// Opens a fresh pty pair, unlocked and granted, ready for a child to
/// use its slave side as its controlling stdio. Returns the master
/// (kept open for the caller's own lifetime; its own `Drop` closes it)
/// and the slave device's path, re-opened fresh for each of the
/// child's three standard streams below -- a single shared `File`
/// handle would make all three the same underlying OS file
/// description, which is not what three independent dup'd fds of a
/// real terminal ever are.
pub fn open_pty() -> (File, PathBuf) {
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    assert!(master_fd >= 0, "posix_openpt failed");
    assert_eq!(unsafe { libc::grantpt(master_fd) }, 0, "grantpt failed");
    assert_eq!(unsafe { libc::unlockpt(master_fd) }, 0, "unlockpt failed");
    // Without `FD_CLOEXEC`, `spawn_osi_on_pty`'s own `Command::spawn`
    // (fork, then exec) would hand osi itself a surviving duplicate
    // of this exact fd -- a real bug a caller of this helper could
    // otherwise never hit before needing the master to actually,
    // fully close while osi was still alive: osi would then still
    // hold the pty's master open all on its own even after the
    // caller's own copy is dropped, so the slave side it cares about
    // (osi's own stdin/stdout/stderr) would never see the EOF/write-
    // error a closed master is supposed to cause at all.
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
    // alternate-screen/cursor sequences. `TIOCSWINSZ` on the master
    // fd alone fails ENOTTY on macOS -- it has to go through an open
    // slave fd, but the size itself belongs to the pty, not that one
    // fd, so it's still in effect once this one closes again.
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
        // which would silently end `drain_continuously`'s own read
        // loop before osi ever wrote a single byte. Leaking this one
        // fd for the rest of the test binary's own short life keeps
        // the slave's own open count never reaching zero in between.
        //
        // Review finding 5 on PR #496: the cost of that is paid by
        // every test that calls this helper, not just the first one --
        // this leaked fd stays open for the whole binary's life, so
        // the real slave fds `spawn_osi_on_pty` opens next are never
        // the last ones left, and `master` (this pty's own read side)
        // can never see a real EOF once osi's own three close on
        // exit. `drain_continuously`'s loop below only ever stops
        // reading because its read errors (the master itself closing
        // when this function's own `File` drops at the end of a
        // test), never because the pipe quietly ran dry.
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
/// for however long this wasn't reading, and risking losing whatever
/// it tried to write once it finally did get to exit).
pub fn drain_continuously(mut master: File) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::clone(&buf);
    std::thread::spawn(move || {
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

/// Polls the replayed screen (every drawn byte so far through
/// [`screen_grid`], 200x60) until it shows `needle`, returning that
/// screen; after `timeout` it returns whatever the screen shows, for
/// the caller's own assertion to name the step that lagged.
pub fn wait_for_screen_text(
    captured_buf: &Arc<Mutex<Vec<u8>>>,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = screen_grid(&whole, 200, 60).join("\n");
        if screen.contains(needle) || Instant::now() >= deadline {
            return screen;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// [`wait_for_screen_text`], but waiting for `needle` to disappear
/// instead.
pub fn wait_for_screen_text_gone(
    captured_buf: &Arc<Mutex<Vec<u8>>>,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let whole = captured_buf.lock().unwrap().clone();
        let screen = screen_grid(&whole, 200, 60).join("\n");
        if !screen.contains(needle) || Instant::now() >= deadline {
            return screen;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Writes `bytes` to the pty's master side, i.e. exactly what a key
/// press on a real terminal would deliver to osi's own raw-mode
/// stdin.
pub fn send_bytes(master: &mut File, bytes: &[u8]) {
    master.write_all(bytes).expect("write to pty master");
    master.flush().expect("flush pty master");
}

fn open_slave(path: &Path) -> OwnedFd {
    // On Linux, a process that has called `setsid()` (no controlling
    // terminal of its own yet) and then opens a tty device *by path*
    // auto-acquires it as its controlling terminal, same as a bare
    // `open()` + no `O_NOCTTY` always has on historical SysV ttys --
    // `osi` itself never asks for one (`TerminalGuard::enter` only
    // `tcgetattr`/`tcsetattr`s an already-open fd), so the three
    // slave opens below, each handed to the spawned `osi` as a
    // *fresh* stdio fd rather than a dup of one already-open one,
    // must say so explicitly.
    let file = File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)
        .unwrap_or_else(|e| panic!("opening pty slave {path:?}: {e}"));
    OwnedFd::from(file)
}

fn stdio_from_slave(path: &Path) -> Stdio {
    let fd = open_slave(path);
    Stdio::from(fd)
}

/// Spawns `osi` with every one of its standard streams attached to
/// the pty's slave side, in its own process group (the same
/// isolation `osi` itself gives `cof`, and the lane rule this
/// repository's tests all follow: "signals only to processes you
/// started, in their own session").
pub fn spawn_osi_on_pty(home: &TestHome, slave_path: &Path, args: &[&OsStr]) -> Child {
    spawn_osi_on_pty_with_env(home, slave_path, args, &[])
}

/// [`spawn_osi_on_pty`], with extra environment variables set on the
/// spawned `osi` before it starts.
pub fn spawn_osi_on_pty_with_env(
    home: &TestHome,
    slave_path: &Path,
    args: &[&OsStr],
    extra_env: &[(&str, &OsStr)],
) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_osi"));
    cmd.args(args);
    cmd.env("HOME", home.path());
    cmd.env("TERM", "xterm-256color");
    // Issue #649: a bare `HOME` override isn't enough on its own --
    // an inherited `XDG_STATE_HOME` or `OSI_HISTORY_DIR` would still
    // point history at the real directory.
    cmd.env("OSI_HISTORY_DIR", home.path().join("history"));
    // Issue #698: same discipline for the run registry.
    cmd.env("OSI_RUNS_DIR", home.path().join("runs"));
    // A machine-wide `ps` scan would otherwise let a Select/All pty
    // test see, and open or signal, an orchestration this test
    // never started -- off here by default; the one test that
    // exercises the scan itself overrides it back on through
    // `extra_env` below.
    cmd.env("OSI_PROCESS_SCAN", "0");
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CYBERDINER_TOKEN",
        "CYBERDINER_EXPO_URL",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "OSI_HISTORY",
        // `effective_log_mode` (DESIGN.md §6.2) forces the plain
        // stream whenever `CI` is set, which GitHub Actions sets for
        // every job regardless of what osi's own stdout actually is
        // -- without removing it here, every pty test here always
        // exercised the plain path on CI, and never the TUI it
        // exists to test.
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
    // `setsid()`, not merely `setpgid(0, 0)` -- matching how a real
    // terminal-launching driver spawns its own child (Python's own
    // `start_new_session=True`), and the only way osi ends up with
    // *no controlling terminal at all* rather than merely its own
    // process group within the test harness's own session. A plain
    // tty slave fd (no explicit `TIOCSCTTY`, and now `O_NOCTTY` on
    // top, `open_slave`'s own doc comment) is still enough for
    // `crossterm::terminal::enable_raw_mode`'s own `tcgetattr`/
    // `tcsetattr`, which only need an open fd referring to some tty
    // device, not a controlling-terminal relationship -- this
    // isolation is here for the same reason `osi` itself gives `cof`
    // its own (DESIGN.md §4.1), so a signal sent to just this pid
    // never reaches the test harness's own session either.
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

/// A long-running document (a `sleep` tool, allow-listed) so a run
/// launched from it stays live long enough for a second `osi` to
/// find it on Select -- 60s, not the 5s a quick probe would use: a
/// shared, busy machine's own scheduling delays can stretch the gap
/// between launching this run and a caller actually checking it is
/// still live well past a few seconds (`terminate_launcher` ends it
/// well before this ever elapses in practice).
pub const SELECT_SLOW_DOC: &str = "effects:\n  - name: slow\n    type: tool\n    provider: shell\n    params:\n      command: sleep\n      args: [\"60\"]\n      allowed_commands: [\"sleep\"]\n";

/// Launches `osi <doc> <config> --out-dir <out> --log` in the
/// background (issue #698: a plain, non-TTY run needs no pty of its
/// own, and writes the same registry entry a TUI launch would) --
/// its own stdout/stderr are discarded, never read, since the point
/// of a Select test is bare `osi`'s own Select screen, not this
/// launcher's own log lines.
pub fn launch_osi_log(
    home: &TestHome,
    work: &Path,
    doc: &Path,
    config: &Path,
    out: &Path,
) -> Child {
    let mut cmd = osi_command(home);
    cmd.current_dir(work)
        .arg(doc)
        .arg(config)
        .arg("--out-dir")
        .arg(out)
        .arg("--log")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().expect("spawn osi --log")
}

/// A SIGTERM, not a SIGKILL -- `osi --log`'s own `SignalWatcher`
/// forwards a caught SIGTERM to the engine it supervises (`cof`),
/// which in turn ends the long-running `sleep` tool underneath it;
/// a SIGKILL on the launcher alone would leave both still running,
/// unreachable to `osi`'s own signal handling at all. Falls back to
/// a real kill if the launcher hasn't actually exited by the time
/// the caller is done with it.
pub fn terminate_launcher(mut child: Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Polls `runs_dir` until it holds at least `want` entries, capped
/// at `timeout` -- the registry entry is written once `run_start` is
/// known, not instantly at spawn.
pub fn wait_for_registry_entries(runs_dir: &Path, want: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let n = std::fs::read_dir(runs_dir)
            .map(|rd| rd.flatten().count())
            .unwrap_or(0);
        if n >= want || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
