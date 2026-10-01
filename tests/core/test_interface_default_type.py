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
from circuitry.core.document_check import interface_default_type_errors
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
