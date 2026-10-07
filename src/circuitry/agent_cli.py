"""Run a coding-agent CLI (pi or Claude Code) headlessly, through its own login.

Shared by the ``pi`` and ``claude_code`` adapters (#366) and the ``agent``
tool. One call is four steps, each its own function so a caller can vary
the command without re-implementing the rest:

1. :func:`child_env` — the environment for the child: the caller's, minus
   the variables that make a CLI think it runs inside a parent session
   (:data:`PARENT_SESSION_ENV`, always removed) and minus ``unset_env``
   (by default :data:`DEFAULT_UNSET_ENV`, so the CLI uses its own login
   rather than an API key that happens to be exported).
2. :func:`pi_command` / :func:`claude_command` — the argv for one headless
   call. Tools are off and nothing is saved unless the caller asks.
3. :func:`run_agent_cli` — runs the command in its own process group,
   tracked by the run's cancellation token (``core.cancellation``), so a
   timeout or a cancelled run kills the CLI and everything it started.
4. :func:`parse_pi_output` / :func:`parse_claude_output` — the CLI's JSON
   output as an :class:`AgentCliResult`, or an :class:`AgentCliError`
   carrying the CLI's own message (an auth failure, a CLI too old for the
   requested model, ...).

Every failure is an :class:`AgentCliError`; a timeout is the subclass
:class:`AgentCliTimeout`. A cancelled run raises
``core.cancellation.RunCancelledBySignal`` unchanged.
"""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from .core.cancellation import get_token, run_tracked

#: Variables a parent pi or Claude Code session exports to its children.
#: A CLI that inherits them acts as part of that session, so they are
#: always removed from the child's environment.
PARENT_SESSION_ENV: tuple[str, ...] = (
    "PI_SESSION_ID",
    "PI_SESSION_FILE",
    "PI_PANE_ARGS",
    "PI_CODING_AGENT",
    "PI_MODEL",
    "PI_PROVIDER",
    "PI_REASONING_LEVEL",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
)

#: The default ``unset_env``: API credentials that would make the CLI bill
#: a key instead of using its own login.
DEFAULT_UNSET_ENV: tuple[str, ...] = ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")

#: The message argument sent with pi's ``@<prompt-file>`` attachment. pi does
#: not read a prompt from stdin, and a long prompt does not belong in argv.
PI_ATTACHED_PROMPT_INSTRUCTION = (
    "The attached file is the prompt. Reply to it as if its contents had been sent as this message."
)

#: How much of a failed CLI's stderr an error message carries.
_STDERR_TAIL_CHARS = 2000

#: pi stop reasons that mean the call failed.
_PI_ERROR_STOP_REASONS = frozenset({"error", "aborted"})


class AgentCliError(RuntimeError):
    """A coding-agent CLI call failed; the message names the CLI and carries
    its own error text when it gave one."""


class AgentCliTimeout(AgentCliError):
    """The CLI did not finish within its timeout; its process group was killed."""


@dataclass(frozen=True)
class AgentCliResult:
    """One CLI call's answer.

    ``text`` is the final assistant text (for Claude Code with a JSON schema,
    ``structured_output`` serialized as JSON). ``tokens_sent`` counts every
    input token the model processed, cache reads and writes included;
    ``tokens_received`` the output tokens. Both are summed over every model
    turn the call made and are ``None`` when the CLI reported no usage.
    ``cost_usd`` is the CLI's own cost figure, ``None`` when it gave none.
    ``finish_reason`` is the CLI's stop reason. ``raw`` keeps the parsed
    output: Claude Code's result object, or pi's events under ``"events"``.
    """

    text: str
    raw: dict[str, Any]
    session_id: str | None = None
    tokens_sent: int | None = None
    tokens_received: int | None = None
    cost_usd: float | None = None
    finish_reason: str | None = None
    structured_output: Any = None


