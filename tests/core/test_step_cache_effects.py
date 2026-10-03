"""`cache:` on `prompt`/`tool` effects (#270): opt-in, off by default. A hit
skips dispatch entirely and reuses the stored value; a miss dispatches
normally and stores the result only once it reaches that effect's own
success condition — never a failure, including one `on_error: continue`
absorbed. `--no-cache` disables both reading and writing for a run.

Storage primitives (TTL parsing, atomic writes, file modes) are covered in
`tests/core/test_step_cache.py`; this file is the effect-level contract.
`tests/conftest.py`'s autouse hermetic fixture points `CIRCUITRY_CACHE_DIR`
at a per-test tmp dir, so every `run()` call below is already isolated.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import _compile_effect
from circuitry.core.document_check import cache_field_errors, structural_errors
from circuitry.core.prompt import PromptDefinition
from circuitry.core.step_cache import NO_CACHE_RUNTIME_CONFIG_KEY, CacheDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins.base import ToolResult


class CountingAdapter:
    """Records every prompt it's asked to generate; fails on demand."""

    name = "counting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.fail_prompts: set[str] = set()

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(prompt)
        if prompt in self.fail_prompts:
            raise RuntimeError(f"boom: {prompt}")
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, name: str, content: str) -> Path:
    p = tmp_path / name
    p.write_text(content, encoding="utf-8")
    return p


def _run(orch: Path, *, adapter: Any, state: dict[str, Any], no_cache: bool = False) -> Any:
    return run(
        RunRequest(
            orchestration_path=orch,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=adapter,
            initial_state=state,
            no_cache=no_cache,
        )
    )


# ---------------------------------------------------------------------------
# Compiler
# ---------------------------------------------------------------------------


def test_compile_prompt_cache_true_normalizes_to_bare_cachedef() -> None:
    effect = {"type": "prompt", "name": "p", "template": "hi", "cache": True}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, PromptDefinition)
    assert defn.cache == CacheDef()


def test_compile_prompt_cache_false_is_none() -> None:
    effect = {"type": "prompt", "name": "p", "template": "hi", "cache": False}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, PromptDefinition)
    assert defn.cache is None


def test_compile_prompt_no_cache_key_is_none() -> None:
    effect = {"type": "prompt", "name": "p", "template": "hi"}
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, PromptDefinition)
    assert defn.cache is None


def test_compile_tool_cache_with_ttl_and_salt() -> None:
    effect = {
        "type": "tool",
        "name": "t",
        "provider": "json",
        "params": {},
        "cache": {"ttl": "7d", "key": "v2"},
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, ToolDefinition)
    assert defn.cache == CacheDef(ttl_seconds=7 * 86400, key_salt="v2")


def test_compile_cache_invalid_ttl_raises() -> None:
    effect = {
        "type": "tool",
        "name": "t",
        "provider": "json",
        "params": {},
        "cache": {"ttl": "not-a-duration"},
    }
    with pytest.raises(ValueError, match="ttl"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_cache_invalid_shape_raises() -> None:
    effect = {"type": "prompt", "name": "p", "template": "hi", "cache": "yes"}
    with pytest.raises(ValueError, match="cache"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


def test_compile_cache_key_must_be_a_string() -> None:
    effect = {
        "type": "tool",
        "name": "t",
        "provider": "json",
        "params": {},
        "cache": {"key": 7},
    }
    with pytest.raises(ValueError, match=r"cache\.key"):
        _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")


# ---------------------------------------------------------------------------
# cof check: cache: is only legal on prompt/tool
# ---------------------------------------------------------------------------


def test_cache_field_errors_rejects_cache_on_a_use_effect() -> None:
    orch = {
        "effects": [
            {"type": "use", "name": "child", "path": "foo.yml", "cache": True}
        ]
    }
    errors = cache_field_errors(orch)
    assert len(errors) == 1
    assert "only allowed on tool/prompt effects" in errors[0]
    assert errors[0] in structural_errors(orch)


def test_cache_field_errors_rejects_cache_on_a_dynamic_effect() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "group",
                "cache": True,
                "effects": [{"type": "prompt", "name": "p", "template": "hi"}],
            }
        ]
    }
    assert len(cache_field_errors(orch)) == 1


