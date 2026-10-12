#!/usr/bin/env python3
"""A differential corpus for `electricity-complexity` (electricity M1 lane
K1, issue #505): every case asks ``circuitry.core.complexity.score`` --
Circuitry's own real function, never a re-derivation of it -- for the
answer, and records the exact wire shape ``core/prompt.py::_complexity_meta``
would write to ``meta.complexity``, plus each signal's own ``estimated``
flag (dropped from ``_complexity_meta`` itself, but still part of what the
Rust port's ``SignalScore`` has to reproduce).

Each case's ``input`` is exactly ``score()``'s own keyword arguments
(``definition``, ``rendered_prompt``, ``weights``, ``keyword_weights``,
``structure``), JSON-serializable throughout (``json.dumps``'s own
``allow_nan=True`` default carries ``NaN``/``Infinity``/``-Infinity``
through verbatim, and a Python integer of any size round-trips as a bare
JSON integer literal) -- the Rust replay (`tests/corpus.rs`) builds an
`electricity_complexity::ScoreInput` straight from this, no translation.

A case that raises records the exception's class name and message instead
of a result (`score()`'s only raise path: `float(some_too-large_int)`'s own
`OverflowError`).

Must be run with Python 3.11 (`generator-constraints.txt`), as CI does.
Usage: python3 generate_complexity_corpus.py [--check]
"""

from __future__ import annotations

import json
import random
import sys
from pathlib import Path
from typing import Any

from circuitry.cli.complexity_config import (
    ComplexityBand,
    ComplexityConfigError,
    RoutingSettings,
    band_named,
    parse_complexity_settings,
)
from circuitry.core.compiler import _compile_prompt
from circuitry.core.complexity import (
    DEFAULT_KEYWORD_WEIGHTS,
    SIGNAL_NAMES,
    score,
)
from circuitry.core.prompt import _complexity_meta
from circuitry.core.router import route_model

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-complexity"
    / "tests"
    / "golden"
    / "complexity_corpus.json"
)

_UNSET = object()


def run_case(
    description: str,
    *,
    definition: Any = _UNSET,
    rendered_prompt: Any = _UNSET,
    weights: Any = _UNSET,
    keyword_weights: Any = _UNSET,
    structure: Any = _UNSET,
) -> dict[str, Any]:
    kwargs: dict[str, Any] = {}
    input_json: dict[str, Any] = {}
    if definition is not _UNSET:
        kwargs["definition"] = definition
        input_json["definition"] = definition
    if rendered_prompt is not _UNSET:
        kwargs["rendered_prompt"] = rendered_prompt
        input_json["rendered_prompt"] = rendered_prompt
    if weights is not _UNSET:
        kwargs["weights"] = weights
        input_json["weights"] = weights
    if keyword_weights is not _UNSET:
        kwargs["keyword_weights"] = keyword_weights
        input_json["keyword_weights"] = keyword_weights
    if structure is not _UNSET:
        kwargs["structure"] = structure
        input_json["structure"] = structure

    try:
        result = score(**kwargs)
    except Exception as exc:
        return {
            "description": description,
            "input": input_json,
            "error": {"class": type(exc).__name__, "message": str(exc)},
        }

    meta = _complexity_meta(result)
    estimated_signals = sorted(s.name for s in result.signals if s.estimated)
    return {
        "description": description,
        "input": input_json,
        "expected": {"meta": meta, "estimated_signals": estimated_signals},
    }


def _basic_definition(**fields: Any) -> dict[str, Any]:
    return fields


def family_prompt_size() -> list[dict[str, Any]]:
    cases = []
    for n in [0, 1, 2, 3, 4, 8, 9, 799, 800, 801, 3199, 3200, 3201]:
        text = "x" * n
        cases.append(run_case(f"prompt_size static length {n}", definition=_basic_definition(template=text)))
        cases.append(run_case(f"prompt_size rendered length {n}", rendered_prompt=text))
    cases.append(run_case("prompt_size rendered empty string", rendered_prompt=""))
    cases.append(run_case("prompt_size cjk", rendered_prompt="你好世界" * 50))
    cases.append(run_case("prompt_size emoji", rendered_prompt="😀🎉👍" * 50))
    cases.append(run_case("prompt_size zwj sequence", rendered_prompt="👨‍👩‍👧‍👦" * 20))
    cases.append(run_case("prompt_size combining marks", rendered_prompt="e\u0301" * 100))
    cases.append(run_case("prompt_size 1MB text", rendered_prompt="a" * (1024 * 1024)))
    return cases


