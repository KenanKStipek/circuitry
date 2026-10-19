"""Fake coding-agent CLIs for the ``agent_cli`` module and the ``pi`` /
``claude_code`` adapters. No test runs a real CLI or calls a real model.

:func:`write_fake_cli` writes an executable Python script that, when
``FAKE_CLI_RECORD`` is set, records its argv, environment, stdin, working
directory and the text of every ``@<file>`` argument as JSON, then runs
``body`` and prints ``stdout``/``stderr`` and exits with ``exit_code``.
"""

from __future__ import annotations

import json
import os
import stat
import sys
import time
from pathlib import Path
from typing import Any

_SCRIPT = """\
#!{python}
import json, os, sys

stdin = sys.stdin.read()
record = os.environ.get("FAKE_CLI_RECORD")
if record:
    attachments = {{}}
    for arg in sys.argv[1:]:
        if arg.startswith("@"):
            with open(arg[1:], encoding="utf-8") as handle:
                attachments[arg[1:]] = handle.read()
    with open(record, "w", encoding="utf-8") as handle:
        json.dump(
            {{
                "argv": sys.argv[1:],
                "env": dict(os.environ),
                "stdin": stdin,
                "cwd": os.getcwd(),
                "attachments": attachments,
            }},
            handle,
        )
{body}
sys.stdout.write({stdout!r})
sys.stderr.write({stderr!r})
sys.exit({exit_code})
"""


