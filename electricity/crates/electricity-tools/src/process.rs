//! `core/cancellation.py::run_tracked` and `kill_process_group`, ported
//! as a signature-only seam (issue #449's gate lane, item 3) -- lane C
//! fills every body in this module; the shapes below are final.
//!
//! `run_tracked` is `subprocess.run`, tracked by this run's own
//! [`CancellationToken`] for the whole time the child can block: the
//! token kills every tracked process group at once on `request()`, and
//! a per-call *timeout* still applies independently.

use crate::PyExc;
use electricity_value::{CancellationToken, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// `core/cancellation.py::_SIGNAL_POLL_SECONDS` -- how often the wait
/// loop below wakes up to check cancellation/the overall deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// `run_tracked`'s own keyword arguments, minus `cmd`/`capture_output`/
/// `text` (this crate always captures and always decodes as text --
/// lane C's own [`Completed`] is the ported shape of that decode, not a
/// bytes/text toggle) -- `timeout`, `input`, `cwd`, `env`, and
/// *new_session* (`start_new_session=True` only while the run is armed,
/// [`crate::ToolCall::armed`]'s own doc comment).
#[derive(Debug, Default, Clone)]
pub struct RunOpts {
    pub timeout: Option<Duration>,
    pub input: Option<Vec<u8>>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: Option<HashMap<String, String>>,
    pub new_session: bool,
    /// Variable names to drop from the child's inherited environment,
    /// applied with `Command::env_remove` only while `env` is `None`
    /// (`agent_cli.py::child_env`'s own removal set, ported) -- unlike
    /// `env: Some(map)` ([`crate::subprocess_tool::merged_env`]'s own
    /// job), this keeps every other variable byte for byte, including
    /// one that isn't valid UTF-8 (Python's `os.environ`/`Popen` round
    /// trip it with `surrogateescape`; a `HashMap<String, String>`
    /// can't hold it at all).
    pub env_remove: Vec<String>,
    /// Variables to set in the child's environment, applied with
    /// `Command::env` after [`RunOpts::env_remove`] (so one named in both
    /// gets this value), only while `env` is `None` (`agent_cli.py::
    /// child_env`'s own `env`, ported: an adapter alias's own variables).
    pub env_set: Vec<(String, String)>,
}

/// A finished child process -- text-mode stdout/stderr (universal-
/// newline translation, locale/UTF-8 strict decoding, lane C's own
/// job), and *exit_code* as Python's own `proc.returncode` convention:
/// non-negative for a normal exit, `-signum` for one a signal killed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Why [`run_tracked`] didn't return a [`Completed`] -- `binary not
/// found: ...`/`{binary!r} exceeded timeout of {N}s`/a decode failure's
/// own `UnicodeDecodeError` text (`plugins/_subprocess.py`'s own three
/// failure shapes), or cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcError {
    NotFound(String),
    Timeout {
        binary: String,
        timeout_seconds: u64,
    },
    Cancelled,
    Decode(String),
    /// An `OSError` (or a subclass) other than the `FileNotFoundError`
    /// [`ProcError::NotFound`] already covers -- e.g. `cwd` naming a
    /// file, not a directory (`NotADirectoryError`), or a permission
    /// error on it. *py_class*/*message* are CPython's own exception
    /// class and `str(exc)` text (PR #467: this
    /// used to collapse into the same `"cancelled"` text as
    /// [`ProcError::Cancelled`] at every call site, which is wrong --
    /// a bad `cwd` is not a cancellation).
    Other {
        py_class: PyExc,
        message: String,
    },
}

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcError::NotFound(message) => write!(f, "{message}"),
            ProcError::Timeout {
                binary,
                timeout_seconds,
            } => write!(
                f,
                "{} exceeded timeout of {timeout_seconds}s",
                Value::Str(binary.clone()).py_repr()
            ),
            ProcError::Cancelled => write!(f, "cancelled"),
            ProcError::Decode(message) => write!(f, "{message}"),
            ProcError::Other { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProcError {}

/// Drops this guard's own tracked *pid* via [`kill_process_group`] --
/// the drop-safety net `exec::tool::execute_tool`'s own `select!`
/// needs (issue #459's "Drop safety" bullet): that call races the
/// dispatch future against `token.cancelled()` and drops it outright on
/// cancellation, never giving [`run_tracked`]'s own wait loop another
/// poll to see the cancellation itself. A local of this shape, created
/// right after `spawn()` and kept alive for the rest of the function, is
/// the only hook available when the *whole* async fn is torn down
/// mid-await rather than returning normally -- Rust runs every live
/// local's own `Drop` impl when the generated state machine is
/// dropped, same as it would for a synchronous scope exit.
struct KillGroupOnDrop {
    pid: Option<u32>,
    /// `true` only while [`RunOpts::new_session`] put this child in its
    /// own session (`setsid`) -- [`run_tracked`]'s own *pgid* equals
    /// *pid* in that case (`_isolated_pgid`'s own shape), so the group
    /// kill below can use it directly without an extra `getpgid` call.
    isolated: bool,
    /// Set once [`run_tracked`]'s own wait loop has observed the child
    /// exit -- lets this guard skip killing anything at all once the
    /// pid is no longer this child's (PR #467: a
    /// `getpgid(pid)`-based kill issued *after* the pid has already
    /// been reaped can, in the rare case the OS has since reused that
    /// pid for an unrelated process, SIGKILL that unrelated process
    /// instead of a no-op). A pid can never be reused while the
    /// process holding it is still alive, so `!reaped.get()` here is
    /// proof *pid* is still this child's own.
    reaped: Rc<Cell<bool>>,
}

impl KillGroupOnDrop {
    /// Takes this guard out of service -- `run_tracked`'s own normal-exit
    /// return path (`core/cancellation.py::run_tracked` has no
    /// `except`/`finally` of its own at all once `_communicate_promptly`
    /// returns without raising, `:874-896`, so nothing there ever calls
    /// `kill_process_group` on this path) calls this right before
    /// returning `Ok`, so the group a background process joined keeps
    /// running past the step that started it (#519) -- while every
    /// other return path (timeout, cancellation, any other unwind,
    /// including a dropped future) leaves the guard armed, so its own
    /// `Drop` below still fires.
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for KillGroupOnDrop {
    // This guard's own version of `run_tracked`'s `except BaseException:` fallback kill (`core/cancellation.py:888-895`).
    fn drop(&mut self) {
        let Some(pid) = self.pid else { return };
        if self.isolated {
            // Safe regardless of whether *pid* (the group's own leader)
            // has already been reaped: a pgid can never be reused while
            // any member of the group -- e.g. a grandchild the leader
            // itself forked -- is still alive, so this never needs
            // `self.reaped`'s own guard the way the plain-pid kill below
            // does (PR #467: that guard exists
            // specifically to protect a *pid*-based kill from ever
            // landing on a reused pid, which a `killpg` on a
            // structurally-still-valid pgid never risks).
            repeat_group_kill(pid);
        } else if !self.reaped.get() {
            kill_process_group(pid);
        }
    }
}

/// A process-backed tool's running output, for a running [`crate::
/// ToolCall::on_live`] callback (#741 part B) -- the tail/byte-count
/// figures `meta.live` needs; `v`/`started_at`/`updated_at` are the
/// caller's own, assembled once it has these.
pub struct LiveTail {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

/// `meta.live`'s own tail cap (#741 part B) -- the same shape the agent
/// tool (#636) and streamed prompts (#674) already publish: the last
/// [`LIVE_TAIL_MAX_LINES`] lines (a trailing, newline-less line counts
/// as one), never more than [`LIVE_TAIL_MAX_BYTES`] bytes.
const LIVE_TAIL_MAX_LINES: usize = 20;
const LIVE_TAIL_MAX_BYTES: usize = 4096;

/// *buf*'s own last [`LIVE_TAIL_MAX_BYTES`] bytes, trimmed further to
/// its last [`LIVE_TAIL_MAX_LINES`] lines -- the byte cap is applied
/// first (so this never scans further back than that many bytes to
/// find line boundaries), the line cap second, so the result is always
/// within both at once. A trailing `\n` never counts as its own empty
/// line; everything else between newlines does, including a final
/// run with no trailing `\n` at all (`tail -n`'s own convention for a
/// file missing its final newline).
fn tail_slice(buf: &[u8], max_lines: usize, max_bytes: usize) -> &[u8] {
    let window = &buf[buf.len().saturating_sub(max_bytes)..];
    if max_lines == 0 || window.is_empty() {
        return window;
    }
    let ends_with_newline = window[window.len() - 1] == b'\n';
    let scanned = if ends_with_newline {
        &window[..window.len() - 1]
    } else {
        window
    };
    let newline_positions: Vec<usize> = scanned
        .iter()
        .enumerate()
        .filter(|&(_, &b)| b == b'\n')
        .map(|(i, _)| i)
        .collect();
    let total_lines = newline_positions.len() + 1;
    if total_lines <= max_lines {
        return window;
    }
    let lines_to_drop = total_lines - max_lines;
    let start = newline_positions[lines_to_drop - 1] + 1;
    &window[start..]
}

/// [`tail_slice`], decoded the way `meta.live`'s own tails are --
/// UTF-8 with invalid bytes replaced (never the final output's own
/// strict [`decode_text`]: a tail is a display value, cut at an
/// arbitrary byte boundary that can split a multi-byte character, and
/// must never fail a running tool over that).
fn tail_text(buf: &[u8]) -> String {
    String::from_utf8_lossy(tail_slice(buf, LIVE_TAIL_MAX_LINES, LIVE_TAIL_MAX_BYTES)).into_owned()
}

fn live_tail(stdout_buf: &[u8], stderr_buf: &[u8]) -> LiveTail {
    LiveTail {
        stdout_tail: tail_text(stdout_buf),
        stderr_tail: tail_text(stderr_buf),
        stdout_bytes: stdout_buf.len() as u64,
        stderr_bytes: stderr_buf.len() as u64,
    }
}

/// `on_live`'s own cadence (#741 part B, `exec/prompt.rs`'s own
/// `meta.live` throttle for #674/#676, ported): `true` at most once a
/// second. A call that isn't due only records that a publish is
/// *pending*, for [`run_tracked`]'s own periodic tick to flush once a
/// second has actually passed even if no further output ever arrives.
fn live_due(published_at: &mut Option<std::time::Instant>, pending: &mut bool) -> bool {
    let now = std::time::Instant::now();
    let due = published_at.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1));
    if due {
        *published_at = Some(now);
        *pending = false;
    } else {
        *pending = true;
    }
    due
}

/// `core/cancellation.py::run_tracked` -- process-group spawn, the
/// stdin-writer task, universal-newline/locale decoding, and the
/// token-tracked wait/kill loop. *on_live*, when given, is called with
/// this run's current output tails/byte counts while it runs, already
/// throttled to [`live_due`]'s own cadence (#741 part B) -- `None` for
/// every caller that never publishes `meta.live` (the agent CLIs,
/// `http`).
pub async fn run_tracked(
    cmd: &[String],
    opts: &RunOpts,
    token: &CancellationToken,
    on_live: Option<&dyn Fn(LiveTail)>,
) -> Result<Completed, ProcError> {
    // `get_token().check()` before `Popen` -- refuse to even start a new
    // child once cancellation has already been requested.
    if token.is_set() {
        return Err(ProcError::Cancelled);
    }
    let Some(program) = cmd.first() else {
        return Err(ProcError::Other {
            py_class: PyExc::RuntimeError,
            message: "run_tracked called with an empty cmd".to_string(),
        });
    };

    let mut command = Command::new(program);
    command.args(&cmd[1..]);
    if let Some(cwd) = &opts.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &opts.env {
        command.env_clear();
        command.envs(env);
    } else {
        for name in &opts.env_remove {
            command.env_remove(name);
        }
        for (name, value) in &opts.env_set {
            command.env(name, value);
        }
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if opts.input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        // `input=None` means the child **inherits** the parent's own
        // stdin, not `/dev/null` -- `Stdio::inherit()` is tokio's own
        // equivalent of `subprocess.Popen` leaving `stdin` unset.
        command.stdin(Stdio::inherit());
    }
    #[cfg(unix)]
    {
        // A `pre_exec` closure is installed unconditionally, not only
        // while `opts.new_session` -- Rust's std only takes its
        // `posix_spawn(2)` fast path when it's absent, and that fast
        // path can only report a generic `ENOENT` for *any*
        // `current_dir` failure on at least one platform this crate
        // runs on (confirmed directly: an identical `Command` whose
        // `cwd` names a plain file reports `NotFound`/`ENOENT` via
        // `posix_spawn`, but `NotADirectoryError`/`ENOTDIR` -- the
        // real reason -- once *any* `pre_exec` forces plain `fork`+
        // `exec` instead, the only path that actually runs `chdir` and
        // reports its own real errno). `describe_spawn_error` needs the
        // latter, in both the isolated and non-isolated case, to match
        // Python's own `_posixsubprocess` (which never takes a
        // `posix_spawn` shortcut for this).
        //
        // `setsid()`, not merely `process_group(0)`, while isolated
        // (PR #467) -- Python's own
        // `start_new_session=True` calls `setsid()`, detaching the
        // child from any controlling terminal entirely.
        // `process_group(0)` alone only calls `setpgid`: the child
        // stays in the *session* of whatever terminal started this
        // run, in a background process group, so a child that reads or
        // writes the inherited tty (`ffmpeg`'s own startup `tcsetattr`,
        // a `cat` reading stdin) is stopped by `SIGTTOU`/`SIGTTIN`
        // until this call's own timeout kills it -- invisible in CI (no
        // tty) but reproducible with any real terminal, or a pty in a
        // test.
        let new_session = opts.new_session;
        unsafe {
            command.pre_exec(move || {
                if new_session && libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ProcError::NotFound(format!("{program}: {err}")));
        }
        Err(err) => {
            let (py_class, message) = describe_spawn_error(&err, program, opts.cwd.as_deref());
            return Err(ProcError::Other { py_class, message });
        }
    };

    let pid = child.id();
    // `setsid()` above makes the child its own session **and** process
    // group leader, so its pgid is its own pid -- captured once, right
    // after spawn, exactly like `core/cancellation.py::_isolated_pgid`:
    // a pgid can never be reused while any member of the group is still
    // alive, so this stays valid for every repeat kill below even after
    // this *pid* itself has been reaped (PR #467 --
    // a `getpgid(pid)` lookup done *after* the reap always fails ESRCH,
    // even while a grandchild forked into the same, still-live pgid
    // keeps the group itself alive).
    let pgid = if opts.new_session { pid } else { None };
    let reaped = Rc::new(Cell::new(false));
    // Lives for the rest of this function -- its own `Drop` is this
    // call's safety net against being torn down mid-await from outside
    // (see the struct's own doc comment above). `mut`: the normal-exit
    // path below calls `disarm()` on it before returning `Ok`.
    let mut kill_on_drop = KillGroupOnDrop {
        pid,
        isolated: opts.new_session,
        reaped: reaped.clone(),
    };

    if let Some(data) = opts.input.clone() {
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                // `_feed_stdin`: a broken pipe (the child never reads,
                // or exits early) ends the write quietly.
                let _ = stdin.write_all(&data).await;
                let _ = stdin.shutdown().await;
            });
        }
    }

    let Some(mut stdout_pipe) = child.stdout.take() else {
        return Err(ProcError::Other {
            py_class: PyExc::RuntimeError,
            message: "child has no stdout pipe".to_string(),
        });
    };
    let Some(mut stderr_pipe) = child.stderr.take() else {
        return Err(ProcError::Other {
            py_class: PyExc::RuntimeError,
            message: "child has no stderr pipe".to_string(),
        });
    };

    let deadline = opts
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);

    // The whole call's own wall-clock deadline covers waiting for the
    // child to exit **and** for both pipes to reach EOF (PR #467) --
    // a grandchild that keeps a pipe open after the
    // tracked child itself has exited (`awk 'BEGIN{system("sleep 600
    // &")}'`) must not make this call hang with no limit once
    // `child.wait()` alone has returned. `child_status`/`stdout_done`/
    // `stderr_done` each go from unset to set independently and never
    // revert, so a `select!` branch already resolved is excluded from
    // every later iteration's own `if` guard -- `child.wait()` and a
    // pipe's own `read()` are both safe to poll again on a fresh
    // `select!` each loop iteration up until their own branch first
    // reports EOF or an error.
    let mut child_status: Option<std::process::ExitStatus> = None;
    // Read straight into these in the loop below, not through a
    // separate `tokio::spawn`'d reader task the way a `read_to_end`
    // only handed back at EOF would need (#741 part B: `on_live` needs
    // the bytes as they arrive, not just the final total) -- this loop
    // already multiplexes `child.wait()`/the poll timer through the
    // same `tokio::select!`, so reading each pipe inline here, re-
    // polled every iteration exactly like those two already are,
    // needs no second task or channel at all.
    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_chunk = [0u8; 8192];
    let mut stderr_chunk = [0u8; 8192];
    // `on_live`'s own publish-pending state across loop iterations
    // (#741 part B) -- unused, and left at its initial `None`/`false`,
    // for every call `on_live` is `None` for.
    let mut live_published_at: Option<std::time::Instant> = None;
    let mut live_pending = false;

    let timed_out = loop {
        if child_status.is_some() && stdout_done && stderr_done {
            break false;
        }
        let remaining =
            deadline.map(|dl| dl.saturating_duration_since(tokio::time::Instant::now()));
        if remaining.is_some_and(|r| r.is_zero()) {
            break true;
        }
        let sleep_for = remaining
            .map(|r| r.min(POLL_INTERVAL))
            .unwrap_or(POLL_INTERVAL);
        tokio::select! {
            status = child.wait(), if child_status.is_none() => {
                match status {
                    Ok(status) => {
                        child_status = Some(status);
                        reaped.set(true);
                    }
                    Err(err) => {
                        return Err(ProcError::Other {
                            py_class: PyExc::RuntimeError,
                            message: format!("wait failed: {err}"),
                        });
                    }
                }
            }
            result = stdout_pipe.read(&mut stdout_chunk), if !stdout_done => {
                match result {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => {
                        stdout_buf.extend_from_slice(&stdout_chunk[..n]);
                        if let Some(on_live) = on_live {
                            if live_due(&mut live_published_at, &mut live_pending) {
                                on_live(live_tail(&stdout_buf, &stderr_buf));
                            }
                        }
                    }
                }
            }
            result = stderr_pipe.read(&mut stderr_chunk), if !stderr_done => {
                match result {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => {
                        stderr_buf.extend_from_slice(&stderr_chunk[..n]);
                        if let Some(on_live) = on_live {
                            if live_due(&mut live_published_at, &mut live_pending) {
                                on_live(live_tail(&stdout_buf, &stderr_buf));
                            }
                        }
                    }
                }
            }
            () = tokio::time::sleep(sleep_for) => {
                // While cancellation is set, keep repeating the group
                // kill until the pipes reach EOF -- `_repeat_group_kill`
                // closes the race window a fork right at the first
                // `kill_process_group` call can slip through. Only ever
                // targets *pgid* directly (never re-derives it from
                // *pid*, and never fires at all when this run isn't
                // isolated) -- see `repeat_group_kill`'s own doc comment.
                // Mirrors `core/cancellation.py::_communicate_promptly`'s
                // own repeat-kill-while-cancelled branch, `:806-810`.
                if token.is_set() {
                    if let Some(pgid) = pgid {
                        repeat_group_kill(pgid);
                    }
                }
                // This loop's own trailing publish (#741 part B): a
                // pending update from inside the 1 s throttle window
                // above still goes out within about a second of it even
                // if no further output ever arrives, the same way
                // `exec/prompt.rs`'s own wake-timer arm flushes a
                // pending `meta.live` update for a streamed prompt.
                if let Some(on_live) = on_live {
                    if live_pending && live_due(&mut live_published_at, &mut live_pending) {
                        on_live(live_tail(&stdout_buf, &stderr_buf));
                    }
                }
            }
        }
    };

    if timed_out {
        // The first kill, same as `core/cancellation.py::kill_process_group`
        // itself: a no-op once the child (not necessarily the whole
        // group) has already exited. Mirrors `run_tracked`'s own
        // `except subprocess.TimeoutExpired:` branch, `:879-887`.
        if let Some(pid) = pid {
            kill_process_group(pid);
        }
        if child_status.is_none() && child.wait().await.is_ok() {
            reaped.set(true);
        }
        // One more kill for a grandchild forked at the instant of the
        // first one -- always against the remembered *pgid* directly
        // (never re-derived via `getpgid(pid)`, which would already
        // report ESRCH once *pid* itself has been reaped even though
        // the grandchild, and so the group, is still alive).
        if let Some(pgid) = pgid {
            repeat_group_kill(pgid);
        }
        let timeout_seconds = opts.timeout.map(|t| t.as_secs()).unwrap_or(0);
        return Err(ProcError::Timeout {
            binary: program.clone(),
            timeout_seconds,
        });
    }

    let status = child_status.expect("loop only breaks `false` once child_status is Some");
    // A decode failure here still leaves the guard armed (the `?`
    // below returns before `disarm()` runs) -- it mirrors Python's own
    // decode, which happens inside `proc.communicate()`, so a
    // `UnicodeDecodeError` there unwinds through `run_tracked`'s own
    // `except BaseException:` and still kills the group, `:888-895`.
    let stdout = decode_text(&stdout_buf)?;
    let stderr = decode_text(&stderr_buf)?;

    // The one path with no kill at all in `core/cancellation.py::
    // run_tracked` (#519): a normal exit disarms the guard instead of
    // letting its `Drop` run `repeat_group_kill`/`kill_process_group`
    // on the whole group, so a background process it started keeps
    // running past this step.
    kill_on_drop.disarm();
    Ok(Completed {
        stdout,
        stderr,
        exit_code: exit_code_from_status(&status),
    })
}