def family_state_references() -> list[dict[str, Any]]:
    templates = [
        "{{plain}}",
        "{{ dotted.name }}",
        "{{ spaced name }}",
        "{{{triple}}}",
        "{{&amp}}",
        "{{#section}}body{{/section}}",
        "{{^inverted}}body{{/inverted}}",
        "{{! a comment }}",
        "{{> partial}}",
        "{{.}}",
        "{{}}",
        "{{ }}",
        "{{x}} {{x}}",
        "{{{{x}}}}",
        "{{x}}}",
        "unclosed {{x",
        "{{x\ny}}",
        "{{=<% %>=}}",
        "{{\x1cx}}",
        "{{\u00a0x}}",
        "{{\u3000x}}",
        "{{café}}",
        "a{{one}}b{{two}}c",
    ]
    cases = [
        run_case(f"state_references template {t!r}", definition=_basic_definition(template=t))
        for t in templates
    ]
    cases.append(
        run_case(
            "state_references tag spans two messages",
            definition=_basic_definition(
                messages=[{"role": "user", "content": "{{span"}, {"role": "assistant", "content": "ning}}"}]
            ),
        )
    )
    return cases


def family_prompt_type() -> list[dict[str, Any]]:
    types = ["boolean", "number", "text", "tool", "json", "object", "array"]
    cases = []
    for t in types:
        cases.append(run_case(f"prompt_type {t}", definition=_basic_definition(template="x", prompt_type=t)))
        cases.append(
            run_case(f"prompt_type {t} upper/spaced", definition=_basic_definition(template="x", prompt_type=f"  {t.upper()}  "))
        )
    cases.append(run_case("prompt_type image (unknown)", definition=_basic_definition(template="x", prompt_type="image")))
    cases.append(run_case("prompt_type empty string", definition=_basic_definition(template="x", prompt_type="")))
    cases.append(run_case("prompt_type non-string int", definition=_basic_definition(template="x", prompt_type=5)))
    cases.append(run_case("prompt_type non-string bool", definition=_basic_definition(template="x", prompt_type=True)))
    cases.append(run_case("prompt_type absent", definition=_basic_definition(template="x")))
    return cases


def family_output_schema() -> list[dict[str, Any]]:
    cases = [run_case("output_schema none", definition=_basic_definition(template="x"))]
    cases.append(run_case("output_schema empty", definition=_basic_definition(template="x", schema={})))
    cases.append(
        run_case(
            "output_schema nested properties",
            definition=_basic_definition(
                template="x",
                schema={
                    "properties": {
                        "a": {"type": "string"},
                        "b": {"properties": {"c": {"type": "number"}, "d": {"type": "number"}}},
                    }
                },
            ),
        )
    )
    cases.append(
        run_case(
            "output_schema items mapping",
            definition=_basic_definition(template="x", schema={"items": {"properties": {"a": {}}}}),
        )
    )
    cases.append(
        run_case(
            "output_schema items list",
            definition=_basic_definition(
                template="x", schema={"items": [{"properties": {"a": {}}}, {"properties": {"b": {}}}]}
            ),
        )
    )
    cases.append(
        run_case(
            "output_schema combinators",
            definition=_basic_definition(
                template="x",
                schema={"oneOf": [{"properties": {"a": {}}}], "anyOf": [{"properties": {"b": {}}}], "allOf": [{"properties": {"c": {}}}]},
            ),
        )
    )
    cases.append(
        run_case(
            "output_schema non-mapping children",
            definition=_basic_definition(template="x", schema={"properties": {"a": 1, "b": "x", "c": None}}),
        )
    )
    cases.append(
        run_case(
            "output_schema properties not a mapping",
            definition=_basic_definition(template="x", schema={"properties": [1, 2, 3]}),
        )
    )

    def deep_schema(depth: int) -> dict[str, Any]:
        node: dict[str, Any] = {"type": "string"}
        for _ in range(depth):
            node = {"properties": {"a": node}}
        return node

    cases.append(run_case("output_schema depth 50", definition=_basic_definition(template="x", schema=deep_schema(50))))
    wide_schema = {"properties": {f"field{i}": {"type": "string"} for i in range(1000)}}
    cases.append(run_case("output_schema width 1000", definition=_basic_definition(template="x", schema=wide_schema)))
    cases.append(run_case("output_schema not a mapping", definition=_basic_definition(template="x", schema="nope")))
    return cases


