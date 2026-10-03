from __future__ import annotations

import textwrap
import time
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import pytest

from circuitry.mcp import server as srv
from circuitry.mcp.runs import Run, RunManager, RunStatus


def _write_yml(tmp_path: Path, name: str, body: str) -> Path:
    p = tmp_path / name
    p.write_text(textwrap.dedent(body), encoding="utf-8")
    return p


@pytest.fixture(autouse=True)
def fresh_manager(monkeypatch: pytest.MonkeyPatch) -> RunManager:
    """Each test gets a clean RunManager with tight timing for fast tests."""
    mgr = RunManager(
        quiesce_max_wait_seconds=2.0,
        cancel_join_timeout=2.0,
        worker_poll_interval=0.05,
    )
    monkeypatch.setattr(srv, "_manager", mgr)
    yield mgr
    # Cancel any runs still alive: their daemon worker thread is fine, but a
    # tree-flow orchestration leaves non-daemon ThreadPoolExecutor workers
    # blocked on response queues, which would hang interpreter shutdown.
    for run_id in list(mgr._runs):
        run = mgr._runs[run_id]
        if not run.status.is_terminal:
            try:
                mgr.cancel_run(run_id)
            except KeyError:
                pass


def _wait_until(predicate, timeout: float = 2.0, interval: float = 0.01) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _settled(
    run_id: str,
    *,
    until: Callable[[dict[str, Any]], bool] | None = None,
    timeout: float = 2.0,
    interval: float = 0.01,
) -> dict[str, Any]:
    """get_run_state, polled until `until` (default: status != 'running').

    _run_orchestration_impl/_submit_response_impl return whatever the
    RunManager's internal quiesce wait considered settled, but a stable
    keyset (worker thread not yet scheduled, or an answered prompt not yet
    popped) looks identical to a genuinely settled one to that wait — same
    race as issue #191, reachable here since this layer wraps the same
    calls. Callers asserting on pending_prompts contents/count, or on a
    specific terminal status, must pass a matching `until` predicate.
    """
    predicate = until or (lambda resp: resp["status"] != "running")
    resp = srv._get_run_state_impl(run_id=run_id)
    deadline = time.monotonic() + timeout
    while not predicate(resp) and time.monotonic() < deadline:
        time.sleep(interval)
        resp = srv._get_run_state_impl(run_id=run_id)
    return resp


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
        assert {"name", "file", "description", "category"}.issubset(e.keys())


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


