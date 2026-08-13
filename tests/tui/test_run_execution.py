"""Pilot tests for the live execution view, one per orchestration shape.

Every run here goes through the real ``runtime_shim.run`` with a scripted
fake adapter, so the tree these tests assert on was produced by the same
events a real run emits — state snapshots and effect-complete
notifications — not by a hand-written state.
"""

from __future__ import annotations

import asyncio
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

pytest.importorskip("textual")

from textual.pilot import Pilot
from textual.widgets import Button, Select, Static

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.tui.execution import DONE, FAILED, PENDING, RUNNING, ExecNode
from circuitry.tui.launch import OrchestrationChoice
from circuitry.tui.run_view import NO_TREE, RunScreen
from circuitry.tui.screens import VIEWS

RUN_SPEC = next(spec for spec in VIEWS if spec.slug == "run")

#: Token counts the fake adapter reports, so aggregates have something to add.
SENT, RECEIVED = 7, 13


@dataclass(frozen=True)
class ScriptedAdapter:
    """Echoes the prompt back, and blows up on one containing ``BOOM``."""

    name: str = "scripted"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        if "BOOM" in prompt:
            raise RuntimeError("scripted failure")
        return GenerateResult(
            text=prompt, raw={"model": model}, tokens_sent=SENT, tokens_received=RECEIVED
        )


@dataclass
class GateAdapter:
    """Holds the run open inside ``generate`` so mid-flight state is visible."""

    entered: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)
    name: str = "gate"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.entered.set()
        self.release.wait(timeout=10)
        return GenerateResult(
            text=prompt, raw={}, tokens_sent=SENT, tokens_received=RECEIVED
        )


# -- orchestrations under test ------------------------------------------------


def _prompt(name: str, template: str = "hello", **extra: Any) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": template, **extra}


CHAIN: dict[str, Any] = {
    "adapter": "scripted",
    "model": "scripted-1",
    "effects": [_prompt("draft"), _prompt("polish")],
}

TREE: dict[str, Any] = {
    "adapter": "scripted",
    "model": "scripted-1",
    "flow": "tree",
    "effects": [_prompt("left"), _prompt("right")],
}

EACH: dict[str, Any] = {
    "adapter": "scripted",
    "model": "scripted-1",
    "effects": [
        _prompt("items", '["a", "b"]', prompt_type="json", schema={"type": "array"}),
        {
            "type": "loop",
            "name": "over_items",
            "each": {"in": "prime.items.value", "as": "item"},
            "body": [_prompt("handle", "handle {{item}}")],
        },
    ],
}

WHILE: dict[str, Any] = {
    "adapter": "scripted",
    "model": "scripted-1",
    "effects": [
        {
            "type": "loop",
            "name": "spin",
            "while": {"mode": "cel", "expr": "false"},
            "min_iterations": 2,
            "max_iterations": 5,
            "body": [_prompt("tick")],
        }
    ],
}


def _conditional(expr: str) -> dict[str, Any]:
    return {
        "adapter": "scripted",
        "model": "scripted-1",
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "cel", "expr": expr},
                "then": [_prompt("yes")],
                "else": [_prompt("no")],
            }
        ],
    }


def _erroring(on_error: str) -> dict[str, Any]:
    return {
        "adapter": "scripted",
        "model": "scripted-1",
        "effects": [
            _prompt("flaky", "BOOM", on_error=on_error),
            _prompt("after"),
        ],
    }


# -- harness ------------------------------------------------------------------


def _write(tmp_path: Path, orch: dict[str, Any], name: str = "demo.yml") -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _screen(path: Path, **kwargs: Any) -> RunScreen:
    kwargs.setdefault("adapter", ScriptedAdapter())
    return RunScreen(
        RUN_SPEC,
        config=CircuitryConfig(),
        choices=[
            OrchestrationChoice(
                key=str(path), label=path.name, path=path, source="local"
            )
        ],
        **kwargs,
    )


async def _open(pilot: Pilot[Any], screen: RunScreen) -> RunScreen:
    await pilot.app.push_screen(screen)
    await pilot.pause()
    screen.query_one("#run-orchestration", Select).value = str(screen._choices[0].key)
    await pilot.pause()
    await pilot.pause()
    return screen


