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
        // Both tee files are opened *before* `cmd.spawn()` (N1): if
        // either `File::create` failed after the engine was already
        // spawned, the early `?` return would drop a plain
        // `std::process::Child` with no `SupervisedChild` ever built
        // around it — never killed, since `SupervisedChild::drop` only
        // runs for an instance that actually got constructed — leaving
        // the engine running, unsupervised, in its own process group,
        // while osp prints an error and exits.
        let stdout_file = File::create(stdout_path)?;
        let stderr_file = File::create(stderr_path)?;

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.process_group(0);

        let mut child = cmd.spawn()?;
        let pid = child.id() as i32;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

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

/// The same `128 + signum` mapping as [`exit_code`], from a `--events`
/// `run_end` line's own `signal` field (DESIGN.md §3's format table)
/// instead of a real `ExitStatus` — `osp watch` has no child process
/// of its own to read a status from, so this is the only way it can
/// report the same exit code `cof` itself used (P2-7) rather than just
/// ok-vs-failed.
pub fn exit_code_for_signal_name(signal: &str) -> Option<i32> {
    match signal {
        "SIGHUP" => Some(129),
        "SIGINT" => Some(130),
        "SIGTERM" => Some(143),
        _ => None,
    }
}

/// Whether a process with this pid still exists, probed the
/// conventional way (`kill(pid, 0)`, which sends no signal and only
/// checks for `ESRCH`). Used by `osp watch` (F4), which has no `Child`
/// handle of its own for a run it didn't start — the engine's pid, when
/// an events stream names one (`run_start`, DESIGN.md §3), is the only
/// way it can tell a genuine abort (no `run_end`, dead process) from a
/// run simply still going.
pub fn process_alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // Anything other than "no such process" (most commonly `EPERM`, a
    // pid that exists but is owned by someone else) still means it's
    // alive — only `ESRCH` is a confirmed death.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
    fn process_alive_is_true_for_this_process_and_false_once_a_child_is_reaped() {
        assert!(process_alive(std::process::id() as i32));

        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }

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

    #[test]
    fn a_stdout_file_that_cannot_be_created_never_spawns_the_engine() {
        // N1 probe: `stdout_path` is a directory, so `File::create`
        // fails. Before the fix, `cmd.spawn()` ran first, so the
        // engine (a `sleep` whose argument is this test's own unique
        // needle) was already running by the time `spawn` returned
        // `Err` -- left behind forever, since no `SupervisedChild` was
        // ever built to `Drop` it.
        let dir = tempfile::tempdir().unwrap();
        let stdout_as_dir = dir.path().join("stdout.txt");
        std::fs::create_dir(&stdout_as_dir).unwrap();

        // A fractional-second `sleep` argument, unique to this test
        // run, serves both roles at once: a real duration `sleep`
        // accepts (so, had it wrongly been spawned, it would still be
        // running for the `pgrep` check below) and a needle unlikely
        // to collide with anything else on a shared machine.
        let needle = format!("5.{}", std::process::id());
        let mut cmd = Command::new("sleep");
        cmd.arg(&needle);

        let result = SupervisedChild::spawn(cmd, &stdout_as_dir, &dir.path().join("stderr.txt"));
        assert!(result.is_err(), "File::create on a directory should fail");

        std::thread::sleep(std::time::Duration::from_millis(200));
        let needle_arg = needle.clone();
        let ps = Command::new("pgrep")
            .arg("-f")
            .arg(needle_arg)
            .output()
            .expect("pgrep should be available");
        assert!(
            ps.stdout.is_empty(),
            "the engine must never be spawned when a tee file can't be created: {}",
            String::from_utf8_lossy(&ps.stdout)
        );
    }

    #[test]
    fn a_panic_while_a_child_is_alive_still_kills_it() {
        // osp must never leave the engine behind on *any* exit path,
        // panics included (issue #424's lane decisions): a
        // `SupervisedChild` held across a panicking scope is dropped
        // during unwind the same way a normal early return drops it, so
        // this exercises the same `Drop` impl through a real unwind,
        // not just a plain `drop()` call.
        let dir = tempfile::tempdir().unwrap();
        let pid = {
            let mut cmd = Command::new("sleep");
            cmd.arg("5");
            let (child, _rx) =
                SupervisedChild::spawn(cmd, &dir.path().join("out"), &dir.path().join("err"))
                    .unwrap();
            let pid = child.pid;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _keep_alive = &child;
                panic!("deliberate panic to exercise Drop during unwind");
            }));
            assert!(result.is_err());
            pid
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(
            !alive,
            "child should have been killed when the panic unwound past SupervisedChild"
        );
    }
}
