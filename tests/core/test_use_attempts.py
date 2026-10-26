"""`meta.attempts` on a retried `use` (#659): one entry per attempt, oldest first."""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.cancellation import RunCancelledBySignal, get_token
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _patch_sleep(monkeypatch: pytest.MonkeyPatch, fn) -> None:
    monkeypatch.setattr(get_token(), "sleep_or_raise", fn)


def _write(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter() -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    return adapter


def _bump_tool(counter_file: Path, name: str = "bump") -> dict[str, Any]:
    """A child's only effect: append to *counter_file* and return the new count."""
    return {
        "type": "tool",
        "name": name,
        "provider": "shell",
        "params": {
            "command": "bash",
            "allowed_commands": ["bash"],
            "args": [
                "-c",
                (
                    f'n=$(cat "{counter_file}" 2>/dev/null || echo 0); '
                    f'n=$((n+1)); echo -n "$n" > "{counter_file}"; echo -n "$n"'
                ),
            ],
        },
    }


def _run_top(
    orch: dict[str, Any], *, runtime_config: dict[str, Any] | None = None
) -> dict[str, Any]:
    root = compile_orchestration(orch=orch)
    store = Store(state={"input": {}})
    DynamicRuntime(
        root, adapter=_mock_adapter(), model="m", runtime_config=runtime_config or {}
    ).execute(store=store)
    return store.state


def test_attempts_absent_when_only_one_attempt_ran(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    child_path = _write(tmp_path, "child.yml", {"effects": [_bump_tool(tmp_path / "c.txt")]})
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child", path=str(child_path), retries=RetryPolicyDef(max_attempts=3, backoff_ms=10)
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert "attempts" not in store.state["run_child"]["meta"]


def test_attempts_lists_every_attempt_oldest_first_and_is_metas_last_key(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    counter_file = tmp_path / "attempts.txt"
    child_path = _write(tmp_path, "child.yml", {"effects": [_bump_tool(counter_file)]})
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"attempt": {"path": "prime.bump.value"}},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="value.attempt == '3'"),
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    meta = store.state["run_child"]["meta"]
    assert list(meta)[-1] == "attempts"
    attempts = meta["attempts"]
    assert len(attempts) == 3
    assert [a["value"] for a in attempts] == [
        {"attempt": "1"},
        {"attempt": "2"},
        {"attempt": "3"},
    ]
    assert attempts[0]["error"] == "expect failed: value.attempt == '3'"
    assert attempts[1]["error"] == "expect failed: value.attempt == '3'"
    assert attempts[2]["error"] is None
    assert [a["expect"]["result"] for a in attempts] == [False, False, True]
    assert meta["retries_used"] == 2
    assert meta["created_at"] == attempts[0]["created_at"]
    assert meta["completed_at"] == attempts[-1]["completed_at"]
    # The deciding (last) attempt repeats the node's own fields.
    assert attempts[-1]["value"] == store.state["run_child"]["value"]
    assert attempts[-1]["expect"] == meta["expect"]
    assert attempts[-1]["error"] == meta["error"]


def test_attempts_kept_on_continue_after_retries_exhausted(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    counter_file = tmp_path / "attempts.txt"
    child_path = _write(tmp_path, "child.yml", {"effects": [_bump_tool(counter_file)]})
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"attempt": {"path": "prime.bump.value"}},
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="value.attempt == 'never'"),
        on_error="continue",
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["value"] is None
    attempts = store.state["run_child"]["meta"]["attempts"]
    assert len(attempts) == 2
    assert [a["value"] for a in attempts] == [{"attempt": "1"}, {"attempt": "2"}]
    assert all(a["error"] is not None for a in attempts)


def test_attempts_carry_child_errors_when_the_child_fails(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    child_path = _write(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "fail",
                    "provider": "shell",
                    "params": {"command": "false", "allowed_commands": ["false"]},
                }
            ]
        },
    )
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
        on_error="continue",
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    attempts = store.state["run_child"]["meta"]["attempts"]
    assert len(attempts) == 2
    for entry in attempts:
        assert entry["value"] is None
        assert entry["expect"] is None
        assert entry["child_errors"] is not None


def test_attempts_is_reset_on_a_reused_node(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    child_path = _write(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "t",
                    "provider": "shell",
                    "params": {"command": "echo", "allowed_commands": ["echo"], "args": ["ok"]},
                }
            ]
        },
    )
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child", path=str(child_path), retries=RetryPolicyDef(max_attempts=3, backoff_ms=10)
    )
    store = Store({})
    node = store.ensure_dict("run_child")
    node["meta"] = {
        "retries_used": 2,
        "attempts": [{"created_at": "T", "completed_at": "T", "value": None, "error": None, "child_errors": None, "expect": None}],
    }

    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert "attempts" not in store.state["run_child"]["meta"]


