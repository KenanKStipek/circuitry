//! Spawning the engine (`process_group(0)`, `stdin` null, piped
//! `stdout`/`stderr`), signal forwarding, and mapping its exit status
//! back to `osp`'s own (DESIGN.md §4.1 and §6.1). POSIX only (design
//! Q10).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// A signal osp forwards to the engine's process group (DESIGN.md
/// §4.1): the three it itself handles (SIGINT twice, SIGTERM, SIGHUP),
/// forwarded as the same signal it received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardSignal {
    Int,
    Term,
    Hup,
}

impl ForwardSignal {
    fn as_raw(self) -> std::ffi::c_int {
        match self {
            ForwardSignal::Int => libc::SIGINT,
            ForwardSignal::Term => libc::SIGTERM,
            ForwardSignal::Hup => libc::SIGHUP,
        }
    }

    /// The signal osp itself just received, mapped from
    /// `signal_hook`'s raw signal numbers.
    pub fn from_raw(raw: std::ffi::c_int) -> Option<Self> {
        if raw == libc::SIGINT {
            Some(ForwardSignal::Int)
        } else if raw == libc::SIGTERM {
            Some(ForwardSignal::Term)
        } else if raw == libc::SIGHUP {
            Some(ForwardSignal::Hup)
        } else {
            None
        }
    }
}

/// The engine's own stderr lines, tailed line by line for the `--log`
/// pane (DESIGN.md §4.1: "Each line becomes an `engine` line"). stdout
/// is only ever captured to `stdout.txt`, never echoed.
pub struct SupervisedChild {
    child: Child,
    pid: i32,
    stdout_thread: Option<thread::JoinHandle<()>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl SupervisedChild {
    /// Spawns `cmd` in its own process group (so a terminal signal
    /// reaches osp alone, never the engine directly), with `stdin`
    /// null, and `stdout`/`stderr` each teed to a file in the run
    /// directory. Returns a channel of the engine's own stderr lines,
    /// trimmed of their trailing newline.
    pub fn spawn(
        mut cmd: Command,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> io::Result<(Self, Receiver<String>)> {
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.process_group(0);

        let mut child = cmd.spawn()?;
        let pid = child.id() as i32;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdout_file = File::create(stdout_path)?;
        let stderr_file = File::create(stderr_path)?;

        let stdout_thread = thread::spawn(move || tee_lines(stdout, stdout_file, None));
        let (tx, rx) = mpsc::channel();
        let stderr_thread = thread::spawn(move || tee_lines(stderr, stderr_file, Some(tx)));

        Ok((
            SupervisedChild {
                child,
                pid,
                stdout_thread: Some(stdout_thread),
                stderr_thread: Some(stderr_thread),
            },
            rx,
        ))
    }

    /// Non-blocking: `Ok(None)` while the engine is still running.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Blocks until the engine exits — only ever called after
    /// `try_wait` has already reported it has, or when osp itself is
    /// about to exit and must not leave it behind.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        if let Some(t) = self.stdout_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.stderr_thread.take() {
            let _ = t.join();
        }
        Ok(status)
    }

    /// Forwards `sig` to the engine's whole process group — never just
    /// the engine's own pid, so a child it spawned is reached too.
    pub fn forward(&self, sig: ForwardSignal) {
        unsafe {
            libc::kill(-self.pid, sig.as_raw());
        }
    }

    /// The last resort (DESIGN.md §4.1): still alive 10s after a second
    /// Ctrl-C.
    pub fn kill_group(&self) {
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
    }
}

impl Drop for SupervisedChild {
    /// osp must never leave the engine behind, panics included
    /// (orchestrator decision on issue #424): if this instance is
    /// dropped while the child is still alive (an unwind, an early
    /// `return`/`?` this code forgot to wait on), kill its whole
    /// process group rather than let it run unsupervised.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill_group();
            let _ = self.child.wait();
        }
    }
}

fn tee_lines(reader: impl io::Read, mut file: File, tx: Option<mpsc::Sender<String>>) {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
                if let Some(tx) = &tx {
                    let _ = tx.send(line.trim_end_matches(['\r', '\n']).to_string());
                }
            }
        }
    }
}

/// osp's own exit code: the engine's, exactly (DESIGN.md §4.1's "osp
/// exits with the engine's exit code") — a signal-terminated process
/// maps to the POSIX `128 + signum` convention `cof` itself uses (129
/// SIGHUP, 130 SIGINT, 143 SIGTERM).
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    if let Some(signal) = status.signal() {
        return 128 + signal;
    }
    1
}

/// Polls pending signals without blocking — used in osp's own 100ms
/// tick (DESIGN.md §6.1) alongside the live-state/events poll, so one
/// loop drives everything.
pub struct SignalWatcher {
    signals: signal_hook::iterator::Signals,
}

impl SignalWatcher {
    pub fn new() -> io::Result<Self> {
        let signals =
            signal_hook::iterator::Signals::new([libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
        Ok(SignalWatcher { signals })
    }

    pub fn pending(&mut self) -> Vec<ForwardSignal> {
        self.signals
            .pending()
            .filter_map(ForwardSignal::from_raw)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_passes_through_a_normal_exit() {
        let status = std::process::ExitStatus::from_raw(0);
        assert_eq!(exit_code(status), 0);
        let status = std::process::ExitStatus::from_raw(1 << 8);
        assert_eq!(exit_code(status), 1);
    }

    #[test]
    fn exit_code_maps_a_signal_to_128_plus_signum() {
        // Raw status encoding on Linux/macOS: low 7 bits are the
        // terminating signal when the process didn't exit normally.
        let status = std::process::ExitStatus::from_raw(libc::SIGINT);
        assert_eq!(exit_code(status), 128 + libc::SIGINT);
    }

    #[test]
    fn spawned_child_is_killed_on_drop_even_if_never_waited_on() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let dir = tempfile::tempdir().unwrap();
        let (child, _rx) =
            SupervisedChild::spawn(cmd, &dir.path().join("out"), &dir.path().join("err")).unwrap();
        let pid = child.pid;
        drop(child);
        // Give the OS a moment to reap/signal; then confirm the pid is
        // no longer alive (ESRCH from a signal-0 probe).
        std::thread::sleep(std::time::Duration::from_millis(200));
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(
            !alive,
            "child should have been killed when SupervisedChild was dropped"
        );
    }
}