def family_output_size() -> list[dict[str, Any]]:
    cases = []
    cases.extend(
        run_case(
            f"output_size params.{key}={value!r}",
            definition=_basic_definition(template="x", params={key: value}),
        )
        for key in ["max_tokens", "num_predict", "max_output_tokens"]
        for value in [100, 100.0, True, "100", 0, -5, float("nan"), float("inf")]
    )
    cases.extend(
        run_case(
            f"output_size schema.{key}={value!r}",
            definition=_basic_definition(template="x", schema={key: value}),
        )
        for key in ["maxLength", "maxItems", "maxProperties"]
        for value in [400, 400.0, True, "400", 0, -5, float("nan"), float("inf")]
    )
    cases.append(
        run_case(
            "output_size maxItems with items.maxLength",
            definition=_basic_definition(template="x", schema={"maxItems": 10, "items": {"maxLength": 40}}),
        )
    )
    cases.append(
        run_case(
            "output_size maxItems without items.maxLength",
            definition=_basic_definition(template="x", schema={"maxItems": 10}),
        )
    )
    cases.append(
        run_case(
            "output_size tie between candidates",
            definition=_basic_definition(template="x", params={"max_tokens": 128}, schema={"maxLength": 512}),
        )
    )
    cases.extend(
        run_case(
            f"output_size half-even tie maxLength={n}",
            definition=_basic_definition(template="x", schema={"maxLength": n * 4 + 2}),
        )
        for n in [2, 6, 10, 14]
    )
    cases.append(
        run_case(
            "output_size fractional max_tokens half-even",
            definition=_basic_definition(template="x", params={"max_tokens": 2.5}),
        )
    )
    cases.append(
        run_case(
            "output_size overflow raise",
            definition=_basic_definition(template="x", params={"max_tokens": 10**400}),
        )
    )
    return cases


def family_structural_position() -> list[dict[str, Any]]:
    cases = [
        run_case(f"structural_position depth {depth}", rendered_prompt="x", structure={"depth": depth})
        for depth in range(11)
    ]
    cases.extend(
        run_case(
            f"structural_position loop_depth {loop_depth}",
            rendered_prompt="x",
            structure={"depth": 1, "loop_depth": loop_depth},
        )
        for loop_depth in [0, 1, 2, 5]
    )
    cases.append(run_case("structural_position reflector", rendered_prompt="x", structure={"depth": 1, "reflector_generated": True}))
    cases.append(run_case("structural_position in_loop true", rendered_prompt="x", structure={"in_loop": True}))
    cases.append(run_case("structural_position in_loop with loop_depth 0", rendered_prompt="x", structure={"loop_depth": 0, "in_loop": True}))
    # from_any junk branches, including warning order.
    cases.append(run_case("structural_position structure none", rendered_prompt="x", structure=None))
    cases.append(run_case("structural_position structure absent", rendered_prompt="x"))
    cases.append(run_case("structural_position structure not a mapping", rendered_prompt="x", structure="nope"))
    cases.append(run_case("structural_position structure a list", rendered_prompt="x", structure=[1, 2, 3]))
    cases.append(run_case("structural_position depth negative", rendered_prompt="x", structure={"depth": -3}))
    cases.append(run_case("structural_position loop_depth negative", rendered_prompt="x", structure={"loop_depth": -2}))
    cases.append(run_case("structural_position depth not a number", rendered_prompt="x", structure={"depth": "three"}))
    cases.append(run_case("structural_position loop_depth not a number", rendered_prompt="x", structure={"loop_depth": "two"}))
    cases.append(run_case("structural_position depth and loop_depth both junk", rendered_prompt="x", structure={"depth": "x", "loop_depth": "y"}))
    cases.append(run_case("structural_position depth is none explicit", rendered_prompt="x", structure={"depth": None}))
    cases.append(run_case("structural_position depth is false", rendered_prompt="x", structure={"depth": False}))
    cases.append(run_case("structural_position depth is zero", rendered_prompt="x", structure={"depth": 0}))
    cases.append(run_case("structural_position depth float truncation", rendered_prompt="x", structure={"depth": 3.9}))
    cases.append(run_case("structural_position depth nan raises", rendered_prompt="x", structure={"depth": float("nan")}))
    cases.append(run_case("structural_position depth inf raises", rendered_prompt="x", structure={"depth": float("inf")}))
    cases.append(run_case("structural_position loop_depth nan raises", rendered_prompt="x", structure={"loop_depth": float("nan")}))
    cases.append(run_case("structural_position loop_depth inf raises", rendered_prompt="x", structure={"loop_depth": float("inf")}))
    cases.append(run_case("structural_position depth negative infinity treated as zero", rendered_prompt="x", structure={"depth": float("-inf")}))
    return cases


