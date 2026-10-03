"""The run-time backstop for a tool effect's own capability (#275):
`ToolRuntime.execute` checks its own provider against the installed ceiling
right where it is about to run, so a gap in the static walk — a document
form it doesn't parse (a legacy `steps:` branch type), a provider only known
after templating — can never let a tool through a ceiling that does not
cover it.
"""

from __future__ import annotations

import pytest

from circuitry.capability_gate import CapabilityConsentError, install_capability_ceiling
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime

_ECHO_PARAMS = {"command": "echo", "args": ["hi"]}


def test_a_tool_beyond_the_installed_ceiling_is_refused() -> None:
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"network"}))
    runtime = ToolRuntime(
        ToolDefinition(name="t", provider="shell", params=_ECHO_PARAMS),
        runtime_config=runtime_config,
    )

    with pytest.raises(CapabilityConsentError, match="shell"):
        runtime.execute(store=Store({}), ctx={})


def test_a_tool_within_the_installed_ceiling_runs() -> None:
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"shell"}))
    runtime = ToolRuntime(
        ToolDefinition(name="t", provider="shell", params=_ECHO_PARAMS),
        runtime_config=runtime_config,
    )
    store = Store({})

    runtime.execute(store=store, ctx={})

    assert store.state["t"]["meta"]["error"] is None


def test_no_ceiling_installed_is_unrestricted() -> None:
    runtime = ToolRuntime(ToolDefinition(name="t", provider="shell", params=_ECHO_PARAMS))
    store = Store({})

    runtime.execute(store=store, ctx={})

    assert store.state["t"]["meta"]["error"] is None


def test_an_untagged_provider_is_unrestricted_by_any_ceiling() -> None:
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset())
    runtime = ToolRuntime(
        ToolDefinition(name="t", provider="uuid", params={}),
        runtime_config=runtime_config,
    )
    store = Store({})

    runtime.execute(store=store, ctx={})

    assert store.state["t"]["meta"]["error"] is None