def write_fake_cli(
    directory: Path,
    name: str,
    *,
    stdout: str = "",
    stderr: str = "",
    exit_code: int = 0,
    body: str = "",
) -> Path:
    path = directory / name
    path.write_text(
        _SCRIPT.format(
            python=sys.executable,
            body=body,
            stdout=stdout,
            stderr=stderr,
            exit_code=exit_code,
        ),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


def read_record(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


#: A ``body`` that starts a long-sleeping child, writes its pid to the file
#: named by ``FAKE_CLI_CHILD_PID``, and then sleeps itself.
SPAWN_CHILD_AND_HANG = """\
import subprocess, time
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
with open(os.environ["FAKE_CLI_CHILD_PID"], "w", encoding="utf-8") as handle:
    handle.write(str(child.pid))
time.sleep(60)
"""


#: A ``body`` that answers like Claude Code's ``--json-schema``, which it
#: implements as a tool: a schema whose top-level ``type`` is not
#: ``"object"`` fails with the API's 400 error, otherwise the answer is the
#: JSON in ``FAKE_CLAUDE_STRUCTURED_OUTPUT`` as ``structured_output``.
CLAUDE_JSON_SCHEMA_TOOL = """\
schema = json.loads(sys.argv[sys.argv.index("--json-schema") + 1])
result = {"type": "result", "subtype": "success", "session_id": "sess-2"}
if schema.get("type") != "object":
    result.update(is_error=True, result="API Error: 400 tools.0.custom.input_schema.type: "
                  "Input should be 'object'")
    print(json.dumps(result))
    sys.exit(1)
structured = json.loads(os.environ["FAKE_CLAUDE_STRUCTURED_OUTPUT"])
result.update(is_error=False, result="", structured_output=structured)
print(json.dumps(result))
sys.exit(0)
"""


def pid_alive(pid: int) -> bool:
    """Whether ``pid`` is running; a killed but not yet reaped (zombie)
    process counts as dead where ``/proc`` can tell."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # EPERM: the id now belongs to another user's process. One this test
        # started is always signalable by it, so that one is gone (#559).
        return False
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
            state = handle.read().rsplit(")", 1)[1].split()[0]
    except (OSError, IndexError):
        return True
    return state not in ("Z", "X")


def wait_until_dead(pid: int, *, seconds: float = 5.0) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if not pid_alive(pid):
            return True
        time.sleep(0.05)
    return not pid_alive(pid)


def pi_events(
    text: str = "hello",
    *,
    stop_reason: str = "stop",
    error_message: str | None = None,
    usage: dict[str, Any] | None = None,
) -> str:
    """A pi ``--mode json`` event stream with one assistant message."""
    message: dict[str, Any] = {
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "stopReason": stop_reason,
    }
    if usage is not None:
        message["usage"] = usage
    if error_message is not None:
        message["errorMessage"] = error_message
    events = [
        {"type": "session", "id": "sess-1", "cwd": "/tmp"},
        {"type": "message_end", "message": {"role": "user", "content": [{"type": "text", "text": "q"}]}},
        {"type": "message_end", "message": message},
        {"type": "agent_end"},
        {"type": "agent_settled"},
    ]
    return "".join(json.dumps(event) + "\n" for event in events)


def claude_result(**fields: Any) -> str:
    """A ``claude --output-format json`` result object."""
    result: dict[str, Any] = {
        "type": "result",
        "subtype": "success",
        "is_error": False,
        "result": "hello",
        "session_id": "sess-2",
        "total_cost_usd": 0.0123,
        "usage": {
            "input_tokens": 10,
            "cache_creation_input_tokens": 100,
            "cache_read_input_tokens": 1000,
            "output_tokens": 5,
        },
        "num_turns": 1,
        "duration_ms": 1200,
    }
    result.update(fields)
    return json.dumps(result)


#: A ``body`` for :func:`write_fake_cli` that replays ``FAKE_STREAM`` (a
#: JSONL file) to stdout, one line at a time, flushing immediately -- so a
#: test watching ``--live-state``/the transcript while this runs sees each
#: line arrive on its own, not only once the whole call finishes. Two
#: pseudo-lines never reach stdout: ``{"sleep": s}`` pauses for *s* seconds;
#: ``{"wait_for": path}`` blocks until *path* exists, so a test can gate the
#: stream with a file instead of sleeping and guessing a timing margin.
#: Every other line is a real recorded CLI line, printed as-is.
REPLAY_BODY = """\
import time as _time
with open(os.environ["FAKE_STREAM"], encoding="utf-8") as _stream:
    for _raw in _stream:
        _raw = _raw.rstrip("\\n")
        if not _raw.strip():
            continue
        try:
            _pseudo = json.loads(_raw)
        except json.JSONDecodeError:
            _pseudo = None
        if isinstance(_pseudo, dict) and set(_pseudo) == {"sleep"}:
            _time.sleep(_pseudo["sleep"])
            continue
        if isinstance(_pseudo, dict) and set(_pseudo) == {"wait_for"}:
            while not os.path.exists(_pseudo["wait_for"]):
                _time.sleep(0.01)
            continue
        sys.stdout.write(_raw + "\\n")
        sys.stdout.flush()
"""


def write_replay_stream(path: Path, lines: list[Any]) -> Path:
    """Write *lines* (dicts, including ``{"sleep": s}``/``{"wait_for": p}``
    pseudo-lines, or already-JSON strings) as JSONL to *path*, for
    :data:`REPLAY_BODY` to replay."""
    with path.open("w", encoding="utf-8") as handle:
        for line in lines:
            handle.write((line if isinstance(line, str) else json.dumps(line)) + "\n")
    return path


def write_replay_cli(directory: Path, name: str, *, exit_code: int = 0) -> Path:
    """A fake CLI (see :func:`write_fake_cli`) that replays whatever JSONL
    file the ``FAKE_STREAM`` env var names, through :data:`REPLAY_BODY` --
    not baked in, so one fake binary can replay a different stream per
    call."""
    return write_fake_cli(directory, name, body=REPLAY_BODY, exit_code=exit_code)


#: Small recorded streams (scrubbed of any real path, id or text) replayed
#: by the tests that gate a fake CLI mid-stream with ``wait_for`` (#636).
#: Both pause at ``gate1``/``gate2``, relative to the agent effect's own
#: ``cwd`` -- a test releases them by creating that file there.
FIXTURES_DIR = Path(__file__).parent / "fixtures" / "agent_live"


def read_fixture_stream(name: str) -> str:
    """The raw JSONL text of ``tests/fixtures/agent_live/<name>``."""
    return (FIXTURES_DIR / name).read_text(encoding="utf-8")
