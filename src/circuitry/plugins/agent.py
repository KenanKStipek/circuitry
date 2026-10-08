"""Agent tool plugin: run a delegated coding-agent session as a tool effect.

One effect is one headless session of pi or Claude Code, with its own tools
on, working in ``cwd`` for as long as the effect's ``timeout_ms`` allows.
The command building, the clean child environment, the run in its own
process group (a timeout or a cancelled run stops the CLI and everything it
started) and the output parsing all come from :mod:`circuitry.agent_cli`;
this module adds the session's contract on top:

* ``result_file`` + ``result_schema``: the agent is told to write a JSON
  file, which is then read and validated. A missing or invalid file gets
  exactly one repair turn in the **same** session (pi ``--session``,
  Claude Code ``--resume``) quoting the errors; still invalid after that,
  the effect fails with them. ``value`` is the parsed JSON. Without
  ``result_file``, ``value`` is the final assistant text.
* ``raw``: ``engine``, ``session_id``, ``turns``, ``tool_calls``,
  ``tokens`` (``sent``/``received``), ``cost`` when the CLI reports one,
  ``repair_turn``, and ``transcript`` — the path of a compact log this
  plugin writes (one line per tool call and per reply). The transcript
  itself never goes into state. Claude Code adds ``permission_mode``,
  ``project_settings`` and ``project_instructions``.

The agent runs with the user's own permissions and is not sandboxed; the
engine's tool lists are the way to narrow it. For pi, ``tools`` is an
allowlist and ``exclude_tools`` a denylist. For Claude Code, ``tools`` is an
allowlist too (``--tools``, its entries pre-approved with ``--allowedTools``
and run under ``--permission-mode dontAsk`` unless ``permission_mode`` says
otherwise), and ``exclude_tools`` (``--disallowedTools``) is the hard deny in
every permission mode. Unless ``trust_project_settings`` is true, a Claude
Code session also ignores the repository's project settings (hooks, the API
key helper, its MCP servers) and gets the repository root's ``CLAUDE.md``
appended to its first prompt. See ``docs/plugins/agent.md``.

Params:
  - ``prompt`` (required, str): multi-line; written to a temporary file
    (pi attaches it with ``@<file>``) or sent on stdin (Claude Code), never
    put in argv.
  - ``engine`` (optional): ``pi`` | ``claude_code``; default
    ``runtime.plugins.agent.engine``, else ``pi``.
  - ``cwd`` (optional, str): where the session runs; default the current
    directory.
  - ``model`` (optional, str); ``thinking`` (pi only); ``permission_mode`` and
    ``trust_project_settings`` (both claude_code only).
  - ``tools`` / ``exclude_tools`` (optional, list[str]; ``tools`` may not be
    empty): pi ``--tools`` / ``--exclude-tools``; Claude Code ``--tools`` with
    ``--allowedTools``, and ``--disallowedTools``.
  - ``session`` (optional, str): resume this session id.
  - ``extra_args`` (optional, list[str]); ``env`` (optional, mapping of
    variables to add); ``unset_env`` (optional, list[str]: replaces the
    default ``ANTHROPIC_API_KEY``/``ANTHROPIC_AUTH_TOKEN``).
  - ``result_file`` (optional, str, relative to ``cwd``) and
    ``result_schema`` (optional, JSON Schema object; needs ``result_file``).

Config, ``runtime.plugins.agent``: ``engine`` (the default engine) and a
block per engine, ``pi`` / ``claude_code``, each with an optional absolute
``binary`` that replaces the ``PATH`` search for ``pi`` / ``claude``.
"""

from __future__ import annotations

import json
import os
import shutil
import tempfile
import time
from collections.abc import Mapping
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Protocol

import jsonschema

from ..agent_cli import (
    DEFAULT_UNSET_ENV,
    AgentCliError,
    child_env,
    claude_command,
    parse_claude_output,
    parse_pi_output,
    pi_command,
    run_agent_cli,
)
from ..preflight import CheckResult
from ._subprocess import _expand_and_validate, resolve_binary
from .base import ToolResult

PI = "pi"
CLAUDE_CODE = "claude_code"

