"""The execution model: plan + snapshot → tree, totals, rendered rows.

No Textual here — these are the rules the Run view draws, exercised
directly. Timestamps are literals so elapsed values are exact.
"""

from __future__ import annotations

from typing import Any

from circuitry.tui.execution import (
    DONE,
    FAILED,
    PENDING,
    RUNNING,
    SKIPPED,
    build_tree,
    count_effects,
    format_totals,
    plan_from_orchestration,
    render_text,
    sum_tokens,
    totals_for,
)

T0 = "2026-01-01T00:00:00+00:00"
T1 = "2026-01-01T00:00:01+00:00"
T2 = "2026-01-01T00:00:02.500000+00:00"


def _meta(**kwargs: Any) -> dict[str, Any]:
    return {"meta": {**kwargs}}


def _done(sent: int = 0, received: int = 0) -> dict[str, Any]:
    return _meta(
        created_at=T0, completed_at=T1, tokens_sent=sent, tokens_received=received
    )


def _running() -> dict[str, Any]:
    return _meta(created_at=T0)


CHAIN: dict[str, Any] = {
    "effects": [
        {"type": "prompt", "name": "draft"},
        {"type": "prompt", "name": "polish"},
    ]
}


# -- plan ---------------------------------------------------------------------


def test_the_plan_mirrors_the_declared_structure() -> None:
    plan = plan_from_orchestration(
        {
            "effects": [
                {"type": "prompt", "name": "one"},
                {
                    "type": "loop",
                    "name": "each_thing",
                    "each": {"in": "prime.one.value", "as": "thing"},
                    "body": [{"type": "tool", "name": "shrink"}],
                },
                {
                    "type": "if",
                    "name": "gate",
                    "if": {"mode": "cel", "expr": "true"},
                    "then": [{"type": "prompt", "name": "yes"}],
                    "else": [{"type": "prompt", "name": "no"}],
                },
            ]
        }
    )
    assert [node.name for node in plan] == ["one", "each_thing", "gate"]
    assert [node.kind for node in plan] == ["prompt", "loop", "conditional"]
    assert plan[1].mode == "each"
    assert [child.name for child in plan[1].children] == ["shrink"]
    assert [child.name for child in plan[2].children] == ["yes"]
    assert [child.name for child in plan[2].else_children] == ["no"]


def test_a_while_loop_is_recognised_by_its_condition() -> None:
    plan = plan_from_orchestration(
        {
            "effects": [
                {
                    "type": "loop",
                    "name": "spin",
                    "while": {"mode": "cel", "expr": "false"},
                    "body": [{"type": "prompt", "name": "tick"}],
                }
            ]
        }
    )
    assert plan[0].mode == "while"


def test_legacy_steps_and_junk_effects_do_not_explode() -> None:
    plan = plan_from_orchestration({"steps": [{"type": "prompt", "name": "old"}]})
    assert [node.name for node in plan] == ["old"]
    assert plan_from_orchestration({"effects": ["nonsense", 3]}) == ()
    assert plan_from_orchestration({}) == ()


# -- chain --------------------------------------------------------------------


def test_a_chain_shows_done_running_and_pending_in_order() -> None:
    plan = plan_from_orchestration(CHAIN)
    nodes = build_tree(plan, {"prime": {"draft": _done(10, 20)}})
    assert [node.status for node in nodes] == [DONE, PENDING]

    nodes = build_tree(plan, {"prime": {"draft": _done(10, 20), "polish": _running()}})
    assert [node.status for node in nodes] == [DONE, RUNNING]


def test_a_finished_effect_carries_its_elapsed_and_tokens() -> None:
    nodes = build_tree(
        plan_from_orchestration(CHAIN), {"prime": {"draft": _done(11, 22)}}
    )
    assert nodes[0].elapsed == 1.0
    assert (nodes[0].tokens_sent, nodes[0].tokens_received) == (11, 22)
    assert "✓ draft (1.0s, ↑11 ↓22)" in render_text(nodes)


def test_the_effect_in_flight_is_shown_running_while_the_root_is(
) -> None:
    """State only lands when an effect finishes, so the tree infers the rest."""
    plan = plan_from_orchestration(CHAIN)
    state = {
        "prime": {
            "meta": {"created_at": T0, "flow": "chain"},
            "draft": _done(1, 1),
        }
    }
    assert [node.status for node in build_tree(plan, state)] == [DONE, RUNNING]

    # Once the root closes, nothing is inferred any more.
    state["prime"]["meta"]["completed_at"] = T2
    assert [node.status for node in build_tree(plan, state)] == [DONE, PENDING]


