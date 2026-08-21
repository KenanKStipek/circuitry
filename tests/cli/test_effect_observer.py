"""``RunRequest.effect_observer`` / ``effect_start_observer``: the per-effect
lifecycle, canonically pathed.

The same hooks runtime plugins receive, exposed to in-process callers so a
live UI can mark effects off as they start and land instead of diffing
snapshots.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={}, tokens_sent=3, tokens_received=5)


def _run(tmp_path: Path, orch: dict[str, Any], **kwargs: Any) -> Any:
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            adapter=EchoAdapter(),
            config=CircuitryConfig(),
            skip_preflight=True,
            **kwargs,
        )
    )


CHAIN: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "effects": [
        {"type": "prompt", "name": "first", "template": "one"},
        {"type": "prompt", "name": "second", "template": "two"},
    ],
}


def test_the_observer_sees_every_effect_in_order(tmp_path: Path) -> None:
    seen: list[tuple[str, Any]] = []
    result = _run(
        tmp_path, CHAIN, effect_observer=lambda path, node: seen.append((path, node))
    )

    assert result.ok
    # Children first, then the root dynamic that contained them.
    assert [path for path, _ in seen] == ["prime.first", "prime.second", "prime"]
    first = dict(seen[0][1])
    assert first["value"] == "one"
    assert first["meta"]["tokens_sent"] == 3


def test_loop_iterations_are_reported_at_their_canonical_paths(tmp_path: Path) -> None:
    seen: list[str] = []
    result = _run(
        tmp_path,
        {
            "adapter": "echo",
            "model": "echo-1",
            "effects": [
                {
                    "type": "loop",
                    "name": "spin",
                    "while": {"mode": "cel", "expr": "false"},
                    "min_iterations": 2,
                    "body": [{"type": "prompt", "name": "tick", "template": "t"}],
                }
            ],
        },
        effect_observer=lambda path, node: seen.append(path),
    )

    assert result.ok
    assert seen[:2] == ["prime.spin.iter_0.tick", "prime.spin.iter_1.tick"]
    assert "prime.spin" in seen


def test_a_run_without_an_observer_still_runs(tmp_path: Path) -> None:
    assert _run(tmp_path, CHAIN).ok


def test_the_observer_and_the_plugins_both_get_the_hook(tmp_path: Path) -> None:
    """Adding a caller-side observer must not displace configured plugins."""
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(CHAIN, sort_keys=False), encoding="utf-8")
    seen: list[str] = []
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=EchoAdapter(),
            skip_preflight=True,
            config=CircuitryConfig(plugins=["plugin_fixtures:make_recording_plugin"]),
            effect_observer=lambda effect_path, node: seen.append(effect_path),
        )
    )

    assert result.ok
    assert seen[:2] == ["prime.first", "prime.second"]
    assert result.state["runtime"]["plugin_marker"]["effect_paths"][:2] == [
        "prime.first",
        "prime.second",
    ]


# -- the start counterpart -----------------------------------------------------


def test_the_start_observer_sees_every_effect_before_it_runs(tmp_path: Path) -> None:
    seen: list[tuple[str, Any]] = []
    result = _run(
        tmp_path,
        CHAIN,
        effect_start_observer=lambda path, node: seen.append((path, dict(node))),
    )

    assert result.ok
    # The root dynamic opens first, then each child in declaration order —
    # the mirror image of the completion order.
    assert [path for path, _ in seen] == ["prime", "prime.first", "prime.second"]
    first = seen[1][1]
    # Announced before dispatch: the value is not there yet, the routing is.
    assert first["value"] is None
    assert first["meta"]["model"] == "echo-1"
    assert first["meta"]["prompt_sent"] == "one"


def test_both_observers_interleave_into_balanced_pairs(tmp_path: Path) -> None:
    seen: list[str] = []
    result = _run(
        tmp_path,
        CHAIN,
        effect_start_observer=lambda path, _node: seen.append(f"start:{path}"),
        effect_observer=lambda path, _node: seen.append(f"complete:{path}"),
    )

    assert result.ok
    assert seen == [
        "start:prime",
        "start:prime.first",
        "complete:prime.first",
        "start:prime.second",
        "complete:prime.second",
        "complete:prime",
    ]


def test_the_start_observer_and_the_plugins_both_get_the_hook(tmp_path: Path) -> None:
    """A caller-side start observer must not displace configured plugins."""
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(CHAIN, sort_keys=False), encoding="utf-8")
    seen: list[str] = []
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=EchoAdapter(),
            skip_preflight=True,
            config=CircuitryConfig(plugins=["plugin_fixtures:make_lifecycle_plugin"]),
            effect_start_observer=lambda effect_path, _node: seen.append(effect_path),
        )
    )

    assert result.ok
    assert seen[:2] == ["prime", "prime.first"]
    assert result.state["runtime"]["lifecycle_marker"]["events"][:2] == [
        "start:prime",
        "start:prime.first",
    ]


def test_a_run_without_a_start_observer_still_runs(tmp_path: Path) -> None:
    assert _run(tmp_path, CHAIN, effect_observer=lambda *_: None).ok