#[cfg(unix)]
fn exit_code_from_status(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(code) => code,
        None => status.signal().map(|signal| -signal).unwrap_or(-1),
    }
}

#[cfg(not(unix))]
fn exit_code_from_status(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Strict UTF-8 decode plus universal-newline translation (`\r\n` and a
/// lone `\r` both become `\n`) -- `plugins/_subprocess.py`'s own text
/// mode, as `subprocess.Popen(text=True)` performs it.
fn decode_text(bytes: &[u8]) -> Result<String, ProcError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(universal_newlines(text)),
        Err(_) => Err(ProcError::Decode(describe_utf8_error(bytes))),
    }
}

fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Why *bytes* is not valid UTF-8, formatted exactly like CPython's
/// `UnicodeDecodeError.__str__` -- a small, self-contained duplicate of
/// `electricity_compiler::prompt_files`'s own (private, differently-
/// owned-lane) helper of the same shape: that module belongs to a
/// different vm-lanes.md row, so this crate carries its own copy rather
/// than reaching across the lane boundary for it.
fn describe_utf8_error(bytes: &[u8]) -> String {
    let (start, consumed, reason) =
        find_utf8_error(bytes).expect("describe_utf8_error called on valid UTF-8");
    if consumed == 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            start + consumed - 1
        )
    }
}