def test_a_run_that_died_leaves_the_rest_pending() -> None:
    plan = plan_from_orchestration(CHAIN)
    state = {
        "prime": {
            "meta": {"created_at": T0, "error": "boom", "flow": "chain"},
            "draft": _meta(created_at=T0, completed_at=T1, error="boom"),
        }
    }
    assert [node.status for node in build_tree(plan, state)] == [FAILED, PENDING]


def test_an_effect_compiled_out_reads_as_skipped() -> None:
    nodes = build_tree(
        plan_from_orchestration(CHAIN),
        {"prime": {"draft": _meta(disabled=True, created_at=T0, completed_at=T1)}},
    )
    assert nodes[0].status == SKIPPED


# -- tree flow ----------------------------------------------------------------


TREE: dict[str, Any] = {
    "effects": [
        {
            "type": "dynamic",
            "name": "fan",
            "flow": "tree",
            "effects": [
                {"type": "prompt", "name": "left"},
                {"type": "prompt", "name": "right"},
            ],
        }
    ]
}


def test_parallel_siblings_read_as_running_together() -> None:
    """Tree flow merges child state late — a blank sibling is still busy."""
    nodes = build_tree(
        plan_from_orchestration(TREE),
        {"prime": {"fan": {**_running(), "meta": {"created_at": T0, "flow": "tree"}}}},
    )
    assert nodes[0].status == RUNNING
    assert nodes[0].detail == "parallel"
    assert [child.status for child in nodes[0].children] == [RUNNING, RUNNING]


def test_an_effect_complete_event_marks_a_sibling_off_early() -> None:
    nodes = build_tree(
        plan_from_orchestration(TREE),
        {"prime": {"fan": {"meta": {"created_at": T0, "flow": "tree"}}}},
        completed=["prime.fan.left"],
    )
    assert [child.status for child in nodes[0].children] == [DONE, RUNNING]


def test_a_finished_tree_shows_every_sibling_done() -> None:
    nodes = build_tree(
        plan_from_orchestration(TREE),
        {
            "prime": {
                "fan": {
                    "meta": {"created_at": T0, "completed_at": T2, "flow": "tree"},
                    "left": _done(1, 2),
                    "right": _done(3, 4),
                }
            }
        },
    )
    assert nodes[0].status == DONE
    assert [child.status for child in nodes[0].children] == [DONE, DONE]


# -- loops --------------------------------------------------------------------


EACH: dict[str, Any] = {
    "effects": [
        {
            "type": "loop",
            "name": "over_items",
            "each": {"in": "prime.items.value", "as": "item"},
            "body": [{"type": "prompt", "name": "handle"}],
        }
    ]
}


def test_a_loop_previews_its_body_until_the_first_iteration_lands() -> None:
    nodes = build_tree(plan_from_orchestration(EACH), {"prime": {}})
    assert [child.label for child in nodes[0].children] == ["handle"]
    assert nodes[0].status == PENDING


def test_iterations_appear_as_they_run() -> None:
    plan = plan_from_orchestration(EACH)
    state = {
        "prime": {
            "over_items": {
                "meta": {"created_at": T0, "mode": "each"},
                "iter_0": {"handle": _done(2, 3)},
            }
        }
    }
    nodes = build_tree(plan, state)
    assert [child.label for child in nodes[0].children] == ["iter 0"]
    assert nodes[0].status == RUNNING
    assert nodes[0].detail == "each"

    state["prime"]["over_items"]["iter_1"] = {"handle": _running()}
    nodes = build_tree(plan, state)
    assert [child.label for child in nodes[0].children] == ["iter 0", "iter 1"]
    assert [child.status for child in nodes[0].children] == [DONE, RUNNING]


def test_iterations_are_ordered_numerically_not_lexically() -> None:
    state = {
        "prime": {
            "over_items": {
                "meta": {"created_at": T0, "completed_at": T2},
                **{f"iter_{i}": {"handle": _done()} for i in (0, 9, 10, 2)},
            }
        }
    }
    nodes = build_tree(plan_from_orchestration(EACH), state)
    assert [child.label for child in nodes[0].children] == [
        "iter 0",
        "iter 2",
        "iter 9",
        "iter 10",
    ]


def test_an_anonymous_loop_takes_its_status_from_its_body() -> None:
    """A transparent control effect writes into its parent's node."""
    plan = plan_from_orchestration(
        {
            "effects": [
                {
                    "type": "loop",
                    "while": {"mode": "cel", "expr": "false"},
                    "body": [{"type": "prompt", "name": "tick"}],
                }
            ]
        }
    )
    nodes = build_tree(plan, {"prime": {"iter_0": {"tick": _done()}}})
    assert nodes[0].label == "<loop>"
    assert nodes[0].status == DONE
    assert [child.label for child in nodes[0].children] == ["iter 0"]


