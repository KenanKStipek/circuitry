"""``meta.live``'s lifecycle across a retry, the repair turn and
``--resume`` (#636): fresh per attempt, carrying ``earlier_attempts``;
continuing through the repair turn under ``phase: repair turn``; and
removed before an unfinished node reruns under ``--resume``.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import write_fake_cli

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.core.cancellation import get_token
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
