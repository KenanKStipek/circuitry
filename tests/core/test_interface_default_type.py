"""An `interface.inputs` default that doesn't match its declared type is
caught by `cof check`, and by every run entry that applies the default (#301).

`check_interface_inputs` already applied a declared `type` to a filled-in
default the same way it applies one to a caller-supplied value; the gap was
that `interface_default_type_errors`/`cof check` never looked at defaults at
all, and `integer` was not a recognized declared type, so a default no
caller could ever actually hit (e.g. a string where a whole number belongs)
passed silently until it reached an effect.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.document_check import (
    interface_default_type_errors,
    interface_unknown_type_errors,
)
from circuitry.core.interface_inputs import check_interface_inputs


def test_quoted_integer_default_is_an_error() -> None:
    interface = {"inputs": {"count": {"type": "integer", "default": "three"}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.count.default" in errors[0]
    assert "'integer'" in errors[0]
    assert "'three'" in errors[0]


def test_quoted_numeral_default_hints_to_unquote() -> None:
    interface = {"inputs": {"n": {"type": "integer", "default": "3"}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "remove the quotes" in errors[0]


def test_list_default_for_number_input_is_an_error() -> None:
    interface = {"inputs": {"ratio": {"type": "number", "default": [1, 2]}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.ratio.default" in errors[0]


def test_matching_default_is_not_an_error() -> None:
    interface = {
        "inputs": {
            "count": {"type": "integer", "default": 3},
            "ratio": {"type": "number", "default": 0.5},
            "label": {"type": "string", "default": "x"},
        }
    }
    assert interface_default_type_errors({"interface": interface, "effects": []}) == []


def test_unknown_declared_type_is_an_error() -> None:
    interface = {"inputs": {"count": {"type": "int", "default": "three"}}}
    errors = interface_unknown_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.count.type" in errors[0]
    assert "'int'" in errors[0]
    for name in ("string", "number", "integer", "boolean", "array", "object"):
        assert name in errors[0]


def test_unknown_declared_type_is_an_error_even_without_a_default() -> None:
    interface = {"inputs": {"count": {"type": "int"}}}
    errors = interface_unknown_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.count.type" in errors[0]


def test_known_declared_type_is_not_an_error() -> None:
    interface = {
        "inputs": {
            name: {"type": name}
            for name in ("string", "number", "integer", "boolean", "array", "object")
        }
    }
    assert interface_unknown_type_errors({"interface": interface, "effects": []}) == []


def test_unquote_hint_omitted_for_boolean_words_yaml_does_not_resolve() -> None:
    """`1`, `y`, `t` stay a string or become an int when actually unquoted in
    YAML (PyYAML's bool resolver only accepts yes/no/on/off/true/false) —
    the hint would send the author to make a change that doesn't fix
    anything, so it must not appear for these."""
    for value in ("1", "y", "t", "0", "n", "f"):
        interface = {"inputs": {"flag": {"type": "boolean", "default": value}}}
        errors = interface_default_type_errors({"interface": interface, "effects": []})
        assert len(errors) == 1
        assert "remove the quotes" not in errors[0], (value, errors[0])


def test_unquote_hint_present_for_a_boolean_word_yaml_does_resolve() -> None:
    interface = {"inputs": {"flag": {"type": "boolean", "default": "yes"}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "remove the quotes" in errors[0]


def test_cof_check_rejects_an_unrecognized_input_type(tmp_path: Path) -> None:
    path = tmp_path / "doc.yml"
    path.write_text(
        "interface:\n"
        "  inputs:\n"
        "    count: {type: int}\n"
        "effects:\n"
        "  - {type: tool, name: echo, provider: json, params: {mode: parse, input: "
        '"{{input.count}}"}}\n',
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is False
    assert "interface.inputs.count.type" in result["errors"][0]


def test_integer_default_rejects_a_float() -> None:
    interface = {"inputs": {"count": {"type": "integer", "default": 3.5}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.count.default" in errors[0]


def test_integer_default_rejects_a_boolean() -> None:
    interface = {"inputs": {"count": {"type": "integer", "default": True}}}
    errors = interface_default_type_errors({"interface": interface, "effects": []})
    assert len(errors) == 1
    assert "interface.inputs.count.default" in errors[0]


def test_integer_input_rejects_a_float_value() -> None:
    interface = {"inputs": {"count": {"type": "integer"}}}
    with pytest.raises(ValueError, match="declared type 'integer'"):
        check_interface_inputs(interface, {"count": 3.5}, label="")


def test_integer_input_rejects_a_boolean_value() -> None:
    interface = {"inputs": {"count": {"type": "integer"}}}
    with pytest.raises(ValueError, match="declared type 'integer'"):
        check_interface_inputs(interface, {"count": True}, label="")


def test_integer_input_rejects_a_cli_float_string() -> None:
    interface = {"inputs": {"count": {"type": "integer"}}}
    with pytest.raises(ValueError, match="could not be converted"):
        check_interface_inputs(interface, {"count": "3.5"}, label="")


def test_cof_check_rejects_a_default_that_does_not_match_its_type(tmp_path: Path) -> None:
    path = tmp_path / "doc.yml"
    path.write_text(
        "interface:\n"
        "  inputs:\n"
        '    count: {type: integer, default: "three"}\n'
        "effects:\n"
        "  - {type: tool, name: echo, provider: json, params: {mode: parse, input: "
        '"{{input.count}}"}}\n',
        encoding="utf-8",
    )
    result = validate(path)
    assert result["ok"] is False
    assert "interface.inputs.count.default" in result["errors"][0]


def test_run_time_default_type_check_fails_at_the_start_not_inside_an_effect() -> None:
    """`count`'s `"three"` default used to reach the effect itself (`integer`
    wasn't a recognized type, so nothing enforced it); it now fails before
    anything runs, the same as a caller-supplied value would."""
    interface = {"inputs": {"count": {"type": "integer", "default": "three"}}}
    with pytest.raises(ValueError, match="declared type 'integer'"):
        check_interface_inputs(interface, {}, label="")


def test_run_rejects_a_default_that_does_not_match_its_declared_type(
    tmp_path: Path,
) -> None:
    path = tmp_path / "doc.yml"
    path.write_text(
        "interface:\n"
        "  inputs:\n"
        '    count: {type: integer, default: "three"}\n'
        "effects:\n"
        "  - {type: tool, name: echo, provider: json, params: {mode: parse, input: "
        '"{{input.count}}"}}\n',
        encoding="utf-8",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
        )
    )
    assert result.ok is False
    assert "input 'count' declared type 'integer'" in (result.error or "")
    assert "echo" not in result.state.get("prime", {})