# -- conditionals -------------------------------------------------------------


BRANCHING: dict[str, Any] = {
    "effects": [
        {
            "type": "if",
            "name": "gate",
            "if": {"mode": "cel", "expr": "true"},
            "then": [{"type": "prompt", "name": "yes"}],
            "else": [{"type": "prompt", "name": "no"}],
        }
    ]
}


def test_both_branches_show_until_the_condition_is_evaluated() -> None:
    nodes = build_tree(plan_from_orchestration(BRANCHING), {"prime": {}})
    assert [child.label for child in nodes[0].children] == ["then", "else"]
    assert all(child.status == PENDING for child in nodes[0].children)


def test_only_the_taken_branch_survives_the_decision() -> None:
    nodes = build_tree(
        plan_from_orchestration(BRANCHING),
        {
            "prime": {
                "gate": {
                    "meta": {"created_at": T0, "completed_at": T1, "branch": "else"},
                    "no": _done(1, 1),
                }
            }
        },
    )
    assert [child.label for child in nodes[0].children] == ["else"]
    assert nodes[0].detail == "→ else"
    assert [row.label for row in nodes[0].children[0].children] == ["no"]
    assert nodes[0].children[0].children[0].status == DONE


# -- errors -------------------------------------------------------------------


def test_an_error_is_rendered_inline_with_its_on_error_outcome() -> None:
    plan = plan_from_orchestration(
        {
            "effects": [
                {"type": "prompt", "name": "flaky", "on_error": "continue"},
                {"type": "prompt", "name": "after"},
            ]
        }
    )
    nodes = build_tree(
        plan,
        {
            "prime": {
                "flaky": _meta(created_at=T0, completed_at=T1, error="adapter blew up"),
                "after": _done(),
            }
        },
    )
    assert nodes[0].status == FAILED
    assert nodes[0].on_error == "continue"
    text = render_text(nodes)
    assert "✗ flaky" in text
    assert "↳ adapter blew up [on_error: continue]" in text
    # continue means the run kept going, and the tree shows that it did.
    assert nodes[1].status == DONE


def test_a_failing_effect_without_a_policy_says_nothing_extra() -> None:
    nodes = build_tree(
        plan_from_orchestration(CHAIN),
        {"prime": {"draft": _meta(created_at=T0, completed_at=T1, error="nope")}},
    )
    assert nodes[0].on_error == ""
    assert "↳ nope\n" in render_text(nodes) + "\n"


# -- aggregates ---------------------------------------------------------------


def test_tokens_are_summed_over_every_effect_meta_in_state() -> None:
    state = {
        "prime": {
            "one": _done(10, 20),
            "loop": {
                "meta": {"created_at": T0},
                "iter_0": {"body": _done(1, 2)},
                "iter_1": {"body": _done(3, 4)},
            },
        }
    }
    assert sum_tokens(state) == (14, 26)


def test_totals_count_effects_but_not_the_rows_that_only_group_them() -> None:
    state = {
        "prime": {
            "over_items": {
                "meta": {"created_at": T0, "completed_at": T2},
                "iter_0": {"handle": _done(1, 1)},
                "iter_1": {"handle": _done(2, 2)},
            }
        }
    }
    nodes = build_tree(plan_from_orchestration(EACH), state)
    # The loop plus one ``handle`` per iteration; ``iter n`` rows are structure.
    assert count_effects(nodes) == (3, 3)

    totals = totals_for(nodes, state, elapsed=2.5)
    assert (totals.tokens_sent, totals.tokens_received) == (3, 3)
    assert format_totals(totals) == "↑3 ↓3 tok  ·  2.5s  ·  3/3 effects"


def test_an_unstarted_run_reports_an_empty_footer() -> None:
    nodes = build_tree(plan_from_orchestration(CHAIN), {})
    assert format_totals(totals_for(nodes, {})) == "↑0 ↓0 tok  ·  —  ·  0/2 effects"


def test_long_runs_read_in_minutes() -> None:
    totals = totals_for((), {}, elapsed=75.5)
    assert "1m15.5s" in format_totals(totals)


# -- rendering ----------------------------------------------------------------


def test_the_tree_is_drawn_with_connectors() -> None:
    state = {
        "prime": {
            "over_items": {
                "meta": {"created_at": T0, "completed_at": T2, "mode": "each"},
                "iter_0": {"handle": _done(1, 2)},
            }
        }
    }
    assert render_text(build_tree(plan_from_orchestration(EACH), state)) == (
        "└─ ✓ over_items each (2.5s)\n"
        "   └─ ✓ iter 0\n"
        "      └─ ✓ handle (1.0s, ↑1 ↓2)"
    )
