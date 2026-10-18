//! `agent_cli.py`, ported (issue #520, part A): the four steps shared by
//! the `claude_code` and `pi` adapters (part B, a later lane) and by any
//! future `agent` tool port --
//!
//! 1. [`env_removal_names`]/[`child_env`] -- which environment variables
//!    a coding-agent CLI must never inherit.
//! 2. [`pi_command`]/[`claude_command`] -- the argv for one headless call.
//! 3. [`run_agent_cli`] -- runs it through [`crate::process::run_tracked`],
//!    in its own process group, translating a timeout or a start failure
//!    into the Python text an adapter reports.
//! 4. [`parse_pi_output`]/[`parse_claude_output`] -- the CLI's JSON output
//!    as an [`AgentCliResult`], or the Python error text on failure.
//!
//! Cancellation is not this module's concern (issue #520's settled
//! question 9): every electricity call site already races its own future
//! against `token.cancelled()`, and dropping that future kills the
//! process group through `process::run_tracked`'s own drop guard. This
//! module never checks cancellation after the fact the way
//! `agent_cli.py::run_agent_cli`'s own `get_token().check()` does.
//!
//! Two narrow gaps against `agent_cli.py`, both unreachable through
//! part B's `u32` timeout and a freshly created temporary `cwd` (so no
//! case or test pins either):
//!
//! - **The start-failure text names `cmd[0]`, never the `cwd`.** CPython's
//!   `_posixsubprocess` reports the working directory instead of the
//!   executable when *that* is what doesn't exist (a `cwd` deleted or
//!   never created between a build and a run). This module always names
//!   `cmd[0]`, matching Python only when the executable itself is the
//!   missing path.
//! - **`float()`'s `OverflowError` on an astronomically large cost
//!   integer isn't reproduced.** [`UsageTotals::add`]'s cost sum stays an
//!   infallible `f64` add; Python's `float()` on a huge `int` can raise.
//!   No real CLI-reported cost reaches this.

use crate::process::{self, ProcError, RunOpts};
use electricity_value::pycompat::{PyIntError, py_int, py_splitlines, py_strip};
use electricity_value::{CancellationToken, Dict, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// `agent_cli.py::PARENT_SESSION_ENV` -- always removed from the child.
pub const PARENT_SESSION_ENV: &[&str] = &[
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "PI_PANE_ARGS",
    "PI_CODING_AGENT",
    "PI_MODEL",
    "PI_PROVIDER",
    "PI_REASONING_LEVEL",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
];

/// `agent_cli.py::DEFAULT_UNSET_ENV` -- the default `unset_env`.
pub const DEFAULT_UNSET_ENV: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

/// `agent_cli.py::PI_ATTACHED_PROMPT_INSTRUCTION`.
pub const PI_ATTACHED_PROMPT_INSTRUCTION: &str = "The attached file is the prompt. Reply to it as if its contents had been sent as this message.";

/// `agent_cli.py::CLAUDE_SCHEMA_VALUE_KEY`.
pub const CLAUDE_SCHEMA_VALUE_KEY: &str = "value";

/// `agent_cli.py::_SCHEMA_ROOT_KEYWORDS`.
const SCHEMA_ROOT_KEYWORDS: &[&str] = &["$defs", "definitions"];

/// `agent_cli.py::_STDERR_TAIL_CHARS`.
const STDERR_TAIL_CHARS: usize = 2000;

/// `agent_cli.py::_PI_ERROR_STOP_REASONS`.
const PI_ERROR_STOP_REASONS: &[&str] = &["error", "aborted"];

/// `agent_cli.py::AgentCliResult`, field for field.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCliResult {
    pub text: String,
    pub raw: Value,
    pub session_id: Option<String>,
    pub tokens_sent: Option<i64>,
    pub tokens_received: Option<i64>,
    pub cost_usd: Option<f64>,
    pub finish_reason: Option<String>,
    pub structured_output: Option<Value>,
}

/// [`run_agent_cli`]'s own failure shape -- the Python text `agent_cli.py`
/// reports for a timeout or a start failure, with
/// [`crate::process::ProcError::Decode`]/[`crate::process::ProcError::Cancelled`]
/// passed through unchanged (issue #520 part B's own retry table: a
/// decode failure and a cancellation are never wrapped as a CLI failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCliRunError {
    /// `{name} did not finish within {timeout:g}s; it was stopped with
    /// everything it started.` (`agent_cli.py:258-262`).
    Timeout(String),
    /// `{name} could not be started ({cmd[0]}): {exc}` (`agent_cli.py:263-264`).
    StartFailed(String),
    /// `ProcError::Decode`'s own `UnicodeDecodeError` text, unchanged.
    Decode(String),
    /// `ProcError::Cancelled`, unchanged.
    Cancelled,
}

impl fmt::Display for AgentCliRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentCliRunError::Timeout(message)
            | AgentCliRunError::StartFailed(message)
            | AgentCliRunError::Decode(message) => write!(f, "{message}"),
            AgentCliRunError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for AgentCliRunError {}

/// [`parse_claude_output`]/[`parse_pi_output`]'s own failure shape -- most
/// branches raise a plain `AgentCliError(message)` in Python
/// ([`AgentCliParseError::Message`]); summing a `NaN`/infinite usage value
/// through `int()` raises `ValueError`/`OverflowError` instead
/// ([`AgentCliParseError::Numeric`], carrying that exact class and text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCliParseError {
    Message(String),
    Numeric(PyIntError),
}

impl fmt::Display for AgentCliParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentCliParseError::Message(message) => write!(f, "{message}"),
            AgentCliParseError::Numeric(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AgentCliParseError {}

/// `agent_cli.py::child_env`'s own removal set: [`PARENT_SESSION_ENV`]
/// plus `unset_env`, deduplicated. [`run_agent_cli`] passes this as
/// [`RunOpts::env_remove`]; [`child_env`] below exists separately so a
/// caller that wants the filtered dict itself (as `agent_cli.py` returns
/// it, and as this module's own corpus/unit tests check it) can get one
/// without spawning anything.
pub fn env_removal_names(unset_env: &[String]) -> Vec<String> {
    let mut names: Vec<String> = PARENT_SESSION_ENV.iter().map(|s| s.to_string()).collect();
    for name in unset_env {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.clone());
        }
    }
    names
}

