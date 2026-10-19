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
  ``repair_turn``, ``context_tokens``, ``context_window``, ``tool_counts``,
  ``turn_tokens``, ``files_changed``, ``lines_added``, ``lines_removed``,
  and ``transcript`` — the path of a compact log this plugin writes, one
  JSON line per turn header, tool call and reply (``params['transcript']``
  says where, see below). The transcript itself never goes into state.
  Claude Code adds ``permission_mode``, ``project_settings`` and
  ``project_instructions``. ``meta.tokens_sent``, ``meta.tokens_received``
  and ``meta.cost_usd`` hold the same totals as ``raw.tokens``/``raw.cost``
  (the session's usage over every turn, including a repair turn), so the
  run's totals and the ``dynamic`` tree's token sums count this effect.
* ``meta.live``: a summary of the session published while it runs, through
  :mod:`circuitry.agent_live` (#636) — numbers, times, ids, the model name
  and tool names only, never prompt/reply/thinking text, tool arguments,
  tool output, file contents or the environment. See
  ``docs/plugins/agent.md``.
* A turn that fails (the CLI errors, or times out) after the session has an
  id or an earlier turn has completed ends the effect as an error (``ok:
  false``, so ``on_error`` and retries behave as for any other tool
  failure) whose ``raw`` still carries the keys above — ``session_id`` to
  resume with ``params['session']``, ``transcript`` to inspect what
  happened — plus ``timed_out: true`` on a timeout. ``meta.error`` is
  unchanged. A turn that fails before either (nothing yet to carry) raises
  as it always has.

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
    put in argv. In a document's literal params it may instead be
    ``{file: {from: <state path>}, within: <dir>}`` (#591): ``core.tool``
    reads that file once, before the first attempt, and passes its text here
    as the string; ``params_json`` can never supply that form (see
    :func:`reject_overlay_prompt`).
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
  - ``transcript`` (optional, str, relative to ``cwd`` when not absolute):
    where the compact transcript is written instead of a temporary
    directory nothing removes. The parent directory is created; the file
    is appended to across the session's turns, including a repair turn.
    Without it, the transcript goes to a temporary directory as before.

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
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Protocol

import jsonschema

from ..agent_cli import (
    DEFAULT_UNSET_ENV,
    AgentCliError,
    AgentCliTimeout,
    child_env,
    claude_command,
    parse_claude_output,
    parse_pi_output,
    pi_command,
    run_agent_cli,
)
from ..agent_live import LiveTracker
from ..core.prompt_files import is_dispatch_file
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


@dataclass(frozen=True)
class AgentTurn:
    """One CLI call of a session: its final text and what it did.

    Tool-call and reply detail is in the transcript file itself (written
    live, as the call streams, by the ``on_line`` callback), not here.
    """

    text: str
    session_id: str | None
    turns: int
    tool_calls: int
    tokens_sent: int | None = None
    tokens_received: int | None = None
    cost_usd: float | None = None


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
        on_line: Callable[[str], None] | None = None,
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
        on_line: Callable[[str], None] | None = None,
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
                cmd,
                env=self.options.env,
                timeout_seconds=timeout_seconds,
                cwd=self.options.cwd,
                on_line=on_line,
            )
        finally:
            prompt_file.unlink(missing_ok=True)
        result = parse_pi_output(proc)
        turns = 0
        tool_calls = 0
        for event in result.raw.get("events", []):
            kind = event.get("type")
            if kind == "tool_execution_start":
                tool_calls += 1
                continue
            message = event.get("message")
            if kind != "message_end" or not isinstance(message, dict):
                continue
            if message.get("role") != "assistant":
                continue
            turns += 1
        return AgentTurn(
            text=result.text,
            session_id=result.session_id or session_id,
            turns=turns,
            tool_calls=tool_calls,
            tokens_sent=result.tokens_sent,
            tokens_received=result.tokens_received,
            cost_usd=result.cost_usd,
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
        on_line: Callable[[str], None] | None = None,
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
            on_line=on_line,
        )
        # The stream's last line is the same result object
        # ``--output-format json`` prints, so the shared parser reads it.
        result = parse_claude_output(proc)
        turns = 0
        tool_calls = 0
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
    live: LiveTracker
    turns: int = 0
    tool_calls: int = 0
    tokens_sent: int | None = None
    tokens_received: int | None = None
    cost_usd: float | None = None
    repair_turn: bool = False
    texts: list[str] = field(default_factory=list)
    #: Whether a turn has ever completed (``add`` was called at least
    #: once). The transcript *file* now exists from the first moment any
    #: turn starts (#636's ``live.start_turn``, called before the CLI even
    #: runs) — ``transcript_path.exists()`` is no longer a reliable signal
    #: that there's anything in it worth resuming or inspecting, so
    #: ``raw()`` and ``_run_turn``'s own "nothing yet to report" check both
    #: use this instead.
    turn_completed: bool = False

    def add(self, turn: AgentTurn) -> None:
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
        self.turn_completed = True
        # The authoritative, cumulative totals this turn's end-of-turn parse
        # just computed replace whatever the live tracker estimated on its
        # own while the turn was streaming (#636).
        self.live.sync_turn_totals(
            turns=self.turns,
            tool_calls=self.tool_calls,
            tokens_sent=self.tokens_sent,
            tokens_received=self.tokens_received,
            cost_usd=self.cost_usd,
        )

    def raw(self) -> dict[str, Any]:
        raw: dict[str, Any] = {
            "engine": self.engine.name,
            "session_id": self.session_id,
            "turns": self.turns,
            "tool_calls": self.tool_calls,
            "tokens": {"sent": self.tokens_sent, "received": self.tokens_received},
            "repair_turn": self.repair_turn,
            # None when no turn has completed yet to write one (a turn
            # that fails before the session has an id or a turn has
            # completed raises instead of reaching here; one that fails
            # after either exists can still get here before a later turn
            # finished).
            "transcript": (str(self.transcript_path) if self.turn_completed else None),
            **self.live.raw_extra(),
        }
        if self.cost_usd is not None:
            raw["cost"] = self.cost_usd
        return raw

    def result(self, *, value: Any, stderr: str | None = None, ok: bool = True) -> ToolResult:
        """A ``ToolResult`` carrying this session's ``raw`` plus its usage
        over every turn so far, under the same meta keys a ``prompt``
        effect uses (#414)."""
        return ToolResult(
            value=value,
            raw=self.raw(),
            stderr=stderr,
            ok=ok,
            tokens_sent=self.tokens_sent,
            tokens_received=self.tokens_received,
            cost_usd=self.cost_usd,
        )


class _TurnFailed(Exception):
    """A turn's ``AgentCliError``/``AgentCliTimeout`` after the session
    already has an id or an earlier turn has completed — enough context
    that the effect can end as an ``ok=False`` result instead of raising
    (#391),
    carrying it so the caller doesn't have to re-derive ``timed_out`` from
    the (now-discarded) exception's class."""

    def __init__(self, message: str, *, timed_out: bool) -> None:
        super().__init__(message)
        self.timed_out = timed_out


def run_agent_session(
    engine: AgentEngine,
    prompt: str,
    *,
    session_id: str | None,
    result_file: Path | None,
    result_schema: Mapping[str, Any] | None,
    timeout_seconds: float,
    scratch_dir: Path,
    transcript_path: Path | None = None,
    live: LiveTracker | None = None,
) -> ToolResult:
    """Run the session's turn, then — with ``result_file`` — read and
    validate the result and give a missing or invalid one exactly one
    repair turn in the same session. ``timeout_seconds`` bounds both turns
    together. A result still invalid after the repair is an ``ok=False``
    result whose ``stderr`` carries the errors (``raw`` keeps the session
    id and transcript so the session can be inspected or resumed); so is a
    turn that fails after the session has an id or an earlier turn has
    completed — ``raw`` carries the same keys, plus ``timed_out`` on a
    timeout. A turn that fails before either raises, as it always has.

    ``transcript_path`` is the compact log's destination, appended to
    across every turn; ``scratch_dir / TRANSCRIPT_NAME`` when ``None``.
    ``live`` publishes ``meta.live`` and writes the transcript as the
    session goes (#636); a caller that doesn't need that (today, only this
    module's own tests) can leave it unset and gets a tracker that
    publishes nowhere, closed automatically once this call returns.
    """
    deadline = time.monotonic() + timeout_seconds
    resolved_transcript_path = transcript_path or (scratch_dir / TRANSCRIPT_NAME)
    owns_live = live is None
    if live is None:
        live = LiveTracker(
            engine=engine.name,
            model="",
            attempt=1,
            transcript_path=resolved_transcript_path,
            publish=lambda _snapshot: None,
        )
    try:
        session = _Session(
            engine=engine,
            session_id=session_id,
            transcript_path=resolved_transcript_path,
            live=live,
        )
        first_prompt = prompt if result_file is None else _with_result_contract(
            prompt, result_file, result_schema
        )
        try:
            session.add(
                _run_turn(
                    session,
                    first_prompt,
                    session_id=session_id,
                    timeout_seconds=timeout_seconds,
                    scratch_dir=scratch_dir,
                    label="turn",
                )
            )
        except _TurnFailed as exc:
            return _turn_failure_result(session, exc)
        if result_file is None:
            return session.result(value=session.texts[-1])

        value, errors = _read_result(result_file, result_schema)
        if not errors:
            return session.result(value=value)
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
        try:
            session.add(
                _run_turn(
                    session,
                    _repair_prompt(result_file, errors),
                    session_id=session.session_id,
                    timeout_seconds=remaining,
                    scratch_dir=scratch_dir,
                    label="repair turn",
                )
            )
        except _TurnFailed as exc:
            return _turn_failure_result(session, exc)
        value, errors = _read_result(result_file, result_schema)
        if errors:
            return _invalid_result(session, result_file, errors, why="after one repair turn")
        return session.result(value=value)
    finally:
        if owns_live:
            live.close()


def _turn_failure_result(session: _Session, exc: _TurnFailed) -> ToolResult:
    result = session.result(value=None, stderr=str(exc), ok=False)
    if exc.timed_out:
        result = replace(result, raw={**result.raw, "timed_out": True})
    return result


def _run_turn(
    session: _Session,
    prompt: str,
    *,
    session_id: str | None,
    timeout_seconds: float,
    scratch_dir: Path,
    label: str,
) -> AgentTurn:
    """One turn of ``session``'s engine. A CLI failure or timeout, once the
    session has an id or an earlier turn has completed, becomes a
    ``_TurnFailed`` naming the session id and the transcript (so a long
    session can still be inspected or resumed) for the caller to turn into
    an ``ok=False`` result; with neither to report, it is re-raised
    unchanged.

    ``label`` (``"turn"`` or ``"repair turn"``) becomes ``meta.live.phase``
    and the transcript's own ``turn`` line, written before the CLI starts
    (#636)."""
    session.live.start_turn(phase=label, session_id=session_id)
    try:
        return session.engine.run_turn(
            prompt,
            session_id=session_id,
            timeout_seconds=timeout_seconds,
            scratch_dir=scratch_dir,
            on_line=session.live.handle_line,
        )
    except AgentCliError as exc:
        notes: list[str] = []
        if session.session_id:
            notes.append(
                f"session {session.session_id} can be resumed with params['session']"
            )
        if session.turn_completed:
            notes.append(f"transcript: {session.transcript_path}")
        if not notes:
            raise
        message = f"{exc} ({'; '.join(notes)})"
        raise _TurnFailed(message, timed_out=isinstance(exc, AgentCliTimeout)) from exc


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
    return session.result(value=None, stderr=message, ok=False)


def _write_prompt_file(scratch_dir: Path, prompt: str) -> Path:
    handle, name = tempfile.mkstemp(prefix="prompt-", suffix=".md", dir=scratch_dir)
    with os.fdopen(handle, "w", encoding="utf-8") as stream:
        stream.write(prompt)
    return Path(name)


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


def _tool_names(params: Mapping[str, Any], key: str) -> tuple[str, ...]:
    names = _str_list(params, key)
    for index, name in enumerate(names):
        if not name.strip():
            raise ValueError(f"agent: params['{key}'][{index}] is blank.")
    return names


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
    if isinstance(value, str) and value.strip().lower() in ("true", "false"):
        value = value.strip().lower() == "true"
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
    try:
        root = _repository_root(cwd)
        path = root / "CLAUDE.md"
        if not path.exists():
            return None
        target = path.resolve()
        if not target.is_file() or not target.is_relative_to(root):
            return None
        text = target.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    if not text.strip():
        return None
    return str(path), text


def reject_overlay_prompt(overlay: Mapping[str, Any], *, prompt_from_file: bool) -> None:
    """``params_json`` is built at run time and can carry generated content, so
    it never supplies ``prompt`` as ``{file: {from: ...}, within: ...}`` (its
    ``within`` is a trust boundary, like ``systemone``'s), and never replaces
    a ``prompt`` the document reads from a file (*prompt_from_file*).
    """
    if "prompt" not in overlay:
        return
    if is_dispatch_file(overlay["prompt"]):
        raise ValueError(
            "params_json must not set params.prompt to {file: {from: <state path>}, "
            "within: <dir>}: a prompt file is only honoured from a document's literal "
            "params block."
        )
    if prompt_from_file:
        raise ValueError(
            "params_json must not set params.prompt: this effect reads params.prompt "
            "from a file ({file: {from: <state path>}, within: <dir>}), which "
            "params_json cannot replace."
        )


@dataclass(frozen=True)
class AgentPlugin:
    name: str = "agent"
    #: ``runtime.plugins.agent.engine``: the engine an effect gets when its
    #: params name none.
    default_engine: str = DEFAULT_ENGINE
    #: ``runtime.plugins.agent.<engine>.binary``: replaces the PATH search.
    binaries: Mapping[str, str] = field(default_factory=dict)

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
        on_live: Callable[[dict[str, Any]], None] | None = None,
        attempt: int = 1,
        earlier_attempts: tuple[int | None, int | None] | None = None,
    ) -> ToolResult:
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
        tools = _tool_names(params, "tools")
        if params.get("tools") is not None and not tools:
            raise ValueError(
                "agent: params['tools'] is empty; leave it out to keep the engine's default tools."
            )
        exclude_tools = _tool_names(params, "exclude_tools")
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
        transcript_param = _optional_str(params, "transcript")
        transcript_path = (cwd_path / transcript_param).resolve() if transcript_param else None
        if transcript_path is not None:
            try:
                transcript_path.parent.mkdir(parents=True, exist_ok=True)
            except OSError as exc:
                raise ValueError(
                    f"agent: params['transcript'] {str(transcript_path)!r}'s directory could not "
                    f"be created: {exc}"
                ) from exc
            if transcript_path.is_dir():
                raise ValueError(
                    f"agent: params['transcript'] {str(transcript_path)!r} is a directory."
                )
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
            exclude_tools=exclude_tools,
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
        # Holds the prompt files while a turn runs, and — without
        # params['transcript'] — the transcript after.
        scratch_dir = Path(tempfile.mkdtemp(prefix=f"cof-agent-{engine_name}-"))
        live = LiveTracker(
            engine=engine_name,
            model=options.model,
            attempt=attempt,
            transcript_path=transcript_path or (scratch_dir / TRANSCRIPT_NAME),
            publish=on_live or (lambda _snapshot: None),
            earlier_attempts=earlier_attempts,
        )
        # Only set once run_agent_session has returned (not raised) — every
        # exception path (nothing yet to resume or inspect; a genuinely
        # unexpected error) removes scratch_dir unconditionally.
        keep_scratch_dir = False
        try:
            try:
                result = run_agent_session(
                    engine,
                    prompt,
                    session_id=_optional_str(params, "session") or None,
                    result_file=result_file,
                    result_schema=result_schema,
                    timeout_seconds=float(timeout_seconds),
                    scratch_dir=scratch_dir,
                    transcript_path=transcript_path,
                    live=live,
                )
                # With params['transcript'], the transcript never lives under
                # scratch_dir (it goes straight to transcript_path), so nothing
                # in it is worth keeping (#392). Without it, a turn that failed
                # before any turn completed leaves scratch_dir/TRANSCRIPT_NAME
                # with nothing but a content-free turn header (#636), not
                # worth keeping either (#391) — ``result.raw["transcript"]``
                # is ``None`` in exactly that case (``_Session.raw``).
                keep_scratch_dir = transcript_path is None and (
                    result.raw.get("transcript") is not None
                )
            finally:
                if not keep_scratch_dir:
                    shutil.rmtree(scratch_dir, ignore_errors=True)
        finally:
            live.close()
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
