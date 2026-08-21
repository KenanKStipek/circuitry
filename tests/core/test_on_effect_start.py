"""The ``on_effect_start`` runtime hook — the completion hook's counterpart.

The interesting property is not that it fires but that it *brackets*: every
start is closed by a complete at the same canonical path, and the pairs nest
like parentheses no matter what the orchestration does in between (loops,
conditional branches, an effect that blows up).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.store import Store


@dataclass(frozen=True)
class EchoAdapter:
    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={}, tokens_sent=3, tokens_received=5)


@dataclass(frozen=True)
class ExplodingAdapter:
    """Fails only for the prompt that asks it to, so one effect in a mixed
    orchestration can fail while its siblings succeed."""

    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "boom" in prompt:
            raise RuntimeError("adapter exploded")
        return GenerateResult(text=prompt, raw={}, tokens_sent=1, tokens_received=1)


def _run(
    tmp_path: Path, orch: dict[str, Any], *, adapter: Any = None, **kwargs: Any
) -> Any:
    path = tmp_path / "orch.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter or EchoAdapter(),
            skip_preflight=True,
            **kwargs,
        )
    )


def _events(state: dict[str, Any]) -> list[str]:
    return list(state["runtime"]["lifecycle_marker"]["events"])


def _assert_bracketed(events: list[str]) -> None:
    """Assert the log reads as balanced, correctly nested parentheses."""
    stack: list[str] = []
    for event in events:
        kind, _, path = event.partition(":")
        if kind == "start":
            stack.append(path)
        else:
            assert stack, f"complete:{path} with no open start (log: {events})"
            opened = stack.pop()
            assert opened == path, (
                f"complete:{path} closed start:{opened} (log: {events})"
            )
    assert not stack, f"unclosed starts: {stack} (log: {events})"


# A prompt, a tool, a loop, a conditional branch and a failing effect in one
# orchestration — the mix the acceptance criteria name.
MIXED: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "plugins": [],
    "effects": [
        {"type": "prompt", "name": "greet", "template": "hello"},
        {
            "type": "tool",
            "name": "sum",
            "provider": "math",
            "params": {"expression": "2+2"},
        },
        {
            "type": "loop",
            "name": "spin",
            "while": {"mode": "cel", "expr": "false"},
            "min_iterations": 2,
            "body": [{"type": "prompt", "name": "tick", "template": "t"}],
        },
        {
            "type": "conditional",
            "name": "gate",
            "if": {"mode": "cel", "expr": "true"},
            "then": [{"type": "prompt", "name": "taken", "template": "yes"}],
            "else": [{"type": "prompt", "name": "skipped", "template": "no"}],
        },
        {
            "type": "tool",
            "name": "divide",
            "provider": "math",
            "params": {"expression": "1/0"},
            "on_error": "continue",
        },
    ],
}


def _lifecycle_cfg() -> CircuitryConfig:
    return CircuitryConfig(plugins=["plugin_fixtures:make_lifecycle_plugin"])


def test_start_and_complete_bracket_a_mixed_orchestration(tmp_path: Path) -> None:
    result = _run(tmp_path, MIXED, config=_lifecycle_cfg())

    assert result.ok is True, result.error
    events = _events(result.state)
    _assert_bracketed(events)

    # Start precedes complete for each effect, and the root dynamic's own
    # pair encloses every child pair.
    assert events[0] == "start:prime"
    assert events[-1] == "complete:prime"
    assert events.index("start:prime.greet") < events.index("complete:prime.greet")
    assert events.index("start:prime.sum") < events.index("complete:prime.sum")

    # Every effect type that fires complete also fires start, and vice versa.
    starts = {e.split(":", 1)[1] for e in events if e.startswith("start:")}
    completes = {e.split(":", 1)[1] for e in events if e.startswith("complete:")}
    assert starts == completes
    assert {"prime", "prime.greet", "prime.sum", "prime.spin"} <= starts


def test_loop_bodies_start_at_their_iteration_paths(tmp_path: Path) -> None:
    """AC: pairs stay balanced for effects inside loops."""
    result = _run(tmp_path, MIXED, config=_lifecycle_cfg())

    events = _events(result.state)
    _assert_bracketed(events)
    assert [e for e in events if ".spin.iter_" in e] == [
        "start:prime.spin.iter_0.tick",
        "complete:prime.spin.iter_0.tick",
        "start:prime.spin.iter_1.tick",
        "complete:prime.spin.iter_1.tick",
    ]
    # The loop's own pair brackets its iterations.
    assert events.index("start:prime.spin") < events.index(
        "start:prime.spin.iter_0.tick"
    )
    assert events.index("complete:prime.spin") > events.index(
        "complete:prime.spin.iter_1.tick"
    )


def test_only_the_taken_conditional_branch_reports(tmp_path: Path) -> None:
    """AC: pairs stay balanced for effects inside conditionals."""
    result = _run(tmp_path, MIXED, config=_lifecycle_cfg())

    events = _events(result.state)
    _assert_bracketed(events)
    assert "start:prime.gate.taken" in events
    assert "complete:prime.gate.taken" in events
    assert not any("skipped" in e for e in events)


def test_a_tolerated_failure_still_closes_its_pair(tmp_path: Path) -> None:
    """``on_error: continue`` — the divide-by-zero tool in MIXED."""
    result = _run(tmp_path, MIXED, config=_lifecycle_cfg())

    assert result.ok is True, result.error
    events = _events(result.state)
    _assert_bracketed(events)
    divide = [e for e in events if e.endswith("prime.divide")]
    assert divide == ["start:prime.divide", "complete:prime.divide"]
    assert result.state["prime"]["divide"]["meta"]["error"]


def test_a_fatal_failure_still_closes_its_pair(tmp_path: Path) -> None:
    """The trap this hook most easily ships with: ``on_error: fail`` aborts
    the run, and the start it already announced must not be left open."""
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {"type": "prompt", "name": "fine", "template": "ok"},
            {"type": "prompt", "name": "bad", "template": "boom", "on_error": "fail"},
            {"type": "prompt", "name": "never", "template": "unreached"},
        ],
    }
    result = _run(
        tmp_path, orch, adapter=ExplodingAdapter(), config=_lifecycle_cfg()
    )

    assert result.ok is False
    events = _events(result.state)
    _assert_bracketed(events)
    assert events == [
        "start:prime",
        "start:prime.fine",
        "complete:prime.fine",
        "start:prime.bad",
        "complete:prime.bad",
        "complete:prime",
    ]
    # The completion the failure emitted carries the error, not a success.
    assert result.state["prime"]["bad"]["meta"]["error"]


def test_a_failing_loop_still_closes_its_pair(tmp_path: Path) -> None:
    """Same trap one container deeper: the loop node's pair must close when
    an iteration raises out of it."""
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [
            {
                "type": "loop",
                "name": "spin",
                "while": {"mode": "cel", "expr": "false"},
                "min_iterations": 2,
                "on_error": "fail",
                "body": [
                    {
                        "type": "prompt",
                        "name": "tick",
                        "template": "boom",
                        "on_error": "fail",
                    }
                ],
            }
        ],
    }
    result = _run(
        tmp_path, orch, adapter=ExplodingAdapter(), config=_lifecycle_cfg()
    )

    assert result.ok is False
    _assert_bracketed(_events(result.state))


def test_a_disabled_effect_opens_and_closes_immediately(tmp_path: Path) -> None:
    profile = tmp_path / "profiles" / "off.yml"
    profile.parent.mkdir(parents=True, exist_ok=True)
    profile.write_text(
        yaml.dump({"effects": {"greet": {"enabled": False}}}), encoding="utf-8"
    )
    orch = {
        "adapter": "echo",
        "model": "echo-1",
        "effects": [{"type": "prompt", "name": "greet", "template": "hi"}],
    }
    result = _run(tmp_path, orch, config=_lifecycle_cfg(), profile_name="off")

    assert result.ok is True, result.error
    events = _events(result.state)
    _assert_bracketed(events)
    assert events[1:3] == ["start:prime.greet", "complete:prime.greet"]
    assert result.state["prime"]["greet"]["meta"]["disabled"] is True


# -- back-compat ---------------------------------------------------------------


def test_a_complete_only_plugin_is_untouched(tmp_path: Path) -> None:
    """AC: a plugin implementing only ``on_effect_complete`` keeps working.

    ``RecordingPlugin`` has no ``on_effect_start``; the dispatch guard must
    skip it silently rather than raising or recording a failure event.
    """
    result = _run(
        tmp_path,
        MIXED,
        config=CircuitryConfig(plugins=["plugin_fixtures:make_recording_plugin"]),
    )

    assert result.ok is True, result.error
    paths = result.state["runtime"]["plugin_marker"]["effect_paths"]
    assert "prime.greet" in paths and "prime" in paths
    hook_failures = [
        e
        for e in result.state["runtime"]["plugins"]["events"]
        if not e["ok"] and e["hook"] in ("on_effect_start", "on_effect_complete")
    ]
    assert hook_failures == []


def test_a_start_only_plugin_works_too(tmp_path: Path) -> None:
    """The guard is symmetric: implementing only the start half is fine."""
    result = _run(
        tmp_path,
        MIXED,
        config=CircuitryConfig(plugins=["plugin_fixtures:make_start_only_plugin"]),
    )

    assert result.ok is True, result.error
    assert "prime.greet" in result.state["runtime"]["start_only_marker"]["effect_paths"]


def test_a_raising_start_hook_does_not_abort_the_run(tmp_path: Path) -> None:
    """Plugin hook failures stay non-fatal, exactly as for ``on_effect_complete``
    (which likewise logs and swallows rather than recording a run event)."""
    import sys
    import types

    mod = types.ModuleType("exploding_start_plugin_fixture")

    class ExplodingStartPlugin:
        name = "exploding-start"

        def on_run_start(self, *, state, context):
            pass

        def on_run_success(self, *, state, context):
            pass

        def on_run_failure(self, *, state, context, error):
            pass

        def on_effect_start(self, *, state, context, effect_path, effect_node):
            raise RuntimeError("effect_start exploded")

    mod.plugin = ExplodingStartPlugin()
    sys.modules["exploding_start_plugin_fixture"] = mod
    try:
        result = _run(
            tmp_path,
            MIXED,
            config=CircuitryConfig(plugins=["exploding_start_plugin_fixture"]),
        )
        assert result.ok is True, result.error
        assert result.state["prime"]["greet"]["value"] == "hello"
    finally:
        sys.modules.pop("exploding_start_plugin_fixture", None)


# -- the complexity score ------------------------------------------------------


def test_the_start_payload_is_the_live_node(tmp_path: Path) -> None:
    """AC: the complexity score reaches the hook when scoring is enabled.

    #103 writes ``meta.complexity`` in ``core/prompt.py`` *before dispatch* —
    the same window this hook fires in. What this asserts is the contract
    that makes that work: the payload is the effect's node itself, so any
    pre-dispatch meta rides along. Until the scorer lands there is no score
    to observe, and its absence is tolerated rather than fatal.
    """
    seen: list[tuple[str, dict[str, Any]]] = []
    store = Store({}, effect_start=lambda path, node: seen.append((path, node)))
    child = store.child("prime")
    node = child.ensure_dict("greet")
    node["meta"] = {"model": "echo-1", "complexity": {"score": 0.42}}
    child.fire_effect_start("greet", node)

    assert seen == [("prime.greet", node)]
    assert seen[0][1]["meta"]["complexity"] == {"score": 0.42}


def test_a_run_without_scoring_carries_no_score(tmp_path: Path) -> None:
    result = _run(tmp_path, MIXED, config=_lifecycle_cfg())

    assert result.ok is True, result.error
    assert "scores" not in result.state["runtime"]["lifecycle_marker"]


def test_a_store_without_the_callback_still_fires_nothing(tmp_path: Path) -> None:
    Store({}).fire_effect_start("greet", {})  # must not raise