def family_keywords() -> list[dict[str, Any]]:
    suffixes = ["", "e", "es", "s", "ed", "ing", "ion", "ions"]

    def phrase_and_base(phrase: str) -> tuple[str, str]:
        stem = phrase.lower().rstrip()
        base = stem[:-1] if len(stem) > 3 and stem.endswith("e") else stem
        return phrase, base

    cases = [
        run_case(f"keyword {phrase!r} + {suffix!r}", rendered_prompt=f"please {base}{suffix} this")
        for phrase, base in (phrase_and_base(p) for p in list(DEFAULT_KEYWORD_WEIGHTS)[:6])
        for suffix in suffixes
    ]
    cases.extend(
        run_case(f"keyword near-miss {near_miss!r}", rendered_prompt=f"a {near_miss} example")
        for near_miss in ["planning", "inference", "reasonable"]
    )

    boundary_cases = [
        "under_score_analyze",
        "analyze_under",
        "123analyze",
        "analyze123",
        "a\u0301nalyze",  # combining mark
        "分析analyze",  # CJK next to the word
        "analyze's",
        "co-analyze",
    ]
    cases.extend(run_case(f"keyword boundary {text!r}", rendered_prompt=text) for text in boundary_cases)

    cases.append(run_case("keyword turkish dotted I", rendered_prompt="İstanbul", keyword_weights={"istanbul": 0.5}))
    cases.append(run_case("keyword greek final sigma", rendered_prompt="ΟΔΥΣΣΕΎΣ", keyword_weights={"οδυσσεύς": 0.5}))
    cases.append(run_case("keyword capital sharp s", rendered_prompt="GROSSE STRAßE ẞ", keyword_weights={"straße": 0.5}))

    cases.append(run_case("keyword table empty disables", rendered_prompt="analyze this", keyword_weights={}))
    cases.append(run_case("keyword table not a mapping", rendered_prompt="analyze this", keyword_weights="nope"))
    cases.append(
        run_case(
            "keyword table weird weights",
            rendered_prompt="analyze compare critique",
            keyword_weights={
                "analyze": 0,
                "compare": -1,
                "critique": float("nan"),
                "deduce": float("inf"),
                "extract": True,
            },
        )
    )
    cases.append(
        run_case(
            "keyword table whitespace and mixed case keys",
            rendered_prompt="ANALYZE this",
            keyword_weights={"  Analyze  ": 0.4},
        )
    )
    cases.append(
        run_case(
            "keyword table short stem ending in e",
            rendered_prompt="we see",
            keyword_weights={"we": 0.5},
        )
    )
    cases.append(
        run_case(
            "keyword table regex metacharacters",
            rendered_prompt="c++ is fun, a-b too, tab\tseparated",
            keyword_weights={"c++": 0.2, "a-b": 0.3, "tab\tseparated": 0.1},
        )
    )
    cases.append(
        run_case(
            "keyword table backslash phrases match literally",
            rendered_prompt="see a\\d here, and a1 and a\\ too",
            keyword_weights={"a\\d": 0.4, "a\\": 0.3, "\\": 0.2},
        )
    )
    cases.append(
        run_case(
            "keyword table backslash phrase does not act as a digit class",
            rendered_prompt="only a1 and a2 here",
            keyword_weights={"a\\d": 0.4},
        )
    )
    cases.append(
        run_case(
            "keyword description contributes",
            definition=_basic_definition(template="plain text", description="please analyze this"),
        )
    )
    cases.append(
        run_case(
            "keyword static vs rendered haystack",
            definition=_basic_definition(template="analyze this template"),
        )
    )
    cases.append(
        run_case(
            "keyword clamp above one",
            rendered_prompt="prove deduce reconcile synthesize cross-reference chain of thought step by step",
        )
    )
    return cases