#: Engine name -> the binary searched on ``PATH`` when none is configured.
ENGINE_BINARIES: dict[str, str] = {PI: "pi", CLAUDE_CODE: "claude"}

DEFAULT_ENGINE = PI

#: The permission mode when ``tools`` is set but ``permission_mode`` is not: a call off the list is denied.
_TOOLS_PERMISSION_MODE = "dontAsk"

_PROJECT_INSTRUCTIONS_HEADER = (
    "\n\n---\n\nProject instructions from the repository's CLAUDE.md "
    "(Claude Code does not load it in this session):\n\n"
)

#: How many schema errors a repair prompt or a failure message quotes.
_MAX_QUOTED_ERRORS = 20

#: The compact transcript's file name inside the effect's scratch directory.
TRANSCRIPT_NAME = "transcript.log"

#: How much of a reply or a tool call's arguments one transcript line keeps.
_TRANSCRIPT_LINE_CHARS = 300


@dataclass(frozen=True)
class AgentTurn:
    """One CLI call of a session: its final text and what it did."""

    text: str
    session_id: str | None
    turns: int
    tool_calls: int
    tokens_sent: int | None = None
    tokens_received: int | None = None
    cost_usd: float | None = None
    #: Compact transcript lines: one per tool call and per reply.
    transcript: tuple[str, ...] = ()


class AgentEngine(Protocol):
    """Runs one turn of a coding-agent session through its CLI."""

    @property
    def name(self) -> str: ...

    def run_turn(
        self,
        prompt: str,
        *,
        session_id: str | None,
        timeout_seconds: float,
        scratch_dir: Path,
    ) -> AgentTurn: ...


@dataclass(frozen=True)
class _EngineOptions:
    """What both engines take from the effect's params."""

    binary: str
    cwd: str
    env: dict[str, str]
    model: str = ""
    tools: tuple[str, ...] = ()
    exclude_tools: tuple[str, ...] = ()
    extra_args: tuple[str, ...] = ()


@dataclass(frozen=True)
class PiEngine:
    """``pi -p --mode json`` with its tools on and the session saved."""

    options: _EngineOptions
    thinking: str = ""
    name: str = PI

    def run_turn(
        self,
        prompt: str,
        *,
        session_id: str | None,
        timeout_seconds: float,
        scratch_dir: Path,
    ) -> AgentTurn:
        prompt_file = _write_prompt_file(scratch_dir, prompt)
        try:
            args: list[str] = []
            if self.options.tools:
                args += ["--tools", ",".join(self.options.tools)]
            if self.options.exclude_tools:
                args += ["--exclude-tools", ",".join(self.options.exclude_tools)]
            cmd = pi_command(
                binary=self.options.binary,
                prompt_file=str(prompt_file),
                model=self.options.model,
                thinking=self.thinking,
                tools=True,
                session_id=session_id,
                persist_session=True,
                extra_args=[*args, *self.options.extra_args],
            )
            proc = run_agent_cli(
                cmd, env=self.options.env, timeout_seconds=timeout_seconds, cwd=self.options.cwd
            )
        finally:
            prompt_file.unlink(missing_ok=True)
        result = parse_pi_output(proc)
        turns = 0
        tool_calls = 0
        transcript: list[str] = []
        for event in result.raw.get("events", []):
            kind = event.get("type")
            if kind == "tool_execution_start":
                tool_calls += 1
                transcript.append(_tool_line(event.get("toolName"), event.get("args")))
                continue
            message = event.get("message")
            if kind != "message_end" or not isinstance(message, dict):
                continue
            if message.get("role") != "assistant":
                continue
            turns += 1
            text = _content_text(message.get("content"))
            if text:
                transcript.append(_reply_line(text))
        return AgentTurn(
            text=result.text,
            session_id=result.session_id or session_id,
            turns=turns,
            tool_calls=tool_calls,
            tokens_sent=result.tokens_sent,
            tokens_received=result.tokens_received,
            cost_usd=result.cost_usd,
            transcript=tuple(transcript),
        )


