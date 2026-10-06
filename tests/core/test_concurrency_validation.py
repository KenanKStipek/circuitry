"""`cof check` surface of #274: `group:` is a leaf-only field, and it must
name a group `runtime.concurrency_groups` actually defines.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.compiler import (
    collect_effect_groups,
    compile_orchestration,
    unknown_concurrency_group_errors,
)
from circuitry.core.document_check import group_field_errors, structural_errors


def _write(tmp_path: Path, text: str) -> Path:
    path = tmp_path / "doc.yml"
    path.write_text(text, encoding="utf-8")
    return path


# ── group: only on leaf effects ─────────────────────────────────────────────


@pytest.mark.parametrize(
    ("container_type", "extra"),
    [
        ("loop", '    each: {in: input.xs, as: x}\n'),
        ("dynamic", ""),
        ("if", ""),
    ],
)
def test_group_on_a_container_is_a_structural_error(container_type: str, extra: str) -> None:
    body_key = "then" if container_type == "if" else ("body" if container_type == "loop" else "effects")
    orch = {
        "effects": [
            {
                "type": container_type,
                "name": "c",
                "group": "gpu",
                body_key: [{"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}],
                **({"if": {"mode": "cel", "expr": "true"}} if container_type == "if" else {}),
            }
        ]
    }
    if container_type == "loop":
        orch["effects"][0]["each"] = {"in": "input.xs", "as": "x"}

    errors = group_field_errors(orch)
    assert len(errors) == 1
    assert "'group' is only allowed on tool/prompt effects" in errors[0]
    assert f"a '{container_type}' effect" in errors[0]


@pytest.mark.parametrize(
    ("container_type", "extra"),
    [
        ("use", {"path": "child.yml"}),
        ("reflector", {"effects": [{"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}]}),
    ],
)
def test_group_on_use_or_reflector_is_also_a_structural_error(
    container_type: str, extra: dict
) -> None:
    """`use` and `reflector` are containers too — neither dispatches itself,
    so `group:` is just as much an error on them as on loop/dynamic/if
    (#274 review P2: only loop/dynamic/if were covered before)."""
    orch = {
        "effects": [
            {"type": container_type, "name": "c", "group": "gpu", **extra},
        ]
    }
    errors = group_field_errors(orch)
    assert len(errors) == 1
    assert "'group' is only allowed on tool/prompt effects" in errors[0]
    assert f"a '{container_type}' effect" in errors[0]


def test_group_on_tool_and_prompt_is_not_a_structural_error() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}, "group": "gpu"},
            {"type": "prompt", "name": "p", "template": "go", "group": "gpu"},
        ]
    }
    assert group_field_errors(orch) == []
    assert structural_errors(orch) == []


def test_group_field_errors_walks_into_nested_containers() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "outer",
                "effects": [
                    {
                        "type": "loop",
                        "name": "inner",
                        "group": "gpu",
                        "each": {"in": "input.xs", "as": "x"},
                        "body": [{"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}],
                    }
                ],
            }
        ]
    }
    errors = group_field_errors(orch)
    assert len(errors) == 1
    assert "effects[0].effects[0]" in errors[0]


def test_group_field_errors_walks_root_finally() -> None:
    orch = {
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
    errors = group_field_errors(orch)
    assert len(errors) == 1
    assert "finally[0]" in errors[0]
    assert errors[0] in structural_errors(orch)


# ── collect_effect_groups / unknown_concurrency_group_errors ───────────────


def test_collect_effect_groups_finds_groups_anywhere_in_the_tree() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "outer",
                "flow": "tree",
                "each": {"in": "input.xs", "as": "x"},
                "body": [
                    {
                        "type": "if",
                        "if": {"mode": "cel", "expr": "true"},
                        "then": [
                            {
                                "type": "tool",
                                "name": "a",
                                "provider": "json",
                                "params": {"input": "1"},
                                "group": "gpu",
                            }
                        ],
                        "else": [
                            {"type": "prompt", "name": "b", "template": "go", "group": "cpu"}
                        ],
                    }
                ],
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    assert collect_effect_groups(root) == {"gpu", "cpu"}


def test_unknown_concurrency_group_errors_names_the_bad_group() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}, "group": "comfy"}
        ]
    }
    root = compile_orchestration(orch=orch)
    errors = unknown_concurrency_group_errors(root, frozenset({"gpu"}))
    assert len(errors) == 1
    assert "'comfy'" in errors[0]
    assert "gpu" in errors[0]


