//! `adapters/_agent_cli.py`, ported -- what the `claude_code` and `pi`
//! adapters (issue #520 part B) share on top of
//! `electricity_tools::agent_cli` (part A): translating that module's
//! own [`electricity_tools::agent_cli::AgentCliRunError`]/
//! [`electricity_tools::agent_cli::AgentCliParseError`] into an
//! [`AdapterError`] (B4's own retry table), the shared preflight
//! `check()` (`adapters/_agent_cli.py::check_binary`), and the shared
//! result-building step (`adapters/_agent_cli.py::generate_result`).

use electricity_tools::agent_cli::{AgentCliParseError, AgentCliResult, AgentCliRunError};
use electricity_vm::adapter::{AdapterError, CheckResult, GenerateOptions, GenerateResult, PyExc};
use electricity_vm::retry::RetryInfo;

/// `adapters/_agent_cli.py::agent_cli_call_errors` -- a timeout is the
/// only retryable failure (B4); every other [`AgentCliRunError`] is an
/// [`AdapterError`] carrying `PyExc::AdapterCallError`, except
/// `Decode` (CPython's own `UnicodeDecodeError` escapes Python's
/// `agent_cli_call_errors` context manager untouched, so
/// `classify_exception` never finds it retryable either) and
/// `Cancelled` (unreachable in practice -- every electricity call site
/// already races `generate` against the run's own cancellation and
/// drops the future first, issue #520's settled question 9 -- given
/// electricity's own plain "cancelled" text for the same corner case
/// elsewhere, `subprocess_tool.rs`'s own `ProcError::Cancelled` arm).
pub fn run_error_to_adapter(err: AgentCliRunError) -> AdapterError {
    match err {
        AgentCliRunError::Timeout(message) => AdapterError {
            message,
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        },
        AgentCliRunError::StartFailed(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::AdapterCallError,
        },
        AgentCliRunError::Decode(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::UnicodeDecodeError,
        },
        AgentCliRunError::Cancelled => AdapterError {
            message: "cancelled".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        },
    }
}

/// `parse_claude_output`/`parse_pi_output`'s own failure shape as an
/// [`AdapterError`] -- never retried (B4: a missing answer, an
/// `is_error`/pi error stop, or a non-zero exit all fail the attempt
/// outright, and a `py_int`/`float()` numeric failure carries its own
/// class, never `AdapterCallError`).
pub fn parse_error_to_adapter(err: AgentCliParseError) -> AdapterError {
    match err {
        AgentCliParseError::Message(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::AdapterCallError,
        },
        AgentCliParseError::Numeric(numeric) => AdapterError {
            message: numeric.to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: numeric.py_class,
        },
    }
}

/// `std::io::Error` from [`electricity_tools::agent_cli::TempDirGuard::new`]
/// or the `prompt.md` write (pi only) -- unreachable in any committed
/// case/test (a healthy `TMPDIR` is assumed the same way Python's own
/// `tempfile.TemporaryDirectory()` call is never wrapped in a
/// try/except of its own), kept as `OSError` rather than panicking so
/// an exotic host (a full disk, an unwritable `TMPDIR`) still reports
/// a call failure instead of crashing the whole run.
pub fn io_error_to_adapter(err: std::io::Error) -> AdapterError {
    AdapterError {
        message: err.to_string(),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::OSError,
    }
}

/// `adapters/_agent_cli.py::check_binary` -- `which(binary)`, never
/// `resolve_binary` (issue #520's settled question 10: no `~`
/// expansion, and a non-executable absolute path is correctly reported
/// missing).
pub fn check_binary(adapter_name: &str, binary: &str) -> CheckResult {
    match electricity_tools::process::which(binary) {
        Some(_) => CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        },
        None => CheckResult {
            ok: false,
            missing: vec![format!("binary:{binary}")],
            message: Some(format!(
                "{binary} is not on PATH: install it and log in, or set \
                 runtime.adapters.{adapter_name}.binary."
            )),
        },
    }
}

/// `binary`'s own `check()` when it resolved to a truthy non-string
/// (`crate::config::require_str` failed) -- Python's own
/// `preflight.py::call_check` wording for an exception raised inside
/// `check()` itself (`check() raised {type(exc).__name__}: {exc}`),
/// reproduced directly here rather than through electricity's own
/// generic `check() panicked: ...` convention (`preflight.rs`'s own
/// `call_adapter_check`): this adapter never panics for this case, it
/// already has the exact message `require_str` would raise with in
/// Python, so there is no reason to settle for the panic-catching
/// wording instead.
pub fn check_type_error(err: &AdapterError) -> CheckResult {
    CheckResult {
        ok: false,
        missing: Vec::new(),
        message: Some(format!("check() raised TypeError: {}", err.message)),
    }
}

/// `adapters/_agent_cli.py::generate_result` -- every set
/// [`GenerateOptions`] field is ignored by both adapters (`used:
/// &[]`), since neither ever reads `temperature`/`params`/`messages`/
/// `images`/... at all.
pub fn build_generate_result(
    adapter_name: &str,
    result: AgentCliResult,
    options: &GenerateOptions,
) -> GenerateResult {
    GenerateResult {
        text: result.text,
        raw: result.raw,
        tokens_sent: result.tokens_sent,
        tokens_received: result.tokens_received,
        finish_reason: result.finish_reason,
        warnings: crate::ignored_options_warning(adapter_name, options, &[]),
        cost_usd: result.cost_usd,
    }
}

/// `model or default_model` (B5): a non-empty call-time *model* always
/// wins; otherwise *default_model* resolves through
/// [`crate::config::resolve_truthy_str`] -- falsy (including a falsy
/// non-string) becomes `""` (no `--model` flag), a truthy non-string
/// raises `TypeError`.
pub fn resolve_model(
    call_model: &str,
    default_model: &electricity_value::Value,
) -> Result<String, AdapterError> {
    if !call_model.is_empty() {
        return Ok(call_model.to_string());
    }
    crate::config::resolve_truthy_str(default_model)
}
