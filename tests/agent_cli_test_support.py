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


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


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
