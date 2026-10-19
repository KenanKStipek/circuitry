"""``meta.live``'s lifecycle across a retry, the repair turn and
``--resume`` (#636): fresh per attempt, carrying ``earlier_attempts``;
continuing through the repair turn under ``phase: repair``; and removed
before an unfinished node reruns under ``--resume``.
"""

from __future__ import annotations

import json
import os
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import wait_until_dead, write_fake_cli

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.core import cancellation
from circuitry.core.cancellation import (
    CancellationToken,
    RunCancelledBySignal,
    get_token,
)
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime

runner = CliRunner()


#: Fails outright on its first call (a marker file tracks that across the
#: retry's fresh process) and succeeds on the second, so one retry carries
#: real per-attempt usage into ``earlier_attempts`` (#636).
FAKE_PI_FAIL_THEN_SUCCEED = """\
marker = os.environ["FAKE_RETRY_MARKER"]
if not os.path.exists(marker):
    open(marker, "w").close()
    sys.exit(3)
for event in [
    {"type": "session", "id": "sess-ok", "cwd": os.getcwd()},
    {"type": "message_end", "message": {"role": "assistant",
        "usage": {"input": 20, "output": 8, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.01}},
        "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}},
    {"type": "agent_end"},
]:
    sys.stdout.write(json.dumps(event) + "\\n")
"""


def test_retried_attempt_gets_a_fresh_meta_live_carrying_earlier_attempts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from circuitry.plugins.agent import AgentPlugin

    monkeypatch.setattr(get_token(), "sleep_or_raise", lambda s: None)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "pi", body=FAKE_PI_FAIL_THEN_SUCCEED)
    plugin = AgentPlugin(default_engine="pi", binaries={"pi": str(fake)})
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    defn = ToolDefinition(
        name="x",
        provider="agent",
        params={
            "prompt": "do it",
            "session": "earlier-id",
            "env": {"FAKE_RETRY_MARKER": str(tmp_path / "retried")},
        },
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=1),
    )
    store = Store({})
    live_calls: list[dict[str, Any]] = []
    original_set = store.set

    def _recording_set(path: str, value: Any) -> None:
        if path.endswith(".meta.live") and isinstance(value, dict):
            live_calls.append(value)
        original_set(path, value)

    store.set = _recording_set  # type: ignore[method-assign]
    ToolRuntime(defn).execute(store=store, ctx={})

    assert live_calls, "the agent plugin never published meta.live"
    assert live_calls[0]["attempt"] == 1
    assert live_calls[0].get("earlier_attempts") is None
    assert any(call["attempt"] == 2 for call in live_calls)
    second_attempt = next(call for call in live_calls if call["attempt"] == 2)
    assert second_attempt["earlier_attempts"] == {"tokens_sent": None, "tokens_received": None}
    assert store.state["x"]["meta"]["raw"]["session_id"] == "sess-ok"
    assert "live" not in store.state["x"]["meta"]