async def _settle(pilot: Pilot[Any], predicate: Any, timeout: float = 15.0) -> bool:
    deadline = asyncio.get_event_loop().time() + timeout
    while asyncio.get_event_loop().time() < deadline:
        if predicate():
            return True
        await pilot.pause()
        await asyncio.sleep(0.01)
    return predicate()


async def _finished(pilot: Pilot[Any], screen: RunScreen) -> RunScreen:
    """Launch and wait for the run to land, then let the last repaint through."""
    screen.query_one("#run-launch", Button).press()
    await _settle(pilot, lambda: screen.last_result is not None)
    await pilot.pause()
    return screen


def _tree_text(screen: RunScreen) -> str:
    return screen.tree_text


def _footer_text(screen: RunScreen) -> str:
    return screen.footer_text


def _flatten(nodes: tuple[ExecNode, ...]) -> list[ExecNode]:
    out: list[ExecNode] = []
    for node in nodes:
        out.append(node)
        out.extend(_flatten(node.children))
    return out


def _find(screen: RunScreen, label: str) -> ExecNode:
    matches = [node for node in _flatten(screen.execution_nodes) if node.label == label]
    assert matches, f"no {label!r} row in:\n{_tree_text(screen)}"
    return matches[0]


def _labels(screen: RunScreen) -> list[str]:
    return [node.label for node in _flatten(screen.execution_nodes)]


def _token_totals(state: Any) -> tuple[int, int]:
    """Independent walk of a final state, for checking the footer's sums."""
    sent = received = 0
    if isinstance(state, dict):
        meta = state.get("meta")
        if isinstance(meta, dict):
            for key, add in (("tokens_sent", 0), ("tokens_received", 1)):
                value = meta.get(key)
                if isinstance(value, int) and not isinstance(value, bool):
                    if add == 0:
                        sent += value
                    else:
                        received += value
        for key, value in state.items():
            if key != "meta":
                child_sent, child_received = _token_totals(value)
                sent += child_sent
                received += child_received
    elif isinstance(state, list):
        for item in state:
            child_sent, child_received = _token_totals(item)
            sent += child_sent
            received += child_received
    return sent, received


# -- the shapes ---------------------------------------------------------------


def test_the_plan_is_drawn_before_anything_runs(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, EACH)))
        return _tree_text(screen), _footer_text(screen)

    tree, footer = run_app(scenario)
    assert "· items" in tree
    assert "· over_items each" in tree
    assert "· handle" in tree  # the body, previewed before the first iteration
    assert footer == "↑0 ↓0 tok  ·  —  ·  0/3 effects"


def test_a_chain_run_ends_with_every_effect_done(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, CHAIN)))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    assert _labels(screen) == ["draft", "polish"]
    assert [node.status for node in screen.execution_nodes] == [DONE, DONE]
    assert all(node.elapsed is not None for node in screen.execution_nodes)
    assert screen.totals.done == screen.totals.total == 2


def test_a_tree_run_renders_its_parallel_siblings(run_app: Any, tmp_path: Path) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, TREE)))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    assert _labels(screen) == ["left", "right"]
    assert [node.status for node in screen.execution_nodes] == [DONE, DONE]


def test_each_loop_iterations_appear_as_the_run_goes(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, EACH)))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    loop = _find(screen, "over_items")
    assert loop.status == DONE
    assert loop.detail == "each"
    assert [child.label for child in loop.children] == ["iter 0", "iter 1"]
    assert all(
        child.children[0].status == DONE and child.children[0].label == "handle"
        for child in loop.children
    )
    # Three prompts ran: the collection plus one per item.
    assert screen.totals.tokens_sent == 3 * SENT


def test_a_while_loop_records_the_iterations_it_took(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, WHILE)))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    loop = _find(screen, "spin")
    assert loop.detail == "while"
    assert [child.label for child in loop.children] == ["iter 0", "iter 1"]
    assert loop.status == DONE


@pytest.mark.parametrize(
    ("expr", "taken", "skipped"), [("true", "yes", "no"), ("false", "no", "yes")]
)
def test_a_conditional_shows_only_the_branch_it_took(
    run_app: Any, tmp_path: Path, expr: str, taken: str, skipped: str
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, _conditional(expr))))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    gate = _find(screen, "gate")
    branch = "then" if taken == "yes" else "else"
    assert gate.detail == f"→ {branch}"
    assert [child.label for child in gate.children] == [branch]
    assert taken in _labels(screen)
    assert skipped not in _labels(screen)


