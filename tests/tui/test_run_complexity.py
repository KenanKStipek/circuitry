"""Complexity scores in the TUI: the column, the breakdown, the absences.

Two halves. The first is the model — reading ``meta.complexity``, sizing
the column, ranking the signals — exercised without Textual, because those
are rules and rules are cheaper to argue with directly. The second drives
a live Run view to prove the thing the story is actually about: the score
is on the row *while the effect is running*, because it rides
``on_effect_start`` rather than ``on_effect_complete``.

The scorer itself is not under test here (``tests/core/`` owns it); what
is under test is the view's behaviour when a score is present, partial, or
absent — the last being the common case, since scoring is off by default.
"""

from __future__ import annotations

import asyncio
import threading
from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.tui import execution as ex
from circuitry.tui.complexity import (
    DEFAULT_BANDS,
    MAX_SCORE,
    NO_SCORE,
    band_for,
)
from circuitry.tui.complexity import read as read_complexity
from circuitry.tui.inspector import EffectMeta, build_state_nodes, detail_lines, find_node

T0 = "2026-01-01T00:00:00+00:00"
T1 = "2026-01-01T00:00:01.500000+00:00"

#: A full payload, shaped like ``ComplexityScore.to_dict()``. Contributions
#: sum to the score, which is the scorer's own contract.
FULL_SCORE: dict[str, Any] = {
    "score": 62.0,
    "max_score": 100.0,
    "mode": "rendered",
    "estimated": False,
    "weight_total": 1.0,
    "signals": [
        {
            "name": "prompt_size",
            "raw": 900.0,
            "normalized": 0.529,
            "weight": 0.28,
            "contribution": 40.0,
            "estimated": False,
            "note": "~900 tokens of rendered prompt",
        },
        {
            "name": "schema_shape",
            "raw": 6.0,
            "normalized": 0.45,
            "weight": 0.16,
            "contribution": 15.0,
            "estimated": False,
            "note": "schema depth 2, 6 field(s)",
        },
        {
            "name": "keywords",
            "raw": 2.0,
            "normalized": 0.30,
            "weight": 0.06,
            "contribution": 7.0,
            "estimated": False,
            "note": "matched analyze, compare",
        },
    ],
    "warnings": [],
}

CHAIN: dict[str, Any] = {
    "effects": [
        {"type": "prompt", "name": "draft"},
        {"type": "prompt", "name": "polish"},
        {"type": "tool", "name": "save"},
    ]
}


def _plan() -> tuple[ex.PlanNode, ...]:
    return ex.plan_from_orchestration(CHAIN)


def _state(**overrides: Any) -> dict[str, Any]:
    """A snapshot with ``draft`` in flight and nothing else touched yet."""
    node: dict[str, Any] = {"meta": {"created_at": T0, **overrides}}
    return {"prime": {"meta": {"created_at": T0, "flow": "chain"}, "draft": node}}


# -- reading the payload ------------------------------------------------------


def test_a_full_payload_reads_into_a_score_and_a_band() -> None:
    score = read_complexity({"complexity": FULL_SCORE})
    assert score is not None
    assert score.score == 62.0
    assert score.max_score == 100.0
    assert score.mode == "rendered"
    assert score.band == "high"  # 62/100 falls in the third default band
    assert [signal.name for signal in score.signals] == [
        "prompt_size",
        "schema_shape",
        "keywords",
    ]


def test_a_partial_payload_is_still_a_score() -> None:
    """What ``tests/core/test_on_effect_start.py`` puts on a node."""
    score = read_complexity({"complexity": {"score": 0.42}})
    assert score is not None
    assert score.score == 0.42
    assert score.max_score == MAX_SCORE
    assert score.signals == ()
    assert score.band == DEFAULT_BANDS[0][1]


def test_a_bare_number_is_accepted() -> None:
    score = read_complexity({"complexity": 41})
    assert score is not None and score.score == 41.0


