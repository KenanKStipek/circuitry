"""An ``agent`` tool effect's usage counts in the run totals and the
``dynamic`` tree's token sums \u2014 in a chain, in a loop and in a ``use``
child (#414). The fake pi CLI here is the same harness
``tests/plugins/test_agent.py`` uses; no test runs a real CLI or calls a
real model.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml
from agent_cli_test_support import pi_events, write_fake_cli

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.dynamic import _sum_tokens

#: One pi call's usage: 5 sent (input), 2 received (output), $0.001.
_USAGE = {"input": 5, "output": 2, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.001}}


class _UnusedAdapter:
    """A tool-only orchestration still needs an adapter name resolved
    (``use`` is conservatively assumed to have prompts), but never calls
    one."""

    name = "fake"

    def generate(self, **kwargs: object) -> object:
        raise AssertionError("no prompt effect in this orchestration should call the adapter")


class _EchoAdapter:
    """A fixed-usage fake adapter for a prompt effect alongside an agent
    tool effect in the same loop pass; no test here calls a real model."""

    name = "fake"

    def __init__(self, *, tokens_sent: int, tokens_received: int) -> None:
        self.tokens_sent = tokens_sent
        self.tokens_received = tokens_received

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        return GenerateResult(
            text="ok", raw={}, tokens_sent=self.tokens_sent, tokens_received=self.tokens_received
        )


def _fake_pi(tmp_path: Path) -> Path:
    return write_fake_cli(tmp_path, "pi", stdout=pi_events(usage=_USAGE))


def _agent_tool(name: str, *, cwd: Path, prompt: str = "go") -> dict:
    return {
        "type": "tool",
        "name": name,
        "provider": "agent",
        "timeout_ms": 30000,
        "params": {"prompt": prompt, "cwd": str(cwd)},
    }


@pytest.fixture
def workdir(tmp_path: Path) -> Path:
    path = tmp_path / "work"
    path.mkdir()
    return path


def test_run_totals_include_agent_effects_in_a_chain_a_loop_and_a_use_child(
    tmp_path: Path, workdir: Path
) -> None:
    fake_pi = _fake_pi(tmp_path)
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        yaml.safe_dump({"effects": [_agent_tool("inner_agent", cwd=workdir)]}),
        encoding="utf-8",
    )
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "effects": [
                    _agent_tool("agent_a", cwd=workdir),
                    _agent_tool("agent_b", cwd=workdir),
                    {
                        "type": "loop",
                        "name": "agent_loop",
                        "each": {"in": "prime.items.value", "as": "it"},
                        "body": [_agent_tool("agent_iter", cwd=workdir, prompt="{{it}}")],
                    },
                    {
                        "type": "use",
                        "name": "agent_use",
                        "path": str(child_path),
                        "outputs": {"child_value": "prime.inner_agent.value"},
                    },
                ]
            }
        ),
        encoding="utf-8",
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"prime": {"items": {"value": ["x", "y"]}}},
            adapter=_UnusedAdapter(),
            config=CircuitryConfig(
                default_adapter="fake",
                default_model="test",
                runtime={"plugins": {"agent": {"pi": {"binary": str(fake_pi)}}}},
            ),
            skip_preflight=True,
        )
    )
    assert result.ok, result.error

    # 2 top-level + 2 loop iterations + 1 use child = 5 agent calls.
    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["tokens_sent"] == 5 * 5
    assert totals["tokens_received"] == 5 * 2
    assert totals["cost_usd"] == pytest.approx(5 * 0.001)


def test_dynamic_tree_token_sum_includes_an_agent_effect_in_a_chain_a_loop_and_a_use_child(
    tmp_path: Path, workdir: Path
) -> None:
    """``core.dynamic._sum_tokens`` needs no change of its own (#414): it
    already reads whatever meta dict it finds, so it picks up an agent
    effect's tokens_sent/tokens_received the moment ToolRuntime writes
    them — including nested under a ``use`` child's own state."""
    fake_pi = _fake_pi(tmp_path)
    child_path = tmp_path / "child.yml"
    child_path.write_text(
        yaml.safe_dump({"effects": [_agent_tool("inner_agent", cwd=workdir)]}),
        encoding="utf-8",
    )
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "effects": [
                    _agent_tool("agent_a", cwd=workdir),
                    {
                        "type": "loop",
                        "name": "agent_loop",
                        "each": {"in": "prime.items.value", "as": "it"},
                        "body": [_agent_tool("agent_iter", cwd=workdir, prompt="{{it}}")],
                    },
                    {
                        # No explicit outputs: full-namespace mode grafts the
                        # child's own prime subtree (inner_agent, with its
                        # meta) directly under this node, so _sum_tokens's
                        # plain dict recursion reaches it without needing
                        # runtime.state.record_children.
                        "type": "use",
                        "name": "agent_use",
                        "path": str(child_path),
                    },
                ]
            }
        ),
        encoding="utf-8",
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"prime": {"items": {"value": ["x", "y", "z"]}}},
            adapter=_UnusedAdapter(),
            config=CircuitryConfig(
                default_adapter="fake",
                default_model="test",
                runtime={"plugins": {"agent": {"pi": {"binary": str(fake_pi)}}}},
            ),
            skip_preflight=True,
        )
    )
    assert result.ok, result.error

    # 1 chain effect + 3 loop iterations + 1 use child = 5 agent nodes.
    # A named loop's state also keeps its last completed pass aliased
    # under "last" (the same dict as "iter_2", not a copy), but _sum_tokens
    # skips that alias so it isn't counted twice.
    sent, received = _sum_tokens(result.state["prime"])
    assert sent == 5 * 5
    assert received == 5 * 2


