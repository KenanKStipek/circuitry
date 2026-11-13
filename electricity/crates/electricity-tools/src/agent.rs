//! `plugins/agent.py`, ported (issue #720): the `agent` tool. One effect
//! is one headless session of `pi` or Claude Code, with its own tools on,
//! working in `params.cwd` for the effect's `timeout_ms`. Built on
//! [`crate::agent_cli`] (command building, the clean child environment,
//! the run in its own process group) and [`crate::agent_live`] (`meta.live`
//! and the compact transcript, #636).
//!
//! `result_file` + `result_schema`: the agent is told to write a JSON
//! file, read and validated once the session's first turn ends; a
//! missing or invalid file gets exactly one repair turn in the same
//! session, then a failure naming the errors. A turn that fails after
//! the session has an id, or an earlier turn has completed, ends the
//! effect as `ok: false` (so `on_error`/retries behave as for any other
//! tool) whose `raw` still carries `session_id`/`transcript`/`timed_out`;
//! one that fails before either raises, unchanged.
//!
//! A cancelled or decode-failed turn (`AgentCliRunError::Cancelled`/
//! `Decode`) always raises, never becomes an `ok: false` result --
//! those are never wrapped as a CLI failure in the first place, the
//! same as every other process-backed tool's own `ProcError::Cancelled`/
//! `Decode`.

use crate::agent_cli::{
    AgentCliParseError, AgentCliRunError, ClaudeCommandOpts, DEFAULT_UNSET_ENV,
    PI_ATTACHED_PROMPT_INSTRUCTION, PiCommandOpts, claude_command, parse_claude_output,
    parse_pi_output, pi_command, run_agent_cli,
};
use crate::agent_live::LiveTracker;
use crate::subprocess_tool::expand_and_validate;
use crate::{CheckResult, ErrorCause, PyExc, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_strip;
use electricity_value::{CancellationToken, Dict, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PI: &str = "pi";
pub const CLAUDE_CODE: &str = "claude_code";
const DEFAULT_ENGINE: &str = PI;
const TOOLS_PERMISSION_MODE: &str = "dontAsk";
const TRANSCRIPT_NAME: &str = "transcript.log";
const PROJECT_INSTRUCTIONS_HEADER: &str = "\n\n---\n\nProject instructions from the repository's CLAUDE.md (Claude Code does not load it in this session):\n\n";

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn value_error(message: String) -> ToolError {
    ToolError {
        py_class: PyExc::ValueError,
        message,
        cause: ErrorCause::None,
    }
}

fn get<'d>(params: &'d Dict, name: &str) -> Option<&'d Value> {
    params.get(&Value::Str(name.to_string()))
}

// ---------------- params ----------------

fn optional_str(params: &Dict, name: &str) -> Result<String, ToolError> {
    match get(params, name) {
        None | Some(Value::None) => Ok(String::new()),
        Some(Value::Str(s)) => Ok(py_strip(s).to_string()),
        Some(_) => Err(value_error(format!(
            "agent: params['{name}'] must be a string."
        ))),
    }
}

fn str_list(params: &Dict, name: &str) -> Result<Vec<String>, ToolError> {
    match get(params, name) {
        None | Some(Value::None) => Ok(Vec::new()),
        Some(Value::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::Str(s) => {
                        if s.contains('\0') {
                            return Err(value_error(format!(
                                "agent: params['{name}'][{index}] contains a null byte."
                            )));
                        }
                        out.push(s.clone());
                    }
                    _ => {
                        return Err(value_error(format!(
                            "agent: params['{name}'] must be a list of strings."
                        )));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(value_error(format!(
            "agent: params['{name}'] must be a list of strings."
        ))),
    }
}

fn tool_names(params: &Dict, name: &str) -> Result<Vec<String>, ToolError> {
    let names = str_list(params, name)?;
    for (index, entry) in names.iter().enumerate() {
        if py_strip(entry).is_empty() {
            return Err(value_error(format!(
                "agent: params['{name}'][{index}] is blank."
            )));
        }
    }
    Ok(names)
}

fn str_mapping(params: &Dict, name: &str) -> Result<Vec<(String, String)>, ToolError> {
    match get(params, name) {
        None | Some(Value::None) => Ok(Vec::new()),
        Some(Value::Dict(dict)) => {
            let mut out = Vec::with_capacity(dict.len());
            for (k, v) in dict.iter() {
                let Value::Str(key_str) = k else {
                    return Err(value_error(format!(
                        "agent: params['{name}'] must be a mapping of variable names to values."
                    )));
                };
                if matches!(v, Value::None) {
                    return Err(value_error(format!(
                        "agent: params['{name}']['{key_str}'] is null; unset_env removes a variable."
                    )));
                }
                out.push((key_str.clone(), value_as_str(v)));
            }
            Ok(out)
        }
        Some(_) => Err(value_error(format!(
            "agent: params['{name}'] must be a mapping of variable names to values."
        ))),
    }
}

fn value_as_str(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        other => other.py_repr(),
    }
}

fn trust_project_settings_param(params: &Dict, engine_name: &str) -> Result<bool, ToolError> {
    let value = get(params, "trust_project_settings");
    let resolved = match value {
        None | Some(Value::None) => return Ok(false),
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Str(s)) => match py_strip(s).to_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    };
    let Some(resolved) = resolved else {
        return Err(value_error(
            "agent: params['trust_project_settings'] must be true or false.".to_string(),
        ));
    };
    if engine_name != CLAUDE_CODE {
        return Err(value_error(
            "agent: params['trust_project_settings'] applies to engine 'claude_code' only."
                .to_string(),
        ));
    }
    Ok(resolved)
}

// ---------------- CLAUDE.md ----------------

fn repository_root(cwd: &Path) -> PathBuf {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut candidate = start.clone();
    loop {
        if candidate.join(".git").exists() {
            return candidate;
        }
        match candidate.parent() {
            Some(parent) => candidate = parent.to_path_buf(),
            None => return start,
        }
    }
}

/// `plugins/agent.py::_project_instructions`.
fn project_instructions(cwd: &Path) -> Option<(String, String)> {
    let root = repository_root(cwd);
    let path = root.join("CLAUDE.md");
    if !path.exists() {
        return None;
    }
    let target = path.canonicalize().ok()?;
    if !target.is_file() || !target.starts_with(&root) {
        return None;
    }
    let bytes = std::fs::read(&target).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        return None;
    }
    Some((path.display().to_string(), text))
}

// ---------------- engine options ----------------

#[derive(Clone)]
struct EngineOptions {
    binary: String,
    cwd: String,
    unset_env: Vec<String>,
    env: Vec<(String, String)>,
    model: String,
    tools: Vec<String>,
    exclude_tools: Vec<String>,
    extra_args: Vec<String>,
}

struct AgentTurn {
    text: String,
    session_id: Option<String>,
    turns: i64,
    tool_calls: i64,
    tokens_sent: Option<i64>,
    tokens_received: Option<i64>,
    cost_usd: Option<f64>,
}

