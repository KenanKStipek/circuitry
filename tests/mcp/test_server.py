from __future__ import annotations

import textwrap
import time
from datetime import datetime, timezone
from pathlib import Path

import pytest

from circuitry.mcp import server as srv
from circuitry.mcp.runs import RunManager


def _write_yml(tmp_path: Path, name: str, body: str) -> Path:
    p = tmp_path / name
    p.write_text(textwrap.dedent(body), encoding="utf-8")
    return p


@pytest.fixture(autouse=True)
def fresh_manager(monkeypatch: pytest.MonkeyPatch) -> RunManager:
    """Each test gets a clean RunManager with tight timing for fast tests."""
    mgr = RunManager(
        quiesce_seconds=0.02,
        quiesce_max_wait_seconds=2.0,
        cancel_join_timeout=2.0,
        worker_poll_interval=0.05,
    )
    monkeypatch.setattr(srv, "_manager", mgr)
    return mgr


def _wait_until(predicate, timeout: float = 2.0, interval: float = 0.01) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


# ---------------------------------------------------------------------------
# 1. list_orchestrations
# ---------------------------------------------------------------------------


def test_list_orchestrations_returns_known_bundled() -> None:
    entries = srv._list_orchestrations_impl()
    names = [e["name"] for e in entries]
    # `learn/hello` is a stable bundled entry.
    assert any("hello" in n for n in names)
    # Each entry has expected keys.
    for e in entries:
        assert set(["name", "file", "description", "category"]).issubset(e.keys())


# ---------------------------------------------------------------------------
# 2-3. validate_orchestration
# ---------------------------------------------------------------------------