def family_weights() -> list[dict[str, Any]]:
    cases = [run_case("weights none", rendered_prompt="x", weights=None)]
    cases.append(run_case("weights partial", rendered_prompt="x", weights={"prompt_size": 2.0}))
    cases.append(run_case("weights unknown keys sorted", rendered_prompt="x", weights={"zeta": 1, "alpha": 1, "prompt_size": 1}))
    cases.append(run_case("weights not a mapping", rendered_prompt="x", weights="nope"))
    cases.append(run_case("weights non-number", rendered_prompt="x", weights={"prompt_size": "big"}))
    cases.append(run_case("weights negative", rendered_prompt="x", weights={"prompt_size": -2}))
    cases.append(run_case("weights all zero", rendered_prompt="x", weights=dict.fromkeys(SIGNAL_NAMES, 0)))
    cases.append(run_case("weights nan", rendered_prompt="x", weights={"prompt_size": float("nan")}))
    cases.append(run_case("weights inf", rendered_prompt="x", weights={"prompt_size": float("inf")}))
    cases.append(run_case("weights round to zero", rendered_prompt="x", weights={"prompt_size": 0.0000001}))
    cases.append(run_case("weights scorer default table", rendered_prompt="x"))
    config_default_weights = {
        "prompt_size": 1.0,
        "state_references": 1.5,
        "prompt_type": 0.75,
        "output_schema": 1.25,
        "output_size": 1.0,
        "structural_position": 0.5,
        "keywords": 1.0,
    }
    cases.append(run_case("weights config default table", rendered_prompt="x", weights=config_default_weights))
    return cases


def family_definition_shape() -> list[dict[str, Any]]:
    cases = [run_case("definition none no rendered")]
    cases.append(run_case("definition none with rendered", rendered_prompt="x"))
    cases.extend(
        run_case(f"definition scalar {scalar!r}", definition=scalar) for scalar in ["text", 5, 5.0, True]
    )
    cases.append(run_case("definition non-string template", definition=_basic_definition(template=5)))
    cases.append(run_case("definition empty template with messages", definition=_basic_definition(template="", messages=[{"role": "user", "content": "hi"}])))
    cases.append(run_case("definition non-string message content", definition=_basic_definition(messages=[{"role": "user", "content": 5}])))
    cases.append(run_case("definition empty message content", definition=_basic_definition(messages=[{"role": "user", "content": ""}])))
    cases.append(run_case("definition messages not a list", definition=_basic_definition(messages="nope")))
    cases.append(run_case("definition static no template or messages", definition=_basic_definition(prompt_type="text")))
    return cases


