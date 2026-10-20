"""`retries` and `expect:` on `tool` effects (#273)."""

from __future__ import annotations

import urllib.error
from unittest.mock import MagicMock

import pytest

from circuitry.core.compiler import _compile_effect
from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult


def _make_store() -> Store:
    return Store({})


def _patch_plugin(monkeypatch: pytest.MonkeyPatch, plugin: MagicMock) -> None:
    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: plugin
    )
    # Retry backoff is a cancellation-aware wait (#356), not a bare
    # time.sleep — patch that instead so these tests don't actually block.
    from circuitry.core.cancellation import get_token

    monkeypatch.setattr(get_token(), "sleep_or_raise", lambda s: None)


# ---------------------------------------------------------------------------
# Compiler
# ---------------------------------------------------------------------------


def test_compile_tool_retries_field() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "retries": {"max_attempts": 3, "backoff_ms": 200},
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.retries == RetryPolicyDef(max_attempts=3, backoff_ms=200)


def test_compile_tool_expect_bare_string_is_cel_shorthand() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": "has(value.prompt_id)",
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.expect == ExpectDef(mode="cel", expr="has(value.prompt_id)")


def test_compile_tool_expect_model_mode_requires_template() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": {"mode": "model"},
    }
    with pytest.raises(ValueError, match="requires a non-empty 'template'"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_tool_expect_bad_cel_syntax_raises() -> None:
    effect = {
        "type": "tool",
        "name": "x",
        "provider": "ffmpeg",
        "expect": "has(value.prompt_id",
    }
    with pytest.raises(ValueError, match="does not parse"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


# ---------------------------------------------------------------------------
# Runtime — retries
# ---------------------------------------------------------------------------


def test_process_tool_retries_on_any_failure_then_succeeds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [RuntimeError("transient"), ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == "done"
    assert store.state["x"]["meta"]["error"] is None
    assert store.state["x"]["meta"]["retries_used"] == 1
    assert plugin.execute.call_count == 2


def test_process_tool_exhausts_retries_and_fails(
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
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="always fails"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 2
    assert "always fails" in store.state["x"]["meta"]["error"]


def test_tool_without_retries_fails_on_first_attempt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("boom")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    with pytest.raises(RuntimeError, match="boom"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 1


# ---------------------------------------------------------------------------
# Runtime — a plugin's ToolResult usage fields (#414)
# ---------------------------------------------------------------------------


def test_a_plugin_reporting_no_usage_writes_no_token_or_cost_meta_keys(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Every provider except ``agent`` leaves ``ToolResult.tokens_sent`` /
    ``tokens_received`` / ``cost_usd`` at their ``None`` default — none of
    those meta keys should appear at all."""
    ok_result = ToolResult(value="done", raw={})
    plugin = MagicMock()
    plugin.execute.side_effect = [ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    for key in ("tokens_sent", "tokens_received", "tokens_sent_total", "tokens_received_total", "cost_usd"):
        assert key not in meta


def test_a_plugins_usage_lands_under_the_same_meta_keys_a_prompt_effect_uses(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok_result = ToolResult(
        value="done", raw={}, tokens_sent=5, tokens_received=2, cost_usd=0.01
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert meta["tokens_sent"] == 5
    assert meta["tokens_received"] == 2
    assert meta["tokens_sent_total"] == 5
    assert meta["tokens_received_total"] == 2
    assert meta["cost_usd"] == 0.01


def test_usage_sums_across_retries_like_a_prompt_effects_tokens_sent_total(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A failed attempt's usage still counts toward this pass's total
    (``tokens_sent_total``/``tokens_received_total``), the same split a
    retried/fallen-back-from prompt effect's own tokens record; the
    non-total fields are the attempt that decided the outcome."""
    fail_result = ToolResult(
        value=None, raw={}, ok=False, stderr="transient", tokens_sent=3, tokens_received=1
    )
    ok_result = ToolResult(
        value="done", raw={}, tokens_sent=5, tokens_received=2, cost_usd=0.02
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert meta["tokens_sent"] == 5
    assert meta["tokens_received"] == 2
    assert meta["tokens_sent_total"] == 3 + 5
    assert meta["tokens_received_total"] == 1 + 2
    assert meta["cost_usd"] == 0.02


def test_usage_before_an_exhausted_retrys_failure_still_counts_in_the_total(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Usage an attempt spent before the effect ultimately fails is kept in
    ``tokens_sent_total``/``tokens_received_total`` — the run's totals
    (``_TotalsAccumulator``) prefer that field over the plain one, so it is
    not lost (#391, #414)."""
    fail_result = ToolResult(
        value=None, raw={}, ok=False, stderr="still failing", tokens_sent=3, tokens_received=1
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, fail_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="still failing"):
        ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert "tokens_sent" not in meta
    assert meta["tokens_sent_total"] == 3 + 3
    assert meta["tokens_received_total"] == 1 + 1


def test_usage_before_a_later_attempt_that_raises_still_counts_in_the_total(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The same as the previous test, but the exhausted attempt raises
    outright instead of returning an ``ok=False`` ``ToolResult`` — the
    agent tool's own failure shape when a fresh session's first turn fails
    before it has an id to report (#391, #414). The running total written
    back by the ok=False attempt must not be lost just because no
    ``ToolResult`` reaches ``meta`` on the attempt that ends the retry
    loop."""
    fail_result = ToolResult(
        value=None, raw={}, ok=False, stderr="still failing", tokens_sent=3, tokens_received=1
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, RuntimeError("boom")]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="boom"):
        ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert "tokens_sent" not in meta
    assert meta["tokens_sent_total"] == 3
    assert meta["tokens_received_total"] == 1


def test_usage_before_a_later_attempt_that_raises_still_reaches_the_run_totals(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Same scenario as the previous test, driven through a real run
    (#391, #414): the running total an earlier ok=False attempt wrote
    must still reach ``state.runtime.last_run.totals`` even though the
    attempt that ends the retry loop never produces a ``ToolResult`` at
    all."""
    import yaml

    from circuitry.cli.config import CircuitryConfig
    from circuitry.cli.runtime_shim import RunRequest, run

    fail_result = ToolResult(
        value=None, raw={}, ok=False, stderr="still failing", tokens_sent=3, tokens_received=1
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, RuntimeError("boom")]
    _patch_plugin(monkeypatch, plugin)

    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "effects": [
                    {
                        "type": "tool",
                        "name": "x",
                        "provider": "shell",
                        "params": {},
                        "retries": {"max_attempts": 2, "backoff_ms": 10},
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=None,
            config=CircuitryConfig(),
            skip_preflight=True,
        )
    )
    assert result.ok is False
    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["tokens_sent"] == 3
    assert totals["tokens_received"] == 1


def test_a_reused_nodes_stale_usage_does_not_survive_a_pass_without_any(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unnamed loop's reused node must not keep a prior pass's tokens
    sitting next to a pass that reported none — the same rule #260 already
    holds for stdout/stderr/exit_code."""
    with_usage = ToolResult(value="a", raw={}, tokens_sent=5, tokens_received=2)
    without_usage = ToolResult(value="b", raw={})
    plugin = MagicMock()
    plugin.execute.side_effect = [with_usage, without_usage]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert store.state["x"]["meta"]["tokens_sent"] == 5

    ToolRuntime(defn).execute(store=store, ctx={})
    meta = store.state["x"]["meta"]
    assert "tokens_sent" not in meta
    assert "tokens_sent_total" not in meta


def test_http_family_5xx_is_retried(monkeypatch: pytest.MonkeyPatch) -> None:
    fail_result = ToolResult(
        value=None, raw={"status": 503}, stdout="", stderr="server busy", exit_code=None, ok=False
    )
    ok_result = ToolResult(value={"ok": True}, raw={"status": 200}, stdout="", stderr="", exit_code=None)
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result, ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == {"ok": True}
    assert plugin.execute.call_count == 2


def test_tool_created_at_is_the_first_attempts_start_not_the_last(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#421: a retried tool's created_at is the start of its first attempt,
    not reset by a later attempt within the same pass — unlike retries_used,
    which does track the attempt that decided the outcome."""
    from circuitry.core import tool as tool_mod

    timestamps = iter(["T0", "T1", "T2"])
    monkeypatch.setattr(tool_mod, "_now_iso", lambda: next(timestamps))

    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [RuntimeError("transient"), ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert meta["created_at"] == "T0"
    assert meta["completed_at"] == "T2"
    assert meta["retries_used"] == 1


def test_tool_exhausting_retries_still_records_created_at_and_retries_used(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#421: retries_used is set on a failed outcome too, not only on
    success — and created_at still names the first attempt's start."""
    from circuitry.core import tool as tool_mod

    timestamps = iter(["T0", "T1", "T2"])
    monkeypatch.setattr(tool_mod, "_now_iso", lambda: next(timestamps))

    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("always fails")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="always fails"):
        ToolRuntime(defn).execute(store=store, ctx={})

    meta = store.state["x"]["meta"]
    assert meta["created_at"] == "T0"
    assert meta["completed_at"] == "T2"
    assert meta["retries_used"] == 1


def test_effect_complete_observer_sees_retries_used_on_exhausted_retry(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#421 P2-6: retries_used is already on the node by the time
    effect_complete fires, even on the exhausted-retries failure path —
    pins the ordering `fire_effect_complete` relies on."""
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("always fails")
    _patch_plugin(monkeypatch, plugin)

    captured: dict = {}

    def _observer(path: str, node: dict) -> None:
        if path == "x":
            captured.update(node)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = Store({}, effect_complete=_observer)
    with pytest.raises(RuntimeError, match="always fails"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert captured["meta"]["retries_used"] == 1


def test_tool_a_single_failed_attempt_leaves_retries_used_absent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """absent, not 0, when the first attempt is also the last (#421) —
    the same rule a prompt's failure path follows."""
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("boom")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    with pytest.raises(RuntimeError, match="boom"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert "retries_used" not in store.state["x"]["meta"]


def test_tool_second_pass_of_a_reused_node_gets_a_fresh_created_at(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unnamed loop's second pass over the same node must not keep the
    first pass's created_at (#421) — the same contract #260 already holds
    for value/retries_used on a reused node."""
    from circuitry.core import tool as tool_mod

    timestamps = iter(["P0_T0", "P0_T1", "P0_T2", "P1_T0", "P1_T1"])
    monkeypatch.setattr(tool_mod, "_now_iso", lambda: next(timestamps))

    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.side_effect = [RuntimeError("transient"), ok_result, ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    runtime = ToolRuntime(defn)

    # Pass 0 needs one retry.
    runtime.execute(store=store, ctx={})
    assert store.state["x"]["meta"]["created_at"] == "P0_T0"
    assert store.state["x"]["meta"]["retries_used"] == 1

    # Pass 1 (the node reused, as an unnamed loop's next pass would) succeeds
    # on its own first attempt: a fresh created_at, and no stale retries_used.
    runtime.execute(store=store, ctx={})
    assert store.state["x"]["meta"]["created_at"] == "P1_T0"
    assert "retries_used" not in store.state["x"]["meta"]


def test_tool_retries_used_is_reset_on_a_reused_node(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A node that needed a retry on an earlier execution and succeeds on
    the first attempt of a later one must not keep showing the earlier
    pass's retries_used (issue #273, the same stale-meta
    problem #260 fixed elsewhere)."""
    ok_result = ToolResult(value="done", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(name="x", provider="shell", params={})
    store = _make_store()
    node = store.ensure_dict("x")
    node["meta"] = {"retries_used": 2}

    ToolRuntime(defn).execute(store=store, ctx={})

    assert "retries_used" not in store.state["x"]["meta"]


def test_http_family_connection_failure_is_retried(monkeypatch: pytest.MonkeyPatch) -> None:
    """http.py wraps a DNS/connection failure as ``RuntimeError(...) from
    URLError`` (never a status code) — classify_exception reads the
    __cause__ chain the same way an adapter dispatch failure is classified,
    so this is retried like curl's own refused-connection exit code is
    (issue #273's review)."""
    conn_err = RuntimeError("HTTP request to http://x failed: Connection refused")
    conn_err.__cause__ = urllib.error.URLError("Connection refused")
    ok_result = ToolResult(value={"ok": True}, raw={"status": 200}, stdout="", stderr="", exit_code=None)
    plugin = MagicMock()
    plugin.execute.side_effect = [conn_err, ok_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["value"] == {"ok": True}
    assert plugin.execute.call_count == 2


def test_http_family_unclassifiable_exception_is_not_retried(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An exception with no retryable cause (no status, no connection-level
    __cause__) stays not-retryable — guessing wrong in the retryable
    direction could spin on a request that will never succeed."""
    plugin = MagicMock()
    plugin.execute.side_effect = RuntimeError("params.url must be a string")
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match=r"params\.url must be a string"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 1


def test_http_family_4xx_is_not_retried(monkeypatch: pytest.MonkeyPatch) -> None:
    fail_result = ToolResult(
        value=None, raw={"status": 404}, stdout="", stderr="not found", exit_code=None, ok=False
    )
    plugin = MagicMock()
    plugin.execute.side_effect = [fail_result]
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="http",
        params={},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="not found"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 1


# ---------------------------------------------------------------------------
# Runtime — expect
# ---------------------------------------------------------------------------


def test_expect_cel_passes_records_meta(monkeypatch: pytest.MonkeyPatch) -> None:
    ok_result = ToolResult(value={"prompt_id": "abc"}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})

    assert store.state["x"]["meta"]["error"] is None
    assert store.state["x"]["meta"]["expect"]["result"] is True
    assert store.state["x"]["meta"]["expect"]["mode"] == "cel"


def test_expect_cel_fails_and_retries_then_succeeds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
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

    assert store.state["x"]["value"] == {"prompt_id": "abc"}
    assert store.state["x"]["meta"]["retries_used"] == 1
    assert plugin.execute.call_count == 2


def test_expect_cel_exhausts_retries_fails_with_expect_message(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    missing_result = ToolResult(value={}, raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = missing_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="has(value.prompt_id)"),
    )
    store = _make_store()
    with pytest.raises(RuntimeError, match="expect failed"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 2
    assert "expect failed" in store.state["x"]["meta"]["error"]


def test_expect_model_mode_asks_adapter_and_records_tokens(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.adapters.base import GenerateResult

    ok_result = ToolResult(value="a summary", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    adapter = MagicMock()
    adapter.generate.return_value = GenerateResult(
        text="yes", raw={}, tokens_sent=12, tokens_received=1
    )

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="model", template="Does this look right? {{value}}"),
    )
    store = _make_store()
    ToolRuntime(defn, adapter=adapter, model="m").execute(store=store, ctx={})

    assert store.state["x"]["meta"]["error"] is None
    expect_meta = store.state["x"]["meta"]["expect"]
    assert expect_meta["mode"] == "model"
    assert expect_meta["result"] is True
    assert expect_meta["tokens_sent"] == 12
    adapter.generate.assert_called_once()


def test_expect_model_mode_without_adapter_raises_configuration_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    ok_result = ToolResult(value="x", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        expect=ExpectDef(mode="model", template="ok?"),
    )
    store = _make_store()
    with pytest.raises(ValueError, match="requires the run's adapter/model"):
        ToolRuntime(defn).execute(store=store, ctx={})

    # The exception is caught inside the attempt loop, not left to escape
    # past the start/complete pair unbalanced (issue #273) —
    # fire_effect_complete still ran before the re-raise.
    assert store.state["x"]["meta"]["completed_at"] is not None
    assert "requires the run's adapter/model" in store.state["x"]["meta"]["error"]


def test_expect_model_mode_missing_adapter_error_is_retried(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With retries configured, the same configuration error still counts
    as a failed attempt (and is retried) rather than crashing the run
    outright (issue #273) — it keeps failing every attempt
    since no adapter ever becomes available, so the run still ends in the
    same error."""
    ok_result = ToolResult(value="x", raw={}, stdout="", stderr="", exit_code=0)
    plugin = MagicMock()
    plugin.execute.return_value = ok_result
    _patch_plugin(monkeypatch, plugin)

    defn = ToolDefinition(
        name="x",
        provider="shell",
        params={},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        expect=ExpectDef(mode="model", template="ok?"),
    )
    store = _make_store()
    with pytest.raises(ValueError, match="requires the run's adapter/model"):
        ToolRuntime(defn).execute(store=store, ctx={})

    assert plugin.execute.call_count == 2
