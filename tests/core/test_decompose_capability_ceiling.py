"""A generated decomposition plan may only use capabilities already
consented for the document it serves (#275 rule 4).
"""

from __future__ import annotations

from circuitry.capability_gate import install_capability_ceiling
from circuitry.core.decompose import _check_capability_ceiling, _Plan

_PLAN_YAML = """
effects:
  - type: tool
    name: t
    provider: shell
"""


def _plan(yaml_text: str) -> _Plan:
    return _Plan(say="", chunks=[], yaml=yaml_text, done=True, result_path="r")


def test_no_ceiling_installed_is_unrestricted() -> None:
    assert _check_capability_ceiling(_plan(_PLAN_YAML), {}) == []


def test_a_plan_within_the_ceiling_passes() -> None:
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"shell"}))
    assert _check_capability_ceiling(_plan(_PLAN_YAML), runtime_config) == []


def test_a_plan_beyond_the_ceiling_is_a_problem() -> None:
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset({"network"}))
    problems = _check_capability_ceiling(_plan(_PLAN_YAML), runtime_config)
    assert len(problems) == 1
    assert "shell" in problems[0]


def test_unparseable_yaml_reports_nothing_here() -> None:
    """Already reported by the schema check elsewhere."""
    runtime_config: dict = {}
    install_capability_ceiling(runtime_config, frozenset())
    assert _check_capability_ceiling(_plan("not: valid: yaml: :"), runtime_config) == []
