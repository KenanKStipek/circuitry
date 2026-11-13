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
/// ToolCall::on_live`] callback (#741) -- the tail/byte-count
/// figures `meta.live` needs; `v`/`started_at`/`updated_at` are the
/// caller's own, assembled once it has these.
pub struct LiveTail {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

/// `meta.live`'s own tail cap (#741): the last [`LIVE_TAIL_MAX_LINES`]
/// lines (a trailing, newline-less line counts as one), never more
/// than [`LIVE_TAIL_MAX_BYTES`] bytes -- the envelope around it
/// (`v`/`started_at`/`updated_at`) is the same shape the agent tool
/// (#636) and streamed prompts (#674) already publish.
const LIVE_TAIL_MAX_LINES: usize = 20;
const LIVE_TAIL_MAX_BYTES: usize = 4096;

/// *text*'s own last *max_bytes* bytes, cut at a `char` boundary (never
/// mid-character) -- the start is dropped, never the end, matching
/// `tail -c`'s own convention. A no-op once *text* is already within
/// the cap. [`tail_text`] only ever calls this on one already-split
/// line at a time, never on several lines joined together, so a cut
/// here is always the spec's own "a single line longer than the cap",
/// never a fragment of some shorter line that happened to sit next to
/// it.
fn cap_to_last_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// *lines* joined with `\n`, dropping whole lines from the front --
/// never a fragment of one -- until the join is itself within
/// *max_bytes* of UTF-8 (`cof`'s own `_join_within_byte_limit`,
/// `subprocess_live.py`). Always terminates within *max_bytes* because
/// [`tail_text`] already caps every individual line to *max_bytes* on
/// its own before calling this, so the newest line alone is always
/// enough to fit, in the worst case.
fn join_within_byte_limit(mut lines: Vec<&str>, max_bytes: usize) -> String {
    loop {
        let joined = lines.join("\n");
        if joined.len() <= max_bytes {
            return joined;
        }
        lines.remove(0);
    }
}

/// How many of *buf*'s own trailing bytes are an incomplete UTF-8
/// sequence -- one that could still become valid if more bytes
/// arrived, never a sequence that's already definitely invalid
/// (`std::str::Utf8Error::error_len() == None` is exactly this case:
/// an incomplete sequence reachable only by running off the end of
/// *buf*). `cof`'s own incremental UTF-8 decoder withholds exactly
/// these bytes rather than replacing them -- a chunk ending in a
/// truncated multi-byte lead byte shows nothing for it yet, waiting
/// for the rest. An earlier, definitely-invalid sequence (one`
/// `error_len()` reports a real length for) is skipped over instead,
/// since the incremental decoder replaces that one immediately and
/// keeps decoding past it -- this keeps scanning from there for a
/// *later* incomplete tail, rather than stopping at the first error
/// found.
fn trailing_incomplete_utf8_len(buf: &[u8]) -> usize {
    let mut offset = 0;
    loop {
        match std::str::from_utf8(&buf[offset..]) {
            Ok(_) => return 0,
            Err(e) => match e.error_len() {
                Some(bad_len) => offset += e.valid_up_to() + bad_len,
                None => return buf.len() - (offset + e.valid_up_to()),
            },
        }
    }
}

/// *buf*'s own trailing raw bytes, decoded the way a tail's own
/// incremental decoder would have them right now -- lossy UTF-8 (never
/// the final output's own strict [`decode_text`]: a tail is a display
/// value, cut at an arbitrary byte boundary, and must never fail a
/// running tool over that), with the same universal-newline
/// translation [`decode_text`] applies to the final output, and two
/// of `cof`'s own incremental decoder's withholding rules applied
/// first so the *moment* a tail resolves a trailing `\r`/split
/// character matches `cof`'s own [`subprocess_live.py`]'s
/// `_TailBuffer` exactly, not just its eventual, fully-resolved text:
/// - an incomplete trailing multi-byte sequence ([`trailing_incomplete_utf8_len`])
///   is excluded entirely, never shown as a replacement character
///   before more bytes confirm what it really is;
/// - a trailing, not-yet-translated `\r` (ambiguous -- the next byte
///   could still complete a `\r\n` pair) is dropped from the decoded
///   text entirely, before translation ever runs, rather than assumed
///   to already be a line end.
///
/// Both are naturally revisited on the next call once more bytes have
/// arrived, since this always re-decodes from the same accumulated
/// buffer `run_tracked` holds, never a stateful partial left over from
/// an earlier call -- so a `\r\n` split across two separate pipe reads
/// still ends up translated as one line end, exactly when `cof`'s own
/// stateful decoder would resolve it too.
fn decode_live(buf: &[u8]) -> String {
    let pending = trailing_incomplete_utf8_len(buf);
    let confirmed = &buf[..buf.len() - pending];
    let mut decoded = String::from_utf8_lossy(confirmed).into_owned();
    if decoded.ends_with('\r') {
        decoded.pop();
    }
    universal_newlines(&decoded)
}

/// *buf*'s own tail, ported exactly from `cof`'s own
/// [`subprocess_live.py`] `_TailBuffer.append`/`_TailBuffer.text`:
/// 1. [`decode_live`], over a raw window generous enough that every
///    line that can possibly survive both caps below is always fully
///    inside it (never the window's own edge landing inside such a
///    line and masquerading as this function's own "single
///    over-long line" case);
/// 2. split on `\n` -- every segment but the last is a complete line
///    (an empty one stays an empty line, never dropped), the last is
///    the current, still-partial line, counted as the tail's own last
///    line, live, the moment it's been read -- but only when it isn't
///    itself empty (`cof`'s own `if self._carry:`): a buffer that ends
///    exactly on a line break never gets a trailing blank line tacked
///    on for it;
/// 3. keep the last [`LIVE_TAIL_MAX_LINES`] of those;
/// 4. cap *each* kept line, independently, to its own last
///    [`LIVE_TAIL_MAX_BYTES`] bytes ([`cap_to_last_bytes`]) -- the
///    only place a single line's own bytes are ever cut, and only
///    ever a suffix of that one line;
/// 5. [`join_within_byte_limit`]: drop whole lines from the front --
///    never a partial one -- until the `\n`-joined result is itself
///    within [`LIVE_TAIL_MAX_BYTES`], with no trailing `\n` ever added
///    beyond what joining the kept lines already produces.
fn tail_text(buf: &[u8]) -> String {
    // 4x the aggregate cap: even in the worst case for both ends of
    // decoding at once -- every one of the up to `LIVE_TAIL_MAX_LINES`
    // lines that could survive step 3 independently near the
    // per-line cap (which the aggregate cap in step 5 alone already
    // limits to at most one or two such lines), *and* `\r\n` shrinking
    // 2:1 on translation (its own worst case) -- the true raw bytes
    // behind that surviving content still fit in half of this window,
    // leaving a window's width of margin to find the oldest surviving
    // line's own start before this function ever reaches the window's
    // own edge.
    const RAW_WINDOW_BYTES: usize = LIVE_TAIL_MAX_BYTES * 4;
    let window = &buf[buf.len().saturating_sub(RAW_WINDOW_BYTES)..];
    let decoded = decode_live(window);

    let mut segments: Vec<&str> = decoded.split('\n').collect();
    // `split` on a non-empty pattern always yields at least one
    // element, so this is only ever `None` for a `decoded` that can't
    // exist (`split` on `""` still gives `[""]`) -- `unwrap_or("")`
    // is just the honest fallback for that impossible case, not a
    // real one this hits.
    let carry = segments.pop().unwrap_or("");
    let mut lines: Vec<&str> = segments;
    if !carry.is_empty() {
        lines.push(carry);
    }
    if lines.len() > LIVE_TAIL_MAX_LINES {
        lines.drain(0..lines.len() - LIVE_TAIL_MAX_LINES);
    }
    let capped: Vec<&str> = lines
        .into_iter()
        .map(|line| cap_to_last_bytes(line, LIVE_TAIL_MAX_BYTES))
        .collect();
    join_within_byte_limit(capped, LIVE_TAIL_MAX_BYTES)
}

fn live_tail(stdout_buf: &[u8], stderr_buf: &[u8]) -> LiveTail {
    LiveTail {
        stdout_tail: tail_text(stdout_buf),
        stderr_tail: tail_text(stderr_buf),
        stdout_bytes: stdout_buf.len() as u64,
        stderr_bytes: stderr_buf.len() as u64,
    }
}

/// `on_live`'s own cadence (#741, `exec/prompt.rs`'s own
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

/// [`run_tracked`]'s own *on_line* feed: emits every complete `\n`-
/// terminated line in `buf[*line_start..]`, lossily decoded with a
/// trailing `\r` trimmed (CRLF), advancing `*line_start` past each one
/// emitted this way.
fn emit_new_lines(buf: &[u8], line_start: &mut usize, on_line: &dyn Fn(&str)) {
    while let Some(relative) = buf[*line_start..].iter().position(|byte| *byte == b'\n') {
        let end = *line_start + relative;
        let line = String::from_utf8_lossy(&buf[*line_start..end]);
        let line = line.strip_suffix('\r').unwrap_or(&line);
        on_line(line);
        *line_start = end + 1;
    }
}

/// [`run_tracked`]'s own *on_line* feed, called once after reading
/// stops: whatever is left in `buf[line_start..]` with no final `\n` is
/// still one more line, the same way Python's own line iteration yields
/// a file's last, newline-less line.
fn emit_trailing_line(buf: &[u8], line_start: usize, on_line: &dyn Fn(&str)) {
    if line_start >= buf.len() {
        return;
    }
    let line = String::from_utf8_lossy(&buf[line_start..]);
    let line = line.strip_suffix('\r').unwrap_or(&line);
    if !line.is_empty() {
        on_line(line);
    }
}

/// `core/cancellation.py::run_tracked` -- process-group spawn, the
/// stdin-writer task, universal-newline/locale decoding, and the
/// token-tracked wait/kill loop. *on_live*, when given, is called with
/// this run's current output tails/byte counts while it runs, already
/// throttled to [`live_due`]'s own cadence (#741) -- `None` for
/// every caller that never publishes `meta.live` (the agent CLIs,
/// `http`).
///
/// *on_line*, when given, is called once for each complete `\n`-
/// terminated line of stdout as it arrives (`core/cancellation.py::
/// _read_lines_promptly`'s own per-line callback, ported for the
/// `agent` tool's live summary, #636) -- a trailing line with no
/// final newline is still flushed once the pipe reaches EOF. Decoded
/// lossily (`String::from_utf8_lossy`, with a trailing `\r` trimmed)
/// rather than through [`decode_text`]'s own strict decode: this feed
/// is only ever a best-effort, intra-run approximation a caller
/// resynchronizes at its own boundaries (`agent_live.rs`'s own turn
/// sync), never the authoritative parse of the run's output, which
/// still goes through this function's own strict, whole-buffer decode
/// once the run ends.
pub async fn run_tracked(
    cmd: &[String],
    opts: &RunOpts,
    token: &CancellationToken,
    on_live: Option<&dyn Fn(LiveTail)>,
    on_line: Option<&dyn Fn(&str)>,
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
    // only handed back at EOF would need (#741: `on_live` needs
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
    // (#741) -- unused, and left at its initial `None`/`false`,
    // for every call `on_live` is `None` for.
    let mut live_published_at: Option<std::time::Instant> = None;
    let mut live_pending = false;
    // [`on_line`]'s own cursor into `stdout_buf`: the byte offset of the
    // first not-yet-emitted line.
    let mut line_start: usize = 0;

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
                        if let Some(on_line) = on_line {
                            emit_new_lines(&stdout_buf, &mut line_start, on_line);
                        }
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
                // This loop's own trailing publish (#741): a
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

    // A trailing line with no final newline (the common case: the last
    // line of a JSON-lines stream written without one) is still flushed
    // once reading stops, whether that's a normal EOF or a timeout --
    // `core/cancellation.py::_read_lines_promptly`'s own `for raw_line in
    // stdout:` yields it the same way.
    if let Some(on_line) = on_line {
        emit_trailing_line(&stdout_buf, line_start, on_line);
    }

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
    struct TailBufferSnapshot {
        text: String,
        total_bytes: u64,
    }

    #[derive(serde::Deserialize)]
    struct TailBufferCase {
        name: String,
        chunks_hex: Vec<String>,
        snapshots: Vec<TailBufferSnapshot>,
    }

    #[derive(serde::Deserialize)]
    struct ProcessToolsCorpus {
        utf8_decode_cases: Vec<Utf8DecodeCase>,
        tail_buffer_cases: Vec<TailBufferCase>,
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

    /// Replays every chunk sequence in `process_tools_corpus.json`'s
    /// own `tail_buffer_cases` byte for byte -- generated from `cof`'s
    /// own real `subprocess_live._TailBuffer`, chunk by chunk, so this
    /// checks [`tail_text`] against its exact, stateful incremental
    /// decoder behavior (a `\r\n`/lone `\r` split across chunks, an
    /// incomplete multi-byte sequence at a chunk's own end, ...), not
    /// only the eventual, fully-resolved text once every chunk has
    /// landed. [`run_tracked`]'s own buffer only ever grows (never
    /// discards bytes already read), so accumulating every chunk here
    /// and calling [`tail_text`] on the growing whole after each one is
    /// the exact same input its own on_live callback ever sees.
    #[test]
    fn tail_text_matches_cofs_own_tail_buffer_chunk_by_chunk() {
        let corpus: ProcessToolsCorpus =
            serde_json::from_str(include_str!("../tests/golden/process_tools_corpus.json"))
                .expect("process_tools_corpus.json must parse");
        assert!(
            !corpus.tail_buffer_cases.is_empty(),
            "corpus must carry at least one tail_buffer_cases entry"
        );
        for case in corpus.tail_buffer_cases {
            assert_eq!(
                case.chunks_hex.len(),
                case.snapshots.len(),
                "case {}: one snapshot expected per chunk",
                case.name
            );
            let mut accumulated: Vec<u8> = Vec::new();
            for (chunk_hex, snapshot) in case.chunks_hex.iter().zip(case.snapshots.iter()) {
                accumulated.extend(hex_decode(chunk_hex));
                assert_eq!(
                    tail_text(&accumulated),
                    snapshot.text,
                    "case {}: text() after chunk ending at byte {}",
                    case.name,
                    accumulated.len()
                );
                assert_eq!(
                    accumulated.len() as u64,
                    snapshot.total_bytes,
                    "case {}: total_bytes after a chunk",
                    case.name
                );
            }
        }
    }

    #[test]
    fn tail_text_keeps_the_last_twenty_lines() {
        let mut buf = String::new();
        for i in 0..25 {
            buf.push_str(&format!("line{i}\n"));
        }
        let text = tail_text(buf.as_bytes());
        let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[0], "line5");
        assert_eq!(lines[19], "line24");
    }

    #[test]
    fn tail_text_shows_the_current_partial_line_live() {
        // The spec's own example (`printf "working\u{2026}"; sleep 60`):
        // no line end has arrived at all yet, but the partial line
        // must still show, immediately, exactly as read so far.
        assert_eq!(tail_text("working\u{2026}".as_bytes()), "working\u{2026}");
    }

    #[test]
    fn tail_text_caps_a_single_over_long_line_to_its_last_bytes_from_a_char_boundary() {
        // One 10,000-byte line, no newline at all -- a single partial
        // line, well under the 20-line cap, so the 4 KiB byte cap is
        // the only thing trimming it, to exactly its own last 4096
        // bytes (ASCII here, so every byte offset is already a char
        // boundary).
        let buf = vec![b'a'; 10_000];
        let text = tail_text(&buf);
        assert_eq!(text.len(), LIVE_TAIL_MAX_BYTES);
        assert!(text.chars().all(|c| c == 'a'));
    }

    #[test]
    fn tail_text_drops_whole_lines_from_the_front_to_stay_within_the_byte_cap() {
        // Ten 1000-byte lines -- each one, and the 20-line cap by
        // count, nowhere near their own limit on their own, so only
        // the aggregate byte cap (10,010 bytes joined) is trimming
        // this, one whole line at a time from the front, down to the
        // last four (1001 bytes apiece with their own separator,
        // 4004 <= 4096; a fifth would push it to 5005).
        fn line(i: usize) -> String {
            format!("{i:04}{}", "a".repeat(996))
        }
        let buf: String = (0..10).map(|i| format!("{}\n", line(i))).collect();
        let text = tail_text(buf.as_bytes());
        assert!(
            text.len() <= LIVE_TAIL_MAX_BYTES,
            "got {} bytes",
            text.len()
        );
        let lines: Vec<&str> = text.split('\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec![line(6), line(7), line(8), line(9)]);
    }

    #[test]
    fn tail_text_never_keeps_a_fragment_of_a_dropped_line() {
        // Two 3000-byte lines, each well under the per-line cap on its
        // own, whose combined 6001 bytes (with the one separator) is
        // over the aggregate cap. A flat byte-suffix cut over the
        // whole joined text (the bug this replaces) would land 1905
        // bytes into the *first* line, keeping its own back half; the
        // first line must instead be dropped whole, leaving only the
        // second line's own text, not one `a` byte surviving.
        let first = "a".repeat(3000);
        let second = "b".repeat(3000);
        let buf = format!("{first}\n{second}");
        let text = tail_text(buf.as_bytes());
        assert_eq!(text, second);
    }

    #[test]
    fn tail_text_replaces_invalid_utf8_with_the_replacement_character() {
        let buf = [b'a', 0xff, b'b'];
        assert_eq!(tail_text(&buf), "a\u{fffd}b");
    }

    #[test]
    fn tail_text_translates_crlf_and_a_lone_cr_the_same_way_the_final_output_does() {
        // ffmpeg's own stderr progress is `\r`-separated, with no `\n`
        // at all; splitting lines on bare `\n` alone would see it as
        // one giant line unless this translation runs first.
        assert_eq!(tail_text(b"line1\rline2\rline3"), "line1\nline2\nline3");
        assert_eq!(tail_text(b"a\r\nb\r\nc"), "a\nb\nc");
    }

    #[test]
    fn tail_text_stays_within_the_byte_cap_even_when_every_byte_is_invalid() {
        // Every byte here is its own invalid UTF-8 lead byte, so lossy
        // decoding alone would expand this window to 3x its own size
        // (one 3-byte replacement character per byte) -- well past
        // `LIVE_TAIL_MAX_BYTES` unless the byte cap is re-applied after
        // that expansion, not only before it.
        let buf = vec![0xffu8; LIVE_TAIL_MAX_BYTES * 2];
        let text = tail_text(&buf);
        assert!(
            text.len() <= LIVE_TAIL_MAX_BYTES,
            "got {} bytes",
            text.len()
        );
        assert!(text.chars().all(|c| c == '\u{fffd}'));
    }

    #[test]
    fn tail_text_cuts_valid_multibyte_text_cleanly_at_the_byte_cap() {
        // 3-byte CJK characters whose own byte length never divides
        // evenly into `LIVE_TAIL_MAX_BYTES`, so the single-line byte
        // cap's own start (`LIVE_TAIL_MAX_BYTES` bytes back from the
        // end) lands mid-character; it must walk forward to the next
        // real char boundary rather than keep a split character's
        // trailing bytes as-is.
        let buf: Vec<u8> = "\u{6587}".repeat(2000).into_bytes();
        let text = tail_text(&buf);
        assert!(
            text.len() <= LIVE_TAIL_MAX_BYTES,
            "got {} bytes",
            text.len()
        );
        assert!(text.chars().all(|c| c == '\u{6587}'), "got {text:?}");
    }

    #[test]
    fn tail_text_treats_a_crlf_split_across_two_pipe_reads_as_one_line_end() {
        // Simulates a `\r\n` arriving split across two separate pipe
        // reads -- the first read's own chunk ends in `\r`, the next
        // one starts with `\n`. Both chunks land in the same
        // accumulated buffer `run_tracked` holds before this ever
        // runs, so there is no separate incremental-decoder state to
        // get this wrong; this pins that the whole-buffer re-decode on
        // every call still sees the join as one line end, not two.
        let mut buf = b"line1\r".to_vec();
        buf.extend_from_slice(b"\nline2");
        assert_eq!(tail_text(&buf), "line1\nline2");
    }

    #[test]
    fn tail_text_never_adds_a_trailing_newline_cof_never_would() {
        // `cof`'s own `_TailBuffer.text()` only ever joins complete
        // lines (plus a non-empty carry) with `\n` *between* them --
        // never a trailing one tacked on because the raw buffer
        // happened to end on a line break. A single complete line
        // shows as just that line, with nothing after it.
        assert_eq!(tail_text(b"line1\n"), "line1");
        // Two complete lines, no final line break at all (the second
        // is still *partial*, counted as the tail's own live last
        // line) -- still never a trailing `\n`.
        assert_eq!(tail_text(b"line1\nline3"), "line1\nline3");
    }

    #[test]
    fn tail_text_keeps_an_empty_line_as_an_empty_line() {
        // A blank line between two others is a genuine complete line
        // of its own (empty), never dropped or collapsed.
        assert_eq!(tail_text(b"a\n\nb"), "a\n\nb");
    }

    #[test]
    fn tail_text_withholds_a_lone_trailing_cr_until_it_is_resolved() {
        // A `\r` with nothing after it yet is ambiguous -- it might
        // still turn into `\r\n` once the next byte arrives -- so
        // `cof`'s own incremental decoder shows nothing for it at all
        // until that's resolved, not an early `\n`. Once a *non*-`\n`
        // byte follows, the withheld `\r` resolves to its own `\n`,
        // prepended ahead of that byte.
        assert_eq!(tail_text(b"line1\r"), "line1");
        assert_eq!(tail_text(b"line1\rX"), "line1\nX");
    }

    #[test]
    fn tail_text_withholds_an_incomplete_trailing_multibyte_sequence() {
        // The lead byte of a 2-byte UTF-8 sequence, with the
        // continuation byte that would complete it not arrived yet --
        // `cof`'s own incremental decoder shows nothing for it, never
        // an early replacement character, until more bytes confirm
        // what it really is (or never resolves at all, if none ever
        // do).
        assert_eq!(tail_text(b"a\xc3"), "a");
        // An unconditionally invalid byte (never a valid lead byte for
        // anything) is a different case entirely -- replaced at once,
        // never withheld.
        assert_eq!(tail_text(b"a\xff"), "a\u{fffd}");
    }

    #[tokio::test]
    async fn run_tracked_calls_on_live_with_growing_separate_tails_and_a_trailing_flush() {
        // Each `printf` is unbuffered, and separated from the next by
        // a sleep well past the main loop's own 200ms poll tick, so
        // the stdout write is always read, and published, on its own
        // before the stderr write happens at all -- `calls[0]` is
        // pinned to stdout alone regardless of which pipe `select!`
        // happens to poll first on a loaded machine. The child then
        // sleeps past the 1 s throttle window with nothing further
        // written, so the *second* line (stderr) is only ever picked
        // up by this loop's own trailing-publish tick, never a third,
        // immediate publish of its own.
        let token = CancellationToken::new();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf out1; sleep 0.3; printf out2 1>&2; sleep 1.5".to_string(),
        ];
        let calls: Rc<RefCell<Vec<LiveTail>>> = Rc::new(RefCell::new(Vec::new()));
        let calls_for_callback = Rc::clone(&calls);
        let on_live = move |tail: LiveTail| {
            calls_for_callback.borrow_mut().push(tail);
        };
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, Some(&on_live), None)
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
    async fn run_tracked_on_line_gets_each_complete_line_and_the_trailing_one_with_no_newline() {
        let token = CancellationToken::new();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'one\\ntwo\\n'; printf three".to_string(),
        ];
        let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let lines_for_callback = Rc::clone(&lines);
        let on_line = move |line: &str| {
            lines_for_callback.borrow_mut().push(line.to_string());
        };
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, None, Some(&on_line))
            .await
            .unwrap();
        assert_eq!(completed.stdout, "one\ntwo\nthree");
        assert_eq!(
            lines.borrow().as_slice(),
            ["one".to_string(), "two".to_string(), "three".to_string()]
        );
    }

    #[tokio::test]
    async fn run_tracked_on_line_never_calls_back_for_an_output_with_no_trailing_content() {
        let token = CancellationToken::new();
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'one\\n'".to_string(),
        ];
        let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let lines_for_callback = Rc::clone(&lines);
        let on_line = move |line: &str| {
            lines_for_callback.borrow_mut().push(line.to_string());
        };
        run_tracked(&cmd, &RunOpts::default(), &token, None, Some(&on_line))
            .await
            .unwrap();
        assert_eq!(lines.borrow().as_slice(), ["one".to_string()]);
    }

    #[tokio::test]
    async fn run_tracked_never_calls_on_live_when_it_is_none() {
        // Every caller `on_live` is `None` for (the agent CLIs, `http`)
        // must cost this loop nothing beyond the `if let Some` checks
        // already there -- this just pins that passing `None` through
        // a real run never panics on an absent callback.
        let token = CancellationToken::new();
        let cmd = vec!["echo".to_string(), "hi".to_string()];
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, None, None)
            .await
            .unwrap();
        assert_eq!(completed.stdout, "hi\n");
    }

    #[tokio::test]
    async fn run_tracked_refuses_to_start_once_cancellation_is_set() {
        let token = CancellationToken::new();
        token.request(2);
        let err = run_tracked(
            &["true".to_string()],
            &RunOpts::default(),
            &token,
            None,
            None,
        )
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
        let completed = run_tracked(&cmd, &RunOpts::default(), &token, None, None)
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
        let err = run_tracked(&cmd, &RunOpts::default(), &token, None, None)
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
        let err = run_tracked(&cmd, &opts, &token, None, None)
            .await
            .unwrap_err();
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
        let completed = run_tracked(&cmd, &opts, &token, None, None).await.unwrap();
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
        let err = run_tracked(&cmd, &opts, &token, None, None)
            .await
            .unwrap_err();
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
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None, None));
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
        let completed = run_tracked(&cmd, &opts, &token, None, None).await.unwrap();
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
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None, None));
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
        let completed = run_tracked(&cmd, &opts, &token, None, None).await.unwrap();
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
        let err = run_tracked(&cmd, &opts, &token, None, None)
            .await
            .unwrap_err();
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
        let err = run_tracked(&cmd, &opts, &token, None, None)
            .await
            .unwrap_err();
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
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token, None, None));
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