def test_cache_field_errors_allows_cache_on_prompt_and_tool() -> None:
    orch = {
        "effects": [
            {"type": "prompt", "name": "p", "template": "hi", "cache": True},
            {
                "type": "tool",
                "name": "t",
                "provider": "json",
                "params": {"op": "parse", "input": "{}"},
                "cache": {"ttl": "1d"},
            },
        ]
    }
    assert cache_field_errors(orch) == []
    assert structural_errors(orch) == []


# ---------------------------------------------------------------------------
# Prompt effect: hit/miss/failure/--no-cache
# ---------------------------------------------------------------------------


def _cached_prompt_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path,
        "p.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "hi {{input.x}}"
    cache: true
""".lstrip(),
    )


def test_prompt_cache_hit_skips_dispatch_and_reuses_the_value(tmp_path: Path) -> None:
    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()
    state = {"input": {"x": "a"}}

    first = _run(orch, adapter=adapter, state=state)
    assert first.ok is True, first.error
    assert adapter.calls == ["hi a"]
    assert first.state["prime"]["step1"]["meta"]["cache"] == {
        "hit": False,
        "key": first.state["prime"]["step1"]["meta"]["cache"]["key"],
    }

    second = _run(orch, adapter=adapter, state={"input": {"x": "a"}})
    assert second.ok is True, second.error
    # No second dispatch: the call list is unchanged.
    assert adapter.calls == ["hi a"]
    assert second.state["prime"]["step1"]["value"] == "ok:hi a"
    cache_meta = second.state["prime"]["step1"]["meta"]["cache"]
    assert cache_meta["hit"] is True
    assert cache_meta["key"] == first.state["prime"]["step1"]["meta"]["cache"]["key"]
    assert cache_meta["created_at"]
    # No tokens spent on a hit.
    assert second.state["prime"]["step1"]["meta"]["tokens_sent"] is None


def test_prompt_cache_miss_on_a_changed_rendered_input(tmp_path: Path) -> None:
    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()

    first = _run(orch, adapter=adapter, state={"input": {"x": "a"}})
    second = _run(orch, adapter=adapter, state={"input": {"x": "b"}})
    assert first.ok is True and second.ok is True
    # Different rendered prompt -> different key -> both dispatch.
    assert adapter.calls == ["hi a", "hi b"]
    assert second.state["prime"]["step1"]["meta"]["cache"]["hit"] is False


def test_prompt_failure_is_never_cached(tmp_path: Path) -> None:
    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()
    adapter.fail_prompts = {"hi a"}
    state = {"input": {"x": "a"}}

    first = _run(orch, adapter=adapter, state=dict(state))
    assert first.ok is False

    adapter.fail_prompts = set()
    second = _run(orch, adapter=adapter, state=dict(state))
    assert second.ok is True, second.error
    # The failed attempt dispatched again — it was never stored.
    assert adapter.calls == ["hi a", "hi a"]

    # Now that a success exists, a third run is a genuine hit.
    third = _run(orch, adapter=adapter, state=dict(state))
    assert adapter.calls == ["hi a", "hi a"]
    assert third.state["prime"]["step1"]["meta"]["cache"]["hit"] is True


def test_prompt_on_error_continue_failure_is_never_cached(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "p.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "hi {{input.x}}"
    cache: true
    on_error: continue
""".lstrip(),
    )
    adapter = CountingAdapter()
    adapter.fail_prompts = {"hi a"}
    state = {"input": {"x": "a"}}

    first = _run(orch, adapter=adapter, state=dict(state))
    assert first.ok is True, first.error
    assert first.state["prime"]["step1"]["meta"]["error"]

    adapter.fail_prompts = set()
    second = _run(orch, adapter=adapter, state=dict(state))
    assert second.ok is True, second.error
    # Dispatched again — the absorbed failure was not stored.
    assert adapter.calls == ["hi a", "hi a"]
    assert not second.state["prime"]["step1"]["meta"]["error"]


