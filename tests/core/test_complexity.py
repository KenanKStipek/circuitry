"""Tests for the deterministic complexity scorer.

No network, no adapters, no store. The scorer is a pure function and these
tests treat it as one: every case is inputs in, breakdown out.
"""

from __future__ import annotations

import ast
import importlib.util
import json
import sys
from dataclasses import FrozenInstanceError, dataclass
from pathlib import Path
from typing import Any

import pytest

from circuitry.core.complexity import (
    DEFAULT_KEYWORD_WEIGHTS,
    DEFAULT_WEIGHTS,
    MAX_SCORE,
    MIN_SCORE,
    SIGNAL_NAMES,
    ComplexityScore,
    SignalScore,
    StructureContext,
    score,
)

# --------------------------------------------------------------------------
# Fixture set — the relative ordering these produce is the contract.
# --------------------------------------------------------------------------

TRIVIAL_REWRITE: dict[str, Any] = {
    "name": "tidy_headline",
    "type": "prompt",
    "template": "Rewrite {{prime.headline.value}} in plain English.",
    "prompt_type": "text",
}

MULTI_REFERENCE_SYNTHESIS: dict[str, Any] = {
    "name": "synthesize_positions",
    "type": "prompt",
    "template": (
        "Compare and synthesize the positions below into one account.\n\n"
        "Analyst: {{prime.analyst_note.value}}\n"
        "Customer: {{prime.customer_note.value}}\n"
        "Support: {{prime.support_thread.value}}\n"
        "Sales: {{prime.sales_call.value}}\n"
        "Legal: {{prime.legal_review.value}}\n\n"
        "Weigh where they disagree and explain why the disagreement matters."
    ),
    "prompt_type": "text",
}

SCHEMA_CONSTRAINED_EXTRACTION: dict[str, Any] = {
    "name": "extract_claims",
    "type": "prompt",
    "template": (
        "Extract every factual claim from the corpus below.\n\n"
        "{{prime.corpus.value}}\n\n"
        "Follow these rules exactly: {{prime.extraction_rules.value}}"
    ),
    "prompt_type": "array",
    "schema": {
        "type": "array",
        "maxItems": 50,
        "items": {
            "type": "object",
            "properties": {
                "claim": {"type": "string"},
                "speaker": {"type": "string"},
                "confidence": {"type": "number"},
                "evidence": {
                    "type": "object",
                    "properties": {
                        "quote": {"type": "string"},
                        "offset": {"type": "number"},
                    },
                },
            },
        },
    },
}

#: A big interpolated blob — what ``{{prime.corpus.value}}`` becomes at runtime.
LARGE_BLOB = "The quarterly report says revenue moved. " * 300


@dataclass(frozen=True)
class _DefinitionLike:
    """Stand-in for a compiled definition object (attributes, not keys)."""

    name: str
    template: str | None = None
    prompt_type: str = "text"
    schema: dict[str, Any] | None = None
    params: dict[str, Any] | None = None
    description: str | None = None
    messages: list[Any] | None = None


def _sum_contributions(result: ComplexityScore) -> float:
    return sum(entry.contribution for entry in result.signals)


# --------------------------------------------------------------------------
# Purity and determinism
# --------------------------------------------------------------------------


def test_repeated_calls_on_identical_inputs_are_identical() -> None:
    first = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)
    second = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    assert first == second
    assert first.score == second.score
    assert first.explain() == second.explain()
    assert json.dumps(first.to_dict(), sort_keys=True) == json.dumps(
        second.to_dict(), sort_keys=True
    )


def test_scoring_does_not_mutate_its_inputs() -> None:
    definition = dict(SCHEMA_CONSTRAINED_EXTRACTION)
    before = json.dumps(definition, sort_keys=True)
    weights = dict(DEFAULT_WEIGHTS)
    keywords = dict(DEFAULT_KEYWORD_WEIGHTS)

    score(definition, weights=weights, keyword_weights=keywords)

    assert json.dumps(definition, sort_keys=True) == before
    assert weights == dict(DEFAULT_WEIGHTS)
    assert keywords == dict(DEFAULT_KEYWORD_WEIGHTS)


