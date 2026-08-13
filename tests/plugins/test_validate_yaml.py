"""Tests for the validate_yaml tool plugin.

The plugin is the deterministic half of any orchestration that writes
orchestrations, so what matters is not just the ok/not-ok verdict but that the
errors it returns are specific enough for a model to act on.
"""

from __future__ import annotations

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.validate_yaml import ValidateYamlPlugin

VALID = """\
effects:
  - type: prompt
    name: greet
    template: "Say hello."
"""


def _verdict(**params: object) -> dict:
    return ValidateYamlPlugin().execute(params=params).value


def test_factory_builds_the_plugin() -> None:
    plugin = build_plugin(plugin_name="validate_yaml", runtime={})
    assert isinstance(plugin, ValidateYamlPlugin)
    assert plugin.name == "validate_yaml"


def test_check_is_satisfied_by_core_dependencies() -> None:
    result = ValidateYamlPlugin().check()
    assert result.ok is True
    assert result.missing == []


def test_result_conforms_to_the_tool_contract() -> None:
    result = ValidateYamlPlugin().execute(params={"yaml": VALID})
    assert validate_tool_result(result, plugin_name="validate_yaml") == []
    assert result.exit_code == 0


def test_valid_orchestration_passes() -> None:
    verdict = _verdict(yaml=VALID)
    assert verdict["ok"] is True
    assert verdict["errors"] == []
    assert verdict["yaml"].startswith("effects:")


def test_echoes_the_document_it_validated() -> None:
    """The echo is what a revision prompt feeds back to the model, so it must be
    the cleaned text — not the raw string it was handed."""
    verdict = _verdict(yaml=f"```yaml\n{VALID}```")
    assert "```" not in verdict["yaml"]
    assert verdict["ok"] is True


def test_fences_can_be_held_against_the_model() -> None:
    verdict = _verdict(yaml=f"```yaml\n{VALID}```", strip_fences=False)
    assert verdict["ok"] is False


def test_empty_document_is_invalid_not_an_exception() -> None:
    verdict = _verdict(yaml="   ")
    assert verdict["ok"] is False
    assert "Empty document" in verdict["errors"][0]


def test_missing_param_is_treated_as_empty() -> None:
    verdict = _verdict()
    assert verdict["ok"] is False


def test_non_string_input_raises() -> None:
    with pytest.raises(ValueError, match=r"params\['yaml'\] as a string"):
        _verdict(yaml=["effects"])


def test_unparseable_yaml_reports_a_parse_error() -> None:
    verdict = _verdict(yaml="effects:\n  - type: prompt\n   name: bad\n")
    assert verdict["ok"] is False
    assert "YAML parse error" in verdict["errors"][0]


def test_non_mapping_document_is_rejected() -> None:
    verdict = _verdict(yaml="- just\n- a\n- list\n")
    assert verdict["ok"] is False
    assert "must be a YAML mapping" in verdict["errors"][0]


def test_missing_effects_key_is_named_explicitly() -> None:
    verdict = _verdict(yaml="adapter: ollama\n")
    assert verdict["ok"] is False
    assert "'effects'" in verdict["errors"][0]


def test_schema_errors_carry_a_json_path_and_the_underlying_cause() -> None:
    """A bare "is not valid under any of the given schemas" is unactionable;
    the deepest sub-error is the part a model can fix."""
    verdict = _verdict(yaml="effects:\n  - type: prompt\n    name: no_body\n")
    assert verdict["ok"] is False
    (error,) = verdict["errors"]
    assert error.startswith("$.effects[0]")
    assert "'template' is a required property" in error


def test_reserved_iteration_names_are_rejected() -> None:
    verdict = _verdict(yaml='effects:\n  - type: prompt\n    name: iter_0\n    template: "x"\n')
    assert verdict["ok"] is False


def test_compiler_catches_what_the_schema_cannot() -> None:
    duplicate = (
        "effects:\n"
        '  - type: prompt\n    name: same\n    template: "a"\n'
        '  - type: prompt\n    name: same\n    template: "b"\n'
    )
    verdict = _verdict(yaml=duplicate)
    assert verdict["ok"] is False
    assert "Duplicate effect name" in verdict["errors"][0]

    # ...and the compiler pass can be turned off when only shape matters.
    assert _verdict(yaml=duplicate, compile=False)["ok"] is True


def test_errors_are_capped_so_they_can_fit_in_a_prompt() -> None:
    many = "effects:\n" + "".join(
        f"  - type: prompt\n    name: bad{i}\n" for i in range(30)
    )
    verdict = _verdict(yaml=many, max_errors=5)
    assert len(verdict["errors"]) == 6
    assert verdict["errors"][-1].startswith("... and ")


def test_error_cap_must_be_a_positive_integer() -> None:
    with pytest.raises(ValueError, match="max_errors"):
        _verdict(yaml=VALID, max_errors=0)
    with pytest.raises(ValueError, match="max_errors"):
        _verdict(yaml=VALID, max_errors="lots")


@pytest.mark.parametrize("flag", ["false", "no", "0", False])
def test_boolean_params_survive_mustache_rendering(flag: object) -> None:
    """Tool params are Mustache-rendered, which can turn a YAML boolean into the
    string "false" — both spellings must mean the same thing."""
    verdict = _verdict(yaml=f"```yaml\n{VALID}```", strip_fences=flag)
    assert verdict["ok"] is False