def family_rounding_stress() -> list[dict[str, Any]]:
    cases = []
    # Weights chosen to push contributions/weight_total/score near a
    # 6-decimal tie and near the 100.0 clamp.
    cases.append(
        run_case(
            "rounding stress near tie weights",
            rendered_prompt="analyze this and prove that",
            weights={
                "prompt_size": 0.0078125,
                "state_references": 0.0234375,
                "prompt_type": 1.0078125,
                "output_schema": 123.0078125,
                "output_size": 9999.9921875,
                "structural_position": 0.1,
                "keywords": 0.2,
            },
        )
    )
    cases.append(
        run_case(
            "rounding stress clamp at 100",
            definition=_basic_definition(
                template="{{a}}{{b}}{{c}}{{d}}" * 50,
                prompt_type="array",
                schema={"properties": {f"f{i}": {} for i in range(50)}},
                params={"max_tokens": 100000},
                description="analyze compare critique deduce derive evaluate",
            ),
            rendered_prompt="x" * 5000,
            structure={"depth": 10, "loop_depth": 5, "reflector_generated": True},
        )
    )
    return cases


def family_seeded_random(count: int) -> list[dict[str, Any]]:
    rng = random.Random(505_448)
    cases = []
    type_pool = ["boolean", "number", "text", "tool", "json", "object", "array", "", "bogus", None, 5]
    keyword_pool = list(DEFAULT_KEYWORD_WEIGHTS)
    alphabet = "abcdefghij ABCDEFGHIJ 0123456789 {}#^/&!>.\n\t"

    def random_text(n: int) -> str:
        return "".join(rng.choice(alphabet) for _ in range(n))

    for i in range(count):
        kind = rng.randrange(6)
        if kind == 0:
            template = random_text(rng.randint(0, 60))
            cases.append(run_case(f"seeded {i} template", definition=_basic_definition(template=template)))
        elif kind == 1:
            rp = random_text(rng.randint(0, 200))
            cases.append(run_case(f"seeded {i} rendered", rendered_prompt=rp))
        elif kind == 2:
            phrase = rng.choice(keyword_pool)
            suffix = rng.choice(["", "e", "es", "s", "ed", "ing", "ion", "ions"])
            cases.append(run_case(f"seeded {i} keyword", rendered_prompt=f"{phrase}{suffix} and {random_text(10)}"))
        elif kind == 3:
            weights = {n: rng.choice([rng.uniform(-2, 2), 0, float("nan"), float("inf")]) for n in rng.sample(SIGNAL_NAMES, rng.randint(1, 7))}
            cases.append(run_case(f"seeded {i} weights", rendered_prompt="x", weights=weights))
        elif kind == 4:
            structure = {"depth": rng.randint(-2, 12), "loop_depth": rng.randint(-2, 6), "reflector_generated": rng.choice([True, False])}
            cases.append(run_case(f"seeded {i} structure", rendered_prompt="x", structure=structure))
        else:
            prompt_type = rng.choice(type_pool)
            cases.append(run_case(f"seeded {i} prompt_type", definition=_basic_definition(template="x", prompt_type=prompt_type)))
    return cases


