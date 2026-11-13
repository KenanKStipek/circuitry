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
//! Known, documented differences from cof (see `electricity/DESIGN.md`
//! §13): `result_schema` reports at most the first violation (never the
//! full `jsonschema.iter_errors` list), labelled `(root)` rather than its
//! own JSON Pointer; `meta.live` is always cleared when the attempt ends,
//! even when the run itself is interrupted (cof leaves it in place then,
//! as the one record of that attempt's session and transcript); and
//! `meta.live.earlier_attempts` (a retried attempt's own token totals
//! from earlier attempts of the same pass) is never present.

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
const MAX_QUOTED_ERRORS: usize = 20;
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

fn write_prompt_file(scratch_dir: &Path, prompt: &str) -> std::io::Result<PathBuf> {
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
    let mut file = std::fs::File::create(&path)?;
    file.write_all(prompt.as_bytes())?;
    Ok(path)
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
    let prompt_file = write_prompt_file(scratch_dir, prompt)
        .map_err(|err| TurnOutcome::Raise(ToolError::message(format!("agent: {err}"))))?;
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
    let run_result = run_agent_cli(
        &cmd,
        &options.unset_env,
        &options.env,
        timeout_seconds,
        Vec::new(),
        Some(PathBuf::from(&options.cwd)),
        token,
        Some(&on_line),
    )
    .await;
    let _ = std::fs::remove_file(&prompt_file);
    let proc = run_result.map_err(|err| TurnOutcome::Failed(TurnFailure::from(err)))?;
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
    let run_result = run_agent_cli(
        &cmd,
        &options.unset_env,
        &options.env,
        timeout_seconds,
        prompt.as_bytes().to_vec(),
        Some(PathBuf::from(&options.cwd)),
        token,
        Some(&on_line),
    )
    .await;
    let proc = run_result.map_err(|err| TurnOutcome::Failed(TurnFailure::from(err)))?;
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

/// `plugins/agent.py::_read_result`. Known divergence (DESIGN.md §13):
/// reports at most the first schema violation, labelled `(root)` rather
/// than cof's own JSON-pointer-shaped path -- see the module doc comment.
fn read_result(result_file: &Path, result_schema: Option<&Value>) -> (Value, Vec<String>) {
    let bytes = match std::fs::read(result_file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (Value::None, vec!["the file was not written".to_string()]);
        }
        Err(err) => {
            return (
                Value::None,
                vec![format!("the file could not be read: {err}")],
            );
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let value = match electricity_json::loads(&text) {
        Ok(value) => value,
        Err(err) => {
            return (
                Value::None,
                vec![format!("the file is not valid JSON: {err}")],
            );
        }
    };
    let Some(schema) = result_schema else {
        return (value, Vec::new());
    };
    let mut errors = match electricity_schema::validate_prompt_schema(schema, &value) {
        electricity_schema::PromptSchemaCheck::Valid => Vec::new(),
        electricity_schema::PromptSchemaCheck::Invalid(message) => {
            vec![format!("(root): {message}")]
        }
        electricity_schema::PromptSchemaCheck::SchemaError(message) => {
            vec![format!("(root): {message}")]
        }
    };
    if errors.len() > MAX_QUOTED_ERRORS {
        let more = errors.len() - MAX_QUOTED_ERRORS;
        errors.truncate(MAX_QUOTED_ERRORS);
        errors.push(format!("... and {more} more"));
    }
    (value, errors)
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

    let (value, errors) = read_result(result_file, result_schema);
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
    let (value, errors) = read_result(result_file, result_schema);
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
        let result_schema = get(params, "result_schema").cloned();
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
                        "agent: params['transcript'] {} directory could not be created: {err}",
                        Value::Str(path.display().to_string()).py_repr()
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
                    return Err(value_error(format!(
                        "agent: result_file {} could not be cleared before the session: {err}",
                        Value::Str(path.display().to_string()).py_repr()
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
            None,
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

fn resolve_relative(base: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
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