fn find_utf8_error(bytes: &[u8]) -> Option<(usize, usize, &'static str)> {
    let mut i = 0;
    while i < bytes.len() {
        let lead = bytes[i];
        if lead < 0x80 {
            i += 1;
            continue;
        }
        let Some((total_len, min1, max1)) = utf8_lead_shape(lead) else {
            return Some((i, 1, "invalid start byte"));
        };
        for seq_idx in 1..total_len {
            let Some(&cont) = bytes.get(i + seq_idx) else {
                return Some((i, seq_idx, "unexpected end of data"));
            };
            let (lo, hi) = if seq_idx == 1 {
                (min1, max1)
            } else {
                (0x80, 0xBF)
            };
            if cont < lo || cont > hi {
                return Some((i, seq_idx, "invalid continuation byte"));
            }
        }
        i += total_len;
    }
    None
}

fn utf8_lead_shape(lead: u8) -> Option<(usize, u8, u8)> {
    match lead {
        0xC2..=0xDF => Some((2, 0x80, 0xBF)),
        0xE0 => Some((3, 0xA0, 0xBF)),
        0xE1..=0xEC => Some((3, 0x80, 0xBF)),
        0xED => Some((3, 0x80, 0x9F)),
        0xEE..=0xEF => Some((3, 0x80, 0xBF)),
        0xF0 => Some((4, 0x90, 0xBF)),
        0xF1..=0xF3 => Some((4, 0x80, 0xBF)),
        0xF4 => Some((4, 0x80, 0x8F)),
        _ => None,
    }
}

