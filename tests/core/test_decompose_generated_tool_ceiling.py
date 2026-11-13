"""A decomposition plan is always generated (#772): its own
``allowed_commands`` never widen the shell allowlist, and a ``service``
effect's ``shell: true`` is always refused. Unlike a ``use``/``reflector``
generated child, it is not held to the tool-provider ceiling (rule 3) --
it has no generating effect of its own to carry an ``allow_tools:``, and
the planner's own merge step commonly needs a tool the enclosing document
never uses itself.
"""

from __future__ import annotations

from circuitry.core.decompose import _check_generated_tool_ceiling


def test_a_tool_the_document_never_uses_is_not_refused() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: shell
    params:
      command: echo
"""
    assert _check_generated_tool_ceiling(plan, {}) == []


def test_a_generated_shell_effect_cannot_widen_the_allowlist() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: shell
    params:
      command: touch
      allowed_commands: [touch]
"""
    problems = _check_generated_tool_ceiling(plan, {})
    assert len(problems) == 1
    assert "allowed_commands" in problems[0]


def test_a_generated_service_shell_true_is_refused() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: service
    params:
      command: touch pwned
      shell: true
"""
    problems = _check_generated_tool_ceiling(plan, {})
    assert len(problems) == 1
    assert "shell: true" in problems[0]


def test_unparseable_yaml_reports_nothing_here() -> None:
    """Already reported by the schema check elsewhere."""
    assert _check_generated_tool_ceiling("not: valid: yaml: :", {}) == []
