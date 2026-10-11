//! `core/cancellation.py::run_tracked` and `kill_process_group`, ported
//! as a signature-only seam (issue #449's gate lane, item 3) -- lane C
//! fills every body in this module; the shapes below are final.
//!
//! `run_tracked` is `subprocess.run`, tracked by this run's own
//! [`CancellationToken`] for the whole time the child can block: the
//! token kills every tracked process group at once on `request()`, and
//! a per-call *timeout* still applies independently.

use electricity_value::{CancellationToken, Value};
use std::collections::HashMap;
use std::io;
use std::process::Stdio;
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
    Other(String),
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
            ProcError::Other(message) => write!(f, "{message}"),
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
struct KillGroupOnDrop(Option<u32>);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_process_group(pid);
        }
    }
}

/// `core/cancellation.py::run_tracked` -- process-group spawn, the
/// stdin-writer task, universal-newline/locale decoding, and the
/// token-tracked wait/kill loop.
pub async fn run_tracked(
    cmd: &[String],
    opts: &RunOpts,
    token: &CancellationToken,
) -> Result<Completed, ProcError> {
    // `get_token().check()` before `Popen` -- refuse to even start a new
    // child once cancellation has already been requested.
    if token.is_set() {
        return Err(ProcError::Cancelled);
    }
    let Some(program) = cmd.first() else {
        return Err(ProcError::Other(
            "run_tracked called with an empty cmd".to_string(),
        ));
    };

    let mut command = Command::new(program);
    command.args(&cmd[1..]);
    if let Some(cwd) = &opts.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &opts.env {
        command.env_clear();
        command.envs(env);
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
    if opts.new_session {
        // A new process group (not a full `setsid()` session -- this
        // crate's own kill path only ever needs group isolation for
        // `kill_process_group`'s own `killpg`, never controlling-
        // terminal detachment) only while armed/`new_session`, mirroring
        // `start_new_session=(token.armed or new_session)`.
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ProcError::NotFound(format!("{program}: {err}")));
        }
        Err(err) => {
            return Err(ProcError::Other(format!(
                "failed to start {program}: {err}"
            )));
        }
    };

    let pid = child.id();
    // Lives for the rest of this function -- its own `Drop` is this
    // call's safety net against being torn down mid-await from outside
    // (see the struct's own doc comment above).
    let _kill_on_drop = KillGroupOnDrop(pid);

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
        return Err(ProcError::Other("child has no stdout pipe".to_string()));
    };
    let Some(mut stderr_pipe) = child.stderr.take() else {
        return Err(ProcError::Other("child has no stderr pipe".to_string()));
    };
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf).await;
        buf
    });

    let deadline = opts
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);

    let status = loop {
        let remaining =
            deadline.map(|dl| dl.saturating_duration_since(tokio::time::Instant::now()));
        if let Some(remaining) = remaining {
            if remaining.is_zero() {
                // The whole call's own wall-clock deadline, not a
                // per-slice one -- kill the group, wait, then one more
                // group kill (`_repeat_group_kill`) for a grandchild
                // forked at the instant of the first kill.
                if let Some(pid) = pid {
                    kill_process_group(pid);
                }
                let _ = child.wait().await;
                if let Some(pid) = pid {
                    kill_process_group(pid);
                }
                stdout_task.abort();
                stderr_task.abort();
                let timeout_seconds = opts.timeout.map(|t| t.as_secs()).unwrap_or(0);
                return Err(ProcError::Timeout {
                    binary: program.clone(),
                    timeout_seconds,
                });
            }
        }
        let sleep_for = remaining
            .map(|r| r.min(POLL_INTERVAL))
            .unwrap_or(POLL_INTERVAL);
        tokio::select! {
            status = child.wait() => break status,
            () = tokio::time::sleep(sleep_for) => {
                // While cancellation is set, keep repeating the group
                // kill until the pipes reach EOF -- `_repeat_group_kill`
                // closes the race window a fork right at the first
                // `kill_process_group` call can slip through.
                if token.is_set() {
                    if let Some(pid) = pid {
                        kill_process_group(pid);
                    }
                }
            }
        }
    };

    let status = status.map_err(|err| ProcError::Other(format!("wait failed: {err}")))?;
    let stdout_bytes = stdout_task.await.unwrap_or_default();
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let stdout = decode_text(&stdout_bytes)?;
    let stderr = decode_text(&stderr_bytes)?;

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
                // with it, so kill just the child (#357 review N1).
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

/// `plugins/_subprocess.py::resolve_binary` -- the first of *candidates*
/// found on `PATH` (or an absolute path that exists), else `None`;
/// never an error -- Python's own version takes a list because a
/// plugin like `imagemagick` ships under several names (`magick`/
/// `convert`), tried in order (review P2-6 on PR #454's second
/// review: the gate's own first signature took one binary and
/// returned `Result`, matching neither shape).
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

/// `shutil.which`'s own POSIX semantics, as far as [`resolve_binary`]
/// needs them: a name with a directory component is checked directly
/// (no `PATH` search); otherwise every `PATH` entry is tried in order,
/// requiring an existing, executable, non-directory file.
fn which(name: &str) -> Option<String> {
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
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_display_quotes_the_binary_like_pythons_own_repr() {
        // Review finding/P2-d on PR #454: `plugins/_subprocess.py`'s
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

    #[tokio::test]
    async fn run_tracked_refuses_to_start_once_cancellation_is_set() {
        let token = CancellationToken::new();
        token.request(2);
        let err = run_tracked(&["true".to_string()], &RunOpts::default(), &token)
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
        let completed = run_tracked(&cmd, &RunOpts::default(), &token)
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
        let err = run_tracked(&cmd, &RunOpts::default(), &token)
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
        let err = run_tracked(&cmd, &opts, &token).await.unwrap_err();
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
        let completed = run_tracked(&cmd, &opts, &token).await.unwrap();
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
        let err = run_tracked(&cmd, &opts, &token).await.unwrap_err();
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
        let mut fut = Box::pin(run_tracked(&cmd, &opts, &token));
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
        let completed = run_tracked(&cmd, &opts, &token).await.unwrap();
        assert_eq!(completed.exit_code, 0);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not wait anywhere near the full 10s deadline"
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