@dataclass(frozen=True)
class ClaudeCodeEngine:
    """``claude -p --output-format stream-json --verbose`` with its tools on
    and the session saved; the prompt goes on stdin."""

    options: _EngineOptions
    permission_mode: str = ""
    trust_project_settings: bool = False
    name: str = CLAUDE_CODE

    def run_turn(
        self,
        prompt: str,
        *,
        session_id: str | None,
        timeout_seconds: float,
        scratch_dir: Path,
    ) -> AgentTurn:
        del scratch_dir  # the prompt goes on stdin
        args: list[str] = []
        if not self.trust_project_settings:
            args += ["--setting-sources", "user"]
        if self.options.tools:
            args += ["--tools", ",".join(_builtin_tool_names(self.options.tools))]
            args += ["--allowedTools", *self.options.tools]
        if self.permission_mode:
            args += ["--permission-mode", self.permission_mode]
        if self.options.exclude_tools:
            args += ["--disallowedTools", *self.options.exclude_tools]
        cmd = _stream_json(
            claude_command(
                binary=self.options.binary,
                model=self.options.model,
                tools=True,
                session_id=session_id,
                persist_session=True,
                extra_args=[*args, *self.options.extra_args],
                strict_mcp=not self.trust_project_settings,
            )
        )
        proc = run_agent_cli(
            cmd,
            env=self.options.env,
            timeout_seconds=timeout_seconds,
            input=prompt,
            cwd=self.options.cwd,
        )
        # The stream's last line is the same result object
        # ``--output-format json`` prints, so the shared parser reads it.
        result = parse_claude_output(proc)
        turns = 0
        tool_calls = 0
        transcript: list[str] = []
        for line in (proc.stdout or "").splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict) or event.get("type") != "assistant":
                continue
            message = event.get("message")
            if not isinstance(message, dict):
                continue
            turns += 1
            content = message.get("content")
            for item in content if isinstance(content, list) else []:
                if not isinstance(item, dict):
                    continue
                if item.get("type") == "tool_use":
                    tool_calls += 1
                    transcript.append(_tool_line(item.get("name"), item.get("input")))
                elif item.get("type") == "text" and isinstance(item.get("text"), str):
                    if item["text"].strip():
                        transcript.append(_reply_line(item["text"]))
        num_turns = result.raw.get("num_turns")
        if isinstance(num_turns, int) and not isinstance(num_turns, bool):
            turns = num_turns
        return AgentTurn(
            text=result.text,
            session_id=result.session_id or session_id,
            turns=turns,
            tool_calls=tool_calls,
            tokens_sent=result.tokens_sent,
            tokens_received=result.tokens_received,
            cost_usd=result.cost_usd,
            transcript=tuple(transcript),
        )


def _stream_json(cmd: list[str]) -> list[str]:
    """``claude_command``'s argv switched from ``--output-format json`` to
    ``stream-json --verbose``, so each tool call is visible in the output."""
    index = cmd.index("--output-format")
    return [*cmd[: index + 1], "stream-json", "--verbose", *cmd[index + 2 :]]


def _builtin_tool_names(tools: tuple[str, ...]) -> list[str]:
    """The distinct built-in tool names in ``tools``, in order; ``mcp__`` entries are not built-in."""
    names: list[str] = []
    for entry in tools:
        name = entry.split("(", 1)[0].strip()
        if name and not name.startswith("mcp__") and name not in names:
            names.append(name)
    return names


@dataclass
class _Session:
    """The running totals of one effect's session across its turns."""

    engine: AgentEngine
    session_id: str | None
    transcript_path: Path
    turns: int = 0
    tool_calls: int = 0
    tokens_sent: int | None = None
    tokens_received: int | None = None
    cost_usd: float | None = None
    repair_turn: bool = False
    texts: list[str] = field(default_factory=list)

    def add(self, turn: AgentTurn, *, label: str) -> None:
        self.session_id = turn.session_id or self.session_id
        self.turns += turn.turns
        self.tool_calls += turn.tool_calls
        if turn.tokens_sent is not None:
            self.tokens_sent = (self.tokens_sent or 0) + turn.tokens_sent
        if turn.tokens_received is not None:
            self.tokens_received = (self.tokens_received or 0) + turn.tokens_received
        if turn.cost_usd is not None:
            self.cost_usd = (self.cost_usd or 0.0) + turn.cost_usd
        self.texts.append(turn.text)
        with self.transcript_path.open("a", encoding="utf-8") as handle:
            handle.write(f"== {label}: {self.engine.name} session {self.session_id or '?'}\n")
            for line in turn.transcript:
                handle.write(line + "\n")

    def raw(self) -> dict[str, Any]:
        raw: dict[str, Any] = {
            "engine": self.engine.name,
            "session_id": self.session_id,
            "turns": self.turns,
            "tool_calls": self.tool_calls,
            "tokens": {"sent": self.tokens_sent, "received": self.tokens_received},
            "repair_turn": self.repair_turn,
            "transcript": str(self.transcript_path),
        }
        if self.cost_usd is not None:
            raw["cost"] = self.cost_usd
        return raw


