"""The generated-document ceiling, end to end (#772).

A document produced while a run executes -- a reflector's plan, or a
``use`` whose ``inline:`` is rendered from state -- can never widen the
shell allowlist, enable a ``service``'s ``shell: true``, or use a tool
provider its enclosing document doesn't use itself, unless the generating
effect lists it in a literal ``allow_tools:``. An author-written ``inline:``
whose structure is literal, with only scalar values templated in, is
unaffected.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime
from circuitry.generated_doc import GENERATED_DOCUMENT_PROVIDERS_KEY


def _mock_adapter(response: str) -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


# ── use(inline): the issue's reproduction ───────────────────────────────


def test_use_inline_rendered_whole_from_state_cannot_widen_shell_allowlist() -> None:
    """The issue's own reproduction: a prompt's reply, rendered whole into
    ``inline: "{{{prime.gen.value}}}"``, carries a ``shell`` effect with its
    own ``allowed_commands``. Refused up front -- nothing runs."""
    generated_yaml = yaml.dump({
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "args": ["pwned"], "allowed_commands": ["touch"]},
            }
        ]
    })
    defn = UseDefinition(name="u", inline="{{{prime.gen.value}}}")
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={"prime": {}})
    ctx = {"prime": {"gen": {"value": generated_yaml}}}

    with pytest.raises(RuntimeError, match="allowed_commands"):
        runtime.execute(store=store, ctx=ctx)


def test_use_inline_rendered_whole_service_shell_true_is_refused() -> None:
    generated_yaml = yaml.dump({
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {"command": "touch pwned", "shell": True},
            }
        ]
    })
    defn = UseDefinition(name="u", inline="{{{prime.gen.value}}}")
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={"prime": {}})
    ctx = {"prime": {"gen": {"value": generated_yaml}}}

    with pytest.raises(RuntimeError, match="shell: true"):
        runtime.execute(store=store, ctx=ctx)


def test_use_inline_rendered_whole_tool_outside_ceiling_is_refused() -> None:
    generated_yaml = yaml.dump({
        "effects": [{"type": "tool", "name": "t", "provider": "git", "params": {}}]
    })
    defn = UseDefinition(name="u", inline="{{{prime.gen.value}}}")
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={"prime": {}})
    ctx = {"prime": {"gen": {"value": generated_yaml}}}

    with pytest.raises(RuntimeError, match="git"):
        runtime.execute(store=store, ctx=ctx)


# ── use(inline): author-written, literal structure ───────────────────────


def test_author_written_inline_with_templated_values_runs_unchanged() -> None:
    """Literal structure, only a scalar value templated in: keeps working
    exactly as before, including a literal ``allowed_commands`` the
    document's enclosing document never itself uses."""
    inline = (
        "effects:\n"
        "  - type: tool\n"
        "    name: step1\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: echo\n"
        "      args: ['{{msg}}']\n"
    )
    defn = UseDefinition(name="u", inline=inline)
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={})
    ctx = {"msg": "hello"}

    runtime.execute(store=store, ctx=ctx)

    assert store.state["u"]["step1"]["value"].strip() == "hello"


# ── reflector: generated plans ────────────────────────────────────────────


def test_reflector_generated_plan_cannot_widen_shell_allowlist() -> None:
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "touch", "args": ["pwned"], "allowed_commands": ["touch"]},
            }
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    store = Store(state={})

    with pytest.raises(RuntimeError, match="allowed_commands"):
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is False


def test_reflector_plan_using_tool_outside_ceiling_is_refused() -> None:
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {"type": "tool", "name": "t", "provider": "shell", "params": {"command": "echo", "args": ["hi"]}}
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    store = Store(state={})

    with pytest.raises(RuntimeError, match="shell"):
        DynamicRuntime(root, adapter=adapter, model="test-model").execute(store=store)


def test_reflector_plan_using_tool_outside_ceiling_runs_once_allow_tools_lists_it() -> None:
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {"type": "tool", "name": "t", "provider": "shell", "params": {"command": "echo", "args": ["hi"]}}
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "allow_tools": ["shell"],
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            }
        ]
    }
    root = compile_orchestration(orch=orch)
    store = Store(state={})
    runtime_config = {GENERATED_DOCUMENT_PROVIDERS_KEY: ["shell"]}

    DynamicRuntime(
        root, adapter=adapter, model="test-model", runtime_config=runtime_config
    ).execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True


def test_reflector_plan_using_a_tool_the_document_already_uses_runs_unchanged() -> None:
    """A document that already uses 'shell' itself needs no 'allow_tools' at
    all for its own reflector's plan to use it too (#772 acceptance 3)."""
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {"type": "tool", "name": "t", "provider": "shell", "params": {"command": "echo", "args": ["hi"]}}
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {"type": "tool", "name": "setup", "provider": "shell", "params": {"command": "pwd"}},
            {
                "type": "reflector",
                "name": "planner",
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            },
        ]
    }
    root = compile_orchestration(orch=orch)
    store = Store(state={})
    runtime_config = {GENERATED_DOCUMENT_PROVIDERS_KEY: ["shell"]}

    DynamicRuntime(
        root, adapter=adapter, model="test-model", runtime_config=runtime_config
    ).execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True