def test_prompt_no_cache_flag_neither_reads_nor_writes(tmp_path: Path) -> None:
    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()
    state = {"input": {"x": "a"}}

    first = _run(orch, adapter=adapter, state=dict(state), no_cache=True)
    second = _run(orch, adapter=adapter, state=dict(state), no_cache=True)
    assert first.ok is True and second.ok is True
    assert adapter.calls == ["hi a", "hi a"]
    assert "cache" not in first.state["prime"]["step1"]["meta"]
    assert "cache" not in second.state["prime"]["step1"]["meta"]

    # And a plain run afterwards (cache back on) still sees no entry from
    # the --no-cache runs above — nothing was ever written.
    third = _run(orch, adapter=adapter, state=dict(state), no_cache=False)
    assert adapter.calls == ["hi a", "hi a", "hi a"]
    assert third.state["prime"]["step1"]["meta"]["cache"]["hit"] is False


def test_prompt_without_cache_configured_never_writes_cache_meta(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "p.yml",
        """
effects:
  - type: prompt
    name: step1
    template: "hi {{input.x}}"
""".lstrip(),
    )
    adapter = CountingAdapter()
    state = {"input": {"x": "a"}}
    first = _run(orch, adapter=adapter, state=dict(state))
    second = _run(orch, adapter=adapter, state=dict(state))
    assert adapter.calls == ["hi a", "hi a"]
    assert "cache" not in first.state["prime"]["step1"]["meta"]
    assert "cache" not in second.state["prime"]["step1"]["meta"]


# ---------------------------------------------------------------------------
# Tool effect: hit/miss/failure/--no-cache (direct ToolRuntime, mocked plugin)
# ---------------------------------------------------------------------------


def _make_store() -> Store:
    return Store({})


def test_tool_cache_hit_skips_dispatch_and_reuses_the_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"},
        cache=CacheDef(),
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert mock_plugin.execute.call_count == 1
    assert store.state["t"]["meta"]["cache"]["hit"] is False

    store2 = _make_store()
    ToolRuntime(defn).execute(store=store2, ctx={})
    # No second dispatch.
    assert mock_plugin.execute.call_count == 1
    assert store2.state["t"]["value"] == "out"
    assert store2.state["t"]["meta"]["cache"]["hit"] is True
    assert store2.state["t"]["meta"]["cache"]["created_at"]


def test_tool_cache_miss_on_changed_rendered_params(monkeypatch: pytest.MonkeyPatch) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn1 = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{\"a\": 1}"},
        cache=CacheDef(),
    )
    defn2 = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{\"a\": 2}"},
        cache=CacheDef(),
    )
    ToolRuntime(defn1).execute(store=_make_store(), ctx={})
    ToolRuntime(defn2).execute(store=_make_store(), ctx={})
    assert mock_plugin.execute.call_count == 2


def test_tool_failure_is_never_cached(monkeypatch: pytest.MonkeyPatch) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value=None, raw={}, stdout="", stderr="boom", exit_code=1, ok=False
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"},
        cache=CacheDef(), on_error="continue",
    )
    ToolRuntime(defn).execute(store=_make_store(), ctx={})
    assert mock_plugin.execute.call_count == 1

    ToolRuntime(defn).execute(store=_make_store(), ctx={})
    # The failed attempt dispatched again; it was never stored.
    assert mock_plugin.execute.call_count == 2