def run_agent_session(
    engine: AgentEngine,
    prompt: str,
    *,
    session_id: str | None,
    result_file: Path | None,
    result_schema: Mapping[str, Any] | None,
    timeout_seconds: float,
    scratch_dir: Path,
) -> ToolResult:
    """Run the session's turn, then — with ``result_file`` — read and
    validate the result and give a missing or invalid one exactly one
    repair turn in the same session. ``timeout_seconds`` bounds both turns
    together. A result still invalid after the repair is an ``ok=False``
    result whose ``stderr`` carries the errors (``raw`` keeps the session
    id and transcript so the session can be inspected or resumed)."""
    deadline = time.monotonic() + timeout_seconds
    session = _Session(
        engine=engine, session_id=session_id, transcript_path=scratch_dir / TRANSCRIPT_NAME
    )
    first_prompt = prompt if result_file is None else _with_result_contract(
        prompt, result_file, result_schema
    )
    session.add(
        _run_turn(
            session,
            first_prompt,
            session_id=session_id,
            timeout_seconds=timeout_seconds,
            scratch_dir=scratch_dir,
        ),
        label="turn",
    )
    if result_file is None:
        return ToolResult(value=session.texts[-1], raw=session.raw())

    value, errors = _read_result(result_file, result_schema)
    if not errors:
        return ToolResult(value=value, raw=session.raw())
    if not session.session_id:
        return _invalid_result(
            session, result_file, errors, why="the CLI reported no session id to resume"
        )
    remaining = deadline - time.monotonic()
    if remaining < 1:
        return _invalid_result(
            session, result_file, errors, why="no time was left for a repair turn"
        )
    session.repair_turn = True
    session.add(
        _run_turn(
            session,
            _repair_prompt(result_file, errors),
            session_id=session.session_id,
            timeout_seconds=remaining,
            scratch_dir=scratch_dir,
        ),
        label="repair turn",
    )
    value, errors = _read_result(result_file, result_schema)
    if errors:
        return _invalid_result(session, result_file, errors, why="after one repair turn")
    return ToolResult(value=value, raw=session.raw())


def _run_turn(
    session: _Session,
    prompt: str,
    *,
    session_id: str | None,
    timeout_seconds: float,
    scratch_dir: Path,
) -> AgentTurn:
    """One turn of ``session``'s engine. A CLI failure or timeout is
    re-raised (same class) naming the session id and the transcript, when
    there are any, so a long session can still be inspected or resumed."""
    try:
        return session.engine.run_turn(
            prompt,
            session_id=session_id,
            timeout_seconds=timeout_seconds,
            scratch_dir=scratch_dir,
        )
    except AgentCliError as exc:
        notes: list[str] = []
        if session.session_id:
            notes.append(
                f"session {session.session_id} can be resumed with params['session']"
            )
        if session.transcript_path.exists():
            notes.append(f"transcript: {session.transcript_path}")
        if not notes:
            raise
        raise type(exc)(f"{exc} ({'; '.join(notes)})") from exc


def _with_result_contract(
    prompt: str, result_file: Path, result_schema: Mapping[str, Any] | None
) -> str:
    contract = (
        "\n\n---\n\nWhen you have finished, write your result as one JSON document "
        f"to this file (overwrite it if it exists):\n{result_file}\n"
    )
    if result_schema is not None:
        contract += (
            "It must match this JSON Schema:\n"
            + json.dumps(result_schema, indent=2, ensure_ascii=False)
            + "\n"
        )
    return prompt.rstrip("\n") + contract