/// `agent_cli.py::child_env` -- `base` without [`PARENT_SESSION_ENV`] and
/// without `unset_env`; every other variable is kept.
pub fn child_env(unset_env: &[String], base: &HashMap<String, String>) -> HashMap<String, String> {
    let removed = env_removal_names(unset_env);
    base.iter()
        .filter(|(key, _)| !removed.iter().any(|name| name == *key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// `agent_cli.py::pi_command`'s own keyword arguments beyond
/// `binary`/`prompt_file`, each defaulted the same way.
#[derive(Debug, Clone)]
pub struct PiCommandOpts {
    pub model: String,
    pub thinking: String,
    pub tools: bool,
    pub session_id: Option<String>,
    pub persist_session: bool,
    pub extra_args: Vec<String>,
    pub instruction: String,
}

impl Default for PiCommandOpts {
    fn default() -> Self {
        PiCommandOpts {
            model: String::new(),
            thinking: String::new(),
            tools: false,
            session_id: None,
            persist_session: false,
            extra_args: Vec::new(),
            instruction: PI_ATTACHED_PROMPT_INSTRUCTION.to_string(),
        }
    }
}

/// `agent_cli.py::pi_command`.
pub fn pi_command(binary: &str, prompt_file: &str, opts: &PiCommandOpts) -> Vec<String> {
    let mut cmd = vec![
        binary.to_string(),
        "-p".to_string(),
        "--mode".to_string(),
        "json".to_string(),
        "--no-approve".to_string(),
    ];
    if !opts.tools {
        cmd.push("--no-tools".to_string());
    }
    let session_truthy = opts.session_id.as_deref().is_some_and(|id| !id.is_empty());
    if session_truthy {
        cmd.push("--session".to_string());
        cmd.push(opts.session_id.clone().unwrap());
    } else if !opts.persist_session {
        cmd.push("--no-session".to_string());
    }
    if !opts.model.is_empty() {
        cmd.push("--model".to_string());
        cmd.push(opts.model.clone());
    }
    if !opts.thinking.is_empty() {
        cmd.push("--thinking".to_string());
        cmd.push(opts.thinking.clone());
    }
    cmd.extend(opts.extra_args.iter().cloned());
    cmd.push(format!("@{prompt_file}"));
    cmd.push(opts.instruction.clone());
    cmd
}

/// `agent_cli.py::claude_command`'s own keyword arguments beyond `binary`.
#[derive(Debug, Clone)]
pub struct ClaudeCommandOpts {
    pub model: String,
    pub json_schema: Option<Value>,
    pub tools: bool,
    pub session_id: Option<String>,
    pub persist_session: bool,
    pub extra_args: Vec<String>,
    pub strict_mcp: bool,
}

impl Default for ClaudeCommandOpts {
    fn default() -> Self {
        ClaudeCommandOpts {
            model: String::new(),
            json_schema: None,
            tools: false,
            session_id: None,
            persist_session: false,
            extra_args: Vec::new(),
            strict_mcp: true,
        }
    }
}

/// `agent_cli.py::claude_command`. Fails only if `--json-schema`'s own
/// `json.dumps(claude_json_schema(json_schema))` can't be written
/// (`electricity_json::WriteError` -- unreachable for a schema that
/// itself came from decoded JSON, since dates/bytes can't appear in it).
pub fn claude_command(
    binary: &str,
    opts: &ClaudeCommandOpts,
) -> Result<Vec<String>, electricity_json::WriteError> {
    let mut cmd = vec![
        binary.to_string(),
        "-p".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    if !opts.tools {
        cmd.push("--tools".to_string());
        cmd.push(String::new());
    }
    if opts.strict_mcp {
        cmd.push("--strict-mcp-config".to_string());
    }
    let session_truthy = opts.session_id.as_deref().is_some_and(|id| !id.is_empty());
    if session_truthy {
        cmd.push("--resume".to_string());
        cmd.push(opts.session_id.clone().unwrap());
    } else if !opts.persist_session {
        cmd.push("--no-session-persistence".to_string());
    }
    if !opts.model.is_empty() {
        cmd.push("--model".to_string());
        cmd.push(opts.model.clone());
    }
    if let Some(schema) = &opts.json_schema {
        let wrapped = claude_json_schema(schema);
        let dumped = electricity_json::dumps(&wrapped, electricity_json::WriteMode::COMPACT)?;
        cmd.push("--json-schema".to_string());
        cmd.push(dumped);
    }
    cmd.extend(opts.extra_args.iter().cloned());
    Ok(cmd)
}

/// `agent_cli.py::claude_wraps_schema`.
pub fn claude_wraps_schema(schema: &Value) -> bool {
    let type_value = schema
        .as_dict()
        .and_then(|dict| dict.get(&Value::Str("type".to_string())));
    type_value != Some(&Value::Str("object".to_string()))
}

/// `agent_cli.py::claude_json_schema`.
pub fn claude_json_schema(schema: &Value) -> Value {
    if !claude_wraps_schema(schema) {
        return schema.clone();
    }
    let dict = schema.as_dict().cloned().unwrap_or_default();
    let mut inner: Dict = Dict::new();
    for (key, value) in dict.iter() {
        if let Value::Str(key_str) = key {
            if SCHEMA_ROOT_KEYWORDS.contains(&key_str.as_str()) {
                continue;
            }
        }
        inner.insert(key.clone(), value.clone());
    }
    let mut properties: Dict = Dict::new();
    properties.insert(
        Value::Str(CLAUDE_SCHEMA_VALUE_KEY.to_string()),
        Value::Dict(inner),
    );
    let mut wrapper: Dict = Dict::new();
    wrapper.insert(
        Value::Str("type".to_string()),
        Value::Str("object".to_string()),
    );
    wrapper.insert(
        Value::Str("properties".to_string()),
        Value::Dict(properties),
    );
    wrapper.insert(
        Value::Str("required".to_string()),
        Value::List(vec![Value::Str(CLAUDE_SCHEMA_VALUE_KEY.to_string())]),
    );
    wrapper.insert(
        Value::Str("additionalProperties".to_string()),
        Value::Bool(false),
    );
    for key in SCHEMA_ROOT_KEYWORDS {
        if let Some(value) = dict.get(&Value::Str(key.to_string())) {
            wrapper.insert(Value::Str(key.to_string()), value.clone());
        }
    }
    Value::Dict(wrapper)
}

/// `agent_cli.py::run_agent_cli`. Always runs in its own process group
/// (`new_session: true`), writes `input` to stdin and closes it, and
/// removes [`PARENT_SESSION_ENV`] plus `unset_env` from the inherited
/// environment through [`RunOpts::env_remove`] (never an `env` map --
/// issue #520's settled question 3), then sets *env* (an adapter alias's
/// own variables) through [`RunOpts::env_set`], so a variable in both
/// `unset_env` and *env* gets the *env* value.
pub async fn run_agent_cli(
    cmd: &[String],
    unset_env: &[String],
    env: &[(String, String)],
    timeout_seconds: f64,
    input: Vec<u8>,
    cwd: Option<PathBuf>,
    token: &CancellationToken,
) -> Result<process::Completed, AgentCliRunError> {
    let name = os_path_basename(&cmd[0]);
    let opts = RunOpts {
        timeout: Some(timeout_duration(timeout_seconds)),
        input: Some(input),
        cwd,
        env: None,
        env_remove: env_removal_names(unset_env),
        env_set: env.to_vec(),
        new_session: true,
    };
    match process::run_tracked(cmd, &opts, token).await {
        Ok(completed) => Ok(completed),
        Err(ProcError::Timeout { .. }) => Err(AgentCliRunError::Timeout(timeout_message(
            &cmd[0],
            timeout_seconds,
        ))),
        Err(ProcError::NotFound(_)) => Err(AgentCliRunError::StartFailed(format!(
            "{name} could not be started ({}): [Errno 2] No such file or directory: {}",
            cmd[0],
            Value::Str(cmd[0].clone()).py_repr()
        ))),
        Err(ProcError::Other { message, .. }) => Err(AgentCliRunError::StartFailed(format!(
            "{name} could not be started ({}): {message}",
            cmd[0]
        ))),
        Err(ProcError::Decode(message)) => Err(AgentCliRunError::Decode(message)),
        Err(ProcError::Cancelled) => Err(AgentCliRunError::Cancelled),
    }
}

/// `os.path.basename` on POSIX: the text after the last `/`, or the
/// whole string when there is none (issue #520's settled question 14:
/// no Windows work, so no `\`-aware variant).
fn os_path_basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[index + 1..],
        None => path,
    }
}

/// [`run_agent_cli`]'s own timeout text (`agent_cli.py:258-262`), pulled
/// out so the differential corpus's `timeout_text_cases` can replay it
/// directly rather than only through a hand-written `py_format_g` check
/// (review finding 3 on PR #525: the corpus generated this text but
/// nothing in Rust ever called the function that builds the sentence
/// `run_agent_cli` actually raises). Takes the *binary* as configured,
/// not its basename, the same as [`run_agent_cli`]'s own `cmd[0]`.
pub fn timeout_message(binary: &str, timeout_seconds: f64) -> String {
    let name = os_path_basename(binary);
    format!(
        "{name} did not finish within {}s; it was stopped with everything it started.",
        py_format_g(timeout_seconds)
    )
}

/// A `Duration` for [`RunOpts::timeout`] that never panics the way
/// `Duration::from_secs_f64` does on a non-finite or too-large `f64`
/// (review finding 8 on PR #525) -- `timeout_seconds` reaches here from
/// run-time config, so `inf`/`NaN`/an out-of-range value must saturate,
/// never crash the call. `f64::max` already turns `NaN` into `0.0`
/// (`NaN.max(y)` returns `y` in Rust, unlike Python's `max`).
fn timeout_duration(timeout_seconds: f64) -> Duration {
    Duration::try_from_secs_f64(timeout_seconds.max(0.0)).unwrap_or(Duration::MAX)
}

/// `agent_cli.py::parse_pi_output`.
pub fn parse_pi_output(proc: &process::Completed) -> Result<AgentCliResult, AgentCliParseError> {
    let mut events: Vec<Dict> = Vec::new();
    for line in py_splitlines(&proc.stdout) {
        if py_strip(&line).is_empty() {
            continue;
        }
        if let Ok(value) = electricity_json::loads(&line) {
            if let Some(dict) = value.as_dict() {
                events.push(dict.clone());
            }
        }
    }

    let mut session_id: Option<String> = None;
    let mut last_message: Option<Dict> = None;
    let mut totals = UsageTotals::default();
    for event in &events {
        if event.get(&Value::Str("type".to_string())) == Some(&Value::Str("session".to_string())) {
            if let Some(Value::Str(id)) = event.get(&Value::Str("id".to_string())) {
                session_id = Some(id.clone());
            }
        }
        if event.get(&Value::Str("type".to_string()))
            != Some(&Value::Str("message_end".to_string()))
        {
            continue;
        }
        let Some(message) = event
            .get(&Value::Str("message".to_string()))
            .and_then(Value::as_dict)
        else {
            continue;
        };
        if message.get(&Value::Str("role".to_string()))
            != Some(&Value::Str("assistant".to_string()))
        {
            continue;
        }
        last_message = Some(message.clone());
        if let Some(usage) = message
            .get(&Value::Str("usage".to_string()))
            .and_then(Value::as_dict)
        {
            let cost_value = usage.get(&Value::Str("cost".to_string()));
            let cost = match cost_value {
                Some(Value::Dict(cost_dict)) => cost_dict.get(&Value::Str("total".to_string())),
                other => other,
            };
            totals
                .add(
                    &[
                        usage.get(&Value::Str("input".to_string())),
                        usage.get(&Value::Str("cacheRead".to_string())),
                        usage.get(&Value::Str("cacheWrite".to_string())),
                    ],
                    usage.get(&Value::Str("output".to_string())),
                    cost,
                )
                .map_err(AgentCliParseError::Numeric)?;
        }
    }

    let Some(last_message) = last_message else {
        return Err(AgentCliParseError::Message(no_answer_message("pi", proc)));
    };
    let text = pi_message_text(&last_message);
    let stop_reason = last_message.get(&Value::Str("stopReason".to_string()));
    let stop_reason_str = match stop_reason {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(stop_reason_str) = &stop_reason_str {
        if PI_ERROR_STOP_REASONS.contains(&stop_reason_str.as_str()) {
            let error_message = last_message.get(&Value::Str("errorMessage".to_string()));
            let detail = match error_message {
                Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                _ => text.clone(),
            };
            let shown = if detail.is_empty() {
                "no message".to_string()
            } else {
                detail
            };
            return Err(AgentCliParseError::Message(format!(
                "pi failed ({stop_reason_str}): {shown}"
            )));
        }
    }
    if proc.exit_code != 0 {
        return Err(AgentCliParseError::Message(no_answer_message("pi", proc)));
    }

    let mut raw = Dict::new();
    raw.insert(
        Value::Str("events".to_string()),
        Value::List(events.into_iter().map(Value::Dict).collect()),
    );
    Ok(AgentCliResult {
        text,
        raw: Value::Dict(raw),
        session_id,
        tokens_sent: totals.sent,
        tokens_received: totals.received,
        cost_usd: totals.cost,
        finish_reason: stop_reason_str,
        structured_output: None,
    })
}

/// `agent_cli.py::_pi_message_text`.
fn pi_message_text(message: &Dict) -> String {
    match message.get(&Value::Str("content".to_string())) {
        Some(Value::Str(s)) => py_strip(s).to_string(),
        Some(Value::List(items)) => {
            let mut text = String::new();
            for item in items {
                let Some(dict) = item.as_dict() else { continue };
                if dict.get(&Value::Str("type".to_string()))
                    != Some(&Value::Str("text".to_string()))
                {
                    continue;
                }
                if let Some(Value::Str(part)) = dict.get(&Value::Str("text".to_string())) {
                    text.push_str(part);
                }
            }
            py_strip(&text).to_string()
        }
        _ => String::new(),
    }
}

/// `agent_cli.py::parse_claude_output`.
pub fn parse_claude_output(
    proc: &process::Completed,
    json_schema: Option<&Value>,
) -> Result<AgentCliResult, AgentCliParseError> {
    let Some(raw) = last_json_object(&proc.stdout) else {
        return Err(AgentCliParseError::Message(no_answer_message(
            "claude", proc,
        )));
    };
    let result_text = match raw.get(&Value::Str("result".to_string())) {
        Some(Value::Str(s)) => s.clone(),
        _ => String::new(),
    };
    // `is True`, never Python `==` (`Value`'s own `PartialEq` is `py_eq`,
    // under which `True == 1 == 1.0` -- issue #520's A4.3 wants `is_error`
    // treated as an error only for the exact bool `true`, never `1`/`1.0`).
    if matches!(
        raw.get(&Value::Str("is_error".to_string())),
        Some(Value::Bool(true))
    ) {
        let subtype = raw.get(&Value::Str("subtype".to_string()));
        let label = match subtype {
            Some(Value::Str(s)) if !s.is_empty() && s != "success" => format!(" ({s})"),
            _ => String::new(),
        };
        let shown = if result_text.is_empty() {
            "no message".to_string()
        } else {
            result_text
        };
        return Err(AgentCliParseError::Message(format!(
            "claude failed{label}: {shown}"
        )));
    }
    if proc.exit_code != 0 {
        return Err(AgentCliParseError::Message(no_answer_message(
            "claude", proc,
        )));
    }

    let structured = raw
        .get(&Value::Str("structured_output".to_string()))
        .filter(|value| **value != Value::None)
        .cloned();
    let (text, structured_output) = match structured {
        Some(structured) => {
            let unwrapped = match (json_schema, &structured) {
                (Some(schema), Value::Dict(dict)) if claude_wraps_schema(schema) => dict
                    .get(&Value::Str(CLAUDE_SCHEMA_VALUE_KEY.to_string()))
                    .cloned()
                    .unwrap_or_else(|| structured.clone()),
                _ => structured.clone(),
            };
            let dumped = electricity_json::dumps(&unwrapped, electricity_json::WriteMode::COMPACT)
                .unwrap_or_default();
            (dumped, Some(unwrapped))
        }
        None => (py_strip(&result_text).to_string(), None),
    };

    let usage_dict = raw
        .get(&Value::Str("usage".to_string()))
        .and_then(Value::as_dict);
    let get_usage = |key: &str| usage_dict.and_then(|dict| dict.get(&Value::Str(key.to_string())));
    let mut totals = UsageTotals::default();
    totals
        .add(
            &[
                get_usage("input_tokens"),
                get_usage("cache_read_input_tokens"),
                get_usage("cache_creation_input_tokens"),
            ],
            get_usage("output_tokens"),
            raw.get(&Value::Str("total_cost_usd".to_string())),
        )
        .map_err(AgentCliParseError::Numeric)?;

    let session_id = match raw.get(&Value::Str("session_id".to_string())) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let finish_reason = match raw.get(&Value::Str("stop_reason".to_string())) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };

    Ok(AgentCliResult {
        text,
        raw: Value::Dict(raw),
        session_id,
        tokens_sent: totals.sent,
        tokens_received: totals.received,
        cost_usd: totals.cost,
        finish_reason,
        structured_output,
    })
}

/// Token/cost sums over every model turn a call made
/// (`agent_cli.py::_UsageTotals`) -- each total stays `None` until a
/// number arrives. `sent`/`received` go through [`py_int`] (`int()`:
/// floats truncate toward zero, `NaN`/infinity raise); `cost` goes
/// through a non-failing `float()` (Python can raise `OverflowError`
/// converting an astronomically large int to `float`, an edge case no
/// real CLI usage figure reaches and this module does not reproduce).
/// The running `i64` sums saturate at the bound (`saturating_add`)
/// rather than panicking in debug or silently wrapping to a negative
/// number in release (review finding 8 on PR #525) -- Python's own `int`
/// never overflows, so this is a safety net against an implausible
/// CLI-reported figure, not a text Python output ever pins.
#[derive(Debug, Default, Clone, PartialEq)]
struct UsageTotals {
    sent: Option<i64>,
    received: Option<i64>,
    cost: Option<f64>,
}

impl UsageTotals {
    fn add(
        &mut self,
        sent_parts: &[Option<&Value>],
        received: Option<&Value>,
        cost: Option<&Value>,
    ) -> Result<(), PyIntError> {
        for part in sent_parts.iter().flatten() {
            if is_number(part) {
                let value = py_int(part)?;
                self.sent = Some(self.sent.unwrap_or(0).saturating_add(value));
            }
        }
        if let Some(value) = received {
            if is_number(value) {
                let value = py_int(value)?;
                self.received = Some(self.received.unwrap_or(0).saturating_add(value));
            }
        }
        if let Some(value) = cost {
            if is_number(value) {
                let value = match value {
                    Value::Float(f) => *f,
                    Value::Int(i) => i.to_f64(),
                    _ => unreachable!("is_number only accepts Int/Float"),
                };
                self.cost = Some(self.cost.unwrap_or(0.0) + value);
            }
        }
        Ok(())
    }
}

/// `agent_cli.py::_is_number` -- an `int` or `float`, never a `bool`.
fn is_number(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_))
}