def test_tool_no_cache_runtime_config_disables_read_and_write(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"},
        cache=CacheDef(),
    )
    runtime_config = {NO_CACHE_RUNTIME_CONFIG_KEY: True}
    ToolRuntime(defn, runtime_config=runtime_config).execute(store=_make_store(), ctx={})
    ToolRuntime(defn, runtime_config=runtime_config).execute(store=_make_store(), ctx={})
    assert mock_plugin.execute.call_count == 2

    # Cache back on: still a miss, since --no-cache never wrote anything.
    store3 = _make_store()
    ToolRuntime(defn).execute(store=store3, ctx={})
    assert mock_plugin.execute.call_count == 3
    assert store3.state["t"]["meta"]["cache"]["hit"] is False


def test_tool_without_cache_configured_never_writes_cache_meta(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(name="t", provider="json", params={"op": "parse", "input": "{}"})
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert "cache" not in store.state["t"]["meta"]


# ---------------------------------------------------------------------------
# Cache errors never fail an otherwise-successful effect (#270 review finding 1)
# ---------------------------------------------------------------------------


def test_tool_cache_lookup_error_degrades_to_a_miss_not_a_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.core import step_cache as step_cache_module

    def _boom(self: Any, key: str, *, ttl_seconds: float | None) -> Any:
        raise OSError("cache dir unavailable")

    monkeypatch.setattr(step_cache_module.StepCache, "get", _boom)

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"}, cache=CacheDef()
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert store.state["t"]["value"] == "out"
    assert store.state["t"]["meta"]["error"] is None


def test_tool_cache_store_error_does_not_fail_a_successful_step(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from circuitry.core import step_cache as step_cache_module

    def _boom(self: Any, key: str, value: Any, *, created_at: str) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(step_cache_module.StepCache, "put", _boom)

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"}, cache=CacheDef()
    )
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert store.state["t"]["value"] == "out"
    assert store.state["t"]["meta"]["error"] is None


def test_prompt_cache_lookup_error_degrades_to_a_miss_not_a_failure(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    from circuitry.core import step_cache as step_cache_module

    def _boom(self: Any, key: str, *, ttl_seconds: float | None) -> Any:
        raise OSError("cache dir unavailable")

    monkeypatch.setattr(step_cache_module.StepCache, "get", _boom)

    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()
    result = _run(orch, adapter=adapter, state={"input": {"x": "a"}})
    assert result.ok is True, result.error
    assert adapter.calls == ["hi a"]
    assert not result.state["prime"]["step1"]["meta"]["error"]


def test_prompt_cache_store_error_does_not_fail_a_successful_step(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    from circuitry.core import step_cache as step_cache_module

    def _boom(self: Any, key: str, value: Any, *, created_at: str) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(step_cache_module.StepCache, "put", _boom)

    orch = _cached_prompt_orch(tmp_path)
    adapter = CountingAdapter()
    result = _run(orch, adapter=adapter, state={"input": {"x": "a"}})
    assert result.ok is True, result.error
    assert result.state["prime"]["step1"]["value"] == "ok:hi a"
    assert not result.state["prime"]["step1"]["meta"]["error"]


# ---------------------------------------------------------------------------
# A tool cache hit resets stale meta on a reused node (#270 review finding 2)
# ---------------------------------------------------------------------------


def test_tool_cache_hit_resets_stale_meta_on_a_reused_node(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={"extra": "data"}, stdout="first-stdout", stderr="first-stderr",
        exit_code=0,
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"}, cache=CacheDef()
    )
    # Same store reused across two passes — an unnamed loop's repeated body
    # or a --state carryover, exactly the #260 scenario.
    store = _make_store()
    ToolRuntime(defn).execute(store=store, ctx={})
    assert store.state["t"]["meta"]["stdout"] == "first-stdout"
    assert store.state["t"]["meta"]["raw"] == {"extra": "data"}

    ToolRuntime(defn).execute(store=store, ctx={})
    assert mock_plugin.execute.call_count == 1
    assert store.state["t"]["meta"]["cache"]["hit"] is True
    assert store.state["t"]["value"] == "out"
    # The earlier pass's stdout/exit_code/raw must not survive next to the
    # cached value.
    assert store.state["t"]["meta"]["stdout"] is None
    assert store.state["t"]["meta"]["stderr"] is None
    assert store.state["t"]["meta"]["exit_code"] is None
    assert "raw" not in store.state["t"]["meta"]


# ---------------------------------------------------------------------------
# An expect: failure is never cached (#270 review finding 8)
# ---------------------------------------------------------------------------


def test_tool_expect_failure_is_never_cached(monkeypatch: pytest.MonkeyPatch) -> None:
    from circuitry.core.expect import ExpectDef

    mock_plugin = MagicMock()
    mock_plugin.execute.return_value = ToolResult(
        value="out", raw={}, stdout="", stderr="", exit_code=0
    )
    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", lambda **kw: mock_plugin)

    defn = ToolDefinition(
        name="t", provider="json", params={"op": "parse", "input": "{}"},
        cache=CacheDef(), expect=ExpectDef(mode="cel", expr="false"), on_error="continue",
    )
    ToolRuntime(defn).execute(store=_make_store(), ctx={})
    assert mock_plugin.execute.call_count == 1

    ToolRuntime(defn).execute(store=_make_store(), ctx={})
    # expect: always failed -> never cached -> dispatched again.
    assert mock_plugin.execute.call_count == 2


# ---------------------------------------------------------------------------
# Cache key changes when a {from:}-resolved param changes (#270 review finding 8)
# ---------------------------------------------------------------------------


def _from_ref_orch(tmp_path: Path) -> Path:
    return _write(
        tmp_path,
        "r.yml",
        """
effects:
  - type: tool
    name: step0
    provider: json
    params:
      mode: parse
      input: "{{input.x}}"
  - type: tool
    name: step1
    provider: json
    params:
      mode: stringify
      input:
        from: "prime.step0.value"
    cache: true
""".lstrip(),
    )


def test_tool_cache_key_changes_when_a_from_resolved_param_changes(tmp_path: Path) -> None:
    orch = _from_ref_orch(tmp_path)
    adapter = CountingAdapter()

    # Plain numbers, not quoted strings: the template engine HTML-escapes
    # `"` on render, so a JSON string literal wouldn't survive intact.
    first = _run(orch, adapter=adapter, state={"input": {"x": "1"}})
    assert first.ok is True, first.error
    assert first.state["prime"]["step1"]["meta"]["cache"]["hit"] is False
    key_a = first.state["prime"]["step1"]["meta"]["cache"]["key"]

    second = _run(orch, adapter=adapter, state={"input": {"x": "1"}})
    assert second.state["prime"]["step1"]["meta"]["cache"]["hit"] is True
    assert second.state["prime"]["step1"]["meta"]["cache"]["key"] == key_a

    third = _run(orch, adapter=adapter, state={"input": {"x": "2"}})
    assert third.state["prime"]["step1"]["meta"]["cache"]["hit"] is False
    assert third.state["prime"]["step1"]["meta"]["cache"]["key"] != key_a


# ---------------------------------------------------------------------------
# cof check also walks the root finally: block (#270 review finding 4)
# ---------------------------------------------------------------------------


def test_cache_field_errors_walks_root_finally() -> None:
    from circuitry.core.document_check import group_field_errors

    orch = {
        "effects": [{"type": "prompt", "name": "p", "template": "hi"}],
        "finally": [{"type": "use", "name": "cleanup", "path": "foo.yml", "cache": True}],
    }
    errors = cache_field_errors(orch)
    assert len(errors) == 1
    assert "finally[0]" in errors[0]
    assert errors[0] in structural_errors(orch)

    group_orch = {
        "effects": [{"type": "prompt", "name": "p", "template": "hi"}],
        "finally": [
            {
                "type": "dynamic",
                "name": "cleanup",
                "group": "g",
                "effects": [{"type": "prompt", "name": "p2", "template": "hi"}],
            }
        ],
    }
    group_errors = group_field_errors(group_orch)
    assert len(group_errors) == 1
    assert "finally[0]" in group_errors[0]