def test_keyword_table_iteration_order_does_not_change_the_result() -> None:
    forward = dict(sorted(DEFAULT_KEYWORD_WEIGHTS.items()))
    backward = dict(sorted(DEFAULT_KEYWORD_WEIGHTS.items(), reverse=True))

    assert score(MULTI_REFERENCE_SYNTHESIS, keyword_weights=forward) == score(
        MULTI_REFERENCE_SYNTHESIS, keyword_weights=backward
    )


def test_module_imports_nothing_that_could_touch_the_world() -> None:
    """The scorer must not reach an adapter, the store, config, or IO."""
    source_path = (
        Path(__file__).resolve().parents[2]
        / "src"
        / "circuitry"
        / "core"
        / "complexity.py"
    )
    tree = ast.parse(source_path.read_text(encoding="utf-8"))

    imported: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            imported.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            # A relative import is a package import; none are allowed.
            assert node.level == 0, f"relative import of {node.module!r}"
            if node.module:
                imported.add(node.module.split(".")[0])

    assert imported <= {"__future__", "re", "dataclasses", "typing"}, imported


def test_module_loads_standalone_without_the_circuitry_package() -> None:
    """Loading the file on its own proves it has no package dependencies."""
    source_path = (
        Path(__file__).resolve().parents[2]
        / "src"
        / "circuitry"
        / "core"
        / "complexity.py"
    )
    spec = importlib.util.spec_from_file_location("_complexity_standalone", source_path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    # dataclasses resolves its own module out of sys.modules while building
    # each class, so register before executing and clean up afterwards.
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    finally:
        sys.modules.pop(spec.name, None)

    assert module.score(TRIVIAL_REWRITE).score == score(TRIVIAL_REWRITE).score


# --------------------------------------------------------------------------
# Explainability — the breakdown must reconstruct the total
# --------------------------------------------------------------------------


def test_every_signal_is_reported_once_in_declared_order() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION)

    assert tuple(entry.name for entry in result.signals) == SIGNAL_NAMES


def test_contributions_sum_to_the_total() -> None:
    for definition in (
        TRIVIAL_REWRITE,
        MULTI_REFERENCE_SYNTHESIS,
        SCHEMA_CONSTRAINED_EXTRACTION,
    ):
        result = score(definition, rendered_prompt=LARGE_BLOB)
        assert _sum_contributions(result) == pytest.approx(result.score, abs=1e-6)


def test_each_contribution_is_the_documented_weighted_share() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    assert result.weight_total == pytest.approx(sum(DEFAULT_WEIGHTS.values()))
    for entry in result.signals:
        expected = (
            MAX_SCORE * entry.weight * entry.normalized / result.weight_total
        )
        assert entry.contribution == pytest.approx(expected, abs=1e-6)
        assert entry.weight == pytest.approx(DEFAULT_WEIGHTS[entry.name])


def test_raw_measurements_are_carried_with_supporting_detail() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    size = result.signal("prompt_size")
    assert size is not None
    assert size.raw == pytest.approx(len(LARGE_BLOB) / 4, rel=0.05)
    assert size.detail["characters"] == len(LARGE_BLOB)
    assert size.detail["source"] == "rendered_prompt"

    references = result.signal("state_references")
    assert references is not None
    assert references.raw == 2
    assert references.detail["references"] == [
        "prime.corpus.value",
        "prime.extraction_rules.value",
    ]

    schema = result.signal("schema_shape")
    assert schema is not None
    assert schema.detail["fields"] == 6
    assert schema.detail["depth"] == 4

    output = result.signal("output_size")
    assert output is not None
    assert output.detail["maxItems"] == 50

    assert all(entry.note for entry in result.signals)