def test_a_named_loops_last_alias_does_not_double_count_its_final_pass(
    tmp_path: Path, workdir: Path
) -> None:
    """``node["last"]`` is the very same dict as ``node["iter_<N-1>"]``, not
    a copy (``LoopRuntime._link_last``) — walking both would count the
    final pass's tokens twice. Each pass here runs both an agent tool
    effect and a prompt effect, so the sum is N times one pass's total, not
    (N+1) times."""
    fake_pi = _fake_pi(tmp_path)
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "effects": [
                    {
                        "type": "loop",
                        "name": "mixed_loop",
                        "each": {"in": "prime.items.value", "as": "it"},
                        "body": [
                            _agent_tool("agent_iter", cwd=workdir, prompt="{{it}}"),
                            {"type": "prompt", "name": "prompt_iter", "template": "{{it}}"},
                        ],
                    }
                ]
            }
        ),
        encoding="utf-8",
    )

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"prime": {"items": {"value": ["x", "y", "z"]}}},
            adapter=_EchoAdapter(tokens_sent=11, tokens_received=4),
            config=CircuitryConfig(
                default_adapter="fake",
                default_model="test",
                runtime={"plugins": {"agent": {"pi": {"binary": str(fake_pi)}}}},
            ),
            skip_preflight=True,
        )
    )
    assert result.ok, result.error

    # 3 passes, each spending 5 sent/2 received (agent, _USAGE) plus 11
    # sent/4 received (prompt, _EchoAdapter) — 3x one pass's total, not 4x
    # from the loop's own "last" alias of its final pass.
    sent, received = _sum_tokens(result.state["prime"])
    assert sent == 3 * (5 + 11)
    assert received == 3 * (2 + 4)


def test_an_effect_a_dynamic_names_last_still_counts() -> None:
    """Only a loop node's `last` (next to its own `iter_<N>` passes) is an
    alias of a pass: `last` is a legal effect name, and a dynamic's own
    child of that name spends tokens like any other."""
    state = {
        "d": {
            "value": None,
            "meta": {},
            "last": {"value": "x", "meta": {"tokens_sent": 5, "tokens_received": 7}},
        }
    }
    assert _sum_tokens(state) == (5, 7)


def test_a_loops_last_counts_once_even_as_a_copy_of_its_final_pass() -> None:
    """The alias is skipped by position (next to `iter_<N>`), not by identity,
    so a state whose `last` is a separate copy of the final pass counts that
    pass once too."""
    final_pass = {"step": {"value": "y", "meta": {"tokens_sent": 3, "tokens_received": 4}}}
    state = {
        "lp": {
            "value": None,
            "meta": {},
            "iter_0": {"step": {"value": "x", "meta": {"tokens_sent": 1, "tokens_received": 2}}},
            "iter_1": final_pass,
            "last": {"step": {"value": "y", "meta": {"tokens_sent": 3, "tokens_received": 4}}},
        }
    }
    assert _sum_tokens(state) == (4, 6)
