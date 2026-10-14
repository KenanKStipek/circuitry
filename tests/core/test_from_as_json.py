"""`{from: <path>, as: json}`: a state value as compact JSON text (#566).

Wherever `{from:}` is accepted — any depth in a tool's `params`, lists
included, and a `use` effect's `inputs` — an `as: json` modifier serializes
the resolved value to compact JSON text, so a string-only param (a CLI
argument, say) can carry an object or an array.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.tool import (
    ParamReference,
    ToolDefinition,
    ToolRuntime,
    param_reference,
)
from circuitry.core.use import reference_as, reference_path
from circuitry.plugins.base import ToolResult

runner = CliRunner()


def _capture_params(monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    captured: dict[str, Any] = {}

    def capturing_plugin(**kw: Any) -> Any:
        m = MagicMock()

        def execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult:
            captured.update(params)
            return ToolResult(value="ok", raw={})

        m.execute.side_effect = execute
        return m

    monkeypatch.setattr("circuitry.plugins.factory.build_plugin", capturing_plugin)
    return captured


def _execute(params: dict[str, Any], ctx: dict[str, Any]) -> Store:
    store = Store({})
    ToolRuntime(ToolDefinition(name="call", provider="mcp", params=params)).execute(
        store=store, ctx=ctx
    )
    return store


# ── Recognition ──────────────────────────────────────────────────────────────


def test_param_reference_carries_as() -> None:
    assert param_reference({"from": "a.b", "as": "json"}) == ParamReference(
        path="a.b", as_format="json"
    )
    assert param_reference({"from": "a", "default": {}, "as": "json"}) == ParamReference(
        path="a", has_default=True, default={}, as_format="json"
    )
    assert param_reference({"from": "a", "as": "json", "other": 1}) is None
    assert param_reference({"as": "json"}) is None


def test_use_input_reference_accepts_as() -> None:
    assert reference_path({"from": "input.x", "as": "json"}) == "input.x"
    assert reference_as({"from": "input.x", "as": "json"}) == "json"
    assert reference_as({"from": "input.x"}) is None
    assert reference_path({"from": "input.x", "as": "json", "default": 1}) is None


# ── Tool params: runtime ─────────────────────────────────────────────────────


def test_tool_param_is_compact_json_text_keeping_key_order(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured = _capture_params(monkeypatch)
    meta = {"zeta": 1, "alpha": [1, {"n": None}], "ok": True, "name": "ünï"}

    _execute(
        {"meta": {"from": "prime.src.value", "as": "json"}},
        {"prime": {"src": {"value": meta}}},
    )

    assert captured["meta"] == '{"zeta":1,"alpha":[1,{"n":null}],"ok":true,"name":"ünï"}'


def test_tool_param_as_json_works_inside_a_list_and_nested_map(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured = _capture_params(monkeypatch)

    _execute(
        {
            "args": ["record", "--meta", {"from": "prime.m.value", "as": "json"}, "--n", "{{tag}}"],
            "env": {"deep": {"from": "prime.m.value", "as": "json"}},
        },
        {"prime": {"m": {"value": {"k": [1, 2]}}}, "tag": "x"},
    )

    assert captured["args"] == ["record", "--meta", '{"k":[1,2]}', "--n", "x"]
    assert captured["env"] == {"deep": '{"k":[1,2]}'}


@pytest.mark.parametrize(
    ("value", "text"),
    [("plain", '"plain"'), (3, "3"), (True, "true"), ([], "[]"), ({}, "{}")],
)
def test_tool_param_as_json_serializes_scalars_and_empties(
    monkeypatch: pytest.MonkeyPatch, value: Any, text: str
) -> None:
    captured = _capture_params(monkeypatch)

    _execute({"v": {"from": "input.v", "as": "json"}}, {"input": {"v": value}})

    assert captured["v"] == text


def test_tool_param_default_is_applied_before_serialization(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured = _capture_params(monkeypatch)

    _execute(
        {
            "empty": {"from": "prime.missing.value", "as": "json", "default": {}},
            "list": {"from": "prime.missing.value", "as": "json", "default": [1, "a"]},
            "found": {"from": "input.v", "as": "json", "default": {"unused": True}},
        },
        {"input": {"v": {"real": 1}}},
    )

    assert captured["empty"] == "{}"
    assert captured["list"] == '[1,"a"]'
    assert captured["found"] == '{"real":1}'


def test_tool_param_as_json_without_default_still_fails_when_unresolved(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _capture_params(monkeypatch)

    with pytest.raises(ValueError, match="did not resolve to a value"):
        _execute({"v": {"from": "prime.missing.value", "as": "json"}}, {})


def test_without_as_the_value_is_still_passed_typed(monkeypatch: pytest.MonkeyPatch) -> None:
    captured = _capture_params(monkeypatch)

    _execute({"v": {"from": "input.v"}}, {"input": {"v": {"k": [1]}}})

    assert captured["v"] == {"k": [1]}


@pytest.mark.parametrize(
    ("value", "reason"),
    [
        (float("nan"), "Out of range float values"),
        (b"bytes", "not JSON serializable"),
        ({"k": {1, 2}}, "not JSON serializable"),
    ],
)
def test_unserializable_value_fails_the_effect_naming_the_path(
    monkeypatch: pytest.MonkeyPatch, value: Any, reason: str
) -> None:
    _capture_params(monkeypatch)
    store = Store({})
    defn = ToolDefinition(
        name="call",
        provider="mcp",
        params={"args": ["x", {"from": "prime.src.value", "as": "json"}]},
    )

    with pytest.raises(ValueError) as excinfo:
        ToolRuntime(defn).execute(store=store, ctx={"prime": {"src": {"value": value}}})

    message = str(excinfo.value)
    assert (
        "Tool effect 'call' param 'params.args[1]': "
        "'{from: prime.src.value, as: json}' did not serialize to JSON"
    ) in message
    assert reason in message
    assert store.state["call"]["meta"]["error"] == message


# ── Tool params: compile time / cof check ────────────────────────────────────


def _tool_orch(param: Any) -> dict[str, Any]:
    return {
        "effects": [
            {
                "type": "tool",
                "name": "call",
                "provider": "json",
                "params": {"mode": "parse", "input": "null", "got": param},
            }
        ]
    }


def test_compile_accepts_as_json_with_and_without_default() -> None:
    compile_orchestration(orch=_tool_orch({"from": "input.x", "as": "json"}))
    compile_orchestration(orch=_tool_orch({"from": "input.x", "as": "json", "default": {}}))
    compile_orchestration(orch=_tool_orch(["a", {"from": "input.x", "as": "json"}]))


@pytest.mark.parametrize("bad", ["yaml", "JSON", "", 5, True, ["json"]])
def test_compile_rejects_an_unknown_as_value(bad: Any) -> None:
    with pytest.raises(ValueError, match=r"unknown 'as: .*' on a '\{from: \.\.\.\}' reference"):
        compile_orchestration(orch=_tool_orch({"from": "input.x", "as": bad}))


def test_compile_unknown_as_names_the_param_path_inside_a_list() -> None:
    with pytest.raises(ValueError, match=r"Tool effect 'call' param 'params\.got\[1\]'"):
        compile_orchestration(orch=_tool_orch(["a", {"from": "input.x", "as": "yaml"}]))


def test_compile_checks_the_root_of_an_as_json_reference() -> None:
    with pytest.raises(ValueError, match="not rooted at a state namespace"):
        compile_orchestration(orch=_tool_orch({"from": "items", "as": "json"}))


def test_compile_rejects_as_json_reference_in_allowed_commands() -> None:
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "call",
                "provider": "shell",
                "params": {
                    "command": "ls",
                    "allowed_commands": ["ls", {"from": "input.cmd", "as": "json"}],
                },
            }
        ]
    }
    with pytest.raises(ValueError, match="security-sensitive setting"):
        compile_orchestration(orch=orch)


def test_cof_check_rejects_an_unknown_as_and_accepts_json(tmp_path: Path) -> None:
    good = tmp_path / "good.yml"
    good.write_text(yaml.dump(_tool_orch({"from": "input.x", "as": "json"})), encoding="utf-8")
    bad = tmp_path / "bad.yml"
    bad.write_text(yaml.dump(_tool_orch({"from": "input.x", "as": "yaml"})), encoding="utf-8")

    ok = runner.invoke(app, ["check", str(good), "--skip-preflight"])
    assert ok.exit_code == 0, ok.output

    failed = runner.invoke(app, ["check", str(bad), "--skip-preflight"])
    assert failed.exit_code != 0
    assert "unknown 'as: yaml'" in " ".join(failed.output.split())


# ── `use` inputs ─────────────────────────────────────────────────────────────

#: A child that hands back, as received, its `got` input.
ECHO = {
    "interface": {
        "inputs": {"got": {"required": False}},
        "outputs": {"got": {"path": "input.got"}},
    },
    "effects": [
        {"type": "tool", "name": "noop", "provider": "json", "params": {"mode": "parse", "input": "true"}}
    ],
}


def _run(orch: dict[str, Any], inputs: dict[str, Any]) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    store = Store(state={"input": dict(inputs)})
    DynamicRuntime(
        root, adapter=MagicMock(), model="test", runtime_config={"_test_marker": True}
    ).execute(store=store)
    return store.state


def _use_orch(tmp_path: Path, got: Any) -> dict[str, Any]:
    child = tmp_path / "echo.yml"
    child.write_text(yaml.dump(ECHO), encoding="utf-8")
    return {"effects": [{"type": "use", "name": "sub", "path": str(child), "inputs": {"got": got}}]}


def test_use_input_as_json_passes_the_value_as_json_text(tmp_path: Path) -> None:
    orch = _use_orch(tmp_path, {"from": "input.rungs", "as": "json"})
    rungs = [{"name": "4k", "factor": 2}, {"name": "1080p", "factor": 4}]

    state = _run(orch, {"rungs": rungs})

    assert state["prime"]["sub"]["value"]["got"] == '[{"name":"4k","factor":2},{"name":"1080p","factor":4}]'


def test_use_input_without_as_is_unchanged(tmp_path: Path) -> None:
    orch = _use_orch(tmp_path, {"from": "input.rungs"})

    state = _run(orch, {"rungs": [1, 2]})

    assert state["prime"]["sub"]["value"]["got"] == [1, 2]


def test_use_input_as_json_unresolved_stays_null(tmp_path: Path) -> None:
    orch = _use_orch(tmp_path, {"from": "prime.missing.value", "as": "json"})

    state = _run(orch, {})

    assert state["prime"]["sub"]["value"]["got"] is None


def test_use_input_as_json_unserializable_names_the_path(tmp_path: Path) -> None:
    orch = _use_orch(tmp_path, {"from": "input.bad", "as": "json"})

    with pytest.raises(Exception, match=r"inputs\.got: '\{from: input\.bad, as: json\}' did not serialize"):
        _run(orch, {"bad": float("nan")})


def test_use_input_rejects_an_unknown_as_at_compile_time(tmp_path: Path) -> None:
    orch = _use_orch(tmp_path, {"from": "input.rungs", "as": "yaml"})

    with pytest.raises(ValueError, match=r"Use effect 'sub' input 'got'.*unknown 'as: yaml'"):
        compile_orchestration(orch=orch)


# ── End to end: a real tool plugin, inside a loop ────────────────────────────


def test_as_json_reaches_a_real_tool_inside_a_loop() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "each_rung",
                "each": {"in": "input.rungs", "as": "r"},
                "body": [
                    {
                        "type": "tool",
                        "name": "show",
                        "provider": "json",
                        "params": {"mode": "parse", "input": {"from": "r", "as": "json"}},
                    }
                ],
            }
        ]
    }
    rungs = [{"name": "4k", "factor": 2}, {"name": "1080p", "factor": 4}]

    state = _run(orch, {"rungs": rungs})

    values = [
        state["prime"]["each_rung"][f"iter_{i}"]["show"]["value"] for i in range(len(rungs))
    ]
    assert values == rungs
