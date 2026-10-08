"""Unknown keys and the schema gate `cof check` and `cof run` share (#261).

A key the effect type does not know is either a near miss of one it does —
an error, because the author meant the known key and it is not applied — or
anything else, which is a warning: ignored, possibly on purpose.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from circuitry.cli.runtime_shim import validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.document_check import (
    schema_errors,
    structural_errors,
    unknown_key_errors,
    unknown_key_warnings,
)

_TOOL = {"type": "tool", "name": "t", "provider": "json", "params": {"input": "1"}}


def _validate(tmp_path: Path, text: str) -> dict:
    path = tmp_path / "doc.yml"
    path.write_text(text, encoding="utf-8")
    return validate(path)


def test_misspelled_while_is_an_error_naming_the_key(tmp_path: Path) -> None:
    result = _validate(
        tmp_path,
        "effects:\n"
        "  - type: loop\n"
        "    name: poll\n"
        "    whlie: {mode: cel, expr: 'true'}\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    assert result["ok"] is False
    assert (
        "effects[0]: unknown key 'whlie' on a 'loop' effect — did you mean 'while'?"
        in result["errors"][0]
    )


@pytest.mark.parametrize(
    ("effect", "key", "meant"),
    [
        ({"type": "loop", "each": {"in": "input.xs"}, "body": [_TOOL], "max_iteration": 3},
         "max_iteration", "max_iterations"),
        ({"type": "prompt", "name": "p", "template": "hi", "adapter": "ollama"},
         "adapter", "provider"),
        ({"type": "prompt", "name": "p", "temlpate": "hi", "template": "hi"},
         "temlpate", "template"),
        ({**_TOOL, "Params": {}}, "Params", "params"),
        ({"type": "yield", "name": "y", "templte": "hi"}, "templte", "template"),
    ],
)
def test_near_miss_keys_are_errors(effect: dict, key: str, meant: str) -> None:
    errors = unknown_key_errors({"effects": [effect]})
    assert len(errors) == 1
    assert f"unknown key '{key}'" in errors[0]
    assert f"did you mean '{meant}'?" in errors[0]


def test_unrelated_unknown_key_is_a_warning_not_an_error(tmp_path: Path) -> None:
    result = _validate(
        tmp_path,
        "effects:\n"
        "  - {type: tool, name: t, provider: json, params: {input: '1'}, owner: ops}\n",
    )
    assert result["ok"] is True
    assert any(
        "effects[0]: unknown key 'owner' on a 'tool' effect is ignored" in w
        for w in result["warnings"]
    )


def test_leftover_cache_key_is_an_ordinary_unknown_key_warning() -> None:
    """The step cache (#336) is gone (#352); a document that still has
    `cache:` on a prompt/tool effect gets the ordinary unknown-key warning,
    not an error — it does not resemble any key either effect type knows."""
    orch = {"effects": [{**_TOOL, "cache": True}]}
    assert unknown_key_errors(orch) == []
    warnings = unknown_key_warnings(orch)
    assert len(warnings) == 1
    assert "unknown key 'cache' on a 'tool' effect is ignored" in warnings[0]

    prompt = {"type": "prompt", "name": "p", "template": "hi", "cache": {"ttl": "7d"}}
    assert unknown_key_errors({"effects": [prompt]}) == []
    warnings = unknown_key_warnings({"effects": [prompt]})
    assert len(warnings) == 1
    assert "unknown key 'cache' on a 'prompt' effect is ignored" in warnings[0]


def test_unknown_keys_in_nested_effects_are_found() -> None:
    orch = {
        "effects": [
            {"type": "dynamic", "name": "d", "effects": [{**_TOOL, "time_out_ms": 5}]}
        ]
    }
    assert unknown_key_errors(orch) == [
        (
            "effects[0].effects[0]: unknown key 'time_out_ms' on a 'tool' effect — "
            "did you mean 'timeout_ms'? As written it is ignored."
        )
    ]


def test_near_miss_key_inside_a_dynamics_finally_is_an_error() -> None:
    """A typo on the exact key `finally:` exists for (`on_eror` instead of
    `on_error`) is caught the same way one in `effects` is (#272 review,
    finding 5) — the near-miss walk used to never enter `finally:` at all."""
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "d",
                "effects": [_TOOL],
                "finally": [{**_TOOL, "name": "cleanup", "on_eror": "continue"}],
            }
        ]
    }
    errors = unknown_key_errors(orch)
    assert len(errors) == 1
    assert "finally" in errors[0]
    assert "unknown key 'on_eror'" in errors[0]
    assert "did you mean 'on_error'?" in errors[0]


def test_near_miss_key_inside_the_root_finally_is_an_error() -> None:
    orch = {
        "effects": [_TOOL],
        "finally": [{**_TOOL, "name": "cleanup", "Params": {}}],
    }
    errors = unknown_key_errors(orch)
    assert len(errors) == 1
    assert "finally[0]" in errors[0]
    assert "did you mean 'params'?" in errors[0]


def test_top_level_prompt_singular_is_a_near_miss_of_prompts() -> None:
    """A stray top-level `prompt:` (#396's `prompts:` map, singular typo) is
    an error naming the intended key, not a silently-ignored warning."""
    errors = unknown_key_errors(
        {"prompt": {"voice": "Plain, direct."}, "effects": [_TOOL]}
    )
    assert len(errors) == 1
    assert "unknown key 'prompt'" in errors[0]
    assert "did you mean 'prompts'?" in errors[0]


def test_top_level_near_miss_is_an_error_and_other_keys_warn() -> None:
    orch = {"modle": "llama3", "name": "mine", "effects": [_TOOL]}
    assert len(unknown_key_errors(orch)) == 1
    assert "top level: unknown key 'modle' on the document — did you mean 'model'?" in (
        unknown_key_errors(orch)[0]
    )
    warnings = unknown_key_warnings(orch)
    assert len(warnings) == 1
    assert "unknown key 'name' on the document is ignored" in warnings[0]


def test_keys_the_runtime_reads_outside_the_schema_are_known() -> None:
    orch = {
        "description": "d",
        "runtime": {},
        "plugins": [],
        "version": "1",
        "effects": [
            {"type": "dynamic", "name": "d", "strategy": "chain", "steps": [_TOOL]},
        ],
    }
    assert unknown_key_errors(orch) == []
    assert unknown_key_warnings(orch) == []


def test_unquoted_yaml_boolean_key_is_reported_as_such() -> None:
    # `on: x` in YAML is the key True.
    warnings = unknown_key_warnings({"effects": [{**_TOOL, True: "x"}]})
    assert len(warnings) == 1
    assert "unknown key True (YAML read the unquoted key as a bool)" in warnings[0]


@pytest.mark.parametrize(
    "loop",
    [
        {"type": "loop", "body": [_TOOL]},
        {
            "type": "loop",
            "body": [_TOOL],
            "each": {"in": "input.xs"},
            "while": {"mode": "cel", "expr": "true"},
        },
    ],
)
def test_loop_needs_exactly_one_of_while_or_each(loop: dict) -> None:
    errors = schema_errors({"effects": [loop]})
    assert errors and errors[0].startswith("effects[0]: value is ")
    with pytest.raises(ValueError, match="exactly one of 'while' or 'each'"):
        compile_orchestration(orch={"effects": [loop]})


@pytest.mark.parametrize(
    ("effect", "unexpected"),
    [
        ({"type": "loop", "each": {"in": "input.xs", "truncat": True}, "body": [_TOOL]},
         "truncat"),
        ({"type": "if", "if": {"mode": "cel", "expr": "true", "treshold": 1}, "then": [_TOOL]},
         "treshold"),
        ({"type": "prompt", "name": "p", "template": "x", "retries": {"max_attempt": 2}},
         "max_attempt"),
    ],
)
def test_closed_nested_blocks_reject_unknown_keys(effect: dict, unexpected: str) -> None:
    errors = structural_errors({"effects": [effect]})
    assert any(
        f"'{unexpected}' was unexpected" in e and e.startswith("effects[0].") for e in errors
    )


def test_schema_errors_name_the_offending_node() -> None:
    errors = schema_errors({"effects": [{"type": "loop", "each": {"in": "x"}, "body": []}]})
    assert errors == ["effects[0].body: value should be non-empty"]
