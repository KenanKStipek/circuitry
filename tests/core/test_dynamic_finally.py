"""`finally:` cleanup blocks on the document root and `dynamic` (#272):
runs after the main effects whether they succeeded or failed, never hides
the original error, and is rejected on every other effect type.
"""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


@dataclass
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


def _write_tool(name: str, path: str, *, on_error: str = "fail") -> dict:
    return {
        "type": "tool",
        "name": name,
        "provider": "fs",
        "on_error": on_error,
        "params": {"mode": "write", "path": path, "content": "x"},
    }


def _read_missing_tool(name: str, path: str, *, on_error: str = "fail") -> dict:
    return {
        "type": "tool",
        "name": name,
        "provider": "fs",
        "on_error": on_error,
        "params": {"mode": "read", "path": path},
    }


def test_finally_runs_on_root_after_success(tmp_path) -> None:
    marker = tmp_path / "stopped.txt"
    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [_write_tool("cleanup", str(marker))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert marker.exists()
    assert store.state["prime"]["meta"]["error"] is None


def test_finally_runs_after_a_body_failure_and_original_error_propagates(
    tmp_path,
) -> None:
    marker = tmp_path / "stopped.txt"
    orch = {
        "effects": [
            _write_tool("start_server", str(tmp_path / "ok.txt")),
            _read_missing_tool("boom", str(tmp_path / "missing.txt")),
        ],
        "finally": [_write_tool("stop_server", str(marker))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    with pytest.raises(Exception, match="boom"):
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert marker.exists(), "finally must run even though the body failed"
    assert "boom" in store.state["prime"]["meta"]["error"]


def test_finally_sees_state_the_body_produced(tmp_path) -> None:
    """A started server's pid (or any sibling output) is in scope for finally."""
    pid_file = tmp_path / "server.pid"
    stop_marker = tmp_path / "stopped-with-pid.txt"
    orch = {
        "effects": [_write_tool("start_server", str(pid_file))],
        "finally": [
            {
                "type": "tool",
                "name": "stop_server",
                "provider": "fs",
                "params": {
                    "mode": "write",
                    "path": str(stop_marker),
                    "content": "{{prime.start_server.value}}",
                },
            }
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert stop_marker.exists()
    assert stop_marker.read_text() == str(pid_file)


def test_finally_failure_after_success_fails_the_run(tmp_path) -> None:
    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [_read_missing_tool("cleanup", str(tmp_path / "missing.txt"))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    with pytest.raises(Exception, match="cleanup"):
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert store.state["prime"]["meta"]["error"] is not None


def test_finally_failure_with_its_own_on_error_continue_does_not_fail_the_run(
    tmp_path,
) -> None:
    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [
            _read_missing_tool(
                "cleanup", str(tmp_path / "missing.txt"), on_error="continue"
            )
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert store.state["prime"]["meta"]["error"] is None


def test_finally_failure_adds_a_note_without_hiding_the_original_error(
    tmp_path,
) -> None:
    orch = {
        "effects": [_read_missing_tool("boom", str(tmp_path / "missing1.txt"))],
        "finally": [_read_missing_tool("cleanup", str(tmp_path / "missing2.txt"))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    with pytest.raises(Exception, match="boom"):
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    meta = store.state["prime"]["meta"]
    assert "boom" in meta["error"]
    assert "cleanup" in meta.get("finally_error", "")


def test_finally_on_nested_dynamic_runs_after_its_own_body(tmp_path) -> None:
    marker = tmp_path / "nested-stopped.txt"
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "with_server",
                "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
                "finally": [_write_tool("stop", str(marker))],
            }
        ],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert marker.exists()


@pytest.mark.parametrize(
    "effect",
    [
        {"type": "tool", "name": "x", "provider": "fs", "params": {}, "finally": []},
        {"type": "use", "name": "x", "path": "does-not-matter.yml", "finally": []},
        {
            "type": "loop",
            "name": "x",
            "each": {"in": "input.items"},
            "body": [{"type": "tool", "name": "y", "provider": "fs", "params": {}}],
            "finally": [],
        },
        {
            "type": "if",
            "if": {"mode": "cel", "expr": "true"},
            "then": [],
            "finally": [],
        },
    ],
)
def test_finally_is_rejected_on_every_effect_type_except_dynamic(effect) -> None:
    orch = {"effects": [effect]}
    with pytest.raises(ValueError, match="only allowed on a 'dynamic' effect"):
        compile_orchestration(orch=orch, root_name="prime")


def test_finally_runs_when_a_use_child_document_finishes(tmp_path) -> None:
    from circuitry.core.use import UseDefinition, UseRuntime

    marker = tmp_path / "child-finally.txt"
    child_orch = {
        "effects": [_write_tool("step", str(tmp_path / "child-ok.txt"))],
        "finally": [_write_tool("cleanup", str(marker))],
    }
    child_path = tmp_path / "child.yml"
    import yaml

    child_path.write_text(yaml.safe_dump(child_orch))

    defn = UseDefinition(name="run_child", path=str(child_path))
    store = Store({})
    UseRuntime(defn, adapter=EchoAdapter(), model="m").execute(store=store, ctx={})
    assert marker.exists()


# ---------------------------------------------------------------------------
# Cancellation (#272 review, finding 1) and a finally failure's own on_error
# (finding 2).
# ---------------------------------------------------------------------------


class _CancelledLikeMCP(BaseException):
    """Stands in for adapters.host_claude.RunCancelled: a BaseException that
    is not a KeyboardInterrupt/SystemExit, the way the MCP server cancels a
    run mid-prompt."""


@dataclass
class _CancellingAdapter:
    exc: BaseException
    name: str = "cancelling"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120):
        raise self.exc


def _dynamic_with_cancelling_prompt(exc: BaseException, *, on_error: str) -> tuple:
    from circuitry.core.dynamic import DynamicDefinition
    from circuitry.core.prompt import PromptDefinition

    body = PromptDefinition(name="ask", template="hi")
    root = DynamicDefinition(name="prime", effects=(body,), on_error=on_error)
    return root, _CancellingAdapter(exc)


def test_mcp_run_cancelled_propagates_even_with_on_error_skip() -> None:
    """A dynamic's on_error: skip only degrades an ordinary failure — an MCP
    RunCancelled-like BaseException must still stop the run (#272 review,
    finding 1), the same as it did before `finally:` existed."""
    root, adapter = _dynamic_with_cancelling_prompt(
        _CancelledLikeMCP("cancelled"), on_error="skip"
    )
    store = Store({})
    with pytest.raises(_CancelledLikeMCP):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)


def test_mcp_run_cancelled_propagates_even_with_on_error_continue() -> None:
    root, adapter = _dynamic_with_cancelling_prompt(
        _CancelledLikeMCP("cancelled"), on_error="continue"
    )
    store = Store({})
    with pytest.raises(_CancelledLikeMCP):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)


def test_keyboard_interrupt_still_propagates_with_on_error_skip() -> None:
    root, adapter = _dynamic_with_cancelling_prompt(
        KeyboardInterrupt(), on_error="skip"
    )
    store = Store({})
    with pytest.raises(KeyboardInterrupt):
        DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)


def test_ordinary_failure_with_on_error_skip_does_not_propagate() -> None:
    """Sanity check: on_error: skip still degrades a real (Exception) body
    failure normally — only a BaseException cancellation bypasses it."""
    root, adapter = _dynamic_with_cancelling_prompt(
        RuntimeError("adapter dispatch failed"), on_error="skip"
    )
    store = Store({})
    DynamicRuntime(root, adapter=adapter, model="m").execute(store=store)
    assert "adapter dispatch failed" in store.state["prime"]["meta"]["error"]


def test_finally_failure_with_dynamics_own_on_error_continue_does_not_fail_the_run(
    tmp_path,
) -> None:
    """A dynamic's own on_error: continue covers a finally failure on an
    otherwise-successful body, not just a body failure (#272 review,
    finding 2)."""
    from circuitry.core.dynamic import DynamicDefinition

    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [_read_missing_tool("cleanup", str(tmp_path / "missing.txt"))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    root_with_on_error = DynamicDefinition(
        name=root.name,
        effects=root.effects,
        finally_effects=root.finally_effects,
        on_error="continue",
    )
    store = Store({})
    # Degraded, not raised — on_error: continue recorded the finally failure
    # on this dynamic's own meta.error (the same thing a body failure's
    # on_error: continue does) rather than propagating it.
    DynamicRuntime(root_with_on_error, adapter=EchoAdapter(), model="m").execute(store=store)
    assert "cleanup" in store.state["prime"]["meta"]["error"]


def test_finally_duplicate_name_with_body_effect_is_a_compile_error(tmp_path) -> None:
    """A finally effect reusing a body effect's name would silently overwrite
    that state node at runtime (both share the dynamic's own child_store) —
    caught at compile time instead (#272 review, finding 8)."""
    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [_write_tool("step", str(tmp_path / "other.txt"))],
    }
    with pytest.raises(ValueError, match="Duplicate effect name 'step'"):
        compile_orchestration(orch=orch, root_name="prime")


def test_sum_tokens_includes_a_tool_or_use_effects_own_expect_tokens() -> None:
    """A tool/use's own `expect: {mode: model}` spends tokens too (#273),
    recorded under meta.expect rather than meta directly \u2014 a parent
    dynamic's own displayed token total must still include them (#273
    review, finding 4, P2 part)."""
    from circuitry.core.dynamic import _sum_tokens

    subtree = {
        "meta": {"tokens_sent": 10, "tokens_received": 5},
        "child": {
            "meta": {
                "tokens_sent": None,
                "tokens_received": None,
                "expect": {"tokens_sent": 7, "tokens_received": 3},
            }
        },
    }
    sent, recv = _sum_tokens(subtree)
    assert sent == 17
    assert recv == 8


def test_finally_failure_path_matches_the_real_state_node(tmp_path) -> None:
    """A finally effect writes into the same child_store namespace as the
    body (no separate 'finally' segment in the state tree) \u2014 the wrapped
    error's path should say so too (#272 review, finding 11)."""
    orch = {
        "effects": [_write_tool("step", str(tmp_path / "ok.txt"))],
        "finally": [_read_missing_tool("stop_server", str(tmp_path / "missing.txt"))],
    }
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store({})
    with pytest.raises(Exception, match=r"^prime\.stop_server: ") as excinfo:
        DynamicRuntime(root, adapter=EchoAdapter(), model="m").execute(store=store)
    assert "prime.finally.stop_server" not in str(excinfo.value)
