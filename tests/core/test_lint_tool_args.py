"""`cof check` warns when YAML turned a tool argument into a non-string (#261).

A tool passes each ``params.args`` entry on as ``str(value)``: an unquoted
``0x1`` arrives as ``1``, ``off`` as ``False``, ``-0`` as ``0``.
"""

from __future__ import annotations

import yaml

from circuitry.core.lint import lint_orchestration


def _lint(args_yaml: str) -> list[str]:
    orch = yaml.safe_load(
        "effects:\n"
        "  - type: tool\n"
        "    name: run_it\n"
        "    provider: shell\n"
        f"    params: {{command: echo, args: {args_yaml}}}\n"
    )
    return lint_orchestration(orch)


def test_yaml_scalar_gotchas_are_warned_with_what_the_tool_receives() -> None:
    warnings = _lint("[0x1, off, -0, ~]")
    assert len(warnings) == 4
    assert warnings[0].startswith(
        "effects[0]: params.args[0] is not a string — YAML read it as the int 1, "
        "so the tool receives the text '1'."
    )
    assert "as the bool False, so the tool receives the text 'False'" in warnings[1]
    assert "as the int 0, so the tool receives the text '0'" in warnings[2]
    assert "as the null None, so the tool receives the text 'None'" in warnings[3]


def test_string_args_are_not_warned() -> None:
    assert _lint('[-p, "0x1", "off", "{{input.dir}}"]') == []


def test_only_tool_args_are_checked() -> None:
    orch = {
        "effects": [
            {"type": "tool", "name": "t", "provider": "json", "params": {"count": 3}},
        ]
    }
    assert lint_orchestration(orch) == []