def _repair_prompt(result_file: Path, errors: list[str]) -> str:
    quoted = "\n".join(f"- {error}" for error in errors)
    return (
        f"The result file {result_file} is not valid:\n{quoted}\n\n"
        f"Write the corrected JSON result to {result_file} now. "
        "Change nothing else."
    )


def _read_result(
    result_file: Path, result_schema: Mapping[str, Any] | None
) -> tuple[Any, list[str]]:
    """The result file's parsed JSON and the reasons it is not valid
    (an empty list when it is)."""
    try:
        text = result_file.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None, ["the file was not written"]
    except OSError as exc:
        return None, [f"the file could not be read: {exc}"]
    try:
        value = json.loads(text)
    except json.JSONDecodeError as exc:
        return None, [f"the file is not valid JSON: {exc}"]
    if result_schema is None:
        return value, []
    validator = jsonschema.validators.validator_for(result_schema)(result_schema)
    errors = [
        f"{'/'.join(str(part) for part in error.absolute_path) or '(root)'}: {error.message}"
        for error in sorted(validator.iter_errors(value), key=lambda e: list(e.absolute_path))
    ]
    if len(errors) > _MAX_QUOTED_ERRORS:
        errors = [*errors[:_MAX_QUOTED_ERRORS], f"... and {len(errors) - _MAX_QUOTED_ERRORS} more"]
    return value, errors


def _invalid_result(
    session: _Session, result_file: Path, errors: list[str], *, why: str
) -> ToolResult:
    message = f"agent: result file {result_file} is still invalid ({why}): " + "; ".join(errors)
    return ToolResult(value=None, raw=session.raw(), stderr=message, ok=False)


def _write_prompt_file(scratch_dir: Path, prompt: str) -> Path:
    handle, name = tempfile.mkstemp(prefix="prompt-", suffix=".md", dir=scratch_dir)
    with os.fdopen(handle, "w", encoding="utf-8") as stream:
        stream.write(prompt)
    return Path(name)


def _content_text(content: Any) -> str:
    if isinstance(content, str):
        return content.strip()
    if not isinstance(content, list):
        return ""
    return "".join(
        item["text"]
        for item in content
        if isinstance(item, dict) and item.get("type") == "text" and isinstance(item.get("text"), str)
    ).strip()


def _one_line(text: str) -> str:
    flat = " ".join(text.split())
    if len(flat) > _TRANSCRIPT_LINE_CHARS:
        flat = flat[: _TRANSCRIPT_LINE_CHARS - 3] + "..."
    return flat


def _tool_line(name: Any, args: Any) -> str:
    try:
        rendered = json.dumps(args, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        rendered = str(args)
    return _one_line(f"tool {name or '?'} {rendered}")


def _reply_line(text: str) -> str:
    return _one_line(f"reply {text}")


# ---------- params ----------


def _optional_str(params: Mapping[str, Any], key: str) -> str:
    value = params.get(key)
    if value is None:
        return ""
    if not isinstance(value, str):
        raise ValueError(f"agent: params['{key}'] must be a string.")
    return value.strip()


def _str_list(params: Mapping[str, Any], key: str) -> tuple[str, ...]:
    value = params.get(key)
    if value is None:
        return ()
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"agent: params['{key}'] must be a list of strings.")
    for index, item in enumerate(value):
        if "\x00" in item:
            raise ValueError(f"agent: params['{key}'][{index}] contains a null byte.")
    return tuple(value)


def _str_mapping(params: Mapping[str, Any], key: str) -> dict[str, str]:
    value = params.get(key)
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ValueError(f"agent: params['{key}'] must be a mapping of variable names to values.")
    for name, item in value.items():
        if item is None:
            raise ValueError(
                f"agent: params['{key}']['{name}'] is null; unset_env removes a variable."
            )
    return {str(name): str(item) for name, item in value.items()}