@pytest.mark.parametrize(
    "meta",
    [
        None,
        {},
        {"model": "gpt-4o"},
        {"complexity": None},
        {"complexity": "high"},
        {"complexity": []},
        {"complexity": {}},
        {"complexity": {"signals": []}},
        {"complexity": {"score": "high"}},
        {"complexity": {"score": None}},
        {"complexity": {"score": True}},
    ],
    ids=[
        "no-meta",
        "empty-meta",
        "unscored-effect",
        "null-entry",
        "string-entry",
        "list-entry",
        "empty-mapping",
        "no-score-key",
        "string-score",
        "null-score",
        "boolean-score",
    ],
)
def test_anything_without_a_usable_number_reads_as_no_score(meta: Any) -> None:
    """One case for the caller to handle: a score, or no score. Never a raise."""
    assert read_complexity(meta) is None


def test_a_score_is_clamped_into_its_declared_range() -> None:
    assert read_complexity({"complexity": {"score": 140.0}}).score == 100.0  # type: ignore[union-attr]
    assert read_complexity({"complexity": {"score": -3.0}}).score == 0.0  # type: ignore[union-attr]
    scaled = read_complexity({"complexity": {"score": 7.0, "max_score": 10.0}})
    assert scaled is not None and scaled.score == 7.0 and scaled.band == "high"


def test_a_junk_max_score_falls_back_to_the_documented_range() -> None:
    score = read_complexity({"complexity": {"score": 62.0, "max_score": 0}})
    assert score is not None and score.max_score == MAX_SCORE


def test_a_junk_signal_entry_is_dropped_not_fatal() -> None:
    score = read_complexity(
        {"complexity": {"score": 10.0, "signals": ["nonsense", {}, {"name": "keywords"}]}}
    )
    assert score is not None
    assert [signal.name for signal in score.signals] == ["keywords"]


# -- bands --------------------------------------------------------------------


@pytest.mark.parametrize(
    ("score", "band"),
    [(0.0, "low"), (25.0, "low"), (25.1, "moderate"), (50.0, "moderate"),
     (62.0, "high"), (75.0, "high"), (75.1, "severe"), (100.0, "severe")],
)
def test_the_default_band_table_covers_the_whole_range(score: float, band: str) -> None:
    assert band_for(score) == band


def test_a_routed_band_wins_over_the_default_table() -> None:
    """The router named it; the TUI must not contradict the router."""
    named = read_complexity({"complexity": {"score": 62.0, "band": "needs-the-big-model"}})
    assert named is not None and named.band == "needs-the-big-model"


def test_a_band_object_carries_its_model_through() -> None:
    score = read_complexity(
        {"complexity": {"score": 62.0, "band": {"name": "hard", "model": "gpt-4o"}}}
    )
    assert score is not None
    assert (score.band, score.model) == ("hard", "gpt-4o")
    assert "gpt-4o" in score.summary


def test_a_degenerate_range_still_names_a_band() -> None:
    assert band_for(5.0, max_score=0.0) == DEFAULT_BANDS[0][1]


# -- which signals dominated --------------------------------------------------


def test_the_dominant_signals_are_the_shortest_set_explaining_the_score() -> None:
    score = read_complexity({"complexity": FULL_SCORE})
    assert score is not None
    # 40 of 62 points is already past half, so one signal is the answer.
    assert [signal.name for signal in score.dominant] == ["prompt_size"]
    assert [signal.name for signal in score.ranked] == [
        "prompt_size",
        "schema_shape",
        "keywords",
    ]


def test_an_evenly_spread_score_names_more_than_one_signal() -> None:
    even = {
        "score": 30.0,
        "signals": [
            {"name": "a", "contribution": 10.0},
            {"name": "b", "contribution": 10.0},
            {"name": "c", "contribution": 10.0},
        ],
    }
    score = read_complexity({"complexity": even})
    assert score is not None
    assert [signal.name for signal in score.dominant] == ["a", "b"]