def test_to_dict_round_trips_through_json() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    payload = json.loads(json.dumps(result.to_dict()))

    assert payload["score"] == result.score
    assert payload["max_score"] == MAX_SCORE
    assert payload["mode"] == "rendered"
    assert [s["name"] for s in payload["signals"]] == list(SIGNAL_NAMES)
    assert sum(s["contribution"] for s in payload["signals"]) == pytest.approx(
        result.score, abs=1e-6
    )


def test_explain_names_every_signal_and_the_total() -> None:
    text = score(MULTI_REFERENCE_SYNTHESIS).explain()

    for name in SIGNAL_NAMES:
        assert name in text
    assert "total" in text
    assert "estimated" in text


def test_signal_lookup_returns_none_for_unknown_names() -> None:
    assert score(TRIVIAL_REWRITE).signal("no_such_signal") is None


# --------------------------------------------------------------------------
# Static vs rendered
# --------------------------------------------------------------------------


def test_static_mode_needs_no_rendered_prompt_and_is_marked_an_estimate() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION)

    assert result.mode == "static"
    assert result.estimated is True
    size = result.signal("prompt_size")
    assert size is not None
    assert size.estimated is True
    assert size.detail["source"] == "template"


def test_rendered_mode_measures_the_real_prompt_and_is_not_an_estimate() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    assert result.mode == "rendered"
    assert result.estimated is False
    assert all(not entry.estimated for entry in result.signals)


def test_interpolating_a_large_blob_raises_the_score_above_the_static_estimate() -> None:
    static = score(SCHEMA_CONSTRAINED_EXTRACTION)
    rendered = score(SCHEMA_CONSTRAINED_EXTRACTION, rendered_prompt=LARGE_BLOB)

    assert rendered.score > static.score


def test_state_references_are_read_from_the_template_even_in_rendered_mode() -> None:
    # The rendered prompt has had its references substituted away; counting
    # them there would report zero for every prompt.
    rendered = score(MULTI_REFERENCE_SYNTHESIS, rendered_prompt="all five, resolved")
    static = score(MULTI_REFERENCE_SYNTHESIS)

    assert rendered.signal("state_references") is not None
    assert static.signal("state_references") is not None
    assert (
        rendered.signal("state_references").raw  # type: ignore[union-attr]
        == static.signal("state_references").raw  # type: ignore[union-attr]
        == 5
    )


def test_a_rendered_prompt_alone_still_scores() -> None:
    result = score(rendered_prompt="Summarize the attached transcript.")

    assert result.mode == "rendered"
    assert result.score > MIN_SCORE


# --------------------------------------------------------------------------
# Signal behaviour
# --------------------------------------------------------------------------


def test_sections_and_triple_staches_count_as_references_comments_do_not() -> None:
    definition = {
        "name": "sectioned",
        "template": (
            "{{! a comment that is not a reference }}"
            "{{#prime.rows.value}}- {{{prime.row.value}}}\n{{/prime.rows.value}}"
            "{{^prime.rows.value}}nothing{{/prime.rows.value}}"
            "{{> partial_not_a_reference }}"
            "{{ prime.rows.value }}"
        ),
    }

    references = score(definition).signal("state_references")

    assert references is not None
    assert references.detail["references"] == ["prime.row.value", "prime.rows.value"]
    assert references.detail["sections"] == ["prime.rows.value"]


def test_messages_stand_in_for_a_missing_template() -> None:
    definition = {
        "name": "chat",
        "messages": [
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "Compare {{prime.a.value}} and {{prime.b.value}}."},
        ],
    }

    result = score(definition)

    references = result.signal("state_references")
    assert references is not None
    assert references.raw == 2
    size = result.signal("prompt_size")
    assert size is not None
    assert size.raw > 0


@pytest.mark.parametrize(
    "cheap,expensive",
    [("boolean", "text"), ("text", "json"), ("json", "array")],
)
def test_output_type_orders_from_cheap_to_structured(cheap: str, expensive: str) -> None:
    def _score(prompt_type: str) -> float:
        entry = score({"name": "x", "template": "go", "prompt_type": prompt_type}).signal(
            "output_type"
        )
        assert entry is not None
        return entry.normalized

    assert _score(cheap) < _score(expensive)


