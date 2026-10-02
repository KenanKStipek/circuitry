"""`retries` and `expect:` on `use` effects (#273)."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import _compile_effect
from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _write_orch(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "yes") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


def test_compile_use_retries_field() -> None:
    effect = {
        "type": "use",
        "name": "x",
        "path": "child.yml",
        "retries": {"max_attempts": 3, "backoff_ms": 200},
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, UseDefinition)
    assert defn.retries == RetryPolicyDef(max_attempts=3, backoff_ms=200)


def test_compile_use_expect_bare_string_is_cel_shorthand() -> None:
    effect = {
        "type": "use",
        "name": "x",
        "path": "child.yml",
        "expect": "has(value.summary)",
    }
    defn = _compile_effect(effect, scope_path="prime", effect_path="prime.effects[0]")
    assert isinstance(defn, UseDefinition)
    assert defn.expect == ExpectDef(mode="cel", expr="has(value.summary)")


def test_use_retries_the_whole_child_run(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A counter file lets the child orchestration fail on its first run and
    succeed on its second — the whole child re-runs from scratch per retry.
    """
    counter_file = tmp_path / "attempts.txt"
    child_path = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "bump",
                    "provider": "shell",
                    "params": {
                        "command": "bash",
                        "allowed_commands": ["bash"],
                        "args": [
                            "-c",
                            (
                                f'n=$(cat "{counter_file}" 2>/dev/null || echo 0); '
                                f'n=$((n+1)); echo -n "$n" > "{counter_file}"; '
                                f'if [ "$n" -lt 2 ]; then exit 1; fi; echo ok'
                            ),
                        ],
                    },
                }
            ]
        },
    )
    monkeypatch.setattr("circuitry.core.use.time.sleep", lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["meta"]["error"] is None
    assert store.state["run_child"]["meta"]["retries_used"] == 1
    assert counter_file.read_text() == "2"


def test_use_exhausts_retries_and_fails(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    child_path = _write_orch(
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
    monkeypatch.setattr("circuitry.core.use.time.sleep", lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = Store({})
    with pytest.raises(Exception, match="fail"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["meta"]["error"] is not None


def test_use_expect_cel_over_mapped_value(tmp_path) -> None:
    child_path = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "make",
                    "provider": "json",
                    "params": {"mode": "stringify"},
                    "params_json": '{"input": {"summary": "hi"}}',
                }
            ]
        },
    )
    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"summary": {"path": "prime.make.value"}},
        expect=ExpectDef(mode="cel", expr="has(value.summary)"),
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["meta"]["error"] is None
    assert store.state["run_child"]["meta"]["expect"]["result"] is True


def test_use_expect_cel_fails_and_retries_then_succeeds(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    counter_file = tmp_path / "attempts.txt"
    child_path = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "bump",
                    "provider": "shell",
                    "params": {
                        "command": "bash",
                        "allowed_commands": ["bash"],
                        "args": [
                            "-c",
                            (
                                f'n=$(cat "{counter_file}" 2>/dev/null || echo 0); '
                                f'n=$((n+1)); echo -n "$n" > "{counter_file}"; '
                                f'echo -n "$n"'
                            ),
                        ],
                    },
                }
            ]
        },
    )
    monkeypatch.setattr("circuitry.core.use.time.sleep", lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"attempt": {"path": "prime.bump.value"}},
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
        expect=ExpectDef(mode="cel", expr="value.attempt == '2'"),
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["meta"]["retries_used"] == 1
    assert store.state["run_child"]["value"]["attempt"] == "2"


def test_use_expect_model_mode_asks_adapter(tmp_path) -> None:
    child_path = _write_orch(
        tmp_path,
        "child.yml",
        {
            "effects": [
                {
                    "type": "tool",
                    "name": "make",
                    "provider": "json",
                    "params": {"mode": "stringify"},
                    "params_json": '{"input": {"summary": "hi"}}',
                }
            ]
        },
    )
    adapter = _mock_adapter(response="yes")
    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        outputs={"summary": {"path": "prime.make.value"}},
        expect=ExpectDef(mode="model", template="Looks right? {{value}}"),
    )
    store = Store({})
    UseRuntime(defn, adapter=adapter, model="m").execute(store=store, ctx={})

    expect_meta = store.state["run_child"]["meta"]["expect"]
    assert expect_meta["mode"] == "model"
    assert expect_meta["result"] is True
    adapter.generate.assert_called()