def test_a_score_nothing_contributed_to_has_no_dominant_signal() -> None:
    score = read_complexity(
        {"complexity": {"score": 0.0, "signals": [{"name": "keywords", "contribution": 0.0}]}}
    )
    assert score is not None and score.dominant == ()
    assert "dominated by" not in "\n".join(score.breakdown_lines())


def test_the_breakdown_marks_the_dominant_signals_and_keeps_the_notes() -> None:
    score = read_complexity({"complexity": FULL_SCORE})
    assert score is not None
    lines = score.breakdown_lines()
    assert lines[0].startswith("▸") and "prompt_size" in lines[0]
    assert lines[1].startswith("  ") and "schema_shape" in lines[1]
    body = "\n".join(lines)
    assert "~900 tokens of rendered prompt" in body
    assert "65%" in body  # 40 of 62 contributed points
    assert body.endswith("dominated by prompt_size")


def test_a_score_with_no_signals_says_so_rather_than_rendering_empty() -> None:
    score = read_complexity({"complexity": {"score": 12.0}})
    assert score is not None
    assert score.breakdown_lines() == ["no signal breakdown recorded"]


def test_warnings_ride_along_under_the_breakdown() -> None:
    score = read_complexity(
        {
            "complexity": {
                "score": 12.0,
                "signals": [{"name": "keywords", "contribution": 12.0}],
                "warnings": ["prompt_type: unknown type 'blob'; treated as 'text'."],
            }
        }
    )
    assert score is not None
    assert score.breakdown_lines()[-1].startswith("! prompt_type:")


def test_an_estimated_signal_is_marked_in_its_row() -> None:
    score = read_complexity(
        {
            "complexity": {
                "score": 12.0,
                "estimated": True,
                "mode": "static",
                "signals": [
                    {"name": "prompt_size", "contribution": 12.0, "estimated": True}
                ],
            }
        }
    )
    assert score is not None
    assert "prompt_size~" in score.breakdown_lines()[0]
    assert "estimated" in score.summary


# -- the column ---------------------------------------------------------------


def test_a_start_event_puts_a_score_on_a_still_running_row() -> None:
    """The point of the start hook: scored before it finishes, not after."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    draft = nodes[0]
    assert draft.status == ex.RUNNING
    assert draft.complexity is not None and draft.complexity.score == 62.0
    assert draft.score_cell() == " 62 high"
    assert " 62 high  ├─ ◐ draft" in ex.render_text(nodes)


def test_the_start_event_wins_over_the_snapshot() -> None:
    """Both can carry a score; the newer one is the one just dispatched."""
    stale = _state(complexity={"score": 5.0})
    nodes = ex.build_tree(_plan(), stale, scores={"prime.draft": FULL_SCORE})
    assert nodes[0].complexity is not None and nodes[0].complexity.score == 62.0


def test_the_snapshot_is_read_when_no_start_event_arrived() -> None:
    """A finished run reopened from ``--out`` has only what state recorded."""
    nodes = ex.build_tree(_plan(), _state(complexity=FULL_SCORE))
    assert nodes[0].complexity is not None and nodes[0].complexity.score == 62.0


def test_an_unusable_start_payload_falls_back_to_the_snapshot() -> None:
    nodes = ex.build_tree(
        _plan(), _state(complexity=FULL_SCORE), scores={"prime.draft": "nonsense"}
    )
    assert nodes[0].complexity is not None and nodes[0].complexity.score == 62.0


def test_effects_with_no_score_render_a_dash_or_nothing() -> None:
    """No blanks, no ``None`` — and no dashes down the rows that cannot score."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    cells = {node.label: node.score_cell() for node in _walk(nodes)}
    assert cells == {"draft": " 62 high", "polish": NO_SCORE.rjust(3), "save": ""}
    text = ex.render_text(nodes)
    assert "None" not in text
    for line in text.split("\n"):
        assert line.strip(), "no row may render blank"