def test_declared_output_limits_raise_the_output_size_signal() -> None:
    unbounded = score(
        {"name": "x", "template": "go", "prompt_type": "array", "schema": {"type": "array"}}
    ).signal("output_size")
    bounded = score(
        {
            "name": "x",
            "template": "go",
            "prompt_type": "array",
            "schema": {"type": "array", "maxItems": 200},
        }
    ).signal("output_size")

    assert unbounded is not None and bounded is not None
    assert unbounded.raw == 0
    assert unbounded.normalized == 0.0
    assert bounded.raw > unbounded.raw
    assert bounded.normalized > unbounded.normalized


def test_max_tokens_param_counts_as_a_declared_output_limit() -> None:
    entry = score(
        {"name": "x", "template": "go", "params": {"max_tokens": 2000}}
    ).signal("output_size")

    assert entry is not None
    assert entry.raw == 2000
    assert entry.detail["max_tokens"] == 2000


def test_structural_position_raises_the_structure_signal() -> None:
    plain = score(TRIVIAL_REWRITE)
    nested = score(
        TRIVIAL_REWRITE,
        structure=StructureContext(depth=3, loop_depth=1, reflector_generated=True),
    )

    plain_entry = plain.signal("structure")
    nested_entry = nested.signal("structure")
    assert plain_entry is not None and nested_entry is not None
    assert plain_entry.normalized == 0.0
    assert nested_entry.normalized > plain_entry.normalized
    assert nested_entry.detail["in_loop"] is True
    assert nested_entry.detail["reflector_generated"] is True
    assert nested.score > plain.score


def test_structure_accepts_a_plain_mapping_with_the_in_loop_shorthand() -> None:
    from_mapping = score(TRIVIAL_REWRITE, structure={"depth": 2, "in_loop": True})
    from_context = score(
        TRIVIAL_REWRITE, structure=StructureContext(depth=2, loop_depth=1)
    )

    assert from_mapping == from_context


def test_keyword_matching_is_stem_tolerant_and_reported() -> None:
    entry = score(
        {"name": "x", "template": "Analyzing the corpus, then reason about it."},
        keyword_weights={"analyze": 0.5, "reason": 0.25, "translate": 0.5},
    ).signal("keywords")

    assert entry is not None
    assert entry.raw == 2
    assert entry.detail["matched"] == {"analyze": 0.5, "reason": 0.25}
    assert entry.normalized == pytest.approx(0.75)


def test_keyword_matching_respects_word_boundaries() -> None:
    entry = score(
        {"name": "x", "template": "The planetarium is a reasonably nice building."},
        keyword_weights={"plan": 1.0, "reason": 1.0},
    ).signal("keywords")

    assert entry is not None
    assert entry.raw == 0


def test_an_empty_keyword_table_disables_the_signal() -> None:
    entry = score(MULTI_REFERENCE_SYNTHESIS, keyword_weights={}).signal("keywords")

    assert entry is not None
    assert entry.raw == 0
    assert entry.contribution == 0.0


def test_keyword_weights_saturate_rather_than_overflow() -> None:
    entry = score(
        {"name": "x", "template": "Analyze, compare, and synthesize."},
        keyword_weights={"analyze": 5.0, "compare": 5.0, "synthesize": 5.0},
    ).signal("keywords")

    assert entry is not None
    assert entry.normalized == 1.0


def test_a_definition_object_scores_the_same_as_the_equivalent_mapping() -> None:
    obj = _DefinitionLike(
        name=SCHEMA_CONSTRAINED_EXTRACTION["name"],
        template=SCHEMA_CONSTRAINED_EXTRACTION["template"],
        prompt_type=SCHEMA_CONSTRAINED_EXTRACTION["prompt_type"],
        schema=SCHEMA_CONSTRAINED_EXTRACTION["schema"],
    )

    assert score(obj).score == score(SCHEMA_CONSTRAINED_EXTRACTION).score


