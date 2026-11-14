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

import time
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime
from circuitry.generated_doc import (
    GENERATED_DOCUMENT_PROVIDERS_KEY,
    GENERATED_DOCUMENT_SHELL_COMMANDS_KEY,
    resolved_shell_allowlist,
)


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


def test_use_inline_rendered_whole_dispatch_time_refusal_is_not_retried() -> None:
    """A '{from: ...}' param only resolves to its real value at dispatch
    time -- the compile-time check of a generated document's text sees
    only the raw '{from: ...}' leaf, never the list it resolves to, so it
    can't catch a service start whose resolved executable isn't allowed.
    The dispatch-time check (#772) catches it right before this attempt
    would otherwise run, against the fully rendered params, and the
    failure is never retried."""
    generated_yaml = yaml.dump({
        "effects": [
            {
                "type": "tool",
                "name": "cmd",
                "provider": "json",
                "params": {"mode": "parse", "input": '["rm", "-rf", "/"]'},
            },
            {
                "type": "tool",
                "name": "t",
                "provider": "service",
                "params": {
                    "action": "start",
                    "name": "svc",
                    "command": {"from": "prime.cmd.value"},
                },
                "retries": {"max_attempts": 3},
            },
        ]
    })
    defn = UseDefinition(name="u", inline="{{{prime.gen.value}}}")
    # The enclosing document already uses 'service' and 'json' itself, so
    # only the resolved command -- never visible to the static walk -- is
    # what this attempt must be refused for.
    runtime = UseRuntime(
        defn,
        adapter=None,
        model=None,
        runtime_config={GENERATED_DOCUMENT_PROVIDERS_KEY: ["service", "json"]},
    )
    store = Store(state={"prime": {}})
    ctx = {"prime": {"gen": {"value": generated_yaml}}}

    # retries: {max_attempts: 3}'s default backoff is 1s -- if this
    # refusal were retried like an ordinary tool failure, three attempts
    # would take over 2s. Finishing well under that proves it ran once.
    start = time.monotonic()
    with pytest.raises(RuntimeError, match="'rm'"):
        runtime.execute(store=store, ctx=ctx)
    assert time.monotonic() - start < 0.5


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


def test_path_child_of_a_generated_document_inherits_its_limits(tmp_path) -> None:
    """A use: {path:}/ref:/orchestration: child loads from disk, but once
    its enclosing document is itself generated, it is generated too and
    inherits that document's ceiling -- it no longer gets a fresh one
    just because it names a separate file (#772)."""
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: t\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: touch\n"
        "      allowed_commands: [touch]\n"
    )
    generated_yaml = yaml.dump({
        "effects": [{"type": "use", "name": "go", "path": str(child_path)}],
    })
    defn = UseDefinition(name="u", inline="{{{prime.gen.value}}}")
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={"prime": {}})
    ctx = {"prime": {"gen": {"value": generated_yaml}}}

    with pytest.raises(RuntimeError, match="shell"):
        runtime.execute(store=store, ctx=ctx)


def test_path_child_of_a_non_generated_document_still_gets_a_fresh_ceiling(tmp_path) -> None:
    """The author's own, literal use: {path:} is unaffected: a path/ref
    child of a document that is not itself generated keeps running as its
    own, independently trusted document."""
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: t\n"
        "    provider: shell\n"
        "    params:\n"
        "      command: echo\n"
        "      args: ['hello']\n"
        "      allowed_commands: [echo]\n"
    )
    defn = UseDefinition(name="u", path=str(child_path))
    runtime = UseRuntime(defn, adapter=None, model=None)
    store = Store(state={})

    runtime.execute(store=store, ctx={})

    assert store.state["u"]["t"]["value"].strip() == "hello"


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


def test_reflector_plan_running_a_command_the_documents_own_shell_step_allows_runs() -> None:
    """The common pattern the reflector guidebook chapter recommends: the
    document's own literal shell step already allows a command outside
    the default allowlist ('printf'); a reflector's plan reusing that
    same allowed_commands on the same command runs (#772)."""
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "printf", "args": ["hi"], "allowed_commands": ["printf"]},
            }
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "seed",
                "provider": "shell",
                "params": {"command": "printf", "args": ["seed"], "allowed_commands": ["printf"]},
            },
            {
                "type": "reflector",
                "name": "planner",
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            },
        ]
    }
    root = compile_orchestration(orch=orch)
    store = Store(state={})
    runtime_config = {
        GENERATED_DOCUMENT_PROVIDERS_KEY: ["shell"],
        GENERATED_DOCUMENT_SHELL_COMMANDS_KEY: sorted(
            resolved_shell_allowlist(frozenset({"printf"}), host_pin=None)
        ),
    }

    DynamicRuntime(
        root, adapter=adapter, model="test-model", runtime_config=runtime_config
    ).execute(store=store)

    planner = store.state["prime"]["planner"]
    assert planner["value"] is True


def test_reflector_plan_running_a_command_the_document_doesnt_allow_is_refused() -> None:
    """The twin of the test above: the same plan, but the document's own
    shell step allows a different command -- refused, with the same text
    this check always uses (#772)."""
    plan_yaml = yaml.dump({
        "done": False,
        "effects": [
            {
                "type": "tool",
                "name": "t",
                "provider": "shell",
                "params": {"command": "printf", "args": ["hi"], "allowed_commands": ["printf"]},
            }
        ],
    })
    adapter = _mock_adapter(plan_yaml)
    orch = {
        "effects": [
            {
                "type": "tool",
                "name": "seed",
                "provider": "shell",
                "params": {"command": "echo", "args": ["seed"]},
            },
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

    with pytest.raises(RuntimeError, match="printf"):
        DynamicRuntime(
            root, adapter=adapter, model="test-model", runtime_config=runtime_config
        ).execute(store=store)

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
