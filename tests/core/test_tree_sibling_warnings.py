"""`cof check` advisory for `{{> name}}` referencing an effect that cannot
be guaranteed to have run yet because a `flow: tree` container dispatches
its own members in no fixed order (#405).
"""

from __future__ import annotations

from circuitry.core.prompt_compose import tree_sibling_warnings


def test_direct_tree_siblings_warn() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {"type": "yield", "name": "b", "template": "{{> a}}"},
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "effects[1]" in warnings[0]
    assert "'{{> a}}'" in warnings[0]
    assert "'a'" in warnings[0]
    assert "flow: tree" in warnings[0]


def test_chain_siblings_never_warn() -> None:
    orch = {
        "flow": "chain",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {"type": "yield", "name": "b", "template": "{{> a}}"},
        ],
    }
    assert tree_sibling_warnings(orch) == []


def test_default_flow_is_chain_so_no_warning() -> None:
    orch = {
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {"type": "yield", "name": "b", "template": "{{> a}}"},
        ],
    }
    assert tree_sibling_warnings(orch) == []


def test_something_nested_under_a_tree_sibling_also_warns() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {
                "type": "dynamic",
                "name": "wrapper",
                "effects": [{"type": "yield", "name": "b", "template": "{{> a}}"}],
            },
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "effects[1].effects[0]" in warnings[0]


def test_an_enclosing_container_that_already_finished_is_fine() -> None:
    """A `flow: tree` dynamic that is itself a `flow: chain` sibling's
    predecessor has fully finished -- including every one of its own
    children -- by the time that later sibling runs, regardless of how
    unordered its own insides were."""
    orch = {
        "flow": "chain",
        "effects": [
            {
                "type": "dynamic",
                "name": "wrapper",
                "flow": "tree",
                "effects": [{"type": "yield", "name": "a", "template": "hello"}],
            },
            {"type": "yield", "name": "after", "template": "{{> wrapper.a}}"},
        ],
    }
    assert tree_sibling_warnings(orch) == []


def test_tree_siblings_inside_a_nested_dynamic_still_warn() -> None:
    orch = {
        "flow": "chain",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {
                "type": "dynamic",
                "name": "wrapper",
                "flow": "tree",
                "effects": [
                    {"type": "yield", "name": "b", "template": "{{> a}}"},
                    {"type": "yield", "name": "c", "template": "{{> b}}"},
                ],
            },
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "effects[1].effects[1]" in warnings[0]
    assert "'b'" in warnings[0]
    assert "wrapper" in warnings[0]


def test_dotted_reference_into_a_named_dynamics_child_also_warns() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {
                "type": "dynamic",
                "name": "pipeline",
                "effects": [{"type": "yield", "name": "outline", "template": "x"}],
            },
            {"type": "yield", "name": "after", "template": "{{> pipeline.outline}}"},
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "pipeline.outline" in warnings[0]


def test_a_loops_own_body_is_always_sequential_regardless_of_its_own_flow() -> None:
    """A loop's own `flow: tree` only changes which *pass* runs when, never
    the order of steps inside one pass -- no warning for a body step
    referencing an earlier body sibling, whatever the loop's own flow is."""
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "numbers",
                "flow": "tree",
                "each": {"in": "{{input.items}}"},
                "body": [
                    {"type": "yield", "name": "step1", "template": "first"},
                    {"type": "yield", "name": "step2", "template": "{{> step1}}"},
                ],
            }
        ]
    }
    assert tree_sibling_warnings(orch) == []


def test_a_loop_body_step_referencing_a_sibling_of_the_enclosing_tree_still_warns() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {
                "type": "loop",
                "name": "numbers",
                "each": {"in": "{{input.items}}"},
                "body": [{"type": "yield", "name": "step1", "template": "{{> a}}"}],
            },
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "effects[1].body[0]" in warnings[0]


def test_an_ifs_then_and_else_branches_are_always_sequential() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {
                "type": "if",
                "if": {"mode": "cel", "expr": "true"},
                "then": [{"type": "yield", "name": "a", "template": "x"}],
                "else": [{"type": "yield", "name": "b", "template": "{{> a}}"}],
            }
        ],
    }
    assert tree_sibling_warnings(orch) == []


def test_finally_always_runs_after_the_whole_of_effects() -> None:
    orch = {
        "flow": "tree",
        "effects": [
            {"type": "yield", "name": "a", "template": "hello"},
            {
                "type": "dynamic",
                "name": "wrapper",
                "finally": [{"type": "yield", "name": "cleanup", "template": "{{> a}}"}],
            },
        ],
    }
    warnings = tree_sibling_warnings(orch)
    assert len(warnings) == 1
    assert "effects[1].finally[0]" in warnings[0]


def test_unresolved_names_are_silently_skipped_not_our_job() -> None:
    """A genuinely unknown name is `check_prompt_composition`'s own error to
    raise -- this advisory has nothing to say about it either way."""
    orch = {
        "flow": "tree",
        "effects": [{"type": "yield", "name": "a", "template": "{{> nope}}"}],
    }
    assert tree_sibling_warnings(orch) == []


def test_non_mapping_orch_returns_no_warnings() -> None:
    assert tree_sibling_warnings([]) == []  # type: ignore[arg-type]