def test_a_compiled_prompt_definition_scores_the_same_as_its_source_mapping() -> None:
    # The runtime (#103) holds compiled definitions, `cof score` (#104) holds
    # raw YAML mappings. Both paths must land on the same number.
    from circuitry.core.compiler import compile_orchestration

    compiled = compile_orchestration(
        orch={"name": "fixture", "effects": [dict(SCHEMA_CONSTRAINED_EXTRACTION)]}
    )

    assert score(compiled.effects[0]).score == score(
        SCHEMA_CONSTRAINED_EXTRACTION
    ).score


# --------------------------------------------------------------------------
# Weights
# --------------------------------------------------------------------------


def test_default_weights_cover_every_signal() -> None:
    assert set(DEFAULT_WEIGHTS) == set(SIGNAL_NAMES)


def test_a_partial_weight_table_falls_back_to_the_defaults() -> None:
    result = score(SCHEMA_CONSTRAINED_EXTRACTION, weights={"prompt_size": 0.5})

    size = result.signal("prompt_size")
    schema = result.signal("schema_shape")
    assert size is not None and schema is not None
    assert size.weight == 0.5
    assert schema.weight == pytest.approx(DEFAULT_WEIGHTS["schema_shape"])


def test_reweighting_shifts_emphasis_without_leaving_the_range() -> None:
    references_only = score(
        MULTI_REFERENCE_SYNTHESIS,
        weights={name: (1.0 if name == "state_references" else 0.0) for name in SIGNAL_NAMES},
    )

    entry = references_only.signal("state_references")
    assert entry is not None
    assert references_only.score == pytest.approx(MAX_SCORE * entry.normalized)
    assert MIN_SCORE <= references_only.score <= MAX_SCORE


def test_scaling_every_weight_leaves_the_score_unchanged() -> None:
    base = score(SCHEMA_CONSTRAINED_EXTRACTION)
    scaled = score(
        SCHEMA_CONSTRAINED_EXTRACTION,
        weights={name: weight * 7 for name, weight in DEFAULT_WEIGHTS.items()},
    )

    assert scaled.score == pytest.approx(base.score, abs=1e-6)


def test_all_zero_weights_score_zero_with_a_warning() -> None:
    result = score(
        SCHEMA_CONSTRAINED_EXTRACTION, weights=dict.fromkeys(SIGNAL_NAMES, 0.0)
    )

    assert result.score == MIN_SCORE
    assert all(entry.contribution == 0.0 for entry in result.signals)
    assert any("every weight is zero" in warning for warning in result.warnings)


def test_the_score_stays_inside_the_documented_range_under_extremes() -> None:
    absurd = {
        "name": "everything",
        "template": "Prove, synthesize, and cross-reference. " * 5000
        + " ".join(f"{{{{prime.ref_{i}.value}}}}" for i in range(200)),
        "prompt_type": "array",
        "schema": {
            "type": "array",
            "maxItems": 100000,
            "maxLength": 1000000,
            "items": {"type": "object", "properties": {f"f{i}": {} for i in range(200)}},
        },
        "params": {"max_tokens": 1000000},
    }

    result = score(
        absurd,
        rendered_prompt="x" * 4_000_000,
        structure=StructureContext(depth=99, loop_depth=9, reflector_generated=True),
    )

    assert MIN_SCORE <= result.score <= MAX_SCORE
    assert all(0.0 <= entry.normalized <= 1.0 for entry in result.signals)


# --------------------------------------------------------------------------
# Relative ordering — the part band tables are written against
# --------------------------------------------------------------------------


def test_default_weights_order_the_fixture_set_by_difficulty() -> None:
    trivial = score(TRIVIAL_REWRITE)
    synthesis = score(MULTI_REFERENCE_SYNTHESIS)
    extraction = score(
        SCHEMA_CONSTRAINED_EXTRACTION,
        rendered_prompt=SCHEMA_CONSTRAINED_EXTRACTION["template"].replace(
            "{{prime.corpus.value}}", LARGE_BLOB
        ),
    )

    assert trivial.score < synthesis.score < extraction.score