/// `agent_cli.py::_last_json_object`.
fn last_json_object(stdout: &str) -> Option<Dict> {
    if let Ok(value) = electricity_json::loads(stdout) {
        if let Some(dict) = value.as_dict() {
            return Some(dict.clone());
        }
    }
    last_json_object_from_lines(stdout)
}

fn last_json_object_from_lines(stdout: &str) -> Option<Dict> {
    for line in py_splitlines(stdout).into_iter().rev() {
        if let Ok(value) = electricity_json::loads(&line) {
            if let Some(dict) = value.as_dict() {
                return Some(dict.clone());
            }
        }
    }
    None
}

/// `agent_cli.py::_no_answer_message`.
fn no_answer_message(name: &str, proc: &process::Completed) -> String {
    let stripped = py_strip(&proc.stderr);
    let count = stripped.chars().count();
    let tail = if count > STDERR_TAIL_CHARS {
        let kept: String = stripped.chars().skip(count - STDERR_TAIL_CHARS).collect();
        format!("...{kept}")
    } else {
        stripped.to_string()
    };
    let detail = if tail.is_empty() {
        String::new()
    } else {
        format!(": {tail}")
    };
    format!(
        "{name} exited with code {} without an answer{detail}",
        proc.exit_code
    )
}

/// Python's `f"{x:g}"` (the shortest fixed-notation rendering at 6
/// significant digits, falling back to `1e+06`-style scientific notation
/// outside it) -- a private copy of `electricity_config::util::py_format_g`
/// (that one is `pub(crate)` to its own crate, and this module's own doc
/// convention, like `process.rs`'s `describe_utf8_error`, is to carry a
/// small duplicate rather than reach across a lane boundary for it).
fn py_format_g(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }
    let negative = x < 0.0;
    let ax = x.abs();
    let precision: i32 = 6;
    // The exponent comes from Rust's own correctly-rounded scientific
    // formatter, never a `log10().floor()` estimate refined by a
    // division (review finding 7 on PR #525): `ax / 10f64.powi(exp)` is
    // itself an inexact float, so a 7+-digit value that rounds up at the
    // 6th significant digit (`9999995.0`, which Python's `%g` carries to
    // `1e+07`) could round the wrong way, and the division approach
    // never notices the carry shifting the exponent by one.
    let scientific = format!("{:.*e}", (precision - 1).max(0) as usize, ax);
    let (mantissa, exp_str) = scientific
        .split_once('e')
        .expect("Rust's `{:e}` formatter always includes an exponent");
    let exp: i32 = exp_str.parse().expect("Rust's exponent is a plain integer");
    if (-4..precision).contains(&exp) {
        let decimals = (precision - 1 - exp).max(0) as usize;
        let mut s = format!("{ax:.decimals$}");
        strip_trailing_zeros(&mut s);
        if negative { format!("-{s}") } else { s }
    } else {
        let mut mant_s = mantissa.to_string();
        strip_trailing_zeros(&mut mant_s);
        let sign = if exp >= 0 { "+" } else { "-" };
        format!(
            "{}{mant_s}e{sign}{:02}",
            if negative { "-" } else { "" },
            exp.abs()
        )
    }
}

