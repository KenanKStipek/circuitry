"""Document-level checks for the top-level `label:` key (#752).

A non-string `label:` is an ordinary schema violation, shared by every
caller of `structural_errors` (`cof check`, `cof run`, a `use` child
loading) like any other wrong-typed key. A malformed Mustache template is
different: `label_syntax_error` is checked only by `cof check`
(`cli.runtime_shim.validate`) — deliberately not folded into
`structural_errors`, so `cof run` and a `use` child reaching one anyway
warn and continue instead of aborting (see `core.label`,
`test_run_label.py`).
"""

from __future__ import annotations

from pathlib import Path

from circuitry.cli.runtime_shim import validate
from circuitry.core.document_check import (
    label_syntax_error,
    schema_errors,
    structural_errors,
)

_EFFECTS = [{"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}]


def test_non_string_label_is_a_schema_error() -> None:
    errors = schema_errors({"label": 123, "effects": _EFFECTS})
    assert errors == ["label: 123 is not of type 'string'"]
    assert structural_errors({"label": 123, "effects": _EFFECTS}) == errors


def test_string_label_passes_schema() -> None:
    assert schema_errors({"label": "ticket {{input.ticket}}", "effects": _EFFECTS}) == []


def test_label_syntax_error_on_malformed_template() -> None:
    reason = label_syntax_error({"label": "{{#open", "effects": _EFFECTS})
    assert reason is not None
    assert reason.startswith("label: malformed Mustache template:")


def test_label_syntax_error_none_for_well_formed_template() -> None:
    assert label_syntax_error({"label": "ticket {{input.ticket}}", "effects": _EFFECTS}) is None


def test_label_syntax_error_none_for_non_string_or_missing() -> None:
    assert label_syntax_error({"label": 123, "effects": _EFFECTS}) is None
    assert label_syntax_error({"effects": _EFFECTS}) is None


def test_label_syntax_error_not_folded_into_structural_errors() -> None:
    """A malformed `label:` is NOT a `structural_errors` violation — only
    `cof check`'s own `label_syntax_error` call catches it (see
    `test_run_label.py` for `cof run` continuing past one)."""
    assert structural_errors({"label": "{{#open", "effects": _EFFECTS}) == []


def test_cof_check_rejects_a_malformed_label(tmp_path: Path) -> None:
    doc = tmp_path / "doc.yml"
    doc.write_text("label: '{{#open'\neffects:\n  - type: tool\n    name: t\n"
                    "    provider: json\n    params:\n      input: '1'\n", encoding="utf-8")
    result = validate(doc)
    assert result["ok"] is False
    assert result["errors"] == ["label: malformed Mustache template: unclosed tag at line 1"]


def test_cof_check_rejects_a_non_string_label(tmp_path: Path) -> None:
    doc = tmp_path / "doc.yml"
    doc.write_text(
        "label: 123\neffects:\n  - type: tool\n    name: t\n"
        "    provider: json\n    params:\n      input: '1'\n",
        encoding="utf-8",
    )
    result = validate(doc)
    assert result["ok"] is False
    assert result["errors"] == ["label: 123 is not of type 'string'"]


def test_cof_check_accepts_a_well_formed_label(tmp_path: Path) -> None:
    doc = tmp_path / "doc.yml"
    doc.write_text(
        "label: 'ticket {{input.ticket}}'\neffects:\n  - type: tool\n    name: t\n"
        "    provider: json\n    params:\n      input: '1'\n",
        encoding="utf-8",
    )
    result = validate(doc)
    assert result["ok"] is True