/// A turn's own failure: the message a resumed session or a raise can
/// report, and whether it was a timeout (`plugins/agent.py::_TurnFailed`/
/// `AgentCliTimeout`).
struct TurnFailure {
    message: String,
    timed_out: bool,
}

/// A failure that always raises, never becomes an `ok: false` result --
/// a prompt file that could not be written at all (`plugins/agent.py`'s
/// own `_write_prompt_file`, outside the turn's own try/except).
enum TurnOutcome {
    Failed(TurnFailure),
    Raise(ToolError),
}

impl From<AgentCliRunError> for TurnFailure {
    fn from(err: AgentCliRunError) -> Self {
        TurnFailure {
            timed_out: matches!(err, AgentCliRunError::Timeout(_)),
            message: err.to_string(),
        }
    }
}

/// `run_agent_cli`'s own failure, turned into this turn's outcome --
/// `Cancelled`/`Decode` are never wrapped as a CLI failure
/// (`agent_cli.rs`'s own `AgentCliRunError` doc comment), so unlike
/// `Timeout`/`StartFailed` they never become an `ok: false` result
/// naming a session or transcript to resume; they raise exactly as
/// every other tool's own `ProcError::Cancelled`/`ProcError::Decode`
/// does.
fn turn_outcome_from_run_error(err: AgentCliRunError) -> TurnOutcome {
    match err {
        AgentCliRunError::Cancelled => TurnOutcome::Raise(ToolError::message("cancelled")),
        AgentCliRunError::Decode(message) => TurnOutcome::Raise(ToolError {
            py_class: PyExc::UnicodeDecodeError,
            message,
            cause: ErrorCause::None,
        }),
        other => TurnOutcome::Failed(TurnFailure::from(other)),
    }
}

impl From<AgentCliParseError> for TurnFailure {
    fn from(err: AgentCliParseError) -> Self {
        TurnFailure {
            timed_out: false,
            message: err.to_string(),
        }
    }
}