def test_validate_orchestration_ok(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "good.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    result = srv._validate_orchestration_impl(str(p))
    assert result == {"ok": True, "errors": [], "warnings": []}


def test_validate_orchestration_with_errors(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "bad.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: this_does_not_exist
            name: oops
    """)
    result = srv._validate_orchestration_impl(str(p))
    assert result["ok"] is False
    assert len(result["errors"]) > 0


def test_validate_orchestration_unknown_path() -> None:
    result = srv._validate_orchestration_impl("/no/such/file.yml")
    assert result["ok"] is False
    assert "not found" in result["errors"][0]


# ---------------------------------------------------------------------------
# 4-5. run_orchestration + submit_response (single-prompt)
# ---------------------------------------------------------------------------


def test_run_orchestration_returns_paused_with_one_prompt(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "one.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: greet
            template: "hi {{who}}"
    """)
    resp = srv._run_orchestration_impl(orchestration=str(p), initial_state={"who": "Ada"})
    assert resp["status"] == "paused"
    assert isinstance(resp["run_id"], str) and len(resp["run_id"]) > 0
    assert len(resp["pending_prompts"]) == 1
    pp = resp["pending_prompts"][0]
    assert pp["prompt"] == "hi Ada"
    # The orchestration pins `model: claude-sonnet-4`, host_claude records it
    # in HostPromptRequest.model, and the server surfaces it on the prompt.
    assert pp["model"] == "claude-sonnet-4"
    assert isinstance(pp["prompt_id"], str)


def test_submit_response_drives_to_completion(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "one.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: greet
            template: "ping"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    pid = started["pending_prompts"][0]["prompt_id"]
    rid = started["run_id"]

    final = srv._submit_response_impl(run_id=rid, prompt_id=pid, response="pong")
    assert final["status"] == "completed"
    assert final["pending_prompts"] == []
    assert final["state"] is not None
    assert final["state"]["prime"]["greet"]["value"] == "pong"


# ---------------------------------------------------------------------------
# 6-7. tree-flow / parallel branches
# ---------------------------------------------------------------------------


def test_run_orchestration_returns_paused_with_multiple_prompts(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "tree2.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: loop
            name: par
            flow: tree
            each:
              in: items
              as: it
            body:
              - type: prompt
                name: ask
                template: "Q: {{it}}"
    """)
    resp = srv._run_orchestration_impl(
        orchestration=str(p), initial_state={"items": ["a", "b"]}
    )
    assert resp["status"] == "paused"
    assert len(resp["pending_prompts"]) == 2
    pids = {pp["prompt_id"] for pp in resp["pending_prompts"]}
    prompts = {pp["prompt"] for pp in resp["pending_prompts"]}
    assert prompts == {"Q: a", "Q: b"}
    assert len(pids) == 2


def test_submit_responses_in_arbitrary_order(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "tree3.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: loop
            name: par
            flow: tree
            each:
              in: items
              as: it
            body:
              - type: prompt
                name: ask
                template: "{{it}}"
    """)
    resp = srv._run_orchestration_impl(
        orchestration=str(p), initial_state={"items": ["a", "b", "c"]}
    )
    assert len(resp["pending_prompts"]) == 3
    rid = resp["run_id"]
    pids = [pp["prompt_id"] for pp in resp["pending_prompts"]]

    # Submit in reverse order.
    last = None
    for pid in reversed(pids):
        last = srv._submit_response_impl(
            run_id=rid, prompt_id=pid, response=f"r-{pid[:4]}"
        )
    assert last["status"] == "completed"

    state = last["state"]
    for i in range(3):
        assert state["prime"]["par"][f"iter_{i}"]["ask"]["value"].startswith("r-")


# ---------------------------------------------------------------------------
# 8. get_run_state surfaces partial state during pause
# ---------------------------------------------------------------------------


def test_get_run_state_during_pause_includes_partial_state(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "two.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: a
            template: "first"
          - type: prompt
            name: b
            template: "second"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    rid = started["run_id"]
    pid_a = started["pending_prompts"][0]["prompt_id"]

    after_a = srv._submit_response_impl(run_id=rid, prompt_id=pid_a, response="A-out")
    # Run is paused on prompt 'b' now; the wire protocol returns state=None
    # for non-terminal statuses on submit/run, so we use get_run_state.
    assert after_a["status"] == "paused"
    assert after_a["state"] is None

    snap = srv._get_run_state_impl(run_id=rid)
    assert snap["status"] == "paused"
    assert snap["state"] is not None
    assert snap["state"]["prime"]["a"]["value"] == "A-out"
    # 'b' has not produced a value yet
    assert snap["state"].get("prime", {}).get("b", {}).get("value") is None


# ---------------------------------------------------------------------------
# 9. cancel_run
# ---------------------------------------------------------------------------


def test_cancel_run_returns_cancelled(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "cancel.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    rid = started["run_id"]
    cancelled = srv._cancel_run_impl(run_id=rid)
    assert cancelled["status"] == "cancelled"
    assert cancelled["pending_prompts"] == []


# ---------------------------------------------------------------------------
# 10-11. unknown ID error responses
# ---------------------------------------------------------------------------


def test_unknown_run_id_returns_error_response() -> None:
    for fn_call in (
        lambda: srv._submit_response_impl(run_id="bogus", prompt_id="x", response="y"),
        lambda: srv._get_run_state_impl(run_id="bogus"),
        lambda: srv._cancel_run_impl(run_id="bogus"),
    ):
        resp = fn_call()
        assert resp.get("ok") is False
        assert "Unknown run_id" in resp.get("error", "")


def test_unknown_prompt_id_returns_error_response(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "one.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    rid = started["run_id"]

    resp = srv._submit_response_impl(
        run_id=rid, prompt_id="not-a-real-prompt-id", response="x"
    )
    assert resp.get("ok") is False
    assert "Unknown prompt_id" in resp.get("error", "")

    # Run still alive: real prompt_id still works.
    real_pid = started["pending_prompts"][0]["prompt_id"]
    final = srv._submit_response_impl(run_id=rid, prompt_id=real_pid, response="ok")
    assert final["status"] == "completed"


# ---------------------------------------------------------------------------
# 12. JSON-safety of state (Path, datetime values)
# ---------------------------------------------------------------------------


def test_state_is_json_serializable() -> None:
    import json

    state = {
        "path": Path("/tmp/foo"),
        "ts": datetime(2026, 5, 8, 12, 0, tzinfo=timezone.utc),
        "nested": {"more_paths": [Path("/a"), Path("/b")], "n": 1, "ok": True},
    }
    safe = srv._to_json_safe(state)
    json.dumps(safe)  # must not raise
    assert safe["path"] == "/tmp/foo"
    assert safe["ts"].startswith("2026-05-08")
    assert safe["nested"]["more_paths"] == ["/a", "/b"]
    assert safe["nested"]["n"] == 1
    assert safe["nested"]["ok"] is True


def test_run_state_response_is_json_safe(tmp_path: Path) -> None:
    """End-to-end: a real run's state dict must round-trip through JSON."""
    import json

    p = _write_yml(tmp_path, "one.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    pid = started["pending_prompts"][0]["prompt_id"]
    final = srv._submit_response_impl(run_id=started["run_id"], prompt_id=pid, response="ok")
    json.dumps(final)  # must not raise


# ---------------------------------------------------------------------------
# 13. override_model wired through MCP layer
# ---------------------------------------------------------------------------


def test_run_orchestration_with_override_model(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "non_claude.yml", """
        adapter: host_claude
        model: gpt-4o
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    started = srv._run_orchestration_impl(
        orchestration=str(p), override_model=True, override_to="claude-opus-4-7"
    )
    assert started["status"] == "paused"
    pid = started["pending_prompts"][0]["prompt_id"]
    final = srv._submit_response_impl(
        run_id=started["run_id"], prompt_id=pid, response="ok"
    )
    assert final["status"] == "completed"
    assert final["state"]["prime"]["x"]["value"] == "ok"


def test_run_orchestration_default_rejects_non_claude_pin(tmp_path: Path) -> None:
    p = _write_yml(tmp_path, "non_claude.yml", """
        adapter: host_claude
        model: gpt-4o
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))  # no override
    assert _wait_until(lambda: started["run_id"] in srv._manager._runs, timeout=1.0)
    final = srv._get_run_state_impl(run_id=started["run_id"])
    assert _wait_until(
        lambda: srv._get_run_state_impl(run_id=started["run_id"])["status"] == "failed",
        timeout=2.0,
    )
    final = srv._get_run_state_impl(run_id=started["run_id"])
    assert "Claude-family" in (final.get("error") or "")


# ---------------------------------------------------------------------------
# 14. Server can be built without errors (smoke)
# ---------------------------------------------------------------------------


def test_server_builds_with_six_tools() -> None:
    server = srv._build_server()
    tools = server._tool_manager.list_tools()
    names = {t.name for t in tools}
    assert names == {
        "list_orchestrations",
        "validate_orchestration",
        "run_orchestration",
        "submit_response",
        "get_run_state",
        "cancel_run",
    }