def test_validate_orchestration_uses_the_resolved_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """MCP's `validate_orchestration` must use the same resolved config the
    real `run_orchestration` tool uses (`resolve_config()`), so a tool
    outside the allowlist fails validation here too — not just at run time
    (#265 part 4)."""
    monkeypatch.setenv("CIRCUITRY_ENABLED_TOOLS", "")  # lock every tool out
    p = _write_yml(tmp_path, "locked.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: tool
            name: t
            provider: ffmpeg
            params: {}
    """)
    result = srv._validate_orchestration_impl(str(p))
    assert result["ok"] is False
    assert any("ffmpeg" in e for e in result["errors"])


def test_validate_orchestration_skips_the_document_adapter_check(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Every MCP run injects a `HostClaudeAdapter` regardless of the
    document's own top-level `adapter:` (RunManager.start_run always passes
    its own `adapter=`), so checking that name here must not reject a
    document that runs fine through `run_orchestration` — the same
    `skip_adapter_check` RunManager's own preflight call already uses
    (#265 part 4)"""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write_yml(tmp_path, "openai.yml", """
        adapter: openai
        model: gpt-4
        effects:
          - type: prompt
            name: x
            template: "hi"
    """)
    result = srv._validate_orchestration_impl(str(p))
    assert result["ok"] is True
    assert result["errors"] == []


def test_validate_orchestration_still_checks_a_per_effect_provider(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Unlike the document-level default, a prompt effect's own `provider:`
    is really built and called at run time (`PromptRuntime._resolve_adapter`)
    even when MCP injects a HostClaudeAdapter, so it must still fail
    validation here (#265 part 9)."""
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    p = _write_yml(tmp_path, "openai_provider.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: x
            provider: openai
            template: "hi"
    """)
    result = srv._validate_orchestration_impl(str(p))
    assert result["ok"] is False
    assert any("OPENAI_API_KEY" in e for e in result["errors"])


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
            template: "hi {{input.who}}"
    """)
    resp = srv._run_orchestration_impl(orchestration=str(p), initial_state={"who": "Ada"})
    assert isinstance(resp["run_id"], str) and len(resp["run_id"]) > 0
    resp = _settled(resp["run_id"])
    assert resp["status"] == "paused"
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
    rid = started["run_id"]
    pid = _settled(rid)["pending_prompts"][0]["prompt_id"]

    srv._submit_response_impl(run_id=rid, prompt_id=pid, response="pong")
    final = _settled(rid)
    assert final["status"] == "completed"
    assert final["pending_prompts"] == []
    assert final["state"] is not None
    assert final["state"]["prime"]["greet"]["value"] == "pong"
    assert final["warnings"] == []


def test_run_response_surfaces_an_untrusted_project_config_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "circuitry.config.json").write_text(
        '{"default_model": "project-model"}', encoding="utf-8"
    )
    monkeypatch.chdir(tmp_path)
    p = _write_yml(tmp_path, "one.yml", """
        adapter: host_claude
        model: claude-sonnet-4
        effects:
          - type: prompt
            name: greet
            template: "ping"
    """)
    started = srv._run_orchestration_impl(orchestration=str(p))
    rid = started["run_id"]
    pid = _settled(rid)["pending_prompts"][0]["prompt_id"]

    srv._submit_response_impl(run_id=rid, prompt_id=pid, response="pong")
    final = _settled(rid)
    assert final["status"] == "completed"
    assert any("Skipped project config" in w for w in final["warnings"])


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
              in: input.items
              as: it
            body:
              - type: prompt
                name: ask
                template: "Q: {{it}}"
    """)
    resp = srv._run_orchestration_impl(
        orchestration=str(p), initial_state={"items": ["a", "b"]}
    )
    resp = _settled(
        resp["run_id"],
        until=lambda r: r["status"] == "paused" and len(r["pending_prompts"]) == 2,
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
              in: input.items
              as: it
            body:
              - type: prompt
                name: ask
                template: "{{it}}"
    """)
    resp = srv._run_orchestration_impl(
        orchestration=str(p), initial_state={"items": ["a", "b", "c"]}
    )
    rid = resp["run_id"]
    resp = _settled(
        rid,
        until=lambda r: r["status"] == "paused" and len(r["pending_prompts"]) == 3,
    )
    assert len(resp["pending_prompts"]) == 3
    pids = [pp["prompt_id"] for pp in resp["pending_prompts"]]

    # Submit in reverse order.
    for pid in reversed(pids):
        srv._submit_response_impl(
            run_id=rid, prompt_id=pid, response=f"r-{pid[:4]}"
        )
    last = _settled(rid, until=lambda r: r["status"] == "completed")
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
    pid_a = _settled(rid)["pending_prompts"][0]["prompt_id"]

    after_a = srv._submit_response_impl(run_id=rid, prompt_id=pid_a, response="A-out")
    # Run is paused on prompt 'b' now; the wire protocol returns state=None
    # for non-terminal statuses on submit/run, so we use get_run_state. Wait
    # for a pending prompt distinct from pid_a, not just "not running" -
    # pid_a can still be the (stale) sole pending entry for a beat after
    # submit_response returns (issue #191's race, same window).
    settled_after_a = _settled(
        rid,
        until=lambda r: r["status"] == "paused"
        and len(r["pending_prompts"]) == 1
        and r["pending_prompts"][0]["prompt_id"] != pid_a,
    )
    assert settled_after_a["status"] == "paused"
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
    real_pid = _settled(rid)["pending_prompts"][0]["prompt_id"]
    srv._submit_response_impl(run_id=rid, prompt_id=real_pid, response="ok")
    assert _settled(rid, until=lambda r: r["status"] == "completed")["status"] == "completed"


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


def test_run_response_writes_loop_last_as_a_reference_not_a_full_copy() -> None:
    """A loop's in-memory ``last`` alias is reported the way ``--out``
    writes it -- ``{"$ref": "iter_N"}`` -- not a second full copy of the
    final pass (#236)."""
    node = {"iter_0": {"value": "a"}, "iter_1": {"value": "b"}}
    node["last"] = node["iter_1"]
    run = Run(
        run_id="run-1",
        orchestration_path=Path("/tmp/orch.yml"),
        status=RunStatus.COMPLETED,
        state={"prime": {"loop": node}},
    )

    payload = srv._run_response(run, include_state=True)

    assert payload["state"]["prime"]["loop"]["last"] == {"$ref": "iter_1"}
    # The run's own live state is untouched -- still a real alias.
    assert run.state["prime"]["loop"]["last"] is run.state["prime"]["loop"]["iter_1"]


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
    pid = _settled(started["run_id"])["pending_prompts"][0]["prompt_id"]
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
    rid = started["run_id"]
    settled = _settled(rid)
    assert settled["status"] == "paused"
    pid = settled["pending_prompts"][0]["prompt_id"]
    srv._submit_response_impl(run_id=rid, prompt_id=pid, response="ok")
    final = _settled(rid, until=lambda r: r["status"] == "completed")
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