def test_unknown_concurrency_group_errors_empty_when_known() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}, "group": "gpu"}
        ]
    }
    root = compile_orchestration(orch=orch)
    assert unknown_concurrency_group_errors(root, frozenset({"gpu"})) == []


# ── cof check (validate()) end to end ───────────────────────────────────────


def test_check_rejects_an_unknown_group_name(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}, group: comfy}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 1}})
    result = validate(path, config=cfg, skip_preflight=True)
    assert result["ok"] is False
    assert any("comfy" in e and "gpu" in e for e in result["errors"])


def test_check_accepts_a_known_group_name(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}, group: gpu}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 1}})
    result = validate(path, config=cfg, skip_preflight=True)
    assert result["ok"] is True, result["errors"]


def test_check_rejects_malformed_concurrency_groups_config(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 0}})
    result = validate(path, config=cfg, skip_preflight=True)
    assert result["ok"] is False
    assert any("concurrency_groups" in e for e in result["errors"])


def test_check_rejects_malformed_max_concurrency_config(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}}\n",
    )
    cfg = CircuitryConfig(runtime={"max_concurrency": -1})
    result = validate(path, config=cfg, skip_preflight=True)
    assert result["ok"] is False
    assert any("max_concurrency" in e for e in result["errors"])


def test_check_rejects_group_on_a_container_effect(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - type: dynamic\n"
        "    name: d\n"
        "    group: gpu\n"
        "    effects: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = validate(path, skip_preflight=True)
    assert result["ok"] is False
    assert "'group' is only allowed on tool/prompt effects" in result["errors"][0]


# ── run(validate_only=True) sees the same checks ────────────────────────────


def test_run_validate_only_rejects_an_unknown_group_name(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}, group: comfy}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 1}})
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=True,
            config=cfg,
        )
    )
    assert result.ok is False
    assert "comfy" in (result.error or "")


def test_run_rejects_an_unknown_group_in_a_use_child(tmp_path: Path) -> None:
    """A `use` child never declares its own `runtime.concurrency_groups` (it
    inherits the run's own, ambient one) — but its own tool/prompt `group:`
    fields are still checked against it when it loads (#274)."""
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}, group: comfy}\n",
        encoding="utf-8",
    )
    parent_path = _write(
        tmp_path,
        "effects:\n"
        f"  - {{type: use, name: child, path: {child_path}}}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 1}})
    adapter = MagicMock()
    adapter.name = "mock"
    result = run(
        RunRequest(
            orchestration_path=parent_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            adapter=adapter,
        )
    )
    assert result.ok is False
    assert "comfy" in (result.error or "")


def test_run_executes_a_grouped_tool_with_a_configured_group(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {mode: stringify, input: 'ran'}, group: gpu}\n",
    )
    cfg = CircuitryConfig(runtime={"concurrency_groups": {"gpu": 1}})
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
        )
    )
    assert result.ok is True, result.error
    assert result.state["prime"]["t"]["value"] == '"ran"'


def test_run_max_concurrency_actually_limits_leaves_end_to_end(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`run()`-level proof, not just "doesn't crash" (#274 review P2): a
    `runtime.max_concurrency: 1` config caps a tree loop's own
    `max_concurrency: 3` dispatches to 1 at a time, the whole way through
    `cli.runtime_shim.run`."""
    import threading
    import time as _time

    lock = threading.Lock()
    in_flight = 0
    max_in_flight = 0
    calls = 0

    class _TrackingPlugin:
        def execute(self, *, params: dict, timeout_seconds: int):
            nonlocal in_flight, max_in_flight, calls
            from circuitry.plugins.base import ToolResult

            with lock:
                in_flight += 1
                max_in_flight = max(max_in_flight, in_flight)
                calls += 1
            _time.sleep(0.03)
            with lock:
                in_flight -= 1
            return ToolResult(value="ok", raw={}, stdout="", stderr="", exit_code=0)

    monkeypatch.setattr(
        "circuitry.plugins.factory.build_plugin", lambda **kw: _TrackingPlugin()
    )
    path = _write(
        tmp_path,
        "effects:\n"
        "  - type: loop\n"
        "    name: outer\n"
        "    flow: tree\n"
        "    max_concurrency: 3\n"
        "    each: {in: input.xs, as: x}\n"
        "    body:\n"
        "      - {type: tool, name: t, provider: json, params: {input: '1'}}\n",
    )
    cfg = CircuitryConfig(runtime={"max_concurrency": 1})
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=cfg,
            initial_state={"input": {"xs": [1, 2, 3, 4, 5]}},
        )
    )
    assert result.ok is True, result.error
    assert calls == 5
    assert max_in_flight == 1
