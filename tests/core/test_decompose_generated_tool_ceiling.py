"""A decomposition plan is always generated (#772): its own
``allowed_commands`` never widen past the host config and the enclosing
document's own literal shell effects, a ``service`` effect's ``shell:
true`` is always refused, and it is held to the tool-provider ceiling like
a ``use``/``reflector`` generated child -- except that it has no
generating effect of its own to carry a literal ``allow_tools:``, so it is
additionally always allowed the fixed, side-effect-free
``DECOMPOSITION_SAFE_PROVIDERS`` set.
"""

from __future__ import annotations

from circuitry.core.decompose import (
    _check_generated_tool_ceiling,
    _planner_path,
    _planner_runtime_config,
)
from circuitry.generated_doc import (
    GENERATED_DOCUMENT_IS_GENERATED_KEY,
    GENERATED_DOCUMENT_PROVIDERS_KEY,
)


def test_a_safe_data_tool_the_document_never_uses_is_not_refused() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: json
    params:
      text: '{}'
"""
    assert _check_generated_tool_ceiling(plan, {}) == []


def test_a_tool_outside_the_safe_set_and_the_document_is_refused() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: python_eval
    params:
      code: "1"
"""
    problems = _check_generated_tool_ceiling(plan, {})
    assert len(problems) == 1
    assert "python_eval" in problems[0]
    assert "allow_tools" not in problems[0]
    assert "json" in problems[0]  # names the fixed safe set


def test_a_provider_the_enclosing_document_uses_itself_is_allowed() -> None:
    plan = """
effects:
  - type: tool
    name: t
    provider: git
    params:
      args: [status]
"""
    runtime_config = {GENERATED_DOCUMENT_PROVIDERS_KEY: ["git"]}
    assert _check_generated_tool_ceiling(plan, runtime_config) == []


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
    problems = _check_generated_tool_ceiling(plan, {"_document_tool_providers": ["shell"]})
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
    problems = _check_generated_tool_ceiling(
        plan, {"_document_tool_providers": ["service"]}
    )
    assert len(problems) == 1
    assert "shell: true" in problems[0]


def test_unparseable_yaml_reports_nothing_here() -> None:
    """Already reported by the schema check elsewhere."""
    assert _check_generated_tool_ceiling("not: valid: yaml: :", {}) == []


def test_planner_runtime_config_runs_as_its_own_non_generated_document() -> None:
    """#772: the planner's own validate_yaml/json self-check must not be
    judged against a generated caller's own (commonly empty) ceiling -- it
    runs as its own, literal document, with its own providers.
    """
    from circuitry.cli.orchestration_loader import load_orchestration_file

    planner_orch = load_orchestration_file(_planner_path({}))
    caller_runtime_config = {
        GENERATED_DOCUMENT_IS_GENERATED_KEY: True,
        GENERATED_DOCUMENT_PROVIDERS_KEY: [],
    }
    out = _planner_runtime_config(caller_runtime_config, planner_orch)
    assert out[GENERATED_DOCUMENT_IS_GENERATED_KEY] is False
    assert "validate_yaml" in out[GENERATED_DOCUMENT_PROVIDERS_KEY]
    assert "json" in out[GENERATED_DOCUMENT_PROVIDERS_KEY]