#: Streams one line (so meta.live gets a session id), then spawns a child
#: and hangs like SPAWN_CHILD_AND_HANG -- enough to be cancelled mid-stream.
FAKE_PI_STREAM_THEN_HANG = """\
import subprocess, time
sys.stdout.write(json.dumps({"type": "session", "id": "hang-session", "cwd": os.getcwd()}) + "\\n")
sys.stdout.flush()
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
with open(os.environ["FAKE_CLI_CHILD_PID"], "w", encoding="utf-8") as handle:
    handle.write(str(child.pid))
time.sleep(60)
"""


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX-only")
def test_a_real_interrupt_leaves_meta_live_in_place(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``RunCancelledBySignal`` is a ``BaseException``, so ``ToolRuntime``'s
    own ``except Exception`` never catches it and never reaches the
    ``meta.pop("live", None)`` on its failure path -- an interrupted
    attempt leaves ``meta.live`` exactly as it last published it, the only
    record of that attempt's session id and transcript (#636).
    """
    from circuitry.plugins.agent import AgentPlugin

    token = CancellationToken()
    monkeypatch.setattr(cancellation, "_token", token)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "pi", body=FAKE_PI_STREAM_THEN_HANG)
    plugin = AgentPlugin(default_engine="pi", binaries={"pi": str(fake)})
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)

    pid_file = tmp_path / "child.pid"
    defn = ToolDefinition(
        name="x",
        provider="agent",
        params={"prompt": "hang", "env": {"FAKE_CLI_CHILD_PID": str(pid_file)}},
    )
    store = Store({})
    outcome: list[BaseException] = []

    def _call() -> None:
        try:
            ToolRuntime(defn).execute(store=store, ctx={})
        except BaseException as exc:
            outcome.append(exc)

    thread = threading.Thread(target=_call)
    thread.start()
    deadline = time.monotonic() + 20

    def _live_session_id() -> str | None:
        node = store.state.get("x", {})
        live = node.get("meta", {}).get("live")
        return live.get("session_id") if isinstance(live, dict) else None

    while _live_session_id() != "hang-session" and time.monotonic() < deadline:
        time.sleep(0.02)
    assert _live_session_id() == "hang-session", "meta.live never reached the running session"
    assert pid_file.exists() and pid_file.read_text()
    child_pid = int(pid_file.read_text())

    token.request()
    thread.join(timeout=20)
    assert not thread.is_alive()
    assert len(outcome) == 1 and isinstance(outcome[0], RunCancelledBySignal)
    assert wait_until_dead(child_pid), "the agent's own child survived the cancellation"

    live_after = store.state["x"]["meta"]["live"]
    assert live_after["session_id"] == "hang-session"


DOCUMENT = """\
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      prompt: "do the task"
      cwd: {cwd}
"""

#: Streams one reply, so the run finishes quickly for the --resume test.
FAKE_PI_DONE = """\
for event in [
    {"type": "session", "id": "fresh-session", "cwd": os.getcwd()},
    {"type": "message_end", "message": {"role": "assistant",
        "usage": {"input": 5, "output": 2, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.001}},
        "content": [{"type": "text", "text": "done"}], "stopReason": "stop"}},
    {"type": "agent_end"},
]:
    sys.stdout.write(json.dumps(event) + "\\n")
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def test_resume_of_an_unfinished_node_clears_the_stale_meta_live(tmp_path: Path) -> None:
    work = tmp_path / "work"
    work.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "pi", body=FAKE_PI_DONE)
    orch = _write(tmp_path / "doc.yml", DOCUMENT.format(cwd=json.dumps(str(work))))
    config = _write(
        tmp_path / "config.json",
        json.dumps({"runtime": {"plugins": {"agent": {"pi": {"binary": str(fake)}}}}}),
    )
    # A crafted state as if the run was interrupted mid-session: the node
    # never completed, but meta.live still carries an earlier, now-stale
    # session's data (#636's point 7).
    stale_state = {
        "prime": {
            "implement": {
                "value": None,
                "meta": {
                    "created_at": "2020-01-01T00:00:00+00:00",
                    "completed_at": None,
                    "provider": "agent",
                    "error": None,
                    "live": {
                        "v": 1, "engine": "pi", "session_id": "stale-session",
                        "phase": "turn", "turns": 99, "tool_calls": 99,
                    },
                },
            }
        }
    }
    state_path = _write(tmp_path / "state.json", json.dumps(stale_state))
    out = tmp_path / "out.json"

    result = runner.invoke(
        app,
        [
            "run",
            str(orch),
            "--config",
            str(config),
            "--state",
            str(state_path),
            "--resume",
            "last",
            "--force",
            "--out",
            str(out),
        ],
    )
    assert result.exit_code == 0, result.stdout
    final_state = json.loads(out.read_text(encoding="utf-8"))
    node = final_state["prime"]["implement"]
    assert node["meta"]["completed_at"] is not None
    assert "live" not in node["meta"]
    assert node["meta"]["raw"]["session_id"] == "fresh-session"