/// `core/cancellation.py::kill_process_group` -- best-effort `SIGKILL`
/// of *pid*'s whole process group; a no-op once the group has already
/// exited. POSIX only, matching the Python reference -- Windows gets no
/// process-group isolation (M1's own out-of-scope list).
pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        let pid = pid as libc::pid_t;
        // SAFETY: plain libc getpgid/kill/killpg calls on a pid this
        // crate itself spawned (or the no-op early return below if it's
        // already gone) -- no raw pointers, no aliasing.
        unsafe {
            let pgid = libc::getpgid(pid);
            if pgid == -1 {
                // ESRCH (or any other errno): already gone -- no-op,
                // matching `proc.poll() is not None` returning early.
                return;
            }
            if pgid == libc::getpgrp() {
                // The child shares this interpreter's own process
                // group -- `killpg` here would take this process down
                // with it, so kill just the child (#357).
                libc::kill(pid, libc::SIGKILL);
                return;
            }
            // `ESRCH`/`EPERM` both ignored -- the macOS zombie case:
            // once a first `SIGKILL` has killed the group's last live
            // member, a second `killpg` at the same, still-correctly-
            // owned pgid can raise `EPERM` before anything reaps it.
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// `core/cancellation.py::_repeat_group_kill` -- a raw `killpg` on the
/// remembered, already-isolated *pgid* (`pid == pgid`, since `setsid`
/// made the child its own session/group leader) -- bypasses
/// [`kill_process_group`]'s own `getpgid(pid)` precheck entirely,
/// deliberately: once the group's own leader has been reaped, that
/// lookup always reports ESRCH, even while a grandchild forked into the
/// same, still-valid pgid keeps the whole group alive -- a pgid can
/// never be reused while any member of it still lives, so repeating the
/// exact same `killpg` call is always safe and never reaches a
/// different, unrelated process.
fn repeat_group_kill(pgid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: a plain libc `killpg` on a pgid this crate itself
        // isolated at spawn time; errors (`ESRCH`/`EPERM`) are both
        // ignored, same as `kill_process_group`'s own last call.
        unsafe {
            libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
    }
}

/// `OSError.__str__`'s own `[Errno {n}] {strerror}: {filename!r}` shape
/// (or, with no `filename`, just `[Errno {n}] {strerror}`) -- for a
/// `Command::spawn` failure other than `ErrorKind::NotFound` (that one
/// is [`ProcError::NotFound`] already, matching Python's own
/// `FileNotFoundError` special case in `plugins/_subprocess.py::
/// run_binary`). *cwd* is reported as the failing path whenever the
/// error itself says "not a directory" (a `cwd` naming a plain file);
/// every other case blames *program*, the executable being spawned.
fn describe_spawn_error(
    err: &io::Error,
    program: &str,
    cwd: Option<&std::path::Path>,
) -> (PyExc, String) {
    let py_class = match err.kind() {
        io::ErrorKind::PermissionDenied => PyExc::PermissionError,
        io::ErrorKind::NotADirectory => PyExc::NotADirectoryError,
        io::ErrorKind::IsADirectory => PyExc::IsADirectoryError,
        _ => PyExc::OSError,
    };
    let filename = if err.kind() == io::ErrorKind::NotADirectory {
        cwd.map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string())
    } else {
        program.to_string()
    };
    let strerror = strerror_text(err);
    let message = match err.raw_os_error() {
        Some(errno) => format!(
            "[Errno {errno}] {strerror}: {}",
            Value::Str(filename).py_repr()
        ),
        None => strerror,
    };
    (py_class, message)
}

