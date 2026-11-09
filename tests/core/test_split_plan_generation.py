"""Unit tests for the size trigger's own plan generation (#672): the tag
rewriter, the generated plan's shape, and the native merge rules --
isolated from the full dispatch seam (covered end to end by the conformance
suite's own ``split-*`` cases and the acceptance probe).
"""

from __future__ import annotations

from typing import Any

import pytest

from circuitry.core.decompose import (
    _apply_native_merge,
    _forwarded_split_config,
    _generate_split_plan,
    _MergeInvalidError,
    _pick_effect_name,
    _pick_parts_key,
    _rewrite_exact,
    _rewrite_prefix,
    _SplitPrep,
)
from circuitry.core.prompt import PromptDefinition, SplitDef, SplitMergeDef

# --------------------------------------------------------------------------
# the tag rewriter
# --------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("template", "expected"),
    [
        ("Text: {{input.doc}}", "Text: {{part}}"),
        ("Text: {{{input.doc}}}", "Text: {{{part}}}"),
        ("Text: {{&input.doc}}", "Text: {{&part}}"),
        ("{{#input.doc}}yes{{/input.doc}}", "{{#part}}yes{{/part}}"),
        ("{{^input.doc}}no{{/input.doc}}", "{{^part}}no{{/part}}"),
        ("{{ input.doc }}", "{{part}}"),
    ],
)
def test_rewrite_exact_covers_every_sigil(template: str, expected: str) -> None:
    assert _rewrite_exact(template, "input.doc", "part") == expected


def test_rewrite_exact_leaves_other_references_untouched() -> None:
    text = "{{input.other}} and {{input.doc}}"
    assert _rewrite_exact(text, "input.doc", "part") == "{{input.other}} and {{part}}"


def test_rewrite_exact_never_touches_a_comment_or_partial_tag() -> None:
    text = "{{! input.doc }}{{> input.doc}}{{input.doc}}"
    assert _rewrite_exact(text, "input.doc", "part") == "{{! input.doc }}{{> input.doc}}{{part}}"


def test_rewrite_prefix_rewrites_the_exact_name_and_every_extension() -> None:
    text = "{{#parts}}{{.}}{{/parts}} and {{parts.0}}"
    out = _rewrite_prefix(text, "parts", "prime.parts.collected.value")
    assert out == (
        "{{#prime.parts.collected.value}}{{.}}{{/prime.parts.collected.value}} "
        "and {{prime.parts.collected.value.0}}"
    )


def test_rewrite_prefix_leaves_an_unrelated_name_untouched() -> None:
    text = "{{partsize}} {{parts}}"
    out = _rewrite_prefix(text, "parts", "prime.parts.collected.value")
    assert out == "{{partsize}} {{prime.parts.collected.value}}"


# --------------------------------------------------------------------------
# collision-free names
# --------------------------------------------------------------------------


def test_pick_parts_key_appends_underscores_until_no_collision() -> None:
    ctx = {"input": {"split_parts": 1, "split_parts_": 2}}
    assert _pick_parts_key(ctx) == "split_parts__"


def test_pick_parts_key_is_split_parts_with_no_collision() -> None:
    assert _pick_parts_key({"input": {}}) == "split_parts"


def test_pick_effect_name_avoids_an_existing_sibling_effect() -> None:
    ctx = {"prime": {"parts": {"value": "x"}}}
    assert _pick_effect_name("parts", ctx) == "parts_"
    assert _pick_effect_name("merge", ctx) == "merge"


def test_pick_effect_name_with_no_collision() -> None:
    assert _pick_effect_name("parts", {"prime": {}}) == "parts"


# --------------------------------------------------------------------------
# the generated plan
# --------------------------------------------------------------------------


def _text_prep(ref_path: str = "input.doc") -> _SplitPrep:
    return _SplitPrep(
        ref_path=ref_path,
        kind="string",
        parts=["chunk one", "chunk two"],
        sizes=[2, 2],
        template_text=f"Summarize: {{{{{ref_path}}}}}",
    )


def test_generated_plan_is_schema_valid() -> None:
    from circuitry.core.compiler import compile_orchestration

    defn = PromptDefinition(name="big", template="Summarize: {{input.doc}}")
    prep = _text_prep()
    _parts_key, plan, _result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode="join",
        split_def=None,
        n_parts=2,
        max_concurrency=None,
        ctx={},
    )
    # Compiles clean under the real orchestration compiler -- the same gate
    # every generated/planned plan runs through before execution.
    compile_orchestration(orch=plan, root_name="prime")


def test_generated_plan_yaml_carries_no_part_content() -> None:
    import yaml as _yaml

    defn = PromptDefinition(name="big", template="Summarize: {{input.doc}}")
    prep = _text_prep()
    _parts_key, plan, _result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode="join",
        split_def=None,
        n_parts=2,
        max_concurrency=None,
        ctx={},
    )
    dumped = _yaml.safe_dump(plan, sort_keys=False)
    assert "chunk one" not in dumped
    assert "chunk two" not in dumped


