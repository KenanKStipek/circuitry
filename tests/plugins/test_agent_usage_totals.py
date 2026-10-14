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


def test_dynamic_tree_token_sum_includes_an_agent_effect_in_a_chain_and_a_loop(
    tmp_path: Path, workdir: Path
) -> None:
    """``core.dynamic._sum_tokens`` needs no change of its own (#414): it
    already reads whatever meta dict it finds, so it picks up an agent
    effect's tokens_sent/tokens_received the moment ToolRuntime writes
    them."""
    fake_pi = _fake_pi(tmp_path)
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
            config=CircuitryConfig(
                runtime={"plugins": {"agent": {"pi": {"binary": str(fake_pi)}}}}
            ),
            skip_preflight=True,
        )
    )
    assert result.ok, result.error

    # 1 chain effect + 3 loop iterations; a named loop's state also keeps
    # its last pass under "last" alongside "iter_2", so the subtree the sum
    # walks has 5 agent nodes, not 4 — unrelated to this change, and true for
    # a prompt effect's tokens in a loop the same way.
    sent, received = _sum_tokens(result.state["prime"])
    assert sent == 5 * 5
    assert received == 5 * 2