def test_the_ordering_holds_with_every_prompt_scored_statically() -> None:
    # `cof score` has no rendered prompts to work with; the ordering has to
    # survive template-only scoring too.
    trivial = score(TRIVIAL_REWRITE)
    synthesis = score(MULTI_REFERENCE_SYNTHESIS)
    extraction = score(SCHEMA_CONSTRAINED_EXTRACTION)

    assert trivial.score < synthesis.score < extraction.score


def test_the_ordering_is_not_a_photo_finish() -> None:
    # Bands are drawn between these, so neighbouring fixtures need daylight
    # between them rather than a rounding difference.
    trivial = score(TRIVIAL_REWRITE).score
    synthesis = score(MULTI_REFERENCE_SYNTHESIS).score
    extraction = score(SCHEMA_CONSTRAINED_EXTRACTION).score

    assert synthesis - trivial > 5.0
    assert extraction - synthesis > 5.0


# --------------------------------------------------------------------------
# Graceful degradation — nothing here may raise
# --------------------------------------------------------------------------


def test_scoring_nothing_at_all_degrades_to_a_warning() -> None:
    result = score(None)

    assert MIN_SCORE <= result.score <= MAX_SCORE
    assert result.mode == "static"
    assert any("nothing to score" in warning for warning in result.warnings)


@pytest.mark.parametrize("junk", ["a string", 7, 3.5, True, b"bytes"])
def test_a_scalar_definition_degrades_instead_of_raising(junk: Any) -> None:
    result = score(junk)

    assert any("definition" in warning for warning in result.warnings)
    assert MIN_SCORE <= result.score <= MAX_SCORE


def test_a_definition_with_no_template_or_messages_degrades() -> None:
    result = score({"name": "empty", "prompt_type": "text"})

    assert any("no template or messages" in warning for warning in result.warnings)
    size = result.signal("prompt_size")
    assert size is not None
    assert size.raw == 0


@pytest.mark.parametrize(
    "definition,fragment",
    [
        ({"name": "x", "template": 42}, "template"),
        ({"name": "x", "template": "go", "prompt_type": "hologram"}, "prompt_type"),
        ({"name": "x", "template": "go", "prompt_type": 7}, "prompt_type"),
        ({"name": "x", "template": "go", "schema": "not-a-mapping"}, "schema"),
    ],
)
def test_malformed_fields_warn_rather_than_raise(
    definition: dict[str, Any], fragment: str
) -> None:
    result = score(definition)

    assert any(fragment in warning for warning in result.warnings)
    assert MIN_SCORE <= result.score <= MAX_SCORE


def test_an_unknown_prompt_type_is_scored_as_text() -> None:
    unknown = score({"name": "x", "template": "go", "prompt_type": "hologram"})
    text = score({"name": "x", "template": "go", "prompt_type": "text"})

    unknown_entry = unknown.signal("output_type")
    text_entry = text.signal("output_type")
    assert unknown_entry is not None and text_entry is not None
    assert unknown_entry.normalized == text_entry.normalized
    assert unknown_entry.detail["prompt_type"] == "hologram"


def test_junk_weights_warn_and_fall_back() -> None:
    result = score(
        TRIVIAL_REWRITE,
        weights={"prompt_size": "heavy", "unknown_signal": 1.0, "structure": -3.0},
    )

    joined = " ".join(result.warnings)
    assert "not a number" in joined
    assert "unknown signal" in joined
    assert "negative" in joined

    size = result.signal("prompt_size")
    structure = result.signal("structure")
    assert size is not None and structure is not None
    assert size.weight == pytest.approx(DEFAULT_WEIGHTS["prompt_size"])
    assert structure.weight == 0.0


@pytest.mark.parametrize("junk", ["weights", 3, [("prompt_size", 1.0)]])
def test_a_non_mapping_weight_table_falls_back_to_the_defaults(junk: Any) -> None:
    result = score(TRIVIAL_REWRITE, weights=junk)

    assert any("expected a mapping" in warning for warning in result.warnings)
    assert result.weight_total == pytest.approx(sum(DEFAULT_WEIGHTS.values()))
    assert [entry.weight for entry in result.signals] == [
        pytest.approx(DEFAULT_WEIGHTS[name]) for name in SIGNAL_NAMES
    ]
    assert result.score == score(TRIVIAL_REWRITE).score