def family_runtime_shaped() -> list[dict[str, Any]]:
    """Family 10: exactly what `_score_and_route` passes to `score()`.

    Each case resolves a synthetic `runtime.complexity.scoring` block with
    `parse_complexity_settings`, then calls `score()` the way
    `core/prompt.py::PromptRuntime._score_and_route` does: `dict(weights)`,
    `dict(keywords) or None`, `structure={"depth": n}`, rendered mode.

    Each case also scores the identical inputs a second time with a real
    compiled `PromptDefinition` (built through `circuitry.core.compiler`
    from synthetic YAML) in place of the mapping `score()` otherwise
    receives, and asserts the two calls agree -- the proof that K2's
    mapping form (the only form a Rust caller can build) is equivalent to
    the object form the real runtime passes. A mismatch fails generation
    outright rather than silently recording only one form.
    """
    scoring_blocks = [
        {"enabled": True},
        {"enabled": True, "weights": {"prompt_size": 2.0, "keywords": 0.0}},
        {"enabled": True, "keywords": {"refactor": 0.6, "optimize": 0.4}},
        {"enabled": True, "weights": {"output_size": 3.0}, "keywords": {}},
    ]
    depths = [0, 1, 3, 2]
    effect = {
        "name": "p",
        "template": "please analyze and refactor {{topic}}",
        "prompt_type": "text",
        "schema": {"properties": {"a": {"type": "string"}}},
        "params": {"max_tokens": 256},
        "description": "optimize the plan",
    }

    cases = []
    for i, (scoring_block, depth) in enumerate(zip(scoring_blocks, depths, strict=True)):
        settings = parse_complexity_settings({"scoring": scoring_block})
        weights = dict(settings.scoring.weights)
        keyword_weights = dict(settings.scoring.keywords) or None
        structure = {"depth": depth}
        rendered_prompt = f"please analyze and refactor the plan, case {i}"

        compiled = _compile_prompt(dict(effect), effect_path="effects.p")
        mapping_definition = {
            "template": compiled.template,
            "messages": None
            if compiled.messages is None
            else [{"role": m.role, "content": m.content} for m in compiled.messages],
            "prompt_type": compiled.prompt_type,
            "schema": compiled.schema,
            "params": compiled.params,
            "description": compiled.description,
        }

        kwargs = {
            "rendered_prompt": rendered_prompt,
            "weights": weights,
            "keyword_weights": keyword_weights,
            "structure": structure,
        }
        from_object = _complexity_meta(score(definition=compiled, **kwargs))
        from_mapping = _complexity_meta(score(definition=mapping_definition, **kwargs))
        assert from_object == from_mapping, (
            f"runtime-shaped case {i}: PromptDefinition and mapping forms diverged: "
            f"{from_object!r} != {from_mapping!r}"
        )

        cases.append(
            run_case(
                f"runtime-shaped case {i} (mapping form, proven equal to PromptDefinition)",
                definition=mapping_definition,
                rendered_prompt=rendered_prompt,
                weights=weights,
                keyword_weights=keyword_weights,
                structure=structure,
            )
        )
    return cases


def run_settings_case(description: str, raw: Any) -> dict[str, Any]:
    try:
        settings = parse_complexity_settings(raw)
    except ComplexityConfigError as exc:
        return {"description": description, "raw": raw, "error": str(exc)}
    return {
        "description": description,
        "raw": raw,
        "expected": {
            "weights": dict(settings.scoring.weights),
            "keywords": dict(settings.scoring.keywords),
        },
    }


def family_settings() -> list[dict[str, Any]]:
    """Family 13: `runtime.complexity` -> resolved `scoring.weights`/
    `keywords` -- overlay onto the config defaults, int to float, and the
    strip-collision rule (last value wins, first position kept).
    """
    cases = [run_settings_case("settings absent block resolves to defaults", None)]
    cases.append(run_settings_case("settings empty scoring block", {"scoring": {}}))
    cases.append(
        run_settings_case(
            "settings partial weights overlay, int becomes float",
            {"scoring": {"weights": {"prompt_size": 9}}},
        )
    )
    cases.append(
        run_settings_case(
            "settings weights unknown signal raises",
            {"scoring": {"weights": {"bogus": 1.0}}},
        )
    )
    cases.append(
        run_settings_case(
            "settings weights not a mapping raises",
            {"scoring": {"weights": "nope"}},
        )
    )
    cases.append(
        run_settings_case(
            "settings keywords not a mapping raises",
            {"scoring": {"keywords": "nope"}},
        )
    )
    cases.append(
        run_settings_case(
            "settings keywords strip collision keeps first position, last value",
            {
                "scoring": {
                    "keywords": {
                        "  analyze  ": 1,
                        "compare": 2,
                        "analyze": 3,
                    }
                }
            },
        )
    )
    cases.append(
        run_settings_case(
            "settings unknown top-level key raises",
            {"bogus": True},
        )
    )
    cases.append(
        run_settings_case(
            "settings full block, enabled, weights and keywords",
            {
                "scoring": {
                    "enabled": True,
                    "weights": {"prompt_size": 2.0, "output_size": 0},
                    "keywords": {"refactor": 0.6, " Optimize ": 0.4},
                }
            },
        )
    )
    return cases


def _band(model: str, max_: float | None, name: str | None) -> dict[str, Any]:
    return {"model": model, "max": max_, "name": name}