def child_env(
    unset_env: Sequence[str] = DEFAULT_UNSET_ENV,
    *,
    base: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """``base`` (default ``os.environ``) without :data:`PARENT_SESSION_ENV`
    and without ``unset_env``; every other variable is kept."""
    removed = set(PARENT_SESSION_ENV) | set(unset_env)
    source = os.environ if base is None else base
    return {key: value for key, value in source.items() if key not in removed}


def pi_command(
    *,
    binary: str,
    prompt_file: str,
    model: str = "",
    thinking: str = "",
    tools: bool = False,
    session_id: str | None = None,
    persist_session: bool = False,
    extra_args: Sequence[str] = (),
    instruction: str = PI_ATTACHED_PROMPT_INSTRUCTION,
) -> list[str]:
    """argv for ``pi -p --mode json``, with the prompt attached as ``@prompt_file``.

    ``model`` is pi's ``provider/id`` and ``thinking`` its level; either is
    left to pi's own default when empty. ``tools=False`` adds ``--no-tools``.
    ``session_id`` resumes that session (``--session``); without one,
    ``persist_session=False`` adds ``--no-session``. ``--no-approve`` is
    always passed, so a project-local pi config never gets trusted.
    ``extra_args`` go before the attachment.
    """
    cmd = [binary, "-p", "--mode", "json", "--no-approve"]
    if not tools:
        cmd.append("--no-tools")
    if session_id:
        cmd += ["--session", session_id]
    elif not persist_session:
        cmd.append("--no-session")
    if model:
        cmd += ["--model", model]
    if thinking:
        cmd += ["--thinking", thinking]
    cmd += list(extra_args)
    cmd += [f"@{prompt_file}", instruction]
    return cmd


def claude_command(
    *,
    binary: str,
    model: str = "",
    json_schema: Mapping[str, Any] | None = None,
    tools: bool = False,
    session_id: str | None = None,
    persist_session: bool = False,
    extra_args: Sequence[str] = (),
) -> list[str]:
    """argv for ``claude -p --output-format json``; the prompt goes on stdin.

    ``model`` is left to Claude Code's default when empty. ``json_schema``
    adds ``--json-schema`` (the answer then arrives as ``structured_output``).
    ``tools=False`` adds ``--tools ""``. ``session_id`` resumes that session
    (``--resume``); without one, ``persist_session=False`` adds
    ``--no-session-persistence``. ``extra_args`` go last.
    """
    cmd = [binary, "-p", "--output-format", "json"]
    if not tools:
        cmd += ["--tools", ""]
    if session_id:
        cmd += ["--resume", session_id]
    elif not persist_session:
        cmd.append("--no-session-persistence")
    if model:
        cmd += ["--model", model]
    if json_schema is not None:
        cmd += ["--json-schema", json.dumps(json_schema)]
    cmd += list(extra_args)
    return cmd


def run_agent_cli(
    cmd: Sequence[str],
    *,
    env: Mapping[str, str],
    timeout_seconds: float,
    input: str = "",
    cwd: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``cmd`` to completion in its own process group and return it,
    whatever its exit code.

    ``input`` is written to stdin, which is then closed. Tracked by
    ``core.cancellation``: a cancelled run kills the whole process group and
    raises ``RunCancelledBySignal``. Raises :class:`AgentCliTimeout` after
    ``timeout_seconds`` (the group is killed first) and
    :class:`AgentCliError` when the binary cannot be started.
    """
    name = os.path.basename(cmd[0])
    try:
        proc = run_tracked(
            cmd,
            input=input,
            timeout=timeout_seconds,
            cwd=cwd,
            env=dict(env),
            new_session=True,
        )
    except subprocess.TimeoutExpired as exc:
        raise AgentCliTimeout(
            f"{name} did not finish within {timeout_seconds:g}s; it was stopped "
            "with everything it started."
        ) from exc
    except OSError as exc:
        raise AgentCliError(f"{name} could not be started ({cmd[0]}): {exc}") from exc
    # A cancellation that killed the group mid-call ends the call here, not
    # as a CLI failure.
    get_token().check()
    return proc


def parse_pi_output(proc: subprocess.CompletedProcess[str]) -> AgentCliResult:
    """Read ``pi --mode json``'s event stream (one JSON object per line).

    The answer is the last assistant message's text. A last assistant
    message that stopped with ``error`` (or ``aborted``) raises
    :class:`AgentCliError` with pi's ``errorMessage``, or the message's own
    text when pi gave none (an auth failure arrives as text). Usage and
    cost are summed over every assistant message.
    """
    events: list[dict[str, Any]] = []
    for line in (proc.stdout or "").splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            events.append(event)

    session_id: str | None = None
    last_message: dict[str, Any] | None = None
    usage_totals = _UsageTotals()
    for event in events:
        if event.get("type") == "session" and isinstance(event.get("id"), str):
            session_id = event["id"]
        message = event.get("message")
        if event.get("type") != "message_end" or not isinstance(message, dict):
            continue
        if message.get("role") != "assistant":
            continue
        last_message = message
        usage = message.get("usage")
        if isinstance(usage, dict):
            cost = usage.get("cost")
            usage_totals.add(
                sent=(usage.get("input"), usage.get("cacheRead"), usage.get("cacheWrite")),
                received=usage.get("output"),
                cost=cost.get("total") if isinstance(cost, dict) else cost,
            )

    if last_message is None:
        raise AgentCliError(_no_answer_message("pi", proc))
    text = _pi_message_text(last_message)
    stop_reason = last_message.get("stopReason")
    if stop_reason in _PI_ERROR_STOP_REASONS:
        error_message = last_message.get("errorMessage")
        detail = error_message if isinstance(error_message, str) and error_message else text
        raise AgentCliError(f"pi failed ({stop_reason}): {detail or 'no message'}")
    if proc.returncode != 0:
        raise AgentCliError(_no_answer_message("pi", proc))
    return AgentCliResult(
        text=text,
        raw={"events": events},
        session_id=session_id,
        tokens_sent=usage_totals.sent,
        tokens_received=usage_totals.received,
        cost_usd=usage_totals.cost,
        finish_reason=stop_reason if isinstance(stop_reason, str) else None,
    )


def parse_claude_output(proc: subprocess.CompletedProcess[str]) -> AgentCliResult:
    """Read ``claude --output-format json``'s single result object.

    ``is_error`` raises :class:`AgentCliError` carrying Claude Code's own
    ``result`` text (e.g. "... version 2.1.280 or newer is required"). With
    ``--json-schema``, the answer is ``structured_output`` and ``text`` is
    that value as JSON.
    """
    raw = _last_json_object(proc.stdout or "")
    if raw is None:
        raise AgentCliError(_no_answer_message("claude", proc))
    result_text = raw.get("result")
    text = result_text if isinstance(result_text, str) else ""
    if raw.get("is_error") is True:
        subtype = raw.get("subtype")
        label = f" ({subtype})" if isinstance(subtype, str) and subtype else ""
        raise AgentCliError(f"claude failed{label}: {text or 'no message'}")
    if proc.returncode != 0:
        raise AgentCliError(_no_answer_message("claude", proc))

    structured = raw.get("structured_output")
    if structured is not None:
        text = json.dumps(structured)
    usage = raw.get("usage")
    usage = usage if isinstance(usage, dict) else {}
    usage_totals = _UsageTotals()
    usage_totals.add(
        sent=(
            usage.get("input_tokens"),
            usage.get("cache_read_input_tokens"),
            usage.get("cache_creation_input_tokens"),
        ),
        received=usage.get("output_tokens"),
        cost=raw.get("total_cost_usd"),
    )
    session_id = raw.get("session_id")
    stop_reason = raw.get("stop_reason")
    return AgentCliResult(
        text=text,
        raw=raw,
        session_id=session_id if isinstance(session_id, str) else None,
        tokens_sent=usage_totals.sent,
        tokens_received=usage_totals.received,
        cost_usd=usage_totals.cost,
        finish_reason=stop_reason if isinstance(stop_reason, str) else None,
        structured_output=structured,
    )


@dataclass
class _UsageTotals:
    """Token and cost sums; each stays ``None`` until a number arrives."""

    sent: int | None = None
    received: int | None = None
    cost: float | None = None

    def add(self, *, sent: Sequence[Any], received: Any, cost: Any) -> None:
        for value in sent:
            if _is_number(value):
                self.sent = (self.sent or 0) + int(value)
        if _is_number(received):
            self.received = (self.received or 0) + int(received)
        if _is_number(cost):
            self.cost = (self.cost or 0.0) + float(cost)


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _pi_message_text(message: dict[str, Any]) -> str:
    content = message.get("content")
    if isinstance(content, str):
        return content.strip()
    if not isinstance(content, list):
        return ""
    parts = [
        item["text"]
        for item in content
        if isinstance(item, dict) and item.get("type") == "text" and isinstance(item.get("text"), str)
    ]
    return "".join(parts).strip()


def _last_json_object(stdout: str) -> dict[str, Any] | None:
    """The whole of ``stdout`` as a JSON object, else its last line that is one."""
    try:
        whole = json.loads(stdout)
    except json.JSONDecodeError:
        whole = None
    if isinstance(whole, dict):
        return whole
    for line in reversed(stdout.splitlines()):
        try:
            candidate = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(candidate, dict):
            return candidate
    return None


def _no_answer_message(name: str, proc: subprocess.CompletedProcess[str]) -> str:
    stderr = (proc.stderr or "").strip()
    if len(stderr) > _STDERR_TAIL_CHARS:
        stderr = "..." + stderr[-_STDERR_TAIL_CHARS:]
    detail = f": {stderr}" if stderr else ""
    return f"{name} exited with code {proc.returncode} without an answer{detail}"