@pytest.mark.parametrize(
    "junk",
    [
        "top-level",
        7,
        {"depth": "deep"},
        {"depth": -4},
        {"loop_depth": None, "reflector_generated": None},
    ],
)
def test_a_malformed_structure_context_degrades_to_top_level(junk: Any) -> None:
    result = score(TRIVIAL_REWRITE, structure=junk)

    entry = result.signal("structure")
    assert entry is not None
    assert entry.detail["depth"] == 0
    assert MIN_SCORE <= result.score <= MAX_SCORE


def test_a_non_mapping_keyword_table_falls_back_to_the_defaults() -> None:
    result = score(MULTI_REFERENCE_SYNTHESIS, keyword_weights=["synthesize"])  # type: ignore[arg-type]

    assert any("keyword_weights" in warning for warning in result.warnings)
    assert result.signal("keywords") == score(MULTI_REFERENCE_SYNTHESIS).signal(
        "keywords"
    )


def test_junk_inside_the_keyword_table_is_skipped() -> None:
    entry = score(
        {"name": "x", "template": "Synthesize the notes."},
        keyword_weights={"synthesize": 0.4, "": 1.0, "compare": "heavy", 7: 1.0, "plan": -1},  # type: ignore[dict-item]
    ).signal("keywords")

    assert entry is not None
    assert entry.detail["matched"] == {"synthesize": 0.4}


def test_a_malformed_schema_still_scores_the_rest_of_the_signals() -> None:
    result = score(
        {
            "name": "x",
            "template": "Extract from {{prime.blob.value}}",
            "prompt_type": "array",
            "schema": {"type": "array", "maxItems": "fifty", "items": "not-a-schema"},
        }
    )

    schema = result.signal("schema_shape")
    output = result.signal("output_size")
    assert schema is not None and output is not None
    assert schema.raw == 0
    assert output.raw == 0
    assert result.score > MIN_SCORE


def test_a_non_string_rendered_prompt_falls_back_to_static_mode() -> None:
    result = score(TRIVIAL_REWRITE, rendered_prompt=object())  # type: ignore[arg-type]

    assert result.mode == "static"
    assert any("rendered_prompt" in warning for warning in result.warnings)


# --------------------------------------------------------------------------
# Demo — a fixture orchestration's effects, scored and ordered
# --------------------------------------------------------------------------


def test_demo_orders_a_fixture_orchestrations_effects_and_explains_the_top(
    capsys: pytest.CaptureFixture[str],
) -> None:
    orchestration = {
        "name": "release_notes",
        "effects": [
            TRIVIAL_REWRITE,
            MULTI_REFERENCE_SYNTHESIS,
            SCHEMA_CONSTRAINED_EXTRACTION,
        ],
    }

    scored: list[tuple[str, ComplexityScore]] = [
        (str(effect["name"]), score(effect)) for effect in orchestration["effects"]
    ]
    ranked = sorted(scored, key=lambda pair: pair[1].score, reverse=True)

    print(f"\norchestration: {orchestration['name']}")
    for name, result in ranked:
        print(f"  {result.score:6.2f}  {name}")
    print()
    print(ranked[0][1].explain())

    out = capsys.readouterr().out
    assert ranked[0][0] == "extract_claims"
    assert [name for name, _ in ranked] == [
        "extract_claims",
        "synthesize_positions",
        "tidy_headline",
    ]
    for name in SIGNAL_NAMES:
        assert name in out


def test_signal_score_is_immutable() -> None:
    entry = score(TRIVIAL_REWRITE).signals[0]

    assert isinstance(entry, SignalScore)
    with pytest.raises(FrozenInstanceError):
        entry.contribution = 1.0  # type: ignore[misc]