def test_containers_and_groups_never_claim_a_score() -> None:
    plan = ex.plan_from_orchestration(
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "over_items",
                    "each": {"in": "prime.items.value", "as": "item"},
                    "body": [{"type": "prompt", "name": "handle"}],
                }
            ]
        }
    )
    state = {
        "prime": {
            "meta": {"created_at": T0},
            "over_items": {
                "meta": {"created_at": T0, "mode": "each"},
                "iter_0": {"handle": {"meta": {"created_at": T0, "completed_at": T1}}},
            },
        }
    }
    nodes = ex.build_tree(plan, state, scores={"prime.over_items.iter_0.handle": FULL_SCORE})
    cells = {node.kind: node.score_cell() for node in _walk(nodes)}
    assert cells["loop"] == "" and cells["iteration"] == ""
    assert cells["prompt"] == " 62 high"


def test_an_unscored_run_draws_no_column_at_all() -> None:
    """Scoring is off by default; the tree must look exactly as it did."""
    nodes = ex.build_tree(_plan(), _state())
    assert ex.score_column_width(nodes) == 0
    assert ex.render_text(nodes).split("\n")[0] == "├─ ◐ draft"


def test_the_column_is_only_as_wide_as_its_widest_cell() -> None:
    narrow = ex.build_tree(_plan(), _state(), scores={"prime.draft": {"score": 10.0}})
    wide = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    assert ex.score_column_width(narrow) == len(" 10 low") + ex.SCORE_GAP
    assert ex.score_column_width(wide) == len(" 62 high") + ex.SCORE_GAP


def test_an_error_row_does_not_repeat_its_effects_score() -> None:
    state = {
        "prime": {
            "meta": {"created_at": T0},
            "draft": {"meta": {"created_at": T0, "completed_at": T1, "error": "boom"}},
        }
    }
    lines = ex.render_lines(ex.build_tree(_plan(), state, scores={"prime.draft": FULL_SCORE}))
    error_line = next(line for line in lines if "boom" in line.text)
    assert error_line.gutter.strip() == ""
    assert len(error_line.gutter) == ex.score_column_width(
        ex.build_tree(_plan(), state, scores={"prime.draft": FULL_SCORE})
    )


# -- narrow terminals ---------------------------------------------------------


def test_the_column_gives_up_its_band_before_it_gives_up_the_number() -> None:
    """Squeezing the column is fair; squeezing the tree beside it is not."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    full = ex.score_column_width(nodes)
    terse = ex.score_column(nodes, width=full + ex.MIN_TREE_WIDTH - 1)

    assert terse.width == len(" 62") + ex.SCORE_GAP
    assert terse.bands is False
    text = ex.render_text(nodes, width=full + ex.MIN_TREE_WIDTH - 1)
    assert " 62  ├─ ◐ draft" in text
    assert "high" not in text


def test_a_tree_with_no_room_at_all_drops_the_column_not_the_rows() -> None:
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    roomy = ex.render_text(nodes, width=ex.MIN_TREE_WIDTH + ex.SCORE_WIDTH + ex.SCORE_GAP)
    tight = ex.render_text(nodes, width=ex.MIN_TREE_WIDTH + ex.SCORE_WIDTH + ex.SCORE_GAP - 1)

    assert " 62" in roomy
    assert "62" not in tight
    # Every row survives the suppression, at its full width.
    assert tight == ex.render_text(ex.build_tree(_plan(), _state()))
    assert [line.split("─ ")[-1] for line in tight.split("\n")] == [
        "◐ draft",
        "· polish",
        "· save",
    ]


def test_the_tree_never_gets_less_than_the_minimum() -> None:
    """Whatever the column decides, the rows keep their cells."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    for width in range(0, 80):
        assert width - ex.score_column_width(nodes, width) >= ex.MIN_TREE_WIDTH or (
            ex.score_column_width(nodes, width) == 0
        )


