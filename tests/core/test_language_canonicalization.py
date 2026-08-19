"""Issue #89 — one documented spelling per construct, every spelling still parses.

The decision recorded on the issue is "forgiving parser, opinionated docs": the
canonical form is what the docs, rules, and curation library teach, and the
deprecated/sugar forms keep working without ever becoming an error. These tests
hold both halves of that at once — the alias runs, *and* validation says so.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.cli.runtime_shim import validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.lint import lint_orchestration
from circuitry.core.outputs import normalize_outputs
from circuitry.core.store import Store
from circuitry.core.use import UseDefinition, UseRuntime


def _write(tmp_path: Path, name: str, content: dict) -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(content), encoding="utf-8")
    return path


def _mock_adapter(response: str = "mock") -> MagicMock:
    adapter = MagicMock()
    adapter.name = "mock"
    result = MagicMock()
    result.text = response
    result.raw = {}
    result.tokens_sent = 10
    result.tokens_received = 5
    adapter.generate.return_value = result
    return adapter


# ── Item 3: outputs — one canonical shape, both contexts ─────────────────────


@pytest.mark.parametrize(
    "raw",
    [
        {"summary": "prime.summarize.value"},
        {"summary": {"path": "prime.summarize.value"}},
        {"summary": {"path": "prime.summarize.value", "type": "string"}},
        {"summary": {"path": "  prime.summarize.value  "}},
        {"summary": "  prime.summarize.value  "},
    ],
)
def test_every_accepted_output_spelling_normalizes_the_same(raw: dict) -> None:
    assert normalize_outputs(raw, context="ctx") == {
        "summary": "prime.summarize.value"
    }


def test_outputs_may_mix_both_forms_in_one_block() -> None:
    normalized = normalize_outputs(
        {"a": "prime.one.value", "b": {"path": "prime.two.value", "type": "number"}},
        context="ctx",
    )
    assert normalized == {"a": "prime.one.value", "b": "prime.two.value"}


def test_absent_outputs_normalize_to_empty() -> None:
    assert normalize_outputs(None, context="ctx") == {}
    assert normalize_outputs({}, context="ctx") == {}


def test_object_output_without_a_path_names_both_accepted_forms() -> None:
    """The error a frontier model hit: it has to say what to write instead."""
    with pytest.raises(ValueError) as exc:
        normalize_outputs({"summary": {"type": "string"}}, context="Use effect 'x'")
    message = str(exc.value)
    assert "Use effect 'x'" in message
    assert "path" in message
    assert "also accepted" in message


def test_output_of_the_wrong_type_is_rejected_with_both_forms_named() -> None:
    with pytest.raises(ValueError) as exc:
        normalize_outputs({"summary": ["prime.x.value"]}, context="ctx")
    assert "canonical" in str(exc.value)
    assert "shorthand" in str(exc.value)


@pytest.mark.parametrize(
    "outputs",
    [
        {"summary": "prime.summarize.value"},
        {"summary": {"path": "prime.summarize.value", "type": "string"}},
    ],
)
def test_use_outputs_accepts_both_forms_end_to_end(
    tmp_path: Path, outputs: dict
) -> None:
    child = _write(
        tmp_path,
        "child.yml",
        {"effects": [{"type": "prompt", "name": "summarize", "template": "Go"}]},
    )
    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "use",
                    "name": "sub",
                    "path": str(child),
                    "outputs": outputs,
                }
            ]
        }
    )
    defn = root.effects[0]
    assert isinstance(defn, UseDefinition)
    assert defn.outputs == {"summary": "prime.summarize.value"}

    store = Store(state={})
    UseRuntime(defn, adapter=_mock_adapter("Done"), model="m").execute(
        store=store, ctx=store.state
    )
    assert store.state["sub"]["value"] == {"summary": "Done"}


@pytest.mark.parametrize(
    "declared",
    [
        {"summary": "prime.summarize.value"},
        {"summary": {"type": "string", "path": "prime.summarize.value"}},
    ],
)
def test_interface_outputs_accepts_both_forms_end_to_end(
    tmp_path: Path, declared: dict
) -> None:
    """The string form used to be use-only; it now works here too."""
    child = _write(
        tmp_path,
        "child.yml",
        {
            "interface": {"outputs": declared},
            "effects": [{"type": "prompt", "name": "summarize", "template": "Go"}],
        },
    )
    store = Store(state={})
    UseRuntime(
        UseDefinition(name="sub", path=str(child)),
        adapter=_mock_adapter("Done"),
        model="m",
    ).execute(store=store, ctx=store.state)
    assert store.state["sub"]["value"] == {"summary": "Done"}


@pytest.mark.parametrize(
    "declared",
    [
        {"summary": "prime.summarize.value"},
        {"summary": {"type": "string", "path": "prime.summarize.value"}},
    ],
)
def test_schema_accepts_both_output_forms_in_both_contexts(
    tmp_path: Path, declared: dict
) -> None:
    path = _write(
        tmp_path,
        "orch.yml",
        {
            "interface": {"outputs": declared},
            "effects": [
                {"type": "prompt", "name": "summarize", "template": "Go"},
                {
                    "type": "use",
                    "name": "sub",
                    "ref": "utilities/critique",
                    "outputs": declared,
                },
            ],
        },
    )
    result = validate(path)
    assert result["ok"] is True, result["errors"]


# ── Item 4: version is free-form and runtime-ignored ─────────────────────────


@pytest.mark.parametrize("version", ["1.2.0", "2026-08-19", 2, 1.5])
def test_version_accepts_free_form_values_and_changes_nothing(
    tmp_path: Path, version: object
) -> None:
    path = _write(
        tmp_path,
        "orch.yml",
        {
            "version": version,
            "effects": [{"type": "prompt", "name": "step", "template": "Go"}],
        },
    )
    result = validate(path)
    assert result["ok"] is True, result["errors"]
    assert result["warnings"] == []


# ── Items 1, 2, 5: aliases and names still parse, but are warned about ───────


def test_type_alias_still_compiles() -> None:
    root = compile_orchestration(
        orch={
            "effects": [
                {
                    "type": "conditional",
                    "if": {"mode": "cel", "expr": "state.x == 1"},
                    "then": [{"type": "prompt", "name": "yes", "template": "y"}],
                }
            ]
        }
    )
    assert len(root.effects) == 1


@pytest.mark.parametrize(
    ("alias", "canonical"),
    [
        ("chain_of_thought", "chain"),
        ("cot", "chain"),
        ("tree_of_thought", "tree"),
        ("tot", "tree"),
    ],
)
def test_flow_aliases_still_compile_to_the_canonical_topology(
    alias: str, canonical: str
) -> None:
    root = compile_orchestration(
        orch={
            "flow": alias,
            "effects": [{"type": "prompt", "name": "step", "template": "Go"}],
        }
    )
    assert root.flow == canonical


def test_lint_names_the_deprecated_effect_type_alias() -> None:
    (warning,) = lint_orchestration(
        {
            "effects": [
                {
                    "type": "conditional",
                    "if": {"mode": "cel", "expr": "state.x == 1"},
                    "then": [{"type": "prompt", "name": "yes", "template": "y"}],
                }
            ]
        }
    )
    assert "effects[0]" in warning
    assert "conditional" in warning
    assert "type: if" in warning


@pytest.mark.parametrize(
    ("alias", "canonical"),
    [
        ("chain_of_thought", "chain"),
        ("cot", "chain"),
        ("tree_of_thought", "tree"),
        ("tot", "tree"),
    ],
)
def test_lint_names_deprecated_flow_aliases_at_top_level_and_on_effects(
    alias: str, canonical: str
) -> None:
    warnings = lint_orchestration(
        {
            "flow": alias,
            "effects": [
                {
                    "type": "dynamic",
                    "name": "group",
                    "flow": alias,
                    "effects": [{"type": "prompt", "name": "step", "template": "Go"}],
                }
            ],
        }
    )
    assert len(warnings) == 2
    assert "top level" in warnings[0]
    assert "effects[0]" in warnings[1]
    assert all(f"flow: {canonical}" in w for w in warnings)


def test_canonical_document_lints_clean() -> None:
    assert (
        lint_orchestration(
            {
                "flow": "chain",
                "effects": [
                    {
                        "type": "if",
                        "name": "route",
                        "if": {"mode": "cel", "expr": "state.x == 1"},
                        "then": [
                            {"type": "prompt", "name": "explain", "template": "y"}
                        ],
                    }
                ],
            }
        )
        == []
    )


@pytest.mark.parametrize("name", ["use", "loop", "if", "dynamic", "prompt", "tool"])
def test_lint_warns_on_effects_named_after_a_type(name: str) -> None:
    (warning,) = lint_orchestration(
        {"effects": [{"type": "prompt", "name": name, "template": "Go"}]}
    )
    assert f"named '{name}'" in warning
    assert "effect-type keyword" in warning


def test_lint_reaches_effects_nested_in_every_container_key() -> None:
    warnings = lint_orchestration(
        {
            "effects": [
                {
                    "type": "if",
                    "if": {"mode": "cel", "expr": "state.x == 1"},
                    "then": [{"type": "prompt", "name": "loop", "template": "a"}],
                    "else": [
                        {
                            "type": "loop",
                            "body": [
                                {"type": "prompt", "name": "use", "template": "b"}
                            ],
                            "while": {"mode": "cel", "expr": "state.x == 1"},
                        }
                    ],
                }
            ]
        }
    )
    assert len(warnings) == 2
    assert "effects[0].then[0]" in warnings[0]
    assert "effects[0].else[0].body[0]" in warnings[1]


def test_lint_survives_a_document_it_cannot_read() -> None:
    """Malformed input is the schema validator's job; lint just stays quiet."""
    assert lint_orchestration(None) == []
    assert lint_orchestration({"effects": "not a list"}) == []
    assert lint_orchestration({"effects": [None, "nope", 3]}) == []


# ── Warnings are advisory: they never make a file invalid ────────────────────


def test_a_deprecated_but_correct_file_validates_ok_with_warnings(
    tmp_path: Path,
) -> None:
    path = _write(
        tmp_path,
        "orch.yml",
        {
            "flow": "cot",
            "effects": [
                {
                    "type": "conditional",
                    "if": {"mode": "cel", "expr": "state.x == 1"},
                    "then": [{"type": "prompt", "name": "loop", "template": "y"}],
                }
            ],
        },
    )
    result = validate(path)
    assert result["ok"] is True, result["errors"]
    assert len(result["warnings"]) == 3


def test_warnings_are_reported_even_when_the_file_is_invalid(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "orch.yml",
        {"flow": "cot", "effects": [{"type": "prompt", "name": "step"}]},
    )
    result = validate(path)
    assert result["ok"] is False
    assert any("flow: chain" in w for w in result["warnings"])