def test_generated_plan_copies_only_the_fields_the_source_set() -> None:
    defn = PromptDefinition(
        name="big",
        template="Summarize: {{input.doc}}",
        model="gpt-4.1",
        group="gpu",
    )
    prep = _text_prep()
    _parts_key, plan, _result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode="join",
        split_def=None,
        n_parts=2,
        max_concurrency=None,
        ctx={},
    )
    map_prompt = plan["effects"][0]["body"][0]
    assert map_prompt["model"] == "gpt-4.1"
    assert map_prompt["group"] == "gpu"
    for absent in ("provider", "provider_fallbacks", "params", "schema", "inputs", "assets"):
        assert absent not in map_prompt


def test_parts_key_never_collides_with_a_declared_input() -> None:
    defn = PromptDefinition(name="big", template="Summarize: {{input.doc}}")
    prep = _text_prep()
    parts_key, plan, _result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode="join",
        split_def=None,
        n_parts=2,
        max_concurrency=None,
        ctx={"input": {"split_parts": "already taken"}},
    )
    assert parts_key == "split_parts_"
    assert plan["interface"]["inputs"] == {parts_key: {"type": "array", "required": True}}


def test_reduce_plan_forwards_the_source_split_block_onto_map_and_merge() -> None:
    split_def = SplitDef(
        merge=SplitMergeDef(template="Combine: {{#parts}}<{{.}}>{{/parts}}"),
        separator=" | ",
        max_parts=4,
        budget_tokens=500,
    )
    defn = PromptDefinition(name="big", template="Summarize: {{input.doc}}", split=split_def)
    prep = _text_prep()
    _parts_key, plan, result_path = _generate_split_plan(
        defn,
        prep=prep,
        merge_mode="reduce",
        split_def=split_def,
        n_parts=2,
        max_concurrency=None,
        ctx={},
    )
    map_prompt = plan["effects"][0]["body"][0]
    merge_prompt = plan["effects"][1]
    assert merge_prompt["name"] == "merge"
    assert result_path == "prime.merge.value"
    for prompt in (map_prompt, merge_prompt):
        assert prompt["split"]["merge"]["template"] == "Combine: {{#parts}}<{{.}}>{{/parts}}"
        assert prompt["split"]["separator"] == " | "
        assert prompt["split"]["max_parts"] == 4
        assert prompt["split"]["budget_tokens"] == 500
    # The merge prompt's own template is rewritten against its loop's
    # collected value; the forwarded split block keeps the *original*,
    # unrewritten "parts" name so a further level rewrites it afresh.
    assert merge_prompt["template"] == (
        "Combine: {{#prime.parts.collected.value}}<{{.}}>{{/prime.parts.collected.value}}"
    )


def test_forwarded_split_config_is_none_without_a_split_block() -> None:
    assert _forwarded_split_config(None) is None


def test_forwarded_split_config_only_includes_fields_the_source_set() -> None:
    split_def = SplitDef(budget_tokens=200)
    out = _forwarded_split_config(split_def)
    assert out == {"budget_tokens": 200}


# --------------------------------------------------------------------------
# native merge
# --------------------------------------------------------------------------


def test_native_merge_join_uses_the_default_separator() -> None:
    defn = PromptDefinition(name="big", template="x", prompt_type="text")
    assert _apply_native_merge("join", ["a", "b"], defn=defn, separator=None) == "a\n\nb"


def test_native_merge_join_uses_a_custom_separator() -> None:
    defn = PromptDefinition(name="big", template="x", prompt_type="text")
    assert _apply_native_merge("join", ["a", "b"], defn=defn, separator=" | ") == "a | b"


def test_native_merge_join_rejects_a_non_string_part() -> None:
    defn = PromptDefinition(name="big", template="x", prompt_type="text")
    with pytest.raises(_MergeInvalidError):
        _apply_native_merge("join", ["a", 2], defn=defn, separator=None)


def test_native_merge_concat_flattens_in_order() -> None:
    defn = PromptDefinition(name="big", template="x", prompt_type="array")
    merged = _apply_native_merge(
        "concat", [["a", "b"], ["c"]], defn=defn, separator=None
    )
    assert merged == ["a", "b", "c"]


def test_native_merge_concat_rejects_a_non_array_part() -> None:
    defn = PromptDefinition(name="big", template="x", prompt_type="array")
    with pytest.raises(_MergeInvalidError):
        _apply_native_merge("concat", [["a"], "b"], defn=defn, separator=None)


def test_native_merge_concat_validates_against_the_schema() -> None:
    schema: dict[str, Any] = {"type": "array", "items": {"type": "string"}}
    defn = PromptDefinition(name="big", template="x", prompt_type="array", schema=schema)
    with pytest.raises(_MergeInvalidError):
        _apply_native_merge("concat", [["a"], [1]], defn=defn, separator=None)


def test_native_merge_concat_accepts_a_schema_valid_result() -> None:
    schema: dict[str, Any] = {"type": "array", "items": {"type": "string"}}
    defn = PromptDefinition(name="big", template="x", prompt_type="array", schema=schema)
    merged = _apply_native_merge("concat", [["a"], ["b"]], defn=defn, separator=None)
    assert merged == ["a", "b"]