def test_an_unknown_width_keeps_the_column() -> None:
    """A pane that has not been laid out has not said there is no room."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    assert "62 high" in ex.render_text(nodes, width=None)


def test_the_column_survives_an_eighty_column_terminal() -> None:
    """The pane is ~2/5 of the screen; the feature has to fit in that."""
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    assert ex.score_column_width(nodes, width=32) > 0


@pytest.mark.parametrize("width", [0, 1, 4, 10, 24, 40, 120])
def test_no_width_makes_the_renderer_raise(width: int) -> None:
    nodes = ex.build_tree(_plan(), _state(), scores={"prime.draft": FULL_SCORE})
    assert ex.render_text(nodes, width=width)


# -- the inspector ------------------------------------------------------------


def _scored_state() -> dict[str, Any]:
    return {
        "prime": {
            "meta": {"created_at": T0, "completed_at": T1},
            "draft": {
                "value": "a draft",
                "meta": {
                    "adapter": "openai",
                    "model": "gpt-4o-mini",
                    "created_at": T0,
                    "completed_at": T1,
                    "complexity": FULL_SCORE,
                },
            },
            "save": {"value": "ok", "meta": {"created_at": T0, "completed_at": T1}},
        }
    }


def test_the_meta_panel_reads_the_score_in_one_line() -> None:
    meta = EffectMeta.from_mapping(_scored_state()["prime"]["draft"]["meta"])
    assert meta.complexity is not None
    assert dict(meta.rows())["complexity"] == "62.0/100  high  (rendered)"


def test_selecting_an_effect_shows_its_signal_breakdown() -> None:
    nodes = build_state_nodes(_scored_state())
    lines = detail_lines(find_node(nodes, "prime.draft"))
    body = "\n".join(lines)
    assert "complexity signals" in body
    assert "prompt_size" in body and "schema_shape" in body and "keywords" in body
    assert "dominated by prompt_size" in body
    # The breakdown sits between the meta panel and the value, not after it.
    assert lines.index("complexity signals") < lines.index("value")


def test_a_nested_value_inherits_its_effects_breakdown() -> None:
    """``…draft.value`` is explained by the effect it hangs under."""
    nodes = build_state_nodes(_scored_state())
    assert "dominated by prompt_size" in "\n".join(
        detail_lines(find_node(nodes, "prime.draft.value"))
    )


def test_an_unscored_effect_shows_no_breakdown_section() -> None:
    nodes = build_state_nodes(_scored_state())
    body = "\n".join(detail_lines(find_node(nodes, "prime.save")))
    assert "complexity" not in body
    assert "adapter" in body  # the rest of the meta panel is untouched


# -- the live view ------------------------------------------------------------

pytest.importorskip("textual")

from textual.pilot import Pilot  # noqa: E402
from textual.widgets import Button, Select  # noqa: E402

from circuitry.cli.config import CircuitryConfig  # noqa: E402
from circuitry.cli.runtime_shim import RunRequest, RunResult  # noqa: E402
from circuitry.tui.launch import OrchestrationChoice, RunSession  # noqa: E402
from circuitry.tui.run_view import RunScreen  # noqa: E402
from circuitry.tui.screens import VIEWS  # noqa: E402

RUN_SPEC = next(spec for spec in VIEWS if spec.slug == "run")

LIVE: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "effects": [
        {"type": "prompt", "name": "draft", "template": "hello"},
        {"type": "prompt", "name": "polish", "template": "again"},
    ],
}


@dataclass
class HeldRun:
    """A runner that emits the lifecycle a real run emits, then waits.

    Stands in for the runtime so the moment between "``draft`` started" and
    "``draft`` finished" can be inspected at leisure. The score rides the
    start hook exactly as :mod:`circuitry.core.prompt` sends it: written
    onto ``meta`` before dispatch, on the effect's own live node.
    """

    score: Any = None
    started: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)

    def __call__(self, request: RunRequest) -> RunResult:
        meta: dict[str, Any] = {"created_at": T0, "adapter": "echo", "model": "echo-1"}
        if self.score is not None:
            meta["complexity"] = self.score
        node: dict[str, Any] = {"value": None, "meta": meta}
        state: dict[str, Any] = {
            "prime": {"meta": {"created_at": T0, "flow": "chain"}, "draft": node}
        }
        assert request.state_observer is not None
        assert request.effect_start_observer is not None
        assert request.effect_observer is not None

        request.state_observer(state)
        request.effect_start_observer("prime.draft", node)
        self.started.set()
        self.release.wait(timeout=10)

        meta["completed_at"] = T1
        node["value"] = "hello"
        request.effect_observer("prime.draft", node)
        request.state_observer(state)
        return RunResult(ok=True, state=deepcopy(state), warnings=[])


def _write(tmp_path: Path, orch: dict[str, Any]) -> Path:
    path = tmp_path / "demo.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _screen(path: Path, runner: Any) -> RunScreen:
    return RunScreen(
        RUN_SPEC,
        config=CircuitryConfig(),
        choices=[
            OrchestrationChoice(key=str(path), label=path.name, path=path, source="local")
        ],
        runner=runner,
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


def _walk(nodes: Any) -> list[Any]:
    out: list[Any] = []
    for node in nodes:
        out.append(node)
        out.extend(_walk(node.children))
    return out


def _row(screen: RunScreen, label: str) -> Any:
    return next(node for node in _walk(screen.execution_nodes) if node.label == label)


def test_the_score_appears_while_the_effect_is_still_running(
    run_app: Any, tmp_path: Path
) -> None:
    """The acceptance criterion, stated as a race the view has to win.

    ``draft`` is held mid-flight. If the column were fed by
    ``on_effect_complete`` there would be nothing to see here — the score
    would arrive with the result it is supposed to precede.
    """
    runner = HeldRun(score=FULL_SCORE)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, LIVE), runner))
        screen.query_one("#run-launch", Button).press()
        assert await _settle(pilot, lambda: runner.started.is_set())
        assert await _settle(pilot, lambda: "62 high" in screen.tree_text)
        mid_tree, mid_status = screen.tree_text, _row(screen, "draft").status
        runner.release.set()
        assert await _settle(pilot, lambda: screen.last_result is not None)
        await pilot.pause()
        return mid_tree, mid_status, screen.tree_text

    mid_tree, mid_status, final_tree = run_app(scenario, size=(100, 30))
    assert mid_status == ex.RUNNING, mid_tree
    assert "62 high" in mid_tree
    assert "◐ draft" in mid_tree
    # And it stays put once the effect lands.
    assert "62 high" in final_tree


def test_a_run_with_no_scores_draws_the_tree_it_always_drew(
    run_app: Any, tmp_path: Path
) -> None:
    """Scoring off: no column, no dashes, no ``None`` in the pane."""
    runner = HeldRun(score=None)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(_write(tmp_path, LIVE), runner))
        screen.query_one("#run-launch", Button).press()
        assert await _settle(pilot, lambda: runner.started.is_set())
        runner.release.set()
        assert await _settle(pilot, lambda: screen.last_result is not None)
        await pilot.pause()
        return screen.tree_text

    tree = run_app(scenario, size=(100, 30))
    assert "None" not in tree and NO_SCORE not in tree
    assert tree.split("\n")[0].startswith("├─ ")


def test_an_eighty_column_terminal_keeps_the_number(
    run_app: Any, tmp_path: Path
) -> None:
    """80x24 is the ordinary terminal; the feature has to survive it.

    The execution pane is a fraction of the screen, so there is no room for
    the band there — the number stays, which is the part that ranks.
    """
    runner = HeldRun(score=FULL_SCORE)

    async def scenario(pilot: Pilot[Any]) -> str:
        screen = await _open(pilot, _screen(_write(tmp_path, LIVE), runner))
        screen.query_one("#run-launch", Button).press()
        assert await _settle(pilot, lambda: runner.started.is_set())
        assert await _settle(pilot, lambda: "62" in screen.tree_text)
        tree = screen.tree_text
        runner.release.set()
        assert await _settle(pilot, lambda: screen.last_result is not None)
        return tree

    tree = run_app(scenario, size=(80, 24))
    assert " 62  ├─ ◐ draft" in tree
    assert "high" not in tree


def test_a_narrow_terminal_drops_the_column_not_the_effect_names(
    run_app: Any, tmp_path: Path
) -> None:
    """Resized while the run is held open, so the repaint timer is live."""
    runner = HeldRun(score=FULL_SCORE)

    async def scenario(pilot: Pilot[Any]) -> tuple[str, str]:
        screen = await _open(pilot, _screen(_write(tmp_path, LIVE), runner))
        screen.query_one("#run-launch", Button).press()
        assert await _settle(pilot, lambda: runner.started.is_set())
        assert await _settle(pilot, lambda: "62 high" in screen.tree_text)
        roomy = screen.tree_text
        await pilot.resize_terminal(30, 12)
        await pilot.pause()
        assert await _settle(pilot, lambda: "62 high" not in screen.tree_text)
        tight = screen.tree_text
        runner.release.set()
        assert await _settle(pilot, lambda: screen.last_result is not None)
        return roomy, tight

    roomy, tight = run_app(scenario, size=(100, 30))
    assert "62 high" in roomy
    assert "62 high" not in tight
    assert "draft" in tight and "polish" in tight


def test_the_session_wires_the_start_hook_into_the_request(tmp_path: Path) -> None:
    """``RunSession`` must hand the runtime an ``effect_start_observer``.

    Without it the score has no route out of the runtime at all, whatever
    the view does with it.
    """
    seen: list[tuple[str, dict[str, Any]]] = []
    runner = HeldRun(score=FULL_SCORE)
    runner.release.set()
    session = RunSession(
        RunRequest(
            orchestration_path=_write(tmp_path, LIVE),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
        ),
        on_effect_start=lambda path, node: seen.append((path, node)),
        runner=runner,
    )
    assert session.request.effect_start_observer is not None
    session.start()
    session.join(timeout=10)

    assert [path for path, _ in seen] == ["prime.draft"]
    # A private copy, and one that predates the effect's completion.
    assert seen[0][1]["meta"]["complexity"] == FULL_SCORE
    assert seen[0][1]["meta"].get("completed_at") is None


def test_the_start_hook_fires_before_the_completion_hook(tmp_path: Path) -> None:
    """Ordering, through the real ``runtime_shim.run``."""

    @dataclass(frozen=True)
    class EchoAdapter:
        name: str = "echo"

        def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> Any:
            from circuitry.adapters.base import GenerateResult

            return GenerateResult(text=prompt, raw={"model": model})

    events: list[tuple[str, str]] = []
    session = RunSession(
        RunRequest(
            orchestration_path=_write(tmp_path, LIVE),
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            adapter=EchoAdapter(),
        ),
        on_effect_start=lambda path, _: events.append(("start", path)),
        on_effect=lambda path, _: events.append(("complete", path)),
    )
    session.start()
    session.join(timeout=30)

    assert session.result is not None and session.result.ok, session.result
    # ``prime`` is the root dynamic wrapping the orchestration; the two
    # prompts are what the view draws rows for.
    assert [event for event in events if event[1] != "prime"] == [
        ("start", "prime.draft"),
        ("complete", "prime.draft"),
        ("start", "prime.polish"),
        ("complete", "prime.polish"),
    ]