@pytest.mark.parametrize("policy", ["continue", "skip"])
def test_a_handled_error_is_shown_inline_and_the_run_carries_on(
    run_app: Any, tmp_path: Path, policy: str
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, _erroring(policy))))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    assert screen.last_result is not None and screen.last_result.ok
    flaky = _find(screen, "flaky")
    assert flaky.status == FAILED
    assert flaky.on_error == policy
    assert f"[on_error: {policy}]" in _tree_text(screen)
    assert "scripted failure" in _tree_text(screen)
    # The next effect still ran, which is the whole point of the policy.
    assert _find(screen, "after").status == DONE


def test_an_unhandled_error_stops_the_run_where_it_broke(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, _erroring("fail"))))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    assert screen.last_result is not None and not screen.last_result.ok
    assert _find(screen, "flaky").status == FAILED
    assert _find(screen, "after").status == PENDING
    assert screen.totals.done < screen.totals.total


# -- aggregates and responsiveness -------------------------------------------


@pytest.mark.parametrize(
    "orch", [CHAIN, TREE, EACH, WHILE], ids=["chain", "tree", "each", "while"]
)
def test_the_footer_totals_match_the_final_state(
    run_app: Any, tmp_path: Path, orch: dict[str, Any]
) -> None:
    async def scenario(pilot: Pilot[Any]) -> RunScreen:
        screen = await _open(pilot, _screen(_write(tmp_path, orch)))
        return await _finished(pilot, screen)

    screen = run_app(scenario)
    assert screen.last_result is not None
    sent, received = _token_totals(screen.last_result.state)
    assert (screen.totals.tokens_sent, screen.totals.tokens_received) == (sent, received)
    assert screen.totals.elapsed is not None
    assert screen.totals.done == screen.totals.total
    assert _footer_text(screen).startswith(f"↑{sent} ↓{received} tok")


def test_the_view_is_live_and_input_still_lands_mid_run(
    run_app: Any, tmp_path: Path
) -> None:
    """A held-open run shows work in flight, and the UI keeps listening."""
    gate = GateAdapter()

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, CHAIN), adapter=gate))
        screen.query_one("#run-launch", Button).press()
        await _settle(pilot, gate.entered.is_set)
        await _settle(pilot, lambda: RUNNING in {n.status for n in _flatten(screen.execution_nodes)})
        mid = _tree_text(screen)

        # Input processed between run events: cancel arrives while the
        # worker is still parked inside the adapter.
        await pilot.press("ctrl+x")
        await pilot.pause()
        cancelling = screen.status_text
        gate.release.set()
        await _settle(pilot, lambda: screen.last_result is not None)
        await pilot.pause()
        return mid, cancelling, _tree_text(screen)

    mid, cancelling, final = run_app(scenario)
    assert "◐ draft" in mid
    assert "· polish" in mid
    assert cancelling.startswith("Cancelling")
    # Cancelling unwinds the run, so the tree stops where it stopped.
    assert "polish" in final


def test_navigating_away_mid_run_does_not_break_anything(
    run_app: Any, tmp_path: Path
) -> None:
    """The run outlives the screen; the repaint must not chase the widgets."""
    gate = GateAdapter()

    async def scenario(pilot: Pilot[Any]) -> tuple[Any, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, CHAIN), adapter=gate))
        screen.query_one("#run-launch", Button).press()
        await _settle(pilot, gate.entered.is_set)
        await pilot.app.pop_screen()
        await pilot.pause()
        gate.release.set()
        session = screen._session
        assert session is not None
        await _settle(pilot, lambda: session.result is not None)
        # Whatever the timer does now, it must not take the app with it.
        await pilot.pause()
        await pilot.press("q")
        await pilot.pause()
        return session.result, screen.tree_text

    result, tree = run_app(scenario)
    assert result is not None and result.ok
    assert "draft" in tree


