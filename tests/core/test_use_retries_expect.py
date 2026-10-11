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


def _patch_sleep(monkeypatch: pytest.MonkeyPatch, fn) -> None:
    """Patch the cancellation-aware retry wait (#356), not a bare time.sleep."""
    from circuitry.core.cancellation import get_token

    monkeypatch.setattr(get_token(), "sleep_or_raise", fn)


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
    _patch_sleep(monkeypatch, lambda s: None)

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
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = Store({})
    with pytest.raises(Exception, match="fail"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert store.state["run_child"]["meta"]["error"] is not None
    assert store.state["run_child"]["meta"]["retries_used"] == 1


def test_use_created_at_is_the_first_attempts_start_not_the_last(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#421: a retried use's created_at is the start of its first attempt,
    not reset by a later attempt within the same pass -- unlike
    retries_used, which does track the attempt that decided the outcome."""
    from circuitry.core import use as use_mod

    timestamps = iter(["T_outer", "T0", "T1"])
    monkeypatch.setattr(use_mod, "_now_iso", lambda: next(timestamps))

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
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = Store({})
    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    meta = store.state["run_child"]["meta"]
    assert meta["created_at"] == "T0"
    assert meta["completed_at"] == "T1"
    assert meta["retries_used"] == 1


def test_use_exhausting_retries_still_records_created_at_and_retries_used(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#421: retries_used is set on a failed outcome too, not only on
    success -- and created_at still names the first attempt's start."""
    from circuitry.core import use as use_mod

    timestamps = iter(["T_outer", "T0", "T1"])
    monkeypatch.setattr(use_mod, "_now_iso", lambda: next(timestamps))

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
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = Store({})
    with pytest.raises(Exception, match="fail"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    meta = store.state["run_child"]["meta"]
    assert meta["created_at"] == "T0"
    assert meta["completed_at"] == "T1"
    assert meta["retries_used"] == 1


def test_use_a_single_failed_attempt_leaves_retries_used_absent(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """absent, not 0, when the first attempt is also the last (#421) --
    the same rule a tool's and a prompt's failure path follow."""
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
    _patch_sleep(monkeypatch, lambda s: None)

    defn = UseDefinition(name="run_child", path=str(child_path))
    store = Store({})
    with pytest.raises(Exception, match="fail"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert "retries_used" not in store.state["run_child"]["meta"]


def test_effect_complete_observer_sees_retries_used_on_exhausted_retry(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#421 P2-6: retries_used is already on the node by the time
    effect_complete fires, even on the exhausted-retries failure path --
    pins the ordering `fire_effect_complete` relies on."""
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
    _patch_sleep(monkeypatch, lambda s: None)

    captured: dict = {}

    def _observer(path: str, node: dict) -> None:
        if path == "run_child":
            captured.update(node)

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=2, backoff_ms=10),
    )
    store = Store({}, effect_complete=_observer)
    with pytest.raises(Exception, match="fail"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert captured["meta"]["retries_used"] == 1


def test_use_missing_path_is_not_retried(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A missing child file fails the exact same way on every attempt —
    retrying it only burns the backoff wait for nothing (#273 review,
    finding 9). A retry sleeps before each attempt after the first, so zero
    sleeps proves this failed on the first attempt."""
    sleep = MagicMock()
    _patch_sleep(monkeypatch, sleep)

    defn = UseDefinition(
        name="run_child",
        path=str(tmp_path / "does-not-exist.yml"),
        retries=RetryPolicyDef(max_attempts=5, backoff_ms=10),
    )
    store = Store({})
    with pytest.raises(RuntimeError, match="not found"):
        UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    sleep.assert_not_called()
    assert store.state["run_child"]["meta"].get("retries_used") is None


def test_use_cycle_is_not_retried(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A cycle (`use` referencing an ancestor of its own call stack) never
    resolves differently on a second attempt (#273 review, finding 9)."""
    sleep = MagicMock()
    _patch_sleep(monkeypatch, sleep)

    child_path = _write_orch(
        tmp_path,
        "self_referencing.yml",
        {"effects": [{"type": "tool", "name": "t", "provider": "shell", "params": {}}]},
    )

    defn = UseDefinition(
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=5, backoff_ms=10),
    )
    store = Store({})
    identity = str(child_path.resolve())
    runtime_config = {"_use_call_stack": [identity]}
    with pytest.raises(RuntimeError, match="cycle detected"):
        UseRuntime(
            defn, adapter=_mock_adapter(), model="m", runtime_config=runtime_config
        ).execute(store=store, ctx={})

    sleep.assert_not_called()


def test_use_retries_used_is_reset_on_a_reused_node(
    tmp_path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A node that needed a retry on an earlier execution and succeeds on
    the first attempt of a later one must not keep showing the earlier
    pass's retries_used (#273 review, finding 10, the same stale-meta
    problem #260 fixed elsewhere)."""
    child_path = _write_orch(
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
        name="run_child",
        path=str(child_path),
        retries=RetryPolicyDef(max_attempts=3, backoff_ms=10),
    )
    store = Store({})
    node = store.ensure_dict("run_child")
    node["meta"] = {"retries_used": 2}

    UseRuntime(defn, adapter=_mock_adapter(), model="m").execute(store=store, ctx={})

    assert "retries_used" not in store.state["run_child"]["meta"]


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
    _patch_sleep(monkeypatch, lambda s: None)

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