def run_route_case(
    description: str,
    *,
    bands: list[dict[str, Any]],
    score_value: float,
    enabled: bool = True,
    respect_explicit: bool = True,
    explicit: bool = False,
) -> dict[str, Any]:
    settings = RoutingSettings(
        enabled=enabled,
        bands=tuple(ComplexityBand(model=b["model"], max=b["max"], name=b["name"]) for b in bands),
        respect_explicit=respect_explicit,
    )
    decision = route_model(score=score_value, settings=settings, explicit=explicit)
    return {
        "description": description,
        "bands": bands,
        "enabled": enabled,
        "respect_explicit": respect_explicit,
        "score": score_value,
        "explicit": explicit,
        "expected": None
        if decision is None
        else {"model": decision.band.model, "max": decision.band.max, "name": decision.band.name},
    }


def family_router() -> list[dict[str, Any]]:
    """Family 14 (route_model half): band tables x scores at, just below
    and just above each `max`; a catch-all-only table; `enabled` x
    `explicit` x `respect_explicit`.
    """
    bands_three = [
        _band("small", 30.0, "low"),
        _band("medium", 70.0, "mid"),
        _band("big", None, None),
    ]
    cases = [
        run_route_case(f"route boundary score={s}", bands=bands_three, score_value=s)
        for s in [0.0, 29.9, 30.0, 30.1, 69.9, 70.0, 70.1, 99.9, 100.0]
    ]
    cases.append(
        run_route_case("route catch-all only", bands=[_band("only", None, None)], score_value=50.0)
    )
    cases.append(
        run_route_case(
            "route empty model band defers",
            bands=[_band("", None, None)],
            score_value=10.0,
        )
    )
    cases.extend(
        run_route_case(
            f"route enabled={enabled} explicit={explicit} "
            f"respect_explicit={respect_explicit}",
            bands=bands_three,
            enabled=enabled,
            respect_explicit=respect_explicit,
            score_value=50.0,
            explicit=explicit,
        )
        for enabled in [True, False]
        for explicit in [True, False]
        for respect_explicit in [True, False]
    )
    return cases


def run_band_named_case(description: str, *, bands: list[dict[str, Any]], name: str) -> dict[str, Any]:
    complexity_bands = tuple(ComplexityBand(model=b["model"], max=b["max"], name=b["name"]) for b in bands)
    found = band_named(name, complexity_bands)
    return {
        "description": description,
        "bands": bands,
        "name": name,
        "expected": None if found is None else {"model": found.model, "max": found.max, "name": found.name},
    }


def family_band_named() -> list[dict[str, Any]]:
    """Family 14 (band_named half): hits, misses and unnamed bands."""
    bands_named = [
        _band("small", 30.0, "low"),
        _band("big", None, None),
    ]
    return [
        run_band_named_case("band_named hit", bands=bands_named, name="low"),
        run_band_named_case("band_named miss", bands=bands_named, name="nonexistent"),
        run_band_named_case(
            "band_named lookup by empty string never matches an unnamed band",
            bands=bands_named,
            name="",
        ),
    ]


def build_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    cases.extend(family_prompt_size())
    cases.extend(family_state_references())
    cases.extend(family_prompt_type())
    cases.extend(family_output_schema())
    cases.extend(family_output_size())
    cases.extend(family_structural_position())
    cases.extend(family_keywords())
    cases.extend(family_weights())
    cases.extend(family_definition_shape())
    cases.extend(family_runtime_shaped())
    cases.extend(family_rounding_stress())
    cases.extend(family_seeded_random(300))
    return cases


def main() -> int:
    cases = build_cases()
    settings_cases = family_settings()
    router_cases = family_router()
    band_named_cases = family_band_named()
    corpus = {
        "score_cases": cases,
        "settings_cases": settings_cases,
        "router_cases": router_cases,
        "band_named_cases": band_named_cases,
    }
    data = (json.dumps(corpus, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_bytes() if OUTPUT.exists() else b""
        if current != data:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_bytes(data)
    total = len(cases) + len(settings_cases) + len(router_cases) + len(band_named_cases)
    print(f"wrote {total} case(s) to {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