def test_attempts_interrupt_during_backoff_leaves_no_entry_for_the_in_flight_attempt(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    counter_file = tmp_path / "attempts.txt"
    child_path = _write(tmp_path, "child.yml", {"effects": [_bump_tool(counter_file)]})

    calls = {"n": 0}

    def _sleep(seconds: float) -> None:
        calls["n"] += 1
        if calls["n"] == 2:
            raise RunCancelledBySignal("run cancelled")

    _patch_sleep(monkeypatch, _sleep)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"attempt": {"path": "prime.bump.value"}},
        retries=RetryPolicyDef(max_attempts=4, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="value.attempt == '4'"),
    )
    store = Store({})
    with pytest.raises(RunCancelledBySignal):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    attempts = store.state["run_child"]["meta"]["attempts"]
    assert len(attempts) == 2
    assert [a["value"] for a in attempts] == [{"attempt": "1"}, {"attempt": "2"}]
    assert store.state["run_child"]["meta"].get("completed_at") is None


# ── inputs: {from: prime.<name>.meta.attempts}, record_children, full-namespace ──


def _child_with_a_first_attempt_only_step(counter_file: Path) -> dict[str, Any]:
    """Bumps *counter_file*, then an `if` that runs `first_only` only when
    the counter reads ``1`` (so only the orchestration's own first attempt
    runs it, whichever attempt that turns out to be)."""
    return {
        "effects": [
            _bump_tool(counter_file),
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": "state.prime.bump.value == '1'"},
                "then": [
                    {
                        "type": "tool",
                        "name": "first_only",
                        "provider": "json",
                        "params": {"mode": "stringify", "input": {"seen": True}},
                    }
                ],
            },
        ]
    }


def test_attempts_record_children_keeps_the_stale_step_only_in_its_own_entry(
    tmp_path: Path,
) -> None:
    """Rule 5: each retry starts from a clean node. With `record_children`, a
    step that only an earlier attempt ran is gone from the node after a later
    attempt succeeds, but it is still there in that attempt's own entry."""
    counter_file = tmp_path / "attempts.txt"
    child = _write(tmp_path, "child.yml", _child_with_a_first_attempt_only_step(counter_file))
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "theme",
                "path": str(child),
                "outputs": {"attempt": {"path": "prime.bump.value"}},
                "retries": {"max_attempts": 3, "backoff_ms": 1},
                "expect": "value.attempt == '2'",
            }
        ]
    }
    state = _run_top(orch, runtime_config={"state": {"record_children": True}})

    node = state["prime"]["theme"]
    assert "first_only" not in node["gate"]
    attempts = node["meta"]["attempts"]
    assert len(attempts) == 2
    assert "first_only" in attempts[0]["children"]["gate"]
    assert "first_only" not in attempts[1]["children"]["gate"]


def test_attempts_full_namespace_mode_also_keeps_a_clean_node(tmp_path: Path) -> None:
    """Rule 5, full-namespace mode (no `outputs:`, no `record_children`): the
    node no longer carries an earlier attempt's step, and entries' `value`
    is `None` because full-namespace mode never maps one."""
    counter_file = tmp_path / "attempts.txt"
    child = _write(tmp_path, "child.yml", _child_with_a_first_attempt_only_step(counter_file))
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "theme",
                "path": str(child),
                "retries": {"max_attempts": 3, "backoff_ms": 1},
                "expect": "state.prime.theme.bump.value == '2'",
            }
        ]
    }
    state = _run_top(orch)

    node = state["prime"]["theme"]
    assert "first_only" not in node["gate"]
    attempts = node["meta"]["attempts"]
    assert len(attempts) == 2
    assert all(a["value"] is None for a in attempts)
    assert "children" not in attempts[0]


def test_attempts_self_reference_sees_only_earlier_attempts(tmp_path: Path) -> None:
    """Rule 4: a retried use's own `inputs`/`expect` can read `meta.attempts`
    as it stood before the current attempt -- `null` on the first, then one
    more entry per attempt after."""
    counter_file = tmp_path / "attempts.txt"
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"inputs": {"earlier": {"required": False}}},
            "effects": [_bump_tool(counter_file)],
        },
    )
    orch = {
        "effects": [
            {
                "type": "use",
                "name": "theme",
                "path": str(child),
                "inputs": {"earlier": {"from": "prime.theme.meta.attempts"}},
                "outputs": {
                    "attempt": {"path": "prime.bump.value"},
                    "earlier": {"path": "input.earlier"},
                },
                "retries": {"max_attempts": 3, "backoff_ms": 1},
                "expect": "value.attempt == '3'",
            }
        ]
    }
    state = _run_top(orch)

    node = state["prime"]["theme"]
    attempts = node["meta"]["attempts"]
    assert len(attempts) == 3
    assert attempts[0]["value"]["earlier"] is None
    assert attempts[1]["value"]["earlier"] == [attempts[0]]
    assert attempts[2]["value"]["earlier"] == [attempts[0], attempts[1]]
    assert node["value"]["earlier"] == [attempts[0], attempts[1]]