def test_picking_another_orchestration_redraws_the_tree(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        first = _write(tmp_path, CHAIN, "chain.yml")
        second = _write(tmp_path, WHILE, "while.yml")
        screen = RunScreen(
            RUN_SPEC,
            config=CircuitryConfig(),
            adapter=ScriptedAdapter(),
            choices=[
                OrchestrationChoice(
                    key=str(path), label=path.name, path=path, source="local"
                )
                for path in (first, second)
            ],
        )
        await pilot.app.push_screen(screen)
        await pilot.pause()
        select = screen.query_one("#run-orchestration", Select)
        select.value = str(first)
        await pilot.pause()
        await pilot.pause()
        chain_tree = _tree_text(screen)
        select.value = str(second)
        await pilot.pause()
        await pilot.pause()
        return chain_tree, _tree_text(screen)

    chain_tree, while_tree = run_app(scenario)
    assert "draft" in chain_tree and "spin" not in chain_tree
    assert "spin while" in while_tree and "draft" not in while_tree


# -- snapshots ----------------------------------------------------------------

T0 = "2026-01-01T00:00:00+00:00"
T1 = "2026-01-01T00:00:01.500000+00:00"

RUNNING_STATE: dict[str, Any] = {
    "prime": {
        "items": {
            "value": ["a", "b"],
            "meta": {
                "created_at": T0,
                "completed_at": T1,
                "tokens_sent": 7,
                "tokens_received": 13,
            },
        },
        "over_items": {
            "meta": {"created_at": T1, "mode": "each"},
            "iter_0": {
                "handle": {
                    "meta": {
                        "created_at": T0,
                        "completed_at": T1,
                        "tokens_sent": 7,
                        "tokens_received": 13,
                    }
                }
            },
            "iter_1": {"handle": {"meta": {"created_at": T1}}},
        },
    }
}

COMPLETED_STATE: dict[str, Any] = {
    "prime": {
        "items": RUNNING_STATE["prime"]["items"],
        "over_items": {
            "meta": {"created_at": T0, "completed_at": T1, "mode": "each"},
            "iter_0": RUNNING_STATE["prime"]["over_items"]["iter_0"],
            "iter_1": {
                "handle": {
                    "meta": {
                        "created_at": T0,
                        "completed_at": T1,
                        "error": "scripted failure",
                        "tokens_sent": 7,
                    }
                }
            },
        },
    }
}


@pytest.mark.parametrize(
    ("name", "state", "elapsed"),
    [
        ("run-executing-100x30", RUNNING_STATE, 4.2),
        ("run-completed-100x30", COMPLETED_STATE, 9.5),
    ],
)
def test_execution_view_snapshots(
    run_app: Any,
    capture_frame: Any,
    snapshot: Any,
    tmp_path: Path,
    name: str,
    state: dict[str, Any],
    elapsed: float,
) -> None:
    """Scripted states, so the frame is the same every time it is drawn."""
    orch = dict(EACH)
    orch["effects"] = [
        _prompt("items", '["a", "b"]', prompt_type="json", schema={"type": "array"}),
        {
            "type": "loop",
            "name": "over_items",
            "each": {"in": "prime.items.value", "as": "item"},
            "body": [_prompt("handle", "handle {{item}}", on_error="continue")],
        },
    ]

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(_write(tmp_path, orch)))
        screen.show_state(state, elapsed=elapsed)
        await pilot.pause()
        return str(capture_frame(pilot.app))

    snapshot.assert_match(run_app(scenario, size=(100, 30)), name)


def test_the_form_and_the_tree_share_the_screen(run_app: Any, tmp_path: Path) -> None:
    """Regression guard: the tree must not push the launch controls away."""

    async def scenario(pilot: Pilot[Any]) -> tuple[bool, bool]:
        screen = await _open(pilot, _screen(_write(tmp_path, CHAIN)))
        return (
            screen.query_one("#run-launch", Button) is not None,
            isinstance(screen.query_one("#run-tree", Static), Static),
        )

    assert run_app(scenario) == (True, True)


def test_an_unpicked_screen_says_where_the_tree_will_be(
    run_app: Any, tmp_path: Path
) -> None:
    async def scenario(pilot: Pilot[Any]) -> str:
        screen = _screen(_write(tmp_path, CHAIN))
        await pilot.app.push_screen(screen)
        await pilot.pause()
        return _tree_text(screen)

    assert run_app(scenario) == NO_TREE
