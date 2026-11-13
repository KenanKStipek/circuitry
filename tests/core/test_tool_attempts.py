"""``meta.attempts`` on a retried `tool` (#668), mirroring `use`'s own
(#659): one entry per attempt, oldest first, present only when more than
one attempt ran.
"""

from __future__ import annotations

from copy import deepcopy
from unittest.mock import MagicMock

import pytest

from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult


def _make_store() -> Store:
    return Store({})


def _patch_plugin(monkeypatch: pytest.MonkeyPatch, plugin: MagicMock) -> None:
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: plugin)
    from circuitry.core.cancellation import get_token

    monkeypatch.setattr(get_token(), "sleep_or_raise", lambda s: None)


def test_attempts_absent_when_only_one_attempt_ran(monkeypatch: pytest.MonkeyPatch) -> None:
    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x", provider="shell", params={}, retries=RetryPolicyDef(max_attempts=3, backoff_ms=10)
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert "attempts" not in store.state["x"]["meta"]
    assert "retries_used" not in store.state["x"]["meta"]


def test_attempts_tool_passes_on_third_attempt_expect_driven(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    present_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [missing_result, missing_result, present_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    node = store.state["x"]
    meta = node["meta"]
    assert node["value"] == {"prompt_id": "abc"}
    assert meta["retries_used"] == 2
    assert list(meta)[-1] == "attempts"

    attempts = meta["attempts"]
    assert len(attempts) == 3
    assert attempts[0]["value"] == {}
    assert attempts[0]["expect"]["result"] is False
    assert attempts[0]["error"] is not None
    assert attempts[1]["expect"]["result"] is False
    assert attempts[2]["value"] == {"prompt_id": "abc"}
    assert attempts[2]["expect"]["result"] is True
    assert attempts[2]["error"] is None

    # The deciding (last) attempt repeats the node's own fields.
    assert attempts[-1]["value"] == node["value"]
    assert attempts[-1]["expect"] == meta["expect"]
    assert attempts[-1]["error"] == meta["error"]


def test_attempts_tool_passes_on_third_attempt_exit_code_driven_via_allow_nonzero(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A process tool run with ``allow_nonzero`` doesn't raise on a nonzero
    exit (``ok`` stays ``True``) -- an ``expect:`` on ``meta.exit_code`` is
    what forces the retry here, not the plugin itself."""
    failing = ToolResult(value="out", raw={}, stdout="", stderr="", exit_code=1)
    succeeding = ToolResult(value="out", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [failing, failing, succeeding]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={"allow_nonzero": True},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="meta.exit_code == 0"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert meta["retries_used"] == 2
    attempts = meta["attempts"]
    assert len(attempts) == 3
    assert [a["exit_code"] for a in attempts] == [1, 1, 0]
    assert attempts[0]["expect"]["result"] is False
    assert attempts[2]["expect"]["result"] is True


def test_attempts_entry_key_order(monkeypatch: pytest.MonkeyPatch) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    present_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [missing_result, present_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    for entry in store.state["x"]["meta"]["attempts"]:
        assert list(entry) == [
            "created_at",
            "completed_at",
            "value",
            "error",
            "expect",
            "exit_code",
        ]


def test_attempts_on_error_continue_keeps_every_attempt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("always fails")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        on_error="continue",
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    node = store.state["x"]
    assert node["value"] is None
    attempts = node["meta"]["attempts"]
    assert len(attempts) == 2
    assert all(a["error"] is not None for a in attempts)
    assert node["meta"]["retries_used"] == 1


def test_attempts_is_reset_on_a_reused_node(monkeypatch: pytest.MonkeyPatch) -> None:
    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x", provider="shell", params={}, retries=RetryPolicyDef(max_attempts=3, backoff_ms=10)
    )
    store = _make_store()
    node = store.ensure_dict("x")
    node["meta"] = {
        "retries_used": 2,
        "attempts": [
            {
                "created_at": "T",
                "completed_at": "T",
                "value": None,
                "error": "boom",
                "expect": None,
                "exit_code": None,
            }
        ],
    }

    ToolRuntime(defn).execute(store=store, ctx={})

    assert "attempts" not in store.state["x"]["meta"]
    assert "retries_used" not in store.state["x"]["meta"]


def test_attempts_live_published_before_the_backoff_wait(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    present_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [missing_result, missing_result, present_result]
    _patch_plugin(monkeypatch, plugin)

    published: list[dict] = []
    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = Store({}, on_write=lambda state: published.append(deepcopy(state)))
    ToolRuntime(defn).execute(store=store, ctx={})

    assert any(
        "attempts" in (snap.get("x", {}).get("meta") or {})
        and len(snap["x"]["meta"]["attempts"]) < 3
        for snap in published
    )