def _trust_project_settings(params: Mapping[str, Any], engine_name: str) -> bool:
    value = params.get("trust_project_settings")
    if value is None:
        return False
    if not isinstance(value, bool):
        raise ValueError("agent: params['trust_project_settings'] must be true or false.")
    if engine_name != CLAUDE_CODE:
        raise ValueError(
            "agent: params['trust_project_settings'] applies to engine 'claude_code' only."
        )
    return value


def _repository_root(cwd: Path) -> Path:
    start = cwd.resolve()
    for candidate in (start, *start.parents):
        if (candidate / ".git").exists():
            return candidate
    return start


def _project_instructions(cwd: Path) -> tuple[str, str] | None:
    """The repository root's CLAUDE.md as (path, text), or None when none is to be appended."""
    root = _repository_root(cwd)
    path = root / "CLAUDE.md"
    if not path.exists():
        return None
    target = path.resolve()
    if not target.is_file() or not target.is_relative_to(root):
        return None
    text = target.read_text(encoding="utf-8", errors="replace")
    if not text.strip():
        return None
    return str(path), text


@dataclass(frozen=True)
class AgentPlugin:
    name: str = "agent"
    #: ``runtime.plugins.agent.engine``: the engine an effect gets when its
    #: params name none.
    default_engine: str = DEFAULT_ENGINE
    #: ``runtime.plugins.agent.<engine>.binary``: replaces the PATH search.
    binaries: Mapping[str, str] = field(default_factory=dict)

    def execute(self, *, params: dict[str, Any], timeout_seconds: int = 300) -> ToolResult:
        prompt = params.get("prompt")
        if not isinstance(prompt, str) or not prompt.strip():
            raise ValueError("agent requires params['prompt'] as a non-empty string.")
        engine_name = _optional_str(params, "engine") or self.default_engine
        if engine_name not in ENGINE_BINARIES:
            raise ValueError(
                f"agent: unknown engine {engine_name!r}; expected one of "
                f"{sorted(ENGINE_BINARIES)}."
            )
        cwd = _optional_str(params, "cwd") or os.getcwd()
        cwd_path = Path(cwd).expanduser()
        if not cwd_path.is_dir():
            raise ValueError(f"agent: cwd {cwd!r} is not a directory.")
        thinking = _optional_str(params, "thinking")
        permission_mode = _optional_str(params, "permission_mode")
        if thinking and engine_name != PI:
            raise ValueError("agent: params['thinking'] applies to engine 'pi' only.")
        if permission_mode and engine_name != CLAUDE_CODE:
            raise ValueError(
                "agent: params['permission_mode'] applies to engine 'claude_code' only."
            )
        trust_project_settings = _trust_project_settings(params, engine_name)
        tools = _str_list(params, "tools")
        if params.get("tools") is not None and not tools:
            raise ValueError(
                "agent: params['tools'] is empty; leave it out to keep the engine's default tools."
            )
        if engine_name == CLAUDE_CODE and tools and not permission_mode:
            permission_mode = _TOOLS_PERMISSION_MODE
        result_file_param = _optional_str(params, "result_file")
        result_schema = params.get("result_schema")
        if result_schema is not None:
            if not result_file_param:
                raise ValueError("agent: params['result_schema'] needs params['result_file'].")
            if not isinstance(result_schema, dict):
                raise ValueError("agent: params['result_schema'] must be a JSON Schema object.")
            try:
                jsonschema.validators.validator_for(result_schema).check_schema(result_schema)
            except jsonschema.SchemaError as exc:
                raise ValueError(
                    f"agent: params['result_schema'] is not a valid JSON Schema: {exc.message}"
                ) from exc
        result_file = (cwd_path / result_file_param).resolve() if result_file_param else None
        unset_env = (
            _str_list(params, "unset_env") if params.get("unset_env") is not None
            else DEFAULT_UNSET_ENV
        )
        project: tuple[str, str] | None = None
        if engine_name == CLAUDE_CODE and not trust_project_settings:
            project = _project_instructions(cwd_path)
            if project is not None:
                prompt += _PROJECT_INSTRUCTIONS_HEADER + project[1]
        options = _EngineOptions(
            binary=self._resolve_binary(engine_name),
            cwd=str(cwd_path),
            env={**child_env(unset_env), **_str_mapping(params, "env")},
            model=_optional_str(params, "model"),
            tools=tools,
            exclude_tools=_str_list(params, "exclude_tools"),
            extra_args=_str_list(params, "extra_args"),
        )
        engine: AgentEngine = (
            PiEngine(options, thinking=thinking)
            if engine_name == PI
            else ClaudeCodeEngine(
                options,
                permission_mode=permission_mode,
                trust_project_settings=trust_project_settings,
            )
        )
        claude_raw: dict[str, Any] = {}
        if engine_name == CLAUDE_CODE:
            claude_raw = {
                "permission_mode": permission_mode or None,
                "project_settings": "trusted" if trust_project_settings else "isolated",
                "project_instructions": project[0] if project else None,
            }

        # A result left over from an earlier run must not pass for this one.
        if result_file is not None:
            try:
                result_file.unlink(missing_ok=True)
            except OSError as exc:
                raise ValueError(
                    f"agent: result_file {str(result_file)!r} could not be cleared before the "
                    f"session: {exc}"
                ) from exc
        # Holds the prompt files while a turn runs, and the transcript after.
        scratch_dir = Path(tempfile.mkdtemp(prefix=f"cof-agent-{engine_name}-"))
        try:
            result = run_agent_session(
                engine,
                prompt,
                session_id=_optional_str(params, "session") or None,
                result_file=result_file,
                result_schema=result_schema,
                timeout_seconds=float(timeout_seconds),
                scratch_dir=scratch_dir,
            )
        except AgentCliError:
            # The error names the transcript when there is one (_run_turn).
            if not (scratch_dir / TRANSCRIPT_NAME).exists():
                shutil.rmtree(scratch_dir, ignore_errors=True)
            raise
        except BaseException:
            # No result or error carries the transcript's path, so nothing
            # could read it.
            shutil.rmtree(scratch_dir, ignore_errors=True)
            raise
        return replace(result, raw={**result.raw, **claude_raw})

    def _resolve_binary(self, engine_name: str) -> str:
        configured = self.binaries.get(engine_name)
        if configured:
            path, error = _expand_and_validate(configured)
            if error:
                raise RuntimeError(
                    f"agent: configured runtime.plugins.agent.{engine_name}.binary="
                    f"{configured!r} {error} (resolved: {path})."
                )
            return path
        default = ENGINE_BINARIES[engine_name]
        found = resolve_binary([default])
        if found is None:
            raise RuntimeError(
                f"agent: {default!r} (engine {engine_name!r}) not found on PATH. Install it, "
                f"or set runtime.plugins.agent.{engine_name}.binary."
            )
        return found

    def check(self) -> CheckResult:
        """OK when at least one engine's CLI is available; the message
        names each engine's state."""
        states: list[str] = []
        missing: list[str] = []
        for engine_name, default in ENGINE_BINARIES.items():
            try:
                states.append(f"{engine_name}: {self._resolve_binary(engine_name)}")
            except RuntimeError as exc:
                states.append(str(exc).removeprefix("agent: "))
                missing.append(f"binary:{default}")
        ok = len(missing) < len(ENGINE_BINARIES)
        return CheckResult(
            ok=ok,
            missing=[] if ok else missing,
            message="; ".join(states),
        )


def make_plugin(cfg: Mapping[str, Any]) -> AgentPlugin:
    """Build the plugin from ``runtime.plugins.agent``."""
    engine = cfg.get("engine") or DEFAULT_ENGINE
    if engine not in ENGINE_BINARIES:
        raise ValueError(
            f"runtime.plugins.agent.engine must be one of {sorted(ENGINE_BINARIES)}, "
            f"got {engine!r}."
        )
    binaries: dict[str, str] = {}
    for engine_name in ENGINE_BINARIES:
        block = cfg.get(engine_name)
        if block is None:
            continue
        if not isinstance(block, dict):
            raise ValueError(f"runtime.plugins.agent.{engine_name} must be a mapping.")
        binary = block.get("binary")
        if binary:
            binaries[engine_name] = str(binary)
    return AgentPlugin(default_engine=str(engine), binaries=binaries)