/// `plugins/agent.py::_builtin_tool_names`.
fn builtin_tool_names(tools: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for entry in tools {
        let name = entry.split('(').next().unwrap_or("").trim();
        if !name.is_empty() && !name.starts_with("mcp__") && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// `agent_cli.py::claude_command`'s own argv switch from
/// `--output-format json` to `stream-json --verbose`
/// (`plugins/agent.py::_stream_json`).
fn stream_json(cmd: Vec<String>) -> Vec<String> {
    let index = cmd
        .iter()
        .position(|arg| arg == "--output-format")
        .expect("claude_command always writes --output-format");
    let mut out = cmd[..=index].to_vec();
    out.push("stream-json".to_string());
    out.push("--verbose".to_string());
    out.extend_from_slice(&cmd[index + 2..]);
    out
}

/// `plugins/agent.py::_write_prompt_file`. Uncaught in Python (no
/// `try`/`except` around `tempfile.mkstemp`/the write), so a failure
/// here carries no `"agent: "` prefix either -- the bare OS-error text
/// [`crate::fs::os_error`] already gives every other plugin's own
/// uncaught `OSError`.
fn write_prompt_file(scratch_dir: &Path, prompt: &str) -> Result<PathBuf, ToolError> {
    use std::io::Write;
    let name = format!(
        "prompt-{}-{}.md",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let path = scratch_dir.join(name);
    let path_display = path.display().to_string();
    let mut file =
        std::fs::File::create(&path).map_err(|err| crate::fs::os_error(&err, &path_display))?;
    file.write_all(prompt.as_bytes())
        .map_err(|err| crate::fs::os_error(&err, &path_display))?;
    Ok(path)
}

/// `pi`'s own `--tools`/`--exclude-tools` flags, then *options*'s own
/// `extra_args` -- the one piece of `run_pi_turn`'s own command-building
/// that isn't already covered by `agent_cli::pi_command`'s own tests.
fn pi_extra_args(options: &EngineOptions) -> Vec<String> {
    let mut extra: Vec<String> = Vec::new();
    if !options.tools.is_empty() {
        extra.push("--tools".to_string());
        extra.push(options.tools.join(","));
    }
    if !options.exclude_tools.is_empty() {
        extra.push("--exclude-tools".to_string());
        extra.push(options.exclude_tools.join(","));
    }
    extra.extend(options.extra_args.iter().cloned());
    extra
}

#[allow(clippy::too_many_arguments)]
async fn run_pi_turn(
    options: &EngineOptions,
    thinking: &str,
    prompt: &str,
    session_id: Option<&str>,
    timeout_seconds: f64,
    scratch_dir: &Path,
    live: &LiveTracker<'_>,
    token: &CancellationToken,
) -> Result<AgentTurn, TurnOutcome> {
    let prompt_file = write_prompt_file(scratch_dir, prompt).map_err(TurnOutcome::Raise)?;
    let extra = pi_extra_args(options);
    let cmd = pi_command(
        &options.binary,
        &prompt_file.to_string_lossy(),
        &PiCommandOpts {
            model: options.model.clone(),
            thinking: thinking.to_string(),
            tools: true,
            session_id: session_id.map(str::to_string),
            persist_session: true,
            extra_args: extra,
            instruction: PI_ATTACHED_PROMPT_INSTRUCTION.to_string(),
        },
    );
    let on_line = |line: &str| live.handle_line(line);
    let on_tick = || live.maybe_flush();
    let run_result = run_agent_cli(
        &cmd,
        &options.unset_env,
        &options.env,
        timeout_seconds,
        Vec::new(),
        Some(PathBuf::from(&options.cwd)),
        token,
        Some(&on_line),
        Some(&on_tick),
    )
    .await;
    let _ = std::fs::remove_file(&prompt_file);
    let proc = run_result.map_err(turn_outcome_from_run_error)?;
    let parsed =
        parse_pi_output(&proc).map_err(|err| TurnOutcome::Failed(TurnFailure::from(err)))?;
    let mut turns = 0i64;
    let mut tool_calls = 0i64;
    if let Some(Value::List(events)) = parsed.raw.as_dict().and_then(|d| d.get(&key("events"))) {
        for event in events {
            let Some(event) = event.as_dict() else {
                continue;
            };
            match event.get(&key("type")).and_then(Value::as_str) {
                Some("tool_execution_start") => tool_calls += 1,
                Some("message_end") => {
                    if let Some(message) = event.get(&key("message")).and_then(Value::as_dict) {
                        if message.get(&key("role")).and_then(Value::as_str) == Some("assistant") {
                            turns += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(AgentTurn {
        text: parsed.text,
        session_id: parsed
            .session_id
            .filter(|s| !s.is_empty())
            .or_else(|| session_id.map(str::to_string)),
        turns,
        tool_calls,
        tokens_sent: parsed.tokens_sent,
        tokens_received: parsed.tokens_received,
        cost_usd: parsed.cost_usd,
    })
}

/// Claude Code's own `--setting-sources`/`--tools`/`--allowedTools`/
/// `--permission-mode`/`--disallowedTools` flags, then *options*'s own
/// `extra_args` -- the one piece of `run_claude_turn`'s own command-
/// building that isn't already covered by `agent_cli::claude_command`'s
/// own tests. `--tools` gets the engine's own builtin names
/// ([`builtin_tool_names`]) and `--allowedTools` the caller's own list
/// verbatim, including an MCP tool's `mcp__server__tool` form
/// [`builtin_tool_names`] drops.
fn claude_extra_args(
    options: &EngineOptions,
    permission_mode: &str,
    trust_project_settings: bool,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if !trust_project_settings {
        args.push("--setting-sources".to_string());
        args.push("user".to_string());
    }
    if !options.tools.is_empty() {
        args.push("--tools".to_string());
        args.push(builtin_tool_names(&options.tools).join(","));
        args.push("--allowedTools".to_string());
        args.extend(options.tools.iter().cloned());
    }
    if !permission_mode.is_empty() {
        args.push("--permission-mode".to_string());
        args.push(permission_mode.to_string());
    }
    if !options.exclude_tools.is_empty() {
        args.push("--disallowedTools".to_string());
        args.extend(options.exclude_tools.iter().cloned());
    }
    args.extend(options.extra_args.iter().cloned());
    args
}

#[allow(clippy::too_many_arguments)]
async fn run_claude_turn(
    options: &EngineOptions,
    permission_mode: &str,
    trust_project_settings: bool,
    prompt: &str,
    session_id: Option<&str>,
    timeout_seconds: f64,
    live: &LiveTracker<'_>,
    token: &CancellationToken,
) -> Result<AgentTurn, TurnOutcome> {
    let args = claude_extra_args(options, permission_mode, trust_project_settings);
    let cmd = claude_command(
        &options.binary,
        &ClaudeCommandOpts {
            model: options.model.clone(),
            json_schema: None,
            tools: true,
            session_id: session_id.map(str::to_string),
            persist_session: true,
            extra_args: args,
            strict_mcp: !trust_project_settings,
        },
    )
    .map_err(|err| TurnOutcome::Raise(ToolError::message(format!("agent: {err:?}"))))?;
    let cmd = stream_json(cmd);
    let on_line = |line: &str| live.handle_line(line);
    let on_tick = || live.maybe_flush();
    let run_result = run_agent_cli(
        &cmd,
        &options.unset_env,
        &options.env,
        timeout_seconds,
        prompt.as_bytes().to_vec(),
        Some(PathBuf::from(&options.cwd)),
        token,
        Some(&on_line),
        Some(&on_tick),
    )
    .await;
    let proc = run_result.map_err(turn_outcome_from_run_error)?;
    let result = parse_claude_output(&proc, None)
        .map_err(|err| TurnOutcome::Failed(TurnFailure::from(err)))?;
    let mut turns = 0i64;
    let mut tool_calls = 0i64;
    for line in proc.stdout.lines() {
        let Ok(event) = electricity_json::loads(line) else {
            continue;
        };
        let Some(event) = event.as_dict() else {
            continue;
        };
        if event.get(&key("type")).and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = event.get(&key("message")).and_then(Value::as_dict) else {
            continue;
        };
        turns += 1;
        if let Some(Value::List(content)) = message.get(&key("content")) {
            for item in content {
                if let Some(item) = item.as_dict() {
                    if item.get(&key("type")).and_then(Value::as_str) == Some("tool_use") {
                        tool_calls += 1;
                    }
                }
            }
        }
    }
    if let Some(Value::Int(n)) = result.raw.as_dict().and_then(|d| d.get(&key("num_turns"))) {
        turns = n.to_f64() as i64;
    }
    Ok(AgentTurn {
        text: result.text,
        session_id: result
            .session_id
            .filter(|s| !s.is_empty())
            .or_else(|| session_id.map(str::to_string)),
        turns,
        tool_calls,
        tokens_sent: result.tokens_sent,
        tokens_received: result.tokens_received,
        cost_usd: result.cost_usd,
    })
}

// ---------------- session ----------------

struct SessionState {
    session_id: Option<String>,
    turns: i64,
    tool_calls: i64,
    tokens_sent: Option<i64>,
    tokens_received: Option<i64>,
    cost_usd: Option<f64>,
    repair_turn: bool,
    turn_completed: bool,
    last_text: String,
}

impl SessionState {
    fn add(&mut self, turn: AgentTurn) {
        if let Some(id) = turn.session_id {
            if !id.is_empty() {
                self.session_id = Some(id);
            }
        }
        self.turns += turn.turns;
        self.tool_calls += turn.tool_calls;
        if let Some(sent) = turn.tokens_sent {
            self.tokens_sent = Some(self.tokens_sent.unwrap_or(0) + sent);
        }
        if let Some(received) = turn.tokens_received {
            self.tokens_received = Some(self.tokens_received.unwrap_or(0) + received);
        }
        if let Some(cost) = turn.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
        self.last_text = turn.text;
        self.turn_completed = true;
    }

    fn raw(&self, engine_name: &str, transcript_path: &Path, live: &LiveTracker<'_>) -> Dict {
        let mut raw = Dict::new();
        raw.insert(key("engine"), Value::Str(engine_name.to_string()));
        raw.insert(
            key("session_id"),
            self.session_id
                .clone()
                .map(Value::Str)
                .unwrap_or(Value::None),
        );
        raw.insert(key("turns"), Value::from(self.turns));
        raw.insert(key("tool_calls"), Value::from(self.tool_calls));
        let mut tokens = Dict::new();
        tokens.insert(
            key("sent"),
            self.tokens_sent.map(Value::from).unwrap_or(Value::None),
        );
        tokens.insert(
            key("received"),
            self.tokens_received.map(Value::from).unwrap_or(Value::None),
        );
        raw.insert(key("tokens"), Value::Dict(tokens));
        raw.insert(key("repair_turn"), Value::Bool(self.repair_turn));
        raw.insert(
            key("transcript"),
            if self.turn_completed {
                Value::Str(transcript_path.display().to_string())
            } else {
                Value::None
            },
        );
        for (k, v) in live.raw_extra() {
            raw.insert(k, v);
        }
        if let Some(cost) = self.cost_usd {
            raw.insert(key("cost"), Value::Float(cost));
        }
        raw
    }

    fn result(
        &self,
        engine_name: &str,
        transcript_path: &Path,
        live: &LiveTracker<'_>,
        value: Value,
        stderr: Option<String>,
        ok: bool,
    ) -> ToolResult {
        ToolResult {
            value,
            raw: Value::Dict(self.raw(engine_name, transcript_path, live)),
            stdout: None,
            stderr,
            exit_code: None,
            ok,
            stdout_parsed: false,
            tokens_sent: self.tokens_sent,
            tokens_received: self.tokens_received,
            cost_usd: self.cost_usd,
        }
    }
}

fn with_result_contract(prompt: &str, result_file: &Path, result_schema: Option<&Value>) -> String {
    let mut contract = format!(
        "\n\n---\n\nWhen you have finished, write your result as one JSON document \
         to this file (overwrite it if it exists):\n{}\n",
        result_file.display()
    );
    if let Some(schema) = result_schema {
        let dumped = electricity_json::dumps(
            schema,
            electricity_json::WriteMode {
                indent: Some(2),
                sort_keys: false,
                ensure_ascii: false,
                separators: electricity_json::Separators::Default,
            },
        )
        .unwrap_or_default();
        contract.push_str("It must match this JSON Schema:\n");
        contract.push_str(&dumped);
        contract.push('\n');
    }
    format!("{}{contract}", prompt.trim_end_matches('\n'))
}

fn repair_prompt(result_file: &Path, errors: &[String]) -> String {
    let quoted = errors
        .iter()
        .map(|e| format!("- {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The result file {} is not valid:\n{quoted}\n\n\
         Write the corrected JSON result to {} now. Change nothing else.",
        result_file.display(),
        result_file.display()
    )
}

/// `plugins/agent.py::_read_result`. Only `FileNotFoundError`/`OSError`
/// become one of this turn's own, quoted "not valid" reasons; a
/// `UnicodeDecodeError` (`result_file.read_text(encoding="utf-8")`'s
/// own strict decode, with no `except` of its own) raises instead,
/// uncaught exactly as it is in Python.
fn read_result(
    result_file: &Path,
    result_schema: Option<&Value>,
) -> Result<(Value, Vec<String>), ToolError> {
    let bytes = match std::fs::read(result_file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Value::None, vec!["the file was not written".to_string()]));
        }
        Err(err) => {
            let path_display = result_file.display().to_string();
            return Ok((
                Value::None,
                vec![format!(
                    "the file could not be read: {}",
                    crate::fs::os_error(&err, &path_display).message
                )],
            ));
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| ToolError {
        py_class: PyExc::UnicodeDecodeError,
        message: crate::process::describe_utf8_error(&bytes),
        cause: ErrorCause::None,
    })?;
    let value = match electricity_json::loads(text) {
        Ok(value) => value,
        Err(err) => {
            return Ok((
                Value::None,
                vec![format!("the file is not valid JSON: {err}")],
            ));
        }
    };
    let Some(schema) = result_schema else {
        return Ok((value, Vec::new()));
    };
    let errors = match electricity_schema::result_schema_errors(schema, &value) {
        electricity_schema::SchemaErrorList::Valid => Vec::new(),
        electricity_schema::SchemaErrorList::Invalid(errors) => errors,
        electricity_schema::SchemaErrorList::SchemaError(message) => {
            vec![format!("(root): {message}")]
        }
    };
    Ok((value, errors))
}

fn invalid_result(
    session: &SessionState,
    engine_name: &str,
    transcript_path: &Path,
    live: &LiveTracker<'_>,
    result_file: &Path,
    errors: &[String],
    why: &str,
) -> ToolResult {
    let message = format!(
        "agent: result file {} is still invalid ({why}): {}",
        result_file.display(),
        errors.join("; ")
    );
    session.result(
        engine_name,
        transcript_path,
        live,
        Value::None,
        Some(message),
        false,
    )
}

fn turn_failure_result(
    session: &SessionState,
    engine_name: &str,
    transcript_path: &Path,
    live: &LiveTracker<'_>,
    failure: &TurnFailure,
) -> ToolResult {
    let mut result = session.result(
        engine_name,
        transcript_path,
        live,
        Value::None,
        Some(failure.message.clone()),
        false,
    );
    if failure.timed_out {
        if let Value::Dict(raw) = &mut result.raw {
            raw.insert(key("timed_out"), Value::Bool(true));
        }
    }
    result
}

/// `plugins/agent.py::_run_turn`'s own notes-or-raise decision.
fn turn_outcome_to_result(
    session: &SessionState,
    engine_name: &str,
    transcript_path: &Path,
    live: &LiveTracker<'_>,
    outcome: TurnOutcome,
) -> Result<ToolResult, ToolError> {
    let failure = match outcome {
        TurnOutcome::Raise(err) => return Err(err),
        TurnOutcome::Failed(failure) => failure,
    };
    let mut notes: Vec<String> = Vec::new();
    if let Some(id) = &session.session_id {
        if !id.is_empty() {
            notes.push(format!(
                "session {id} can be resumed with params['session']"
            ));
        }
    }
    if session.turn_completed {
        notes.push(format!("transcript: {}", transcript_path.display()));
    }
    if notes.is_empty() {
        return Err(ToolError::message(failure.message));
    }
    let message = format!("{} ({})", failure.message, notes.join("; "));
    Ok(turn_failure_result(
        session,
        engine_name,
        transcript_path,
        live,
        &TurnFailure {
            message,
            timed_out: failure.timed_out,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    engine_name: &str,
    options: &EngineOptions,
    thinking: &str,
    permission_mode: &str,
    trust_project_settings: bool,
    prompt: &str,
    session_id: Option<&str>,
    timeout_seconds: f64,
    scratch_dir: &Path,
    live: &LiveTracker<'_>,
    token: &CancellationToken,
) -> Result<AgentTurn, TurnOutcome> {
    if engine_name == PI {
        run_pi_turn(
            options,
            thinking,
            prompt,
            session_id,
            timeout_seconds,
            scratch_dir,
            live,
            token,
        )
        .await
    } else {
        run_claude_turn(
            options,
            permission_mode,
            trust_project_settings,
            prompt,
            session_id,
            timeout_seconds,
            live,
            token,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_agent_session(
    engine_name: &str,
    options: &EngineOptions,
    thinking: &str,
    permission_mode: &str,
    trust_project_settings: bool,
    prompt: &str,
    session_id: Option<&str>,
    result_file: Option<&Path>,
    result_schema: Option<&Value>,
    timeout_seconds: f64,
    scratch_dir: &Path,
    transcript_path: &Path,
    live: &LiveTracker<'_>,
    token: &CancellationToken,
) -> Result<ToolResult, ToolError> {
    let deadline = Instant::now()
        + Duration::try_from_secs_f64(timeout_seconds.max(0.0)).unwrap_or(Duration::MAX);
    let mut session = SessionState {
        session_id: session_id.map(str::to_string),
        turns: 0,
        tool_calls: 0,
        tokens_sent: None,
        tokens_received: None,
        cost_usd: None,
        repair_turn: false,
        turn_completed: false,
        last_text: String::new(),
    };

    let first_prompt = match result_file {
        Some(path) => with_result_contract(prompt, path, result_schema),
        None => prompt.to_string(),
    };
    live.start_turn("turn", session.session_id.as_deref(), Some("turn"));
    match run_turn(
        engine_name,
        options,
        thinking,
        permission_mode,
        trust_project_settings,
        &first_prompt,
        session.session_id.as_deref(),
        timeout_seconds,
        scratch_dir,
        live,
        token,
    )
    .await
    {
        Ok(turn) => {
            session.add(turn);
            live.sync_turn_totals(
                session.turns,
                session.tool_calls,
                session.tokens_sent,
                session.tokens_received,
                session.cost_usd,
            );
        }
        Err(outcome) => {
            return turn_outcome_to_result(&session, engine_name, transcript_path, live, outcome);
        }
    }

    let Some(result_file) = result_file else {
        return Ok(session.result(
            engine_name,
            transcript_path,
            live,
            Value::Str(session.last_text.clone()),
            None,
            true,
        ));
    };

    let (value, errors) = read_result(result_file, result_schema)?;
    if errors.is_empty() {
        return Ok(session.result(engine_name, transcript_path, live, value, None, true));
    }
    if session.session_id.is_none() {
        return Ok(invalid_result(
            &session,
            engine_name,
            transcript_path,
            live,
            result_file,
            &errors,
            "the CLI reported no session id to resume",
        ));
    }
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f64();
    if remaining < 1.0 {
        return Ok(invalid_result(
            &session,
            engine_name,
            transcript_path,
            live,
            result_file,
            &errors,
            "no time was left for a repair turn",
        ));
    }
    session.repair_turn = true;
    let repair = repair_prompt(result_file, &errors);
    live.start_turn("repair", session.session_id.as_deref(), Some("repair turn"));
    match run_turn(
        engine_name,
        options,
        thinking,
        permission_mode,
        trust_project_settings,
        &repair,
        session.session_id.as_deref(),
        remaining,
        scratch_dir,
        live,
        token,
    )
    .await
    {
        Ok(turn) => {
            session.add(turn);
            live.sync_turn_totals(
                session.turns,
                session.tool_calls,
                session.tokens_sent,
                session.tokens_received,
                session.cost_usd,
            );
        }
        Err(outcome) => {
            return turn_outcome_to_result(&session, engine_name, transcript_path, live, outcome);
        }
    }
    let (value, errors) = read_result(result_file, result_schema)?;
    if !errors.is_empty() {
        return Ok(invalid_result(
            &session,
            engine_name,
            transcript_path,
            live,
            result_file,
            &errors,
            "after one repair turn",
        ));
    }
    Ok(session.result(engine_name, transcript_path, live, value, None, true))
}

// ---------------- plugin ----------------

#[derive(Debug, Default)]
pub struct AgentPlugin {
    default_engine: String,
    pi_binary: Option<String>,
    claude_binary: Option<String>,
}

impl AgentPlugin {
    fn resolve_binary(&self, engine_name: &str) -> Result<String, ToolError> {
        let configured = if engine_name == PI {
            self.pi_binary.as_deref()
        } else {
            self.claude_binary.as_deref()
        };
        if let Some(configured) = configured {
            let (path, error) = expand_and_validate(configured);
            return match error {
                None => Ok(path),
                Some(error) => Err(ToolError {
                    py_class: PyExc::RuntimeError,
                    message: format!(
                        "agent: configured runtime.plugins.agent.{engine_name}.binary={} \
                         {error} (resolved: {path}).",
                        Value::Str(configured.to_string()).py_repr()
                    ),
                    cause: ErrorCause::None,
                }),
            };
        }
        let default = if engine_name == PI { "pi" } else { "claude" };
        match crate::process::resolve_binary(&[default]) {
            Some(found) => Ok(found),
            None => Err(ToolError {
                py_class: PyExc::RuntimeError,
                message: format!(
                    "agent: {} (engine {}) not found on PATH. Install it, or set \
                     runtime.plugins.agent.{engine_name}.binary.",
                    Value::Str(default.to_string()).py_repr(),
                    Value::Str(engine_name.to_string()).py_repr()
                ),
                cause: ErrorCause::None,
            }),
        }
    }
}

#[async_trait(?Send)]
impl ToolPlugin for AgentPlugin {
    fn name(&self) -> &str {
        "agent"
    }

    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let params = params.as_dict().ok_or_else(|| {
            value_error("agent requires params['prompt'] as a non-empty string.".to_string())
        })?;
        let prompt = match get(params, "prompt") {
            Some(Value::Str(s)) if !py_strip(s).is_empty() => s.clone(),
            _ => {
                return Err(value_error(
                    "agent requires params['prompt'] as a non-empty string.".to_string(),
                ));
            }
        };
        let engine_name = {
            let name = optional_str(params, "engine")?;
            if name.is_empty() {
                self.default_engine.clone()
            } else {
                name
            }
        };
        if engine_name != PI && engine_name != CLAUDE_CODE {
            return Err(value_error(format!(
                "agent: unknown engine {}; expected one of {}.",
                Value::Str(engine_name.clone()).py_repr(),
                Value::List(vec![Value::from(CLAUDE_CODE), Value::from(PI)]).py_repr()
            )));
        }
        let cwd = {
            let raw = optional_str(params, "cwd")?;
            if raw.is_empty() {
                std::env::current_dir()
                    .map_err(|err| ToolError::message(format!("agent: {err}")))?
                    .to_string_lossy()
                    .into_owned()
            } else {
                raw
            }
        };
        let cwd_path = shellexpand_home(&cwd);
        if !cwd_path.is_dir() {
            return Err(value_error(format!(
                "agent: cwd {} is not a directory.",
                Value::Str(cwd.clone()).py_repr()
            )));
        }
        let thinking = optional_str(params, "thinking")?;
        let permission_mode = optional_str(params, "permission_mode")?;
        if !thinking.is_empty() && engine_name != PI {
            return Err(value_error(
                "agent: params['thinking'] applies to engine 'pi' only.".to_string(),
            ));
        }
        if !permission_mode.is_empty() && engine_name != CLAUDE_CODE {
            return Err(value_error(
                "agent: params['permission_mode'] applies to engine 'claude_code' only."
                    .to_string(),
            ));
        }
        let trust_project_settings = trust_project_settings_param(params, &engine_name)?;
        let tools = tool_names(params, "tools")?;
        if matches!(get(params, "tools"), Some(v) if !matches!(v, Value::None)) && tools.is_empty()
        {
            return Err(value_error(
                "agent: params['tools'] is empty; leave it out to keep the engine's default tools."
                    .to_string(),
            ));
        }
        let exclude_tools = tool_names(params, "exclude_tools")?;
        let mut permission_mode = permission_mode;
        if engine_name == CLAUDE_CODE && !tools.is_empty() && permission_mode.is_empty() {
            permission_mode = TOOLS_PERMISSION_MODE.to_string();
        }
        let result_file_param = optional_str(params, "result_file")?;
        let result_schema = match get(params, "result_schema") {
            None | Some(Value::None) => None,
            Some(value) => Some(value.clone()),
        };
        if let Some(schema) = &result_schema {
            if result_file_param.is_empty() {
                return Err(value_error(
                    "agent: params['result_schema'] needs params['result_file'].".to_string(),
                ));
            }
            if !matches!(schema, Value::Dict(_)) {
                return Err(value_error(
                    "agent: params['result_schema'] must be a JSON Schema object.".to_string(),
                ));
            }
            if let Some(message) = electricity_schema::schema_well_formedness_error(schema) {
                return Err(value_error(format!(
                    "agent: params['result_schema'] is not a valid JSON Schema: {message}"
                )));
            }
        }
        let result_file = if result_file_param.is_empty() {
            None
        } else {
            Some(resolve_relative(&cwd_path, &result_file_param))
        };
        let transcript_param = optional_str(params, "transcript")?;
        let transcript_path_param = if transcript_param.is_empty() {
            None
        } else {
            Some(resolve_relative(&cwd_path, &transcript_param))
        };
        if let Some(path) = &transcript_path_param {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    value_error(format!(
                        "agent: params['transcript'] {}'s directory could not be created: {}",
                        Value::Str(path.display().to_string()).py_repr(),
                        crate::fs::os_error(&err, &parent.display().to_string()).message
                    ))
                })?;
            }
            if path.is_dir() {
                return Err(value_error(format!(
                    "agent: params['transcript'] {} is a directory.",
                    Value::Str(path.display().to_string()).py_repr()
                )));
            }
        }
        let unset_env = match get(params, "unset_env") {
            None | Some(Value::None) => DEFAULT_UNSET_ENV
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            _ => str_list(params, "unset_env")?,
        };
        let session_param = optional_str(params, "session")?;
        let extra_args = str_list(params, "extra_args")?;
        let env_pairs = str_mapping(params, "env")?;
        let model = optional_str(params, "model")?;

        let mut prompt = prompt;
        let mut claude_project_instructions: Option<String> = None;
        if engine_name == CLAUDE_CODE && !trust_project_settings {
            if let Some((path, text)) = project_instructions(&cwd_path) {
                prompt.push_str(PROJECT_INSTRUCTIONS_HEADER);
                prompt.push_str(&text);
                claude_project_instructions = Some(path);
            }
        }

        let binary = self.resolve_binary(&engine_name)?;
        let options = EngineOptions {
            binary,
            cwd: cwd_path.to_string_lossy().into_owned(),
            unset_env,
            env: env_pairs,
            model,
            tools,
            exclude_tools,
            extra_args,
        };

        if let Some(path) = &result_file {
            if let Err(err) = std::fs::remove_file(path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    let path_display = path.display().to_string();
                    return Err(value_error(format!(
                        "agent: result_file {} could not be cleared before the session: {}",
                        Value::Str(path_display.clone()).py_repr(),
                        crate::fs::os_error(&err, &path_display).message
                    )));
                }
            }
        }

        let scratch = ScratchDir::new(&engine_name)
            .map_err(|err| ToolError::message(format!("agent: {err}")))?;
        let transcript_path = transcript_path_param
            .clone()
            .unwrap_or_else(|| scratch.path.join(TRANSCRIPT_NAME));

        let publish = |value: Value| {
            if let Some(f) = call.on_agent_live {
                f(value);
            }
        };
        let live = LiveTracker::new(
            &engine_name,
            &options.model,
            i64::from(call.agent_attempt),
            &transcript_path,
            &publish,
            call.agent_earlier_attempts,
        );

        let session_id = if session_param.is_empty() {
            None
        } else {
            Some(session_param)
        };
        let result = run_agent_session(
            &engine_name,
            &options,
            &thinking,
            &permission_mode,
            trust_project_settings,
            &prompt,
            session_id.as_deref(),
            result_file.as_deref(),
            result_schema.as_ref(),
            f64::from(call.timeout_seconds),
            &scratch.path,
            &transcript_path,
            &live,
            call.token,
        )
        .await;

        let keep_scratch = transcript_path_param.is_none()
            && matches!(
                &result,
                Ok(ToolResult { raw: Value::Dict(raw), .. })
                    if !matches!(raw.get(&key("transcript")), None | Some(Value::None))
            );
        if !keep_scratch {
            let _ = std::fs::remove_dir_all(&scratch.path);
        }
        std::mem::forget(scratch);

        let mut result = result?;
        if engine_name == CLAUDE_CODE {
            if let Value::Dict(raw) = &mut result.raw {
                raw.insert(
                    key("permission_mode"),
                    if permission_mode.is_empty() {
                        Value::None
                    } else {
                        Value::Str(permission_mode)
                    },
                );
                raw.insert(
                    key("project_settings"),
                    Value::Str(
                        if trust_project_settings {
                            "trusted"
                        } else {
                            "isolated"
                        }
                        .to_string(),
                    ),
                );
                raw.insert(
                    key("project_instructions"),
                    claude_project_instructions
                        .map(Value::Str)
                        .unwrap_or(Value::None),
                );
            }
        }
        Ok(result)
    }

    fn check(&self) -> CheckResult {
        let mut states = Vec::new();
        let mut missing = Vec::new();
        for (engine_name, default) in [(PI, "pi"), (CLAUDE_CODE, "claude")] {
            match self.resolve_binary(engine_name) {
                Ok(binary) => states.push(format!("{engine_name}: {binary}")),
                Err(err) => {
                    let message = err.message.strip_prefix("agent: ").unwrap_or(&err.message);
                    states.push(message.to_string());
                    missing.push(format!("binary:{default}"));
                }
            }
        }
        let ok = missing.len() < 2;
        CheckResult {
            ok,
            missing: if ok { Vec::new() } else { missing },
            message: Some(states.join("; ")),
        }
    }
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(engine_name: &str) -> std::io::Result<Self> {
        let guard =
            crate::agent_cli::TempDirGuard::new(&format!("electricity-agent-{engine_name}-"))?;
        let path = guard.path().to_path_buf();
        std::mem::forget(guard);
        Ok(ScratchDir { path })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn shellexpand_home(path: &str) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if path == "~" {
            return PathBuf::from(home);
        }
        if let Some(rest) = path.strip_prefix("~/") {
            let mut p = PathBuf::from(home);
            p.push(rest);
            return p;
        }
    }
    PathBuf::from(path)
}

/// `(cwd_path / path).resolve()`: `result_file`/`transcript`, the two
/// params cof resolves (`cwd_path` itself stays as `expanduser()` left
/// it).
fn resolve_relative(base: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    python_resolve(&joined)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `pathlib.Path.resolve()`'s own `strict=False` default: every symlink
/// in the longest existing ancestor is followed (so a symlinked `/tmp`
/// resolves to `/private/tmp` on macOS), and whatever doesn't exist yet
/// (a `result_file`/`transcript` effect's own params usually name a
/// path not yet written) is appended lexically -- `.`/`..` collapsed,
/// never resolved against the filesystem, exactly as Python leaves a
/// nonexistent tail.
fn python_resolve(path: &Path) -> PathBuf {
    let normalized = normalize_lexically(path);
    if let Ok(resolved) = normalized.canonicalize() {
        return resolved;
    }
    let mut existing = normalized.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(resolved) => {
                let mut out = resolved;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    return normalized;
                };
                tail.push(name);
                match existing.parent() {
                    Some(parent) => existing = parent.to_path_buf(),
                    None => return normalized,
                }
            }
        }
    }
}

/// `plugins/agent.py::make_plugin`.
pub fn make_plugin(cfg: &Value) -> Result<Box<dyn ToolPlugin>, ToolError> {
    let empty = Dict::new();
    let cfg_dict = cfg.as_dict().unwrap_or(&empty);
    let engine = match cfg_dict.get(&key("engine")) {
        None | Some(Value::None) => DEFAULT_ENGINE.to_string(),
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        Some(Value::Str(_)) => DEFAULT_ENGINE.to_string(),
        Some(other) => {
            return Err(value_error(format!(
                "runtime.plugins.agent.engine must be one of ['claude_code', 'pi'], got {}.",
                other.py_repr()
            )));
        }
    };
    if engine != PI && engine != CLAUDE_CODE {
        return Err(value_error(format!(
            "runtime.plugins.agent.engine must be one of ['claude_code', 'pi'], got {}.",
            Value::Str(engine).py_repr()
        )));
    }
    let mut binaries: [Option<String>; 2] = [None, None];
    for (index, engine_name) in [PI, CLAUDE_CODE].into_iter().enumerate() {
        let Some(block) = cfg_dict.get(&key(engine_name)) else {
            continue;
        };
        if matches!(block, Value::None) {
            continue;
        }
        let Value::Dict(block) = block else {
            return Err(value_error(format!(
                "runtime.plugins.agent.{engine_name} must be a mapping."
            )));
        };
        if let Some(Value::Str(binary)) = block.get(&key("binary")) {
            if !binary.is_empty() {
                binaries[index] = Some(binary.clone());
            }
        }
    }
    Ok(Box::new(AgentPlugin {
        default_engine: engine,
        pi_binary: binaries[0].take(),
        claude_binary: binaries[1].take(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;

    static NONE: Value = Value::None;

    fn call(token: &CancellationToken) -> ToolCall<'_> {
        ToolCall {
            timeout_seconds: 5,
            config: &NONE,
            token,
            armed: false,
            secret_headers: &[],
            scrub: None,
            on_live: None,
            on_agent_live: None,
            agent_attempt: 1,
            agent_earlier_attempts: None,
            limited: false,
            confine_to: None,
        }
    }

    fn params(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::from(k), v);
        }
        Value::Dict(dict)
    }

    /// A plugin whose own configured binary never resolves to anything
    /// real -- every test here that needs to clear every param check and
    /// reach `resolve_binary` must still never start a real `pi`/`claude`
    /// process, regardless of what the host
    /// running this test happens to have on `PATH`.
    fn plugin_with_unresolvable_binaries() -> AgentPlugin {
        AgentPlugin {
            default_engine: PI.to_string(),
            pi_binary: Some("/nonexistent/circuitry-test-agent-pi".to_string()),
            claude_binary: Some("/nonexistent/circuitry-test-agent-claude".to_string()),
        }
    }

    fn engine_options(
        tools: &[&str],
        exclude_tools: &[&str],
        extra_args: &[&str],
    ) -> EngineOptions {
        EngineOptions {
            binary: "pi".to_string(),
            cwd: ".".to_string(),
            unset_env: Vec::new(),
            env: Vec::new(),
            model: String::new(),
            tools: tools.iter().map(|s| s.to_string()).collect(),
            exclude_tools: exclude_tools.iter().map(|s| s.to_string()).collect(),
            extra_args: extra_args.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ---- pi_extra_args ----

    #[test]
    fn pi_extra_args_is_empty_with_no_tools_or_extra_args() {
        let options = engine_options(&[], &[], &[]);
        assert_eq!(pi_extra_args(&options), Vec::<String>::new());
    }

    #[test]
    fn pi_extra_args_lists_tools_then_exclude_tools_then_extra_args() {
        let options = engine_options(&["read", "write"], &["shell"], &["--verbose"]);
        assert_eq!(
            pi_extra_args(&options),
            vec![
                "--tools".to_string(),
                "read,write".to_string(),
                "--exclude-tools".to_string(),
                "shell".to_string(),
                "--verbose".to_string(),
            ]
        );
    }

    // ---- claude_extra_args ----

    #[test]
    fn claude_extra_args_isolates_project_settings_by_default() {
        let options = engine_options(&[], &[], &[]);
        assert_eq!(
            claude_extra_args(&options, "", false),
            vec!["--setting-sources".to_string(), "user".to_string()]
        );
    }

    #[test]
    fn claude_extra_args_omits_setting_sources_once_project_settings_are_trusted() {
        let options = engine_options(&[], &[], &[]);
        assert_eq!(claude_extra_args(&options, "", true), Vec::<String>::new());
    }

    #[test]
    fn claude_extra_args_tools_become_builtin_names_plus_the_full_allowed_list() {
        let options = engine_options(&["read", "write", "mcp__server__tool"], &[], &[]);
        assert_eq!(
            claude_extra_args(&options, "", true),
            vec![
                "--tools".to_string(),
                "read,write".to_string(),
                "--allowedTools".to_string(),
                "read".to_string(),
                "write".to_string(),
                "mcp__server__tool".to_string(),
            ]
        );
    }

    #[test]
    fn claude_extra_args_exclude_tools_become_disallowed_tools_verbatim() {
        let options = engine_options(&[], &["shell", "mcp__server__danger"], &[]);
        assert_eq!(
            claude_extra_args(&options, "", true),
            vec![
                "--disallowedTools".to_string(),
                "shell".to_string(),
                "mcp__server__danger".to_string(),
            ]
        );
    }

    #[test]
    fn claude_extra_args_permission_mode_and_extra_args_come_last_and_in_order() {
        let options = engine_options(&["read"], &["shell"], &["--debug"]);
        assert_eq!(
            claude_extra_args(&options, "dontAsk", true),
            vec![
                "--tools".to_string(),
                "read".to_string(),
                "--allowedTools".to_string(),
                "read".to_string(),
                "--permission-mode".to_string(),
                "dontAsk".to_string(),
                "--disallowedTools".to_string(),
                "shell".to_string(),
                "--debug".to_string(),
            ]
        );
    }

    // ---- builtin_tool_names ----

    #[test]
    fn builtin_tool_names_drops_mcp_tools_and_trailing_parens_and_dedupes() {
        let names = builtin_tool_names(&[
            "read".to_string(),
            "read".to_string(),
            "bash(ls)".to_string(),
            "mcp__server__tool".to_string(),
        ]);
        assert_eq!(names, vec!["read".to_string(), "bash".to_string()]);
    }

    // ---- permission mode default (execute()'s own pre-dispatch logic) ----

    #[tokio::test]
    async fn permission_mode_defaults_to_dont_ask_once_tools_are_set_for_claude_code() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let params = params(vec![
            ("prompt", Value::from("do it")),
            ("engine", Value::from(CLAUDE_CODE)),
            ("tools", Value::List(vec![Value::from("read")])),
        ]);
        let err = plugin.execute(params, &call).await.unwrap_err();
        // Cleared every param check, including `tools`'s own emptiness
        // check, and reached binary resolution -- the one place this
        // test's own unresolvable, configured binary can fail safely,
        // without ever starting a real `pi`/`claude` process.
        assert!(err.message.contains("circuitry-test-agent-claude"));
    }

    // ---- param validation errors ----

    #[tokio::test]
    async fn prompt_must_be_a_non_empty_string() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(params(vec![("prompt", Value::from("   "))]), &call)
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent requires params['prompt'] as a non-empty string."
        );
    }

    #[tokio::test]
    async fn an_unknown_engine_is_an_error() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("engine", Value::from("codex")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: unknown engine 'codex'; expected one of ['claude_code', 'pi']."
        );
    }

    #[tokio::test]
    async fn cwd_must_be_a_directory() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("cwd", Value::from("/nonexistent/circuitry-test-agent-cwd")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: cwd '/nonexistent/circuitry-test-agent-cwd' is not a directory."
        );
    }

    #[tokio::test]
    async fn thinking_applies_to_pi_only() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("engine", Value::from(CLAUDE_CODE)),
                    ("thinking", Value::from("high")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['thinking'] applies to engine 'pi' only."
        );
    }

    #[tokio::test]
    async fn permission_mode_applies_to_claude_code_only() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("permission_mode", Value::from("dontAsk")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['permission_mode'] applies to engine 'claude_code' only."
        );
    }

    #[tokio::test]
    async fn trust_project_settings_must_be_a_bool() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("engine", Value::from(CLAUDE_CODE)),
                    ("trust_project_settings", Value::from("sometimes")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['trust_project_settings'] must be true or false."
        );
    }

    #[tokio::test]
    async fn trust_project_settings_applies_to_claude_code_only() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("trust_project_settings", Value::from(true)),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['trust_project_settings'] applies to engine 'claude_code' only."
        );
    }

    #[tokio::test]
    async fn an_empty_tools_list_is_an_error() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("tools", Value::List(Vec::new())),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['tools'] is empty; leave it out to keep the engine's default tools."
        );
    }

    #[tokio::test]
    async fn result_schema_needs_result_file() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("result_schema", Value::Dict(Dict::new())),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['result_schema'] needs params['result_file']."
        );
    }

    #[tokio::test]
    async fn result_schema_must_be_an_object() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("result_file", Value::from("result.json")),
                    ("result_schema", Value::from("nope")),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "agent: params['result_schema'] must be a JSON Schema object."
        );
    }

    #[tokio::test]
    async fn result_schema_must_be_a_well_formed_json_schema() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let mut schema = Dict::new();
        schema.insert(Value::from("type"), Value::from(123));
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("result_file", Value::from("result.json")),
                    ("result_schema", Value::Dict(schema)),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert!(
            err.message
                .starts_with("agent: params['result_schema'] is not a valid JSON Schema: ")
        );
    }

    /// `result_schema: null` (the shape
    /// a rendered `params_json` overlay can carry for an omitted key) is
    /// absent, exactly like a missing key -- never `params['result_schema']
    /// needs params['result_file']`. With no `result_file` either, this
    /// clears every param check and reaches binary resolution, which this
    /// test's own unresolvable, configured binary fails safely.
    #[tokio::test]
    async fn result_schema_null_is_treated_as_absent() {
        let token = CancellationToken::new();
        let plugin = plugin_with_unresolvable_binaries();
        let call = call(&token);
        let err = plugin
            .execute(
                params(vec![
                    ("prompt", Value::from("do it")),
                    ("result_schema", Value::None),
                ]),
                &call,
            )
            .await
            .unwrap_err();
        assert!(!err.message.contains("result_schema"));
        assert!(err.message.contains("circuitry-test-agent-pi"));
    }

    // ---- project_instructions (CLAUDE.md append) ----

    #[test]
    fn project_instructions_reads_claude_md_from_the_git_root() {
        let dir = crate::agent_cli::TempDirGuard::new("circuitry-test-agent-claude-md-").unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "Follow these project rules.").unwrap();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let (path, text) = project_instructions(&nested).expect("CLAUDE.md should be found");
        assert_eq!(text, "Follow these project rules.");
        assert!(path.ends_with("CLAUDE.md"));
    }

    #[test]
    fn project_instructions_is_none_with_no_claude_md() {
        let dir =
            crate::agent_cli::TempDirGuard::new("circuitry-test-agent-no-claude-md-").unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();

        assert!(project_instructions(root).is_none());
    }

    #[test]
    fn project_instructions_is_none_for_a_blank_claude_md() {
        let dir =
            crate::agent_cli::TempDirGuard::new("circuitry-test-agent-blank-claude-md-").unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "   \n  ").unwrap();

        assert!(project_instructions(root).is_none());
    }

    // ---- read_result ----

    #[test]
    fn read_result_reports_every_violation_sorted_by_path_and_capped() {
        let dir = crate::agent_cli::TempDirGuard::new("circuitry-test-agent-read-result-").unwrap();
        let result_file = dir.path().join("result.json");
        std::fs::write(&result_file, r#"{"b": 1, "a": 1, "nested": {"x": 1}}"#).unwrap();
        let mut properties = Dict::new();
        properties.insert(Value::from("a"), Value::from(serde_dict("string")));
        properties.insert(Value::from("b"), Value::from(serde_dict("string")));
        let mut nested_properties = Dict::new();
        nested_properties.insert(Value::from("x"), Value::from(serde_dict("string")));
        let mut nested = Dict::new();
        nested.insert(Value::from("type"), Value::from("object"));
        nested.insert(Value::from("properties"), Value::Dict(nested_properties));
        properties.insert(Value::from("nested"), Value::Dict(nested));
        let mut schema = Dict::new();
        schema.insert(Value::from("type"), Value::from("object"));
        schema.insert(Value::from("properties"), Value::Dict(properties));
        let schema = Value::Dict(schema);

        let (_value, errors) = read_result(&result_file, Some(&schema)).unwrap();
        assert_eq!(
            errors,
            vec![
                "a: 1 is not of type 'string'".to_string(),
                "b: 1 is not of type 'string'".to_string(),
                "nested/x: 1 is not of type 'string'".to_string(),
            ]
        );
    }

    fn serde_dict(type_name: &str) -> Dict {
        let mut d = Dict::new();
        d.insert(Value::from("type"), Value::from(type_name));
        d
    }

    #[test]
    fn read_result_raises_on_invalid_utf8() {
        let dir = crate::agent_cli::TempDirGuard::new("circuitry-test-agent-utf8-").unwrap();
        let result_file = dir.path().join("result.json");
        std::fs::write(&result_file, [0xff, 0xfe, 0xfd]).unwrap();

        let err = read_result(&result_file, None).unwrap_err();
        assert_eq!(err.py_class, PyExc::UnicodeDecodeError);
    }

    #[test]
    fn resolve_relative_follows_symlinks_in_existing_ancestors() {
        let dir = crate::agent_cli::TempDirGuard::new("circuitry-test-agent-resolve-").unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let resolved = resolve_relative(&link, "result.json");
        assert_eq!(resolved, real.canonicalize().unwrap().join("result.json"));
    }
}
