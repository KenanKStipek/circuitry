//! `core/cancellation.py::run_tracked` and `kill_process_group`, ported
//! as a signature-only seam (issue #449's gate lane, item 3) -- lane C
//! fills every body in this module; the shapes below are final.
//!
//! `run_tracked` is `subprocess.run`, tracked by this run's own
//! [`CancellationToken`] for the whole time the child can block: the
//! token kills every tracked process group at once on `request()`, and
//! a per-call *timeout* still applies independently.

use electricity_value::CancellationToken;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

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
            } => write!(f, "{binary:?} exceeded timeout of {timeout_seconds}s"),
            ProcError::Cancelled => write!(f, "cancelled"),
            ProcError::Decode(message) => write!(f, "{message}"),
            ProcError::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProcError {}

/// `core/cancellation.py::run_tracked` -- signature only; lane C fills
/// the body (process-group spawn, the stdin-writer task, universal-
/// newline/locale decoding, and the token-tracked wait/kill loop).
pub async fn run_tracked(
    _cmd: &[String],
    _opts: &RunOpts,
    _token: &CancellationToken,
) -> Result<Completed, ProcError> {
    Err(ProcError::Other(
        "electricity_tools::process::run_tracked is not implemented yet (lane C)".to_string(),
    ))
}

/// `core/cancellation.py::kill_process_group` -- best-effort `SIGKILL`
/// of *pid*'s whole process group; a no-op once the group has already
/// exited. Signature only; lane C fills the body (POSIX only, matching
/// the Python reference -- Windows gets no process-group isolation).
pub fn kill_process_group(_pid: u32) {}

/// `plugins/_subprocess.py::resolve_binary` -- signature only; lane C
/// fills the body (PATH lookup, a configured absolute/relative
/// override, the exact `binary not found: ...` message).
pub fn resolve_binary(_binary: &str, _search_path: Option<&Path>) -> Result<String, ProcError> {
    Err(ProcError::Other(
        "electricity_tools::process::resolve_binary is not implemented yet (lane C)".to_string(),
    ))
}