/// `exc.strerror`: the bare OS error message, without Rust's own
/// `io::Error::to_string()` " (os error N)" suffix -- the same small
/// helper `electricity_config::config::strerror` carries under its own
/// lane; this crate keeps its own copy rather than reaching across the
/// lane boundary for it.
fn strerror_text(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

/// `plugins/_subprocess.py::resolve_binary` -- the first of *candidates*
/// found on `PATH` (or an absolute path that exists), else `None`;
/// never an error -- Python's own version takes a list because a
/// plugin like `imagemagick` ships under several names (`magick`/
/// `convert`), tried in order (the gate's own first signature took
/// one binary and returned `Result`, matching neither shape).
pub fn resolve_binary(candidates: &[&str]) -> Option<String> {
    for name in candidates {
        if name.is_empty() {
            continue;
        }
        let path = std::path::Path::new(name);
        if path.is_absolute() && path.exists() {
            return Some((*name).to_string());
        }
        if let Some(found) = which(name) {
            return Some(found);
        }
    }
    None
}

/// `os.defpath` -- `shutil.which`'s own fallback `PATH` when the
/// environment doesn't set one at all (distinct from an *empty*
/// `PATH`, which stays empty: `os.environ.get("PATH", os.defpath)`
/// only substitutes this on a missing key, never on a present-but-empty
/// value).
const DEFPATH: &str = "/bin:/usr/bin";

/// `shutil.which`'s own POSIX semantics -- a name with a directory
/// component is checked directly (no `PATH` search); otherwise every
/// `PATH` entry is tried in order (falling back to [`DEFPATH`] when the
/// variable is unset), requiring an existing, executable, non-directory
/// file. Public (issue #520's part A) for an adapter's own `check()`:
/// unlike [`resolve_binary`], this never accepts a non-executable
/// absolute path just because it exists.
pub fn which(name: &str) -> Option<String> {
    let path = std::path::Path::new(name);
    if path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        return if is_executable_file(path) {
            Some(name.to_string())
        } else {
            None
        };
    }
    let path_var = std::env::var_os("PATH").unwrap_or_else(|| std::ffi::OsString::from(DEFPATH));
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// `shutil.which`'s own `_access_check`: exists, `os.access(path,
/// os.X_OK)`, and not a directory -- `os.access` (PR #467),
/// not a manual permission-bit check: it asks the kernel
/// directly, honouring ACLs/setuid/filesystem mount flags a plain
/// `mode & 0o111` check can't see.
fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if path.is_dir() {
            return false;
        }
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a plain libc `access` syscall on a NUL-free path.
        unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn timeout_display_quotes_the_binary_like_pythons_own_repr() {
        // PR #454: `plugins/_subprocess.py`'s
        // own `{binary!r}` is single-quoted; Rust's `{:?}` on a `String`
        // is double-quoted.
        let err = ProcError::Timeout {
            binary: "ffmpeg".to_string(),
            timeout_seconds: 30,
        };
        assert_eq!(err.to_string(), "'ffmpeg' exceeded timeout of 30s");
    }

    #[test]
    fn universal_newlines_translates_crlf_and_lone_cr() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn decode_text_reports_cpythons_exact_message_on_a_bad_byte() {
        let err = decode_text(&[0xff]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
    }

    #[derive(serde::Deserialize)]
    struct Utf8DecodeCase {
        bytes_hex: String,
        expected_error: Option<String>,
        expected_text: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct ProcessToolsCorpus {
        utf8_decode_cases: Vec<Utf8DecodeCase>,
    }

    #[test]
    fn decode_text_matches_pythons_own_bytes_decode_utf8() {
        let corpus: ProcessToolsCorpus =
            serde_json::from_str(include_str!("../tests/golden/process_tools_corpus.json"))
                .expect("process_tools_corpus.json must parse");
        for case in corpus.utf8_decode_cases {
            let bytes = hex_decode(&case.bytes_hex);
            let result = decode_text(&bytes);
            match (case.expected_error, case.expected_text) {
                (Some(expected), None) => {
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        expected,
                        "hex={}",
                        case.bytes_hex
                    )
                }
                (None, Some(expected)) => {
                    assert_eq!(
                        result.unwrap(),
                        universal_newlines(&expected),
                        "hex={}",
                        case.bytes_hex
                    )
                }
                other => panic!("inconsistent corpus case: {other:?}"),
            }
        }
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn tail_slice_keeps_the_last_twenty_lines() {
        let mut buf = String::new();
        for i in 0..25 {
            buf.push_str(&format!("line{i}\n"));
        }
        let tail = tail_slice(buf.as_bytes(), 20, 1_000_000);
        let text = std::str::from_utf8(tail).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[0], "line5");
        assert_eq!(lines[19], "line24");
    }

    #[test]
    fn tail_slice_keeps_at_most_four_kib_even_well_under_the_line_cap() {
        // One 10,000-byte line, no newline at all -- a single partial
        // line, well under the 20-line cap, so the 4 KiB byte cap is
        // the only thing trimming it.
        let buf = vec![b'a'; 10_000];
        let tail = tail_slice(&buf, 20, 4096);
        assert_eq!(tail.len(), 4096);
        assert!(tail.iter().all(|&b| b == b'a'));
    }

    #[test]
    fn tail_slice_counts_an_unterminated_final_line_as_its_own_line() {
        let buf = b"a\nb\nc";
        assert_eq!(tail_slice(buf, 2, 4096), b"b\nc");
        assert_eq!(tail_slice(buf, 1, 4096), b"c");
        // A trailing `\n` never counts as its own empty line.
        assert_eq!(tail_slice(b"a\nb\nc\n", 2, 4096), b"b\nc\n");
    }

    #[test]
    fn tail_text_replaces_invalid_utf8_with_the_replacement_character() {
        let buf = [b'a', 0xff, b'b'];
        assert_eq!(tail_text(&buf), "a\u{fffd}b");
    }

    #[tokio::test]
    async fn run_tracked_calls_on_live_with_growing_separate_tails_and_a_trailing_flush() {
        // `printf` of each stream's own line is unbuffered (unlike a
        // libc-buffered `echo` inside a script that might coalesce
        // both writes into one pipe read), so the main loop's own
        // first `read()` reliably sees stdout alone, publishing it at
        // once (#741 part B: no prior publish this call, so the first
        // update is never throttled) -- the child then sleeps past the
        // 1 s throttle window with nothing further written, so the
        // *second* line (stderr) is only ever picked up by this loop's
        // own trailing-publish tick, never a third, immediate publish
        // of its own.
        let token = CancellationToken::new();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf out1; printf out2 1>&2; sleep 1.5".to_string(),
        ];
        let calls: Rc<RefCell<Vec<LiveTail>>> = Rc::new(RefCell::new(Vec::new()));
        let calls_for_callback = Rc::clone(&calls);
        let on_live = move |tail: LiveTail| {
            calls_for_callback.borrow_mut().push(tail);
        };
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, Some(&on_live))
            .await
            .unwrap();
        assert_eq!(completed.stdout, "out1");
        assert_eq!(completed.stderr, "out2");
        let calls = calls.borrow();
        assert!(
            calls.len() >= 2,
            "expected an immediate publish plus a later, trailing one; got {}",
            calls.len()
        );
        assert_eq!(calls[0].stdout_tail, "out1");
        assert_eq!(calls[0].stderr_tail, "");
        assert_eq!(calls[0].stdout_bytes, 4);
        assert_eq!(calls[0].stderr_bytes, 0);
        let last = calls.last().unwrap();
        assert_eq!(last.stdout_tail, "out1");
        assert_eq!(last.stderr_tail, "out2");
        assert_eq!(last.stderr_bytes, 4);
    }

    #[tokio::test]
    async fn run_tracked_never_calls_on_live_when_it_is_none() {
        // Every caller `on_live` is `None` for (the agent CLIs, `http`)
        // must cost this loop nothing beyond the `if let Some` checks
        // already there -- this just pins that passing `None` through
        // a real run never panics on an absent callback.
        let token = CancellationToken::new();
        let cmd = vec!["echo".to_string(), "hi".to_string()];
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, None)
            .await
            .unwrap();
        assert_eq!(completed.stdout, "hi\n");
    }

    #[tokio::test]
    async fn run_tracked_refuses_to_start_once_cancellation_is_set() {
        let token = CancellationToken::new();
        token.request(2);
        let err = run_tracked(&["true".to_string()], &RunOpts::default(), &token, None)
            .await
            .unwrap_err();
        assert_eq!(err, ProcError::Cancelled);
    }

    #[tokio::test]
    async fn run_tracked_captures_stdout_stderr_and_exit_code() {
        let token = CancellationToken::new();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf out; printf err 1>&2; exit 3".to_string(),
        ];
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, None)
            .await
            .unwrap();
        assert_eq!(completed.stdout, "out");
        assert_eq!(completed.stderr, "err");
        assert_eq!(completed.exit_code, 3);
    }

    #[tokio::test]
    async fn run_tracked_reports_not_found_for_a_missing_binary() {
        let token = CancellationToken::new();
        let cmd = vec!["electricity-tools-definitely-missing-binary".to_string()];
        let err = run_tracked(&cmd, &RunOpts::default(), &token, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ProcError::NotFound(_)));
    }

    #[tokio::test]
    async fn run_tracked_times_out_and_kills_the_child() {
        let token = CancellationToken::new();
        let opts = RunOpts {
            timeout: Some(Duration::from_millis(100)),
            new_session: true,
            ..RunOpts::default()
        };
        let cmd = vec!["sleep".to_string(), "30".to_string()];
        let err = run_tracked(&cmd, &opts, &token, None).await.unwrap_err();
        assert!(matches!(err, ProcError::Timeout { .. }));
    }

    #[tokio::test]
    async fn run_tracked_feeds_input_to_the_childs_stdin() {
        let token = CancellationToken::new();
        let opts = RunOpts {
            input: Some(b"hello\n".to_vec()),
            ..RunOpts::default()
        };
        let cmd = vec!["cat".to_string()];
        let completed = run_tracked(&cmd, &opts, &token, None).await.unwrap();
        assert_eq!(completed.stdout, "hello\n");
    }

    #[test]
    fn resolve_binary_finds_the_first_candidate_on_path() {
        assert_eq!(
            resolve_binary(&["definitely-not-a-real-binary", "sh"]),
            which("sh")
        );
    }

    #[test]
    fn resolve_binary_returns_none_when_no_candidate_exists() {
        assert_eq!(
            resolve_binary(&["definitely-not-a-real-binary-either"]),
            None
        );
    }

    #[test]
    fn kill_process_group_on_an_already_gone_pid_is_a_no_op() {
        // A pid this process almost certainly never had -- `getpgid`
        // returns ESRCH, hitting the no-op branch directly; this just
        // proves it doesn't panic.
        kill_process_group(u32::MAX);
    }

    #[tokio::test]
    async fn an_unarmed_timeout_kills_only_the_child_never_the_host_group() {
        // `new_session: false` (the default -- an embedded/library
        // call) keeps the child in this test process's own group. If
        // `kill_process_group` ever mistakenly `killpg`ed that group
        // instead of the one child, this whole test binary would die
        // with it -- reaching the assertion below at all is the proof.
        let token = CancellationToken::new();
        let opts = RunOpts {
            timeout: Some(Duration::from_millis(100)),
            ..RunOpts::default()
        };
        let cmd = vec!["sleep".to_string(), "30".to_string()];
        let err = run_tracked(&cmd, &opts, &token, None).await.unwrap_err();
        assert!(matches!(err, ProcError::Timeout { .. }));
    }

    #[tokio::test]
    async fn dropping_the_future_mid_run_kills_the_child_the_drop_guard() {
        // `exec::tool::execute_tool`'s own `select!` drops the dispatch
        // future outright on cancellation, never giving `run_tracked`'s
        // own wait loop another poll -- `KillGroupOnDrop` is the only
        // thing that can still kill the child in that case.
        let token = CancellationToken::new();
        let opts = RunOpts {
            new_session: true,
            ..RunOpts::default()
        };
        // A distinctive duration (never used by any other sleep-based
        // test in this module) so `pgrep -f` below can never match a
        // sibling test's own still-running child when tests run in
        // parallel.
        let cmd = vec!["sleep".to_string(), "8462".to_string()];
        // `Box::pin`, not `tokio::pin!` -- the macro's own pinned
        // local is a reference to a *sibling* stack slot, so `drop`ing
        // the name it shadows only drops that reference, not the real
        // future; this test needs the real owned future actually
        // dropped at a precise point, which only a `Pin<Box<_>>`'s own
        // `drop` gives.
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None));
        // One poll is enough to spawn the child and construct the
        // guard; the future is then dropped without ever completing.
        tokio::select! {
            _ = &mut fut => panic!("must not finish on the very first poll"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        drop(fut);
        // The guard's own `Drop` already ran synchronously inside
        // `drop(fut)` above (`kill_process_group` is itself
        // synchronous) -- give the kernel a moment to actually reap
        // it, then confirm no `sleep 8462` survives this test.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let still_running = std::process::Command::new("pgrep")
            .args(["-f", "sleep 8462"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        assert!(
            !still_running,
            "the dropped run_tracked future must have killed its own child"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_background_process_survives_a_step_that_finishes_normally() {
        // Issue #519: `sh`'s own `sleep 8473 &` keeps running in the
        // isolated group after `sh` itself has exited and this call
        // has returned `Ok` -- the guard must be disarmed on this path,
        // not left to `killpg` the whole group the way its `Drop`
        // would on every other return path. `#[cfg(unix)]` (not just
        // the `pid_is_alive`/`libc::kill` calls below): on Linux a
        // killed orphan is reaped within a few milliseconds, well
        // under the first liveness check's own timing, so without the
        // second check below this test could pass even with the fix
        // reverted.
        let token = CancellationToken::new();
        let opts = RunOpts {
            new_session: true,
            ..RunOpts::default()
        };
        // The background `sleep`'s own stdout/stderr are redirected
        // away, not inherited -- a process that keeps this step's own
        // stdout pipe open is the *other* case the issue names, which
        // both engines already handle the same way (both wait for the
        // pipe to close); this test is about the group surviving a
        // normal exit, so the background process must not also hold
        // this call's own pipe open.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8473 >/dev/null 2>&1 & echo $!".to_string(),
        ];
        let completed = run_tracked(&cmd, &opts, &token, None).await.unwrap();
        assert_eq!(completed.exit_code, 0);
        let bg_pid: u32 = completed.stdout.trim().parse().expect("sh printed $!");
        assert!(
            pid_is_alive(bg_pid),
            "the background process must still be running right after the step returned"
        );
        // A second check, ~300ms later: a killed orphan is reaped long
        // before that on every platform this crate runs on, so this is
        // what actually catches the bug on Linux (where the first
        // check alone can observe the pid as still-alive-but-zombie
        // even on the buggy path, for a window too short to be a
        // reliable signal on its own).
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            pid_is_alive(bg_pid),
            "the background process must still be running 300ms after the step returned"
        );
        // Clean it up ourselves -- by pid, never a bare name, so this
        // never reaches another lane's own processes on this machine.
        unsafe {
            libc::kill(bg_pid as libc::pid_t, libc::SIGKILL);
        }
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: u32) -> bool {
        // `kill(pid, 0)`: no signal sent, just an existence check --
        // `ESRCH` means gone, anything else (including success) means
        // the pid is still live.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// A scratch directory for a single test's own pidfile, removed on
    /// drop -- same shape as [`ScratchBinDir`] above, kept separate
    /// because this one holds a pidfile a background shell writes into,
    /// not a `PATH` candidate.
    struct ScratchPidDir {
        dir: std::path::PathBuf,
    }

    impl ScratchPidDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-tools-process-test-{}-{}",
                std::process::id(),
                name
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchPidDir { dir }
        }

        fn pidfile(&self) -> std::path::PathBuf {
            self.dir.join("bg.pid")
        }
    }

    impl Drop for ScratchPidDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn in_loop_cancellation_kills_a_background_process_when_new_session_is_set() {
        // Unlike `dropping_the_future_mid_run_kills_the_child_the_drop_guard`
        // above (which tears the future down from *outside*, never
        // giving `run_tracked`'s own wait loop another poll), this
        // awaits the call to completion after requesting cancellation --
        // exercising the *other* kill path: the wait loop's own
        // `token.is_set()` branch, which repeats `killpg` on the
        // isolated group until the pipes reach EOF.
        let scratch = ScratchPidDir::new("in-loop-cancellation");
        let pidfile = scratch.pidfile();
        let token = CancellationToken::new();
        let opts = RunOpts {
            new_session: true,
            ..RunOpts::default()
        };
        // A duration unique to this test (never used by any other
        // sleep-based test in this module) so the cleanup below, and
        // `pgrep`, can never touch a sibling test's own child.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "sleep 8474 >/dev/null 2>&1 & echo $! > {}; sleep 30",
                pidfile.display()
            ),
        ];
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None));
        // One poll (via `select!`, not a bare `.await` sitting on an
        // un-started future) is enough to actually spawn the shell and
        // its background child before cancellation is requested.
        tokio::select! {
            _ = &mut fut => panic!("must not finish before cancellation is requested"),
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        token.request(2);
        // Awaited to completion here (unlike the drop-guard test above,
        // which drops the future outright) -- this exercises the wait
        // loop's own `token.is_set()` branch, not `KillGroupOnDrop`.
        let _ = fut.await;

        let bg_pid: u32 = std::fs::read_to_string(&pidfile)
            .expect("the background sleep must have written its own pid")
            .trim()
            .parse()
            .expect("pidfile must contain a plain pid");
        // Poll rather than a single hard sleep -- faster on a quiet
        // machine, still tolerant of load on a shared one, but never
        // waits longer than the ~300ms budget before failing.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
        while pid_is_alive(bg_pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !pid_is_alive(bg_pid),
            "cancellation requested mid-run must kill the isolated group's \
             background process within ~300ms, not just the tracked child"
        );
    }

    #[tokio::test]
    async fn stdin_larger_than_a_pipe_buffer_does_not_hang_a_child_that_never_reads() {
        // A pipe buffer is typically 64 KiB; `true` never reads its own
        // stdin at all. The writer thread must see `BrokenPipeError`
        // (quietly) rather than this call waiting the full deadline.
        let token = CancellationToken::new();
        let opts = RunOpts {
            input: Some(vec![b'x'; 4 * 1024 * 1024]),
            timeout: Some(Duration::from_secs(10)),
            ..RunOpts::default()
        };
        let cmd = vec!["true".to_string()];
        let started = tokio::time::Instant::now();
        let completed = run_tracked(&cmd, &opts, &token, None).await.unwrap();
        assert_eq!(completed.exit_code, 0);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not wait anywhere near the full 10s deadline"
        );
    }

    fn pgrep_matches(pattern: &str) -> bool {
        std::process::Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn a_timeout_kills_a_grandchild_that_keeps_the_pipe_open_after_the_tracked_child_exits() {
        // PR #467: `sh`'s own `sleep 8470 &` forks a
        // grandchild that inherits the same stdout pipe and keeps it
        // open well past `echo hi; exit 0` -- a pipe-drain phase with no
        // deadline of its own would hang here, ignoring `timeout_ms`
        // entirely (the issue's own `awk 'BEGIN{system("sleep ... &")}'`
        // example, same shape).
        let token = CancellationToken::new();
        let opts = RunOpts {
            timeout: Some(Duration::from_millis(300)),
            new_session: true,
            ..RunOpts::default()
        };
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8470 & echo hi".to_string(),
        ];
        let started = tokio::time::Instant::now();
        let err = run_tracked(&cmd, &opts, &token, None).await.unwrap_err();
        assert!(matches!(err, ProcError::Timeout { .. }));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the pipe-drain phase must share the same deadline as the \
             child's own exit, not wait unboundedly for the grandchild"
        );
        // The repeat kill after the timeout's own `wait()` must reach
        // the grandchild via the remembered *pgid* directly, not a
        // `getpgid(pid)` lookup on a pid `sh` has already had reaped.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !pgrep_matches("sleep 8470"),
            "a timed-out run must not leak the grandchild it forked"
        );
    }

    #[tokio::test]
    async fn an_unarmed_timeout_still_returns_promptly_even_with_a_surviving_grandchild() {
        // The unarmed/non-isolated case has no pgid to repeat-kill
        // (`_repeat_group_kill(None)` is a no-op in the Python
        // reference too) -- the grandchild is allowed to survive, but
        // this call must still return `Timeout` promptly rather than
        // hanging on the pipe that grandchild keeps open.
        let token = CancellationToken::new();
        let opts = RunOpts {
            timeout: Some(Duration::from_millis(300)),
            ..RunOpts::default()
        };
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8471 & echo hi".to_string(),
        ];
        let started = tokio::time::Instant::now();
        let err = run_tracked(&cmd, &opts, &token, None).await.unwrap_err();
        assert!(matches!(err, ProcError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
        // This test cleans up the leftover itself -- an unarmed run has
        // no process-group isolation to clean it up with, by design.
        let _ = std::process::Command::new("pkill")
            .args(["-f", "sleep 8471"])
            .status();
    }

    #[tokio::test]
    async fn dropping_the_future_after_the_child_exits_still_kills_a_surviving_grandchild() {
        // PR #467: by the time this future is
        // dropped, `sh` itself has already exited and been reaped --
        // the drop guard's own pid-based `kill_process_group` would be
        // a no-op against a reaped pid, so it must fall back to the
        // remembered *pgid* directly (`repeat_group_kill`) to still
        // reach the grandchild.
        let token = CancellationToken::new();
        let opts = RunOpts {
            new_session: true,
            ..RunOpts::default()
        };
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8472 & echo hi".to_string(),
        ];
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None));
        // `sh` itself (printing "hi" and exiting) finishes almost at
        // once; this gives it ample time to exit and be reaped by this
        // future's own wait loop before the drop below, while the
        // grandchild it forked keeps sleeping.
        tokio::select! {
            _ = &mut fut => panic!("must not finish: the grandchild still holds the pipe open"),
            () = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
        drop(fut);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !pgrep_matches("sleep 8472"),
            "the drop guard must still kill a grandchild once the tracked \
             child itself has already been reaped"
        );
    }

    /// A scratch directory with *names* (each an empty, `chmod +x`
    /// file) created in it, removed on drop -- `resolve_binary`'s own
    /// absolute-path branch (`is_absolute() && exists()`, no `PATH`
    /// search at all) exercises the exact same ordered "first present
    /// candidate wins" logic the real `awk`/`imagemagick` candidate
    /// lists need, without this test ever mutating the real process-
    /// wide `PATH` env var -- `cargo test`'s own default parallel
    /// harness shares one `PATH` across every test thread, so a test
    /// that mutated it here could flake any other test spawning a real
    /// subprocess at the same moment.
    struct ScratchBinDir {
        dir: std::path::PathBuf,
    }

    impl ScratchBinDir {
        fn with(names: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-tools-resolve-binary-test-{}-{}",
                std::process::id(),
                names.join("-")
            ));
            std::fs::create_dir_all(&dir).unwrap();
            for name in names {
                let path = dir.join(name);
                std::fs::write(&path, "").unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
            }
            ScratchBinDir { dir }
        }

        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for ScratchBinDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn resolve_binary_prefers_an_earlier_present_candidate_in_order() {
        let scratch = ScratchBinDir::with(&["magick", "convert"]);
        let magick = scratch.path("magick");
        let convert = scratch.path("convert");
        assert_eq!(resolve_binary(&[&magick, &convert]), Some(magick.clone()));
    }

    #[test]
    fn resolve_binary_falls_back_to_the_second_candidate_when_the_first_is_absent() {
        let scratch = ScratchBinDir::with(&["convert"]);
        let missing_magick = scratch.path("magick");
        let convert = scratch.path("convert");
        assert_eq!(
            resolve_binary(&[&missing_magick, &convert]),
            Some(convert.clone())
        );
    }
}