fn strip_trailing_zeros(s: &mut String) {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
}

/// The two adapters' own temporary working directory (issue #520 part
/// B, `adapters/claude_code.py:53`/`adapters/pi.py:46` --
/// `tempfile.TemporaryDirectory(prefix=...)`), added here rather than
/// in `electricity-adapters` itself (additive to this, part A's own
/// file -- issue #520's own part-A/part-B seam) because `libc`, needed
/// for [`libc::mkdtemp`], is already a dependency of this crate; part
/// B's own crate has no dependency on it, and the issue's own ruling 2
/// asks for "no new crates and no new dependencies."
///
/// Built from [`std::env::temp_dir`] (`TMPDIR`, or the platform
/// default) made absolute first (issue #520's own settled question 12:
/// `tempfile.mkdtemp`'s own result is always absolute, even under a
/// *relative* `TMPDIR` -- confirmed directly: CPython's own
/// `_candidate_tempdir_list` runs every candidate through
/// `os.path.abspath`, where Rust's `std::env::temp_dir` does not), then
/// `mkdtemp(3)`'s own `<dir>/<prefix>XXXXXX` template -- the random
/// suffix and the directory's `0700` mode both come from `mkdtemp`
/// itself, needing no separate `chmod`. [`Drop`] removes the directory
/// recursively, best-effort (a failure here is silently ignored, same
/// as Python's own `TemporaryDirectory.cleanup`'s `ignore_errors`
/// companion in this port): on every way out of the caller's own
/// `generate` -- success, a CLI failure, a timeout, or the call's own
/// future simply dropped by a racing cancellation -- the guard's own
/// stack slot still unwinds, running this.
pub struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    /// *prefix* is the whole leading literal before `mkdtemp`'s own
    /// random suffix (`"circuitry-claude-"`/`"circuitry-pi-"" -- B2),
    /// not a directory component of its own.
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let mut base = absolutize(std::env::temp_dir())?;
        base.push(format!("{prefix}XXXXXX"));
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let mut template = CString::new(base.as_os_str().as_bytes())
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "temp dir prefix contains a NUL byte",
                    )
                })?
                .into_bytes_with_nul();
            // SAFETY: `template` is a NUL-terminated, mutable buffer
            // long enough for `mkdtemp` to rewrite its own trailing
            // `XXXXXX` in place; its return value is either that same
            // pointer (success) or NULL with `errno` set (failure).
            let result = unsafe { libc::mkdtemp(template.as_mut_ptr() as *mut libc::c_char) };
            if result.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let len = template
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(template.len());
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(&template[..len]));
            Ok(TempDirGuard { path })
        }
        #[cfg(not(unix))]
        {
            // Issue #520's settled question 14: no Windows work --
            // electricity builds and tests on Linux/macOS only, so this
            // branch exists only so the crate still compiles on a
            // third platform's own `cargo check`, never runs in CI.
            let _ = base;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "TempDirGuard::new is only implemented on Unix",
            ))
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// *base*, made absolute against the current directory when it isn't
/// already -- [`TempDirGuard::new`]'s own `TMPDIR`-may-be-relative
/// case (issue #520's settled question 12), pulled out so a test can
/// drive it directly with a synthetic relative path instead of
/// mutating the process's real `TMPDIR`/cwd (both global, process-wide
/// state a parallel test run can't safely share).
fn absolutize(base: PathBuf) -> std::io::Result<PathBuf> {
    if base.is_absolute() {
        Ok(base)
    } else {
        Ok(std::env::current_dir()?.join(base))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn completed(stdout: &str, stderr: &str, exit_code: i32) -> process::Completed {
        process::Completed {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
        }
    }

    // --- child_env / env_removal_names -------------------------------

    #[test]
    fn env_removal_names_unions_parent_session_env_and_unset_env() {
        let names = env_removal_names(&["ANTHROPIC_API_KEY".to_string(), "MY_VAR".to_string()]);
        assert!(names.contains(&"CLAUDECODE".to_string()));
        assert!(names.contains(&"PI_SESSION_ID".to_string()));
        assert!(names.contains(&"ANTHROPIC_API_KEY".to_string()));
        assert!(names.contains(&"MY_VAR".to_string()));
    }

    #[test]
    fn child_env_keeps_everything_but_the_removed_set() {
        let mut base = HashMap::new();
        base.insert("CLAUDECODE".to_string(), "1".to_string());
        base.insert("ANTHROPIC_API_KEY".to_string(), "secret".to_string());
        base.insert("PATH".to_string(), "/bin".to_string());
        let unset: Vec<String> = DEFAULT_UNSET_ENV.iter().map(|s| s.to_string()).collect();
        let kept = child_env(&unset, &base);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.get("PATH"), Some(&"/bin".to_string()));
    }

    #[test]
    fn child_env_with_an_empty_unset_env_still_removes_parent_session_env() {
        let mut base = HashMap::new();
        base.insert("CLAUDE_CODE_ENTRYPOINT".to_string(), "cli".to_string());
        base.insert("ANTHROPIC_API_KEY".to_string(), "secret".to_string());
        let kept = child_env(&[], &base);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.get("ANTHROPIC_API_KEY"), Some(&"secret".to_string()));
    }

    // --- pi_command / claude_command ----------------------------------

    #[test]
    fn pi_command_builds_the_default_headless_argv() {
        let cmd = pi_command("pi", "/tmp/prompt.md", &PiCommandOpts::default());
        assert_eq!(
            cmd,
            vec![
                "pi",
                "-p",
                "--mode",
                "json",
                "--no-approve",
                "--no-tools",
                "--no-session",
                "@/tmp/prompt.md",
                PI_ATTACHED_PROMPT_INSTRUCTION,
            ]
        );
    }

    #[test]
    fn pi_command_with_a_session_model_and_thinking() {
        let opts = PiCommandOpts {
            model: "anthropic/claude".to_string(),
            thinking: "high".to_string(),
            session_id: Some("sess-1".to_string()),
            extra_args: vec!["--foo".to_string()],
            ..PiCommandOpts::default()
        };
        let cmd = pi_command("pi", "/tmp/p.md", &opts);
        assert_eq!(
            cmd,
            vec![
                "pi",
                "-p",
                "--mode",
                "json",
                "--no-approve",
                "--no-tools",
                "--session",
                "sess-1",
                "--model",
                "anthropic/claude",
                "--thinking",
                "high",
                "--foo",
                "@/tmp/p.md",
                PI_ATTACHED_PROMPT_INSTRUCTION,
            ]
        );
    }

    #[test]
    fn pi_command_with_tools_and_persist_session_omits_the_off_flags() {
        let opts = PiCommandOpts {
            tools: true,
            persist_session: true,
            ..PiCommandOpts::default()
        };
        let cmd = pi_command("pi", "/tmp/p.md", &opts);
        assert!(!cmd.contains(&"--no-tools".to_string()));
        assert!(!cmd.contains(&"--no-session".to_string()));
    }

    #[test]
    fn claude_command_builds_the_default_headless_argv() {
        let cmd = claude_command("claude", &ClaudeCommandOpts::default()).unwrap();
        assert_eq!(
            cmd,
            vec![
                "claude",
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
            ]
        );
    }

    #[test]
    fn claude_command_with_a_model_and_an_object_schema() {
        let mut schema = Dict::new();
        schema.insert(
            Value::Str("type".to_string()),
            Value::Str("object".to_string()),
        );
        let opts = ClaudeCommandOpts {
            model: "claude-x".to_string(),
            json_schema: Some(Value::Dict(schema)),
            ..ClaudeCommandOpts::default()
        };
        let cmd = claude_command("claude", &opts).unwrap();
        assert_eq!(cmd[cmd.len() - 4], "--model");
        assert_eq!(cmd[cmd.len() - 3], "claude-x");
        assert_eq!(cmd[cmd.len() - 2], "--json-schema");
        assert_eq!(cmd.last().unwrap(), "{\"type\": \"object\"}");
    }

    #[test]
    fn claude_command_with_a_session_resumes_and_skips_no_session_persistence() {
        let opts = ClaudeCommandOpts {
            session_id: Some("abc".to_string()),
            ..ClaudeCommandOpts::default()
        };
        let cmd = claude_command("claude", &opts).unwrap();
        assert!(cmd.contains(&"--resume".to_string()));
        assert!(!cmd.contains(&"--no-session-persistence".to_string()));
    }

    // --- claude_json_schema ---------------------------------------------

    #[test]
    fn claude_json_schema_sends_an_object_schema_unchanged() {
        let mut schema = Dict::new();
        schema.insert(
            Value::Str("type".to_string()),
            Value::Str("object".to_string()),
        );
        schema.insert(
            Value::Str("properties".to_string()),
            Value::Dict(Dict::new()),
        );
        let result = claude_json_schema(&Value::Dict(schema.clone()));
        assert_eq!(result, Value::Dict(schema));
    }

    #[test]
    fn claude_json_schema_wraps_a_non_object_schema_and_hoists_defs() {
        let mut schema = Dict::new();
        schema.insert(
            Value::Str("type".to_string()),
            Value::Str("array".to_string()),
        );
        schema.insert(Value::Str("$defs".to_string()), Value::Dict(Dict::new()));
        let result = claude_json_schema(&Value::Dict(schema));
        let wrapper = result.as_dict().expect("expected a dict").clone();
        assert_eq!(
            wrapper.get(&Value::Str("type".to_string())),
            Some(&Value::Str("object".to_string()))
        );
        assert!(wrapper.contains_key(&Value::Str("$defs".to_string())));
        let properties = wrapper
            .get(&Value::Str("properties".to_string()))
            .and_then(Value::as_dict)
            .unwrap();
        let inner = properties
            .get(&Value::Str(CLAUDE_SCHEMA_VALUE_KEY.to_string()))
            .and_then(Value::as_dict)
            .unwrap();
        assert!(!inner.contains_key(&Value::Str("$defs".to_string())));
    }

    #[test]
    fn claude_json_schema_wraps_a_typeless_schema() {
        let schema = Dict::new();
        assert!(claude_wraps_schema(&Value::Dict(schema.clone())));
        let result = claude_json_schema(&Value::Dict(schema));
        assert!(matches!(result, Value::Dict(_)));
    }

    // --- parse_claude_output ---------------------------------------------

    #[test]
    fn parse_claude_output_reads_a_plain_text_result() {
        let proc = completed(r#"{"result": "hello", "is_error": false}"#, "", 0);
        let result = parse_claude_output(&proc, None).unwrap();
        assert_eq!(result.text, "hello");
        assert!(result.structured_output.is_none());
    }

    #[test]
    fn parse_claude_output_fails_on_is_error_true_with_a_shown_subtype() {
        let proc = completed(
            r#"{"result": "boom", "is_error": true, "subtype": "error_max_turns"}"#,
            "",
            0,
        );
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert_eq!(err.to_string(), "claude failed (error_max_turns): boom");
    }

    #[test]
    fn parse_claude_output_is_error_with_subtype_success_hides_the_label() {
        let proc = completed(
            r#"{"result": "", "is_error": true, "subtype": "success"}"#,
            "",
            0,
        );
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert_eq!(err.to_string(), "claude failed: no message");
    }

    #[test]
    fn parse_claude_output_is_error_true_wins_over_a_nonzero_exit_check_order() {
        let proc = completed(r#"{"result": "x", "is_error": true}"#, "stderr text", 2);
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert_eq!(err.to_string(), "claude failed: x");
    }

    #[test]
    fn parse_claude_output_nonzero_exit_with_a_good_result_still_fails() {
        let proc = completed(r#"{"result": "ok", "is_error": false}"#, "oops", 2);
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "claude exited with code 2 without an answer: oops"
        );
    }

    #[test]
    fn parse_claude_output_no_json_at_all_reports_the_tail_of_stderr() {
        let proc = completed("not json", "some stderr", 1);
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "claude exited with code 1 without an answer: some stderr"
        );
    }

    #[test]
    fn parse_claude_output_uses_the_last_json_object_line() {
        let proc = completed(
            "log line one\nlog line two\n{\"result\": \"final\", \"is_error\": false}",
            "",
            0,
        );
        let result = parse_claude_output(&proc, None).unwrap();
        assert_eq!(result.text, "final");
    }

    #[test]
    fn parse_claude_output_unwraps_a_wrapped_structured_answer() {
        let mut schema = Dict::new();
        schema.insert(
            Value::Str("type".to_string()),
            Value::Str("array".to_string()),
        );
        let mut structured = Dict::new();
        structured.insert(
            Value::Str(CLAUDE_SCHEMA_VALUE_KEY.to_string()),
            Value::List(vec![Value::Int(1.into()), Value::Int(2.into())]),
        );
        let proc = completed(
            &electricity_json::dumps(
                &Value::Dict({
                    let mut d = Dict::new();
                    d.insert(
                        Value::Str("result".to_string()),
                        Value::Str("```json\n[1,2]\n```".to_string()),
                    );
                    d.insert(Value::Str("is_error".to_string()), Value::Bool(false));
                    d.insert(
                        Value::Str("structured_output".to_string()),
                        Value::Dict(structured),
                    );
                    d
                }),
                electricity_json::WriteMode::COMPACT,
            )
            .unwrap(),
            "",
            0,
        );
        let result = parse_claude_output(&proc, Some(&Value::Dict(schema))).unwrap();
        assert_eq!(result.text, "[1, 2]");
        assert_eq!(
            result.structured_output,
            Some(Value::List(vec![
                Value::Int(1.into()),
                Value::Int(2.into())
            ]))
        );
    }

    #[test]
    fn parse_claude_output_structured_output_false_still_counts_as_present() {
        let mut d = Dict::new();
        d.insert(
            Value::Str("result".to_string()),
            Value::Str("ignored".to_string()),
        );
        d.insert(Value::Str("is_error".to_string()), Value::Bool(false));
        d.insert(
            Value::Str("structured_output".to_string()),
            Value::Bool(false),
        );
        let proc = completed(
            &electricity_json::dumps(&Value::Dict(d), electricity_json::WriteMode::COMPACT)
                .unwrap(),
            "",
            0,
        );
        let result = parse_claude_output(&proc, None).unwrap();
        assert_eq!(result.text, "false");
        assert_eq!(result.structured_output, Some(Value::Bool(false)));
    }

    #[test]
    fn parse_claude_output_sums_usage_and_reads_cost_and_stop_reason() {
        let mut usage = Dict::new();
        usage.insert(
            Value::Str("input_tokens".to_string()),
            Value::Int(10.into()),
        );
        usage.insert(
            Value::Str("cache_read_input_tokens".to_string()),
            Value::Int(5.into()),
        );
        usage.insert(
            Value::Str("cache_creation_input_tokens".to_string()),
            Value::Int(2.into()),
        );
        usage.insert(
            Value::Str("output_tokens".to_string()),
            Value::Int(7.into()),
        );
        let mut d = Dict::new();
        d.insert(
            Value::Str("result".to_string()),
            Value::Str("ok".to_string()),
        );
        d.insert(Value::Str("is_error".to_string()), Value::Bool(false));
        d.insert(Value::Str("usage".to_string()), Value::Dict(usage));
        d.insert(Value::Str("total_cost_usd".to_string()), Value::Float(0.5));
        d.insert(
            Value::Str("stop_reason".to_string()),
            Value::Str("end_turn".to_string()),
        );
        let proc = completed(
            &electricity_json::dumps(&Value::Dict(d), electricity_json::WriteMode::COMPACT)
                .unwrap(),
            "",
            0,
        );
        let result = parse_claude_output(&proc, None).unwrap();
        assert_eq!(result.tokens_sent, Some(17));
        assert_eq!(result.tokens_received, Some(7));
        assert_eq!(result.cost_usd, Some(0.5));
        assert_eq!(result.finish_reason, Some("end_turn".to_string()));
    }

    #[test]
    fn parse_claude_output_propagates_a_nan_usage_value_as_a_numeric_error() {
        let mut usage = Dict::new();
        usage.insert(
            Value::Str("input_tokens".to_string()),
            Value::Float(f64::NAN),
        );
        let mut d = Dict::new();
        d.insert(
            Value::Str("result".to_string()),
            Value::Str("ok".to_string()),
        );
        d.insert(Value::Str("is_error".to_string()), Value::Bool(false));
        d.insert(Value::Str("usage".to_string()), Value::Dict(usage));
        let proc = completed(
            &electricity_json::dumps(&Value::Dict(d), electricity_json::WriteMode::COMPACT)
                .unwrap(),
            "",
            0,
        );
        let err = parse_claude_output(&proc, None).unwrap_err();
        assert!(matches!(err, AgentCliParseError::Numeric(_)));
        assert_eq!(err.to_string(), "cannot convert float NaN to integer");
    }

    // --- parse_pi_output ---------------------------------------------

    fn assistant_message_end(text: &str) -> String {
        let mut content = Dict::new();
        content.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        content.insert(
            Value::Str("content".to_string()),
            Value::Str(text.to_string()),
        );
        let mut event = Dict::new();
        event.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event.insert(Value::Str("message".to_string()), Value::Dict(content));
        electricity_json::dumps(&Value::Dict(event), electricity_json::WriteMode::COMPACT).unwrap()
    }

    #[test]
    fn parse_pi_output_reads_the_last_assistant_message() {
        let stdout = format!(
            "{}\n{}\n",
            assistant_message_end("first"),
            assistant_message_end("second")
        );
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "second");
    }

    #[test]
    fn parse_pi_output_skips_blank_and_non_json_lines() {
        let stdout = format!("\nnot json\n{}\n", assistant_message_end("hi"));
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "hi");
    }

    #[test]
    fn parse_pi_output_no_assistant_message_fails() {
        let proc = completed("", "nope", 0);
        let err = parse_pi_output(&proc).unwrap_err();
        assert_eq!(
            err.to_string(),
            "pi exited with code 0 without an answer: nope"
        );
    }

    #[test]
    fn parse_pi_output_error_stop_reason_uses_error_message() {
        let mut message = Dict::new();
        message.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        message.insert(
            Value::Str("content".to_string()),
            Value::Str("".to_string()),
        );
        message.insert(
            Value::Str("stopReason".to_string()),
            Value::Str("error".to_string()),
        );
        message.insert(
            Value::Str("errorMessage".to_string()),
            Value::Str("auth failed".to_string()),
        );
        let mut event = Dict::new();
        event.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event.insert(Value::Str("message".to_string()), Value::Dict(message));
        let stdout =
            electricity_json::dumps(&Value::Dict(event), electricity_json::WriteMode::COMPACT)
                .unwrap();
        let proc = completed(&stdout, "", 1);
        let err = parse_pi_output(&proc).unwrap_err();
        assert_eq!(err.to_string(), "pi failed (error): auth failed");
    }

    #[test]
    fn parse_pi_output_aborted_with_no_error_message_falls_back_to_text() {
        let mut message = Dict::new();
        message.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        message.insert(
            Value::Str("content".to_string()),
            Value::Str("partial text".to_string()),
        );
        message.insert(
            Value::Str("stopReason".to_string()),
            Value::Str("aborted".to_string()),
        );
        let mut event = Dict::new();
        event.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event.insert(Value::Str("message".to_string()), Value::Dict(message));
        let stdout =
            electricity_json::dumps(&Value::Dict(event), electricity_json::WriteMode::COMPACT)
                .unwrap();
        let proc = completed(&stdout, "", 1);
        let err = parse_pi_output(&proc).unwrap_err();
        assert_eq!(err.to_string(), "pi failed (aborted): partial text");
    }

    #[test]
    fn parse_pi_output_text_parts_joined_and_stripped() {
        let mut part1 = Dict::new();
        part1.insert(
            Value::Str("type".to_string()),
            Value::Str("text".to_string()),
        );
        part1.insert(
            Value::Str("text".to_string()),
            Value::Str(" hello ".to_string()),
        );
        let mut part2 = Dict::new();
        part2.insert(
            Value::Str("type".to_string()),
            Value::Str("text".to_string()),
        );
        part2.insert(
            Value::Str("text".to_string()),
            Value::Str("world ".to_string()),
        );
        let mut message = Dict::new();
        message.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        message.insert(
            Value::Str("content".to_string()),
            Value::List(vec![Value::Dict(part1), Value::Dict(part2)]),
        );
        let mut event = Dict::new();
        event.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event.insert(Value::Str("message".to_string()), Value::Dict(message));
        let stdout =
            electricity_json::dumps(&Value::Dict(event), electricity_json::WriteMode::COMPACT)
                .unwrap();
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "hello world");
    }

    #[test]
    fn parse_pi_output_sums_usage_with_an_object_and_a_bare_number_cost() {
        let mut usage1 = Dict::new();
        usage1.insert(Value::Str("input".to_string()), Value::Int(3.into()));
        usage1.insert(Value::Str("cacheRead".to_string()), Value::Int(1.into()));
        usage1.insert(Value::Str("cacheWrite".to_string()), Value::Int(1.into()));
        usage1.insert(Value::Str("output".to_string()), Value::Int(2.into()));
        let mut cost1 = Dict::new();
        cost1.insert(Value::Str("total".to_string()), Value::Float(0.25));
        usage1.insert(Value::Str("cost".to_string()), Value::Dict(cost1));
        let mut message1 = Dict::new();
        message1.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        message1.insert(
            Value::Str("content".to_string()),
            Value::Str("a".to_string()),
        );
        message1.insert(Value::Str("usage".to_string()), Value::Dict(usage1));
        let mut event1 = Dict::new();
        event1.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event1.insert(Value::Str("message".to_string()), Value::Dict(message1));

        let mut usage2 = Dict::new();
        usage2.insert(Value::Str("input".to_string()), Value::Int(4.into()));
        usage2.insert(Value::Str("output".to_string()), Value::Int(1.into()));
        usage2.insert(Value::Str("cost".to_string()), Value::Float(0.25));
        let mut message2 = Dict::new();
        message2.insert(
            Value::Str("role".to_string()),
            Value::Str("assistant".to_string()),
        );
        message2.insert(
            Value::Str("content".to_string()),
            Value::Str("b".to_string()),
        );
        message2.insert(Value::Str("usage".to_string()), Value::Dict(usage2));
        let mut event2 = Dict::new();
        event2.insert(
            Value::Str("type".to_string()),
            Value::Str("message_end".to_string()),
        );
        event2.insert(Value::Str("message".to_string()), Value::Dict(message2));

        let stdout = format!(
            "{}\n{}\n",
            electricity_json::dumps(&Value::Dict(event1), electricity_json::WriteMode::COMPACT)
                .unwrap(),
            electricity_json::dumps(&Value::Dict(event2), electricity_json::WriteMode::COMPACT)
                .unwrap(),
        );
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.tokens_sent, Some(3 + 1 + 1 + 4));
        assert_eq!(result.tokens_received, Some(3));
        assert_eq!(result.cost_usd, Some(0.5));
    }

    #[test]
    fn parse_pi_output_a_raw_line_separator_inside_an_event_splits_that_line() {
        // `str.splitlines()` treats U+2028 as a line boundary, so a
        // single JSON event containing a raw one in a string value
        // becomes two non-JSON fragments and is dropped (issue #520's
        // settled question 8).
        let event = assistant_message_end("ok");
        let truncated = event.replace("ok", "line1\u{2028}line2");
        let stdout = format!("{}\n{}\n", assistant_message_end("fallback"), truncated);
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "fallback");
    }

    #[test]
    fn parse_pi_output_a_raw_c0_line_separator_inside_an_event_splits_that_line() {
        // `\x1c` (one of the four C0 "other line/paragraph separator"
        // controls `str.splitlines()` treats as a boundary) is the same
        // quirk as U+2028, pinned separately since nothing covered it
        // (review finding 2 on PR #525).
        let event = assistant_message_end("ok");
        let truncated = event.replace("ok", "line1\x1cline2");
        let stdout = format!("{}\n{}\n", assistant_message_end("fallback"), truncated);
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "fallback");
    }

    #[test]
    fn parse_pi_output_a_raw_c1_next_line_separator_inside_an_event_splits_that_line() {
        // `\x85` (NEL, a C1 control `str.splitlines()` also treats as a
        // boundary) -- the same quirk as U+2028, pinned separately since
        // nothing covered it (review finding 2 on PR #525).
        let event = assistant_message_end("ok");
        let truncated = event.replace("ok", "line1\u{85}line2");
        let stdout = format!("{}\n{}\n", assistant_message_end("fallback"), truncated);
        let proc = completed(&stdout, "", 0);
        let result = parse_pi_output(&proc).unwrap();
        assert_eq!(result.text, "fallback");
    }

    // --- _no_answer_message --------------------------------------------

    #[test]
    fn no_answer_message_with_no_stderr_omits_the_colon() {
        let proc = completed("", "", 1);
        assert_eq!(
            no_answer_message("claude", &proc),
            "claude exited with code 1 without an answer"
        );
    }

    #[test]
    fn no_answer_message_truncates_stderr_over_2000_codepoints() {
        let long = "é".repeat(2500);
        let proc = completed("", &long, 1);
        let message = no_answer_message("claude", &proc);
        assert!(message.starts_with("claude exited with code 1 without an answer: ..."));
        let tail = message.split(": ...").nth(1).unwrap();
        assert_eq!(tail.chars().count(), 2000);
    }

    #[test]
    fn no_answer_message_negative_exit_code_for_a_signal() {
        let proc = completed("", "", -9);
        assert_eq!(
            no_answer_message("pi", &proc),
            "pi exited with code -9 without an answer"
        );
    }

    // --- run_agent_cli --------------------------------------------------

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn run_agent_cli_delivers_stdin_and_closes_it() {
        let cmd = vec!["cat".to_string()];
        let result = run_agent_cli(&cmd, &[], &[], 5.0, b"hello".to_vec(), None, &token())
            .await
            .unwrap();
        assert_eq!(result.stdout, "hello");
    }

    #[tokio::test]
    async fn run_agent_cli_pi_style_empty_stdin_is_a_closed_pipe() {
        // With `cat` alone, electricity's own inherited stdin (CI runs
        // with `/dev/null` attached) would also make `cat` print nothing,
        // so an empty `result.stdout` can't by itself prove a pipe was
        // ever attached (review finding 9 on PR #525). `test -p
        // /dev/stdin` fails on an inherited `/dev/null` (a character
        // device) and succeeds on a real pipe, closed or not, so its exit
        // code pins the shape of the fd `run_agent_cli` wired up.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "test -p /dev/stdin && cat; echo \"rc=$?\"".to_string(),
        ];
        let result = run_agent_cli(&cmd, &[], &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap();
        assert_eq!(result.stdout, "rc=0\n");
    }

    #[tokio::test]
    async fn run_agent_cli_times_out_with_the_basename_and_g_formatted_seconds() {
        let cmd = vec!["sleep".to_string(), "30".to_string()];
        let err = run_agent_cli(&cmd, &[], &[], 0.1, Vec::new(), None, &token())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "sleep did not finish within 0.1s; it was stopped with everything it started."
        );
        assert!(matches!(err, AgentCliRunError::Timeout(_)));
    }

    #[tokio::test]
    async fn run_agent_cli_timeout_kills_a_child_the_process_started() {
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8521 & echo hi".to_string(),
        ];
        run_agent_cli(&cmd, &[], &[], 0.1, Vec::new(), None, &token())
            .await
            .unwrap_err();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let still_running = std::process::Command::new("pgrep")
            .args(["-f", "sleep 8521"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        assert!(
            !still_running,
            "the timeout must kill the whole process group"
        );
    }

    #[tokio::test]
    async fn run_agent_cli_a_dropped_future_kills_the_child_and_its_grandchild() {
        // The issue asks for "the CLI and a child it started" (review
        // finding 9 on PR #525): a bare `sleep 8522` has no grandchild to
        // prove the whole process group dies, only the direct child.
        // `sh -c 'sleep 8522 & wait'` makes `sh` the direct child and the
        // backgrounded `sleep 8522` its grandchild.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 8522 & wait".to_string(),
        ];
        let token = token();
        let mut fut = Box::pin(run_agent_cli(
            &cmd,
            &[],
            &[],
            30.0,
            Vec::new(),
            None,
            &token,
        ));
        tokio::select! {
            _ = &mut fut => panic!("must not finish on the first poll"),
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        drop(fut);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let still_running = std::process::Command::new("pgrep")
            .args(["-f", "sleep 8522"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        assert!(
            !still_running,
            "dropping run_agent_cli's future must kill its grandchild too"
        );
    }

    #[tokio::test]
    async fn run_agent_cli_reports_a_missing_binary_with_pythons_errno_text() {
        let cmd = vec!["/circuitry-test-missing-claude".to_string()];
        let err = run_agent_cli(&cmd, &[], &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "circuitry-test-missing-claude could not be started (/circuitry-test-missing-claude): \
             [Errno 2] No such file or directory: '/circuitry-test-missing-claude'"
        );
    }

    #[tokio::test]
    async fn run_agent_cli_reports_a_non_executable_file() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-agent-cli-test-not-executable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let cmd = vec![path.to_string_lossy().into_owned()];
        let err = run_agent_cli(&cmd, &[], &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap_err();
        assert!(matches!(err, AgentCliRunError::StartFailed(_)));
        assert_eq!(
            err.to_string(),
            format!(
                "claude could not be started ({path}): [Errno 13] Permission denied: '{path}'",
                path = path.to_string_lossy()
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_agent_cli_decode_failure_is_not_wrapped_as_a_cli_failure() {
        // Octal `\377`, not hex `\xff`: POSIX `printf` guarantees octal
        // escapes everywhere, but dash's (Debian/Ubuntu `/bin/sh`) builtin
        // `printf` writes `\xff` as four literal characters, where bash's
        // (macOS's default `/bin/sh`) interprets it as one raw byte --
        // this must produce the same invalid UTF-8 byte on every CI runner.
        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '\\377'".to_string(),
        ];
        let err = run_agent_cli(&cmd, &[], &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap_err();
        assert!(matches!(err, AgentCliRunError::Decode(_)));
    }

    #[tokio::test]
    async fn run_agent_cli_removes_parent_session_env_and_default_unset_env() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-agent-cli-test-env-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("report-env");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor v in CLAUDECODE ANTHROPIC_API_KEY NEUTRAL_VAR; do\n  if printenv \"$v\" >/dev/null 2>&1; then echo \"$v=set\"; else echo \"$v=unset\"; fi\ndone\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: this test doesn't run in parallel with another test
        // reading these specific variable names.
        unsafe {
            std::env::set_var("CLAUDECODE", "1");
            std::env::set_var("ANTHROPIC_API_KEY", "secret");
            std::env::set_var("NEUTRAL_VAR", "kept");
        }
        // Through `sh`, not the script directly: a parallel test's fork
        // can hold this file open for writing for a moment after this
        // test's own `std::fs::write` returns (issue #623), and only
        // `exec`ing the script itself -- never reading it -- can fail
        // with ETXTBSY during that window.
        let cmd = vec!["sh".to_string(), script.to_string_lossy().into_owned()];
        let unset: Vec<String> = DEFAULT_UNSET_ENV.iter().map(|s| s.to_string()).collect();
        let result = run_agent_cli(&cmd, &unset, &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("CLAUDECODE");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("NEUTRAL_VAR");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.stdout.contains("CLAUDECODE=unset"));
        assert!(result.stdout.contains("ANTHROPIC_API_KEY=unset"));
        assert!(result.stdout.contains("NEUTRAL_VAR=set"));
    }

    #[tokio::test]
    async fn run_agent_cli_sets_env_after_the_removals() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-agent-cli-test-env-set-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("report-env-set");
        std::fs::write(
            &script,
            "#!/bin/sh\necho \"claudecode=${CLAUDECODE:-unset} key=${ANTHROPIC_API_KEY:-unset} probe=${ALIAS_PROBE:-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Through `sh`, not the script directly: see issue #623.
        let cmd = vec!["sh".to_string(), script.to_string_lossy().into_owned()];
        let unset: Vec<String> = DEFAULT_UNSET_ENV.iter().map(|s| s.to_string()).collect();
        let env = vec![
            ("CLAUDECODE".to_string(), "from-env".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "from-env".to_string()),
            ("ALIAS_PROBE".to_string(), "alpha".to_string()),
        ];
        let result = run_agent_cli(&cmd, &unset, &env, 5.0, Vec::new(), None, &token())
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            result.stdout.trim(),
            "claudecode=from-env key=from-env probe=alpha"
        );
        assert!(std::env::var("ALIAS_PROBE").is_err());
    }

    #[tokio::test]
    async fn run_agent_cli_env_remove_keeps_a_non_utf8_inherited_variable_byte_for_byte() {
        // `env_remove` (never an `env` map, issue #520's settled question
        // 3) exists precisely because a `HashMap<String, String>` can't
        // hold a variable that isn't valid UTF-8 at all -- unlike
        // `merged_env`, which would drop it (review finding 5 on PR #525).
        // `od` turns the inherited bytes into plain ASCII hex so this
        // test's own stdout stays valid UTF-8 for `run_tracked`'s decode.
        use std::os::unix::ffi::OsStrExt;
        let dir = std::env::temp_dir().join(format!(
            "electricity-agent-cli-test-nonutf8-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("report-bytes");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintenv AGENT_CLI_TEST_BAD_UTF8 | od -An -tx1 | tr -d ' \\n'\necho\nif printenv AGENT_CLI_TEST_TO_REMOVE >/dev/null 2>&1; then echo \"removed=set\"; else echo \"removed=unset\"; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: this test doesn't run in parallel with another test
        // reading these specific variable names; both are removed on
        // teardown before any other test can observe them.
        unsafe {
            std::env::set_var(
                "AGENT_CLI_TEST_BAD_UTF8",
                std::ffi::OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f]),
            );
            std::env::set_var("AGENT_CLI_TEST_TO_REMOVE", "secret");
        }
        // Through `sh`, not the script directly: see issue #623.
        let cmd = vec!["sh".to_string(), script.to_string_lossy().into_owned()];
        let unset = vec!["AGENT_CLI_TEST_TO_REMOVE".to_string()];
        let result = run_agent_cli(&cmd, &unset, &[], 5.0, Vec::new(), None, &token())
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("AGENT_CLI_TEST_BAD_UTF8");
            std::env::remove_var("AGENT_CLI_TEST_TO_REMOVE");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(result.stdout.contains("666fff6f"));
        assert!(result.stdout.contains("removed=unset"));
    }

    // --- timeout_duration ------------------------------------------

    #[test]
    fn timeout_duration_saturates_instead_of_panicking_on_infinity() {
        assert_eq!(timeout_duration(f64::INFINITY), Duration::MAX);
    }

    #[test]
    fn timeout_duration_treats_nan_as_zero_rather_than_panicking() {
        assert_eq!(timeout_duration(f64::NAN), Duration::ZERO);
    }

    // --- UsageTotals::add -------------------------------------------

    #[test]
    fn usage_totals_add_saturates_instead_of_panicking_on_i64_overflow() {
        let mut totals = UsageTotals {
            sent: Some(i64::MAX),
            received: None,
            cost: None,
        };
        totals.add(&[Some(&Value::from(1i64))], None, None).unwrap();
        assert_eq!(totals.sent, Some(i64::MAX));
    }

    // --- py_format_g ------------------------------------------------

    #[test]
    fn py_format_g_matches_pythons_g_format_for_the_timeout_cases() {
        assert_eq!(py_format_g(1.0), "1");
        assert_eq!(py_format_g(120.0), "120");
        assert_eq!(py_format_g(999999.0), "999999");
        assert_eq!(py_format_g(1000000.0), "1e+06");
        assert_eq!(py_format_g(4294967295.0), "4.29497e+09");
    }

    #[test]
    fn py_format_g_handles_a_round_trip_carry_into_the_next_exponent() {
        // The naive `ax / 10f64.powi(exp)` division this replaced (review
        // finding 7 on PR #525) rounds `9999995.0`'s mantissa to
        // `10.00000`, never reshaping the exponent the way Python's `%g`
        // carries it to `1e+07`.
        assert_eq!(py_format_g(9999995.0), "1e+07");
        assert_eq!(py_format_g(1000015.0), "1.00002e+06");
        assert_eq!(py_format_g(2000025.0), "2.00002e+06");
        assert_eq!(py_format_g(1500005.0), "1.5e+06");
        assert_eq!(py_format_g(2500005.0), "2.5e+06");
        assert_eq!(py_format_g(1234565.0), "1.23456e+06");
    }

    // --- TempDirGuard -------------------------------------------------

    #[test]
    fn temp_dir_guard_creates_an_absolute_0700_directory_under_the_prefix() {
        let guard = TempDirGuard::new("circuitry-agent-cli-test-").unwrap();
        assert!(guard.path().is_absolute());
        assert!(guard.path().is_dir());
        let name = guard.path().file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("circuitry-agent-cli-test-"));
        let mode = std::fs::metadata(guard.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn absolutize_joins_a_relative_base_against_the_current_directory() {
        let relative = PathBuf::from("circuitry-agent-cli-reltest");
        let resolved = absolutize(relative.clone()).unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(resolved, std::env::current_dir().unwrap().join(relative));
    }

    #[test]
    fn absolutize_keeps_an_already_absolute_base_unchanged() {
        let absolute = std::env::temp_dir();
        assert_eq!(absolutize(absolute.clone()).unwrap(), absolute);
    }

    #[test]
    fn temp_dir_guard_removes_the_directory_on_drop() {
        let guard = TempDirGuard::new("circuitry-agent-cli-test-").unwrap();
        let path = guard.path().to_path_buf();
        assert!(path.is_dir());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn temp_dir_guard_two_calls_never_collide() {
        let a = TempDirGuard::new("circuitry-agent-cli-test-").unwrap();
        let b = TempDirGuard::new("circuitry-agent-cli-test-").unwrap();
        assert_ne!(a.path(), b.path());
    }
}
