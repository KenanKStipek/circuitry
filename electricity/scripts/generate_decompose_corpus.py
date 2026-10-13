#!/usr/bin/env python3
"""Generate the golden corpus for lane L2a of issue #448 (#521):
`electricity-vm::decompose`'s pure helpers, by actually running
Circuitry's own real functions in `core/decompose.py`.

Every section below calls the real function directly, with one
exception: the seed-state section patches `circuitry.core.decompose.
_run_isolated` to capture the `initial_state` it was given, since that
value is otherwise local to `_execute_plan`'s own body. The cycle and
missing-result sections need no patch at all -- a cycle is detected
(and `_execute_plan` raises) before `_run_isolated` is ever called, and
the missing-result case runs a real, tiny `tool`/`json` orchestration
end to end (no model call, so no adapter is ever actually invoked).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, `-c electricity/scripts/generator-constraints.txt`).
Usage: python3 generate_decompose_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path
from typing import Any
from unittest import mock

import yaml as _yaml

from circuitry.cli.complexity_config import (
    ComplexityBand,
    ComplexitySettings,
    DecompositionSettings,
    RoutingSettings,
)
from circuitry.core.decompose import (
    _describe_inputs,
    _execute_plan,
    _failure,
    _Plan,
    _route_up_model,
    _run_planner,
    _validate_plan,
    maybe_decompose,
)
from circuitry.core.prompt import PromptDefinition
from circuitry.core.store import Store

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "decompose_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_validate_yaml_corpus.py's).
# ---------------------------------------------------------------------


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def encode_opt_str(value: str | None) -> dict | None:
    return None if value is None else {"t": "str", "v": value}


# ---------------------------------------------------------------------
# route_up_model
# ---------------------------------------------------------------------


def build_route_up_model_cases() -> list[dict[str, Any]]:
    cases = []

    def case(name: str, settings: ComplexitySettings, model: str | None) -> None:
        defn = PromptDefinition(name="ask", model=model)
        result = _route_up_model(settings, defn)
        cases.append({"name": name, "model": model, "settings": _settings_repr(settings), "expected": result})

    off = ComplexitySettings()
    case("routing_off", off, None)

    empty_bands = ComplexitySettings(routing=RoutingSettings(enabled=True, bands=()))
    case("routing_on_no_bands", empty_bands, None)

    bands = (ComplexityBand(model="small", max=10.0), ComplexityBand(model="big", max=None))
    respect = ComplexitySettings(routing=RoutingSettings(enabled=True, bands=bands, respect_explicit=True))
    case("respect_explicit_with_model", respect, "pinned")
    case("respect_explicit_no_model", respect, None)

    ignore = ComplexitySettings(routing=RoutingSettings(enabled=True, bands=bands, respect_explicit=False))
    case("ignore_explicit_with_model", ignore, "pinned")
    case("ignore_explicit_no_model", ignore, None)
    case("ignore_explicit_empty_string_model", ignore, "")

    return cases


def _settings_repr(settings: ComplexitySettings) -> dict[str, Any]:
    return {
        "routing_enabled": settings.routing.enabled,
        "respect_explicit": settings.routing.respect_explicit,
        "bands": [{"model": b.model, "max": b.max, "name": b.name} for b in settings.routing.bands],
    }


# ---------------------------------------------------------------------
# describe_inputs
# ---------------------------------------------------------------------


def build_describe_inputs_cases() -> list[dict[str, Any]]:
    cases = []

    def case(name: str, ctx: dict[Any, Any]) -> None:
        cases.append({"name": name, "ctx": encode(ctx), "expected": _describe_inputs(ctx)})

    case("empty_input_namespace", {"input": {}})
    case("quoting_needing_keys", {"input": {"b": 1, "a with space": 2}})
    case("non_string_keys_in_input_ignored", {"input": {1: "x", "a": 2, None: "y"}})
    case(
        "fallback_excludes_namespaces_and_underscored",
        {"topic": "x", "prime": {}, "runtime": {}, "_private": 1},
    )
    case("fallback_non_string_key_dropped", {1: "x", "topic": "y"})
    case("fallback_no_keys_at_all", {"prime": {}, "runtime": {}})
    case("fallback_sorted_by_code_point", {"zeta": 1, "alpha": 2, "Beta": 3})

    return cases


# ---------------------------------------------------------------------
# source_output -- the inline `yaml.safe_dump({"prompt_type": ...,
# "schema": ...}, sort_keys=False)` line inside `_run_planner` itself,
# reproduced verbatim (not worth promoting to its own function for one
# caller) -- real PyYAML output either way.
# ---------------------------------------------------------------------


def build_source_output_cases() -> list[dict[str, Any]]:
    cases = []

    def case(name: str, prompt_type: str, schema: Any) -> None:
        text = str(_yaml.safe_dump({"prompt_type": prompt_type, "schema": schema}, sort_keys=False))
        cases.append({"name": name, "prompt_type": prompt_type, "schema": encode(schema), "expected": text})

    case("text_no_schema", "text", None)
    case("object_nested_schema", "object", {"type": "object", "properties": {"a": {"type": "string"}}})
    case(
        "long_description",
        "object",
        {"type": "object", "description": "x" * 90},
    )
    case("non_ascii", "text", {"description": "café —日本語"})
    case("special_strings", "text", {"a": "yes", "b": "null", "c": "1.0", "d": "~"})
    case("infinity", "number", {"minimum": float("inf")})
    case("empty_object", "object", {})
    case("none_schema_again", "boolean", None)

    return cases


# ---------------------------------------------------------------------
# _run_planner -- `_run_isolated` patched (the same pattern
# `build_seed_state_cases` already uses below) to capture the planner
# input it was actually given (`planner_input`'s own round trip) and to
# hand back a chosen planner state, read back through the real
# `_planner_outputs`/the bundled planner's own interface (`read_plan`'s
# own round trip) -- reviewed on PR #524 as the one section the issue
# names that this corpus was missing entirely.
# ---------------------------------------------------------------------


def _planner_returned_state(
    say: Any, chunks: Any, yaml_text: Any, done: Any, result_path: Any
) -> dict[str, Any]:
    return {
        "prime": {
            "plan": {"value": {"say": say, "chunks": chunks}},
            "check": {"value": {"yaml": yaml_text}},
            "done": {"value": done},
            "result_path": {"value": result_path},
        }
    }


def build_run_planner_cases() -> list[dict[str, Any]]:
    cases = []

    def case(
        name: str,
        *,
        source_template: str,
        ctx: dict[str, Any],
        prompt_type: str,
        schema: Any,
        max_chunks: int,
        returned_state: dict[str, Any],
    ) -> None:
        defn = PromptDefinition(name="ask", prompt_type=prompt_type, schema=schema)
        store = Store(state={})
        captured: dict[str, Any] = {}

        def fake_run_isolated(
            orch: Any,
            *,
            initial_state: Any,
            parent_store: Any,
            node: Any,
            node_path: Any,
            adapter: Any,
            model: Any,
            runtime_config: Any,
            timeout_seconds: Any,
            verbose: Any,
            display_depth: Any,
        ) -> Store:
            captured["initial_state"] = initial_state
            return Store(state=returned_state)

        with mock.patch("circuitry.core.decompose._run_isolated", fake_run_isolated):
            plan = _run_planner(
                defn,
                store=store,
                node={},
                node_path="prime.ask",
                ctx=ctx,
                source_template=source_template,
                adapter=None,
                model="x",
                runtime_config={},
                max_chunks=max_chunks,
                timeout_seconds=5,
                verbose=False,
                display_depth=0,
            )
        cases.append(
            {
                "name": name,
                "source_template": source_template,
                "ctx": encode(ctx),
                "prompt_type": prompt_type,
                "schema": encode(schema),
                "max_chunks": max_chunks,
                "returned_state": encode(returned_state),
                "expected_planner_input": encode(captured["initial_state"]),
                "expected_plan_say": encode(plan.say),
                "expected_plan_chunks": encode(plan.chunks),
                "expected_plan_yaml": encode_opt_str(plan.yaml),
                "expected_plan_done": encode(plan.done),
                "expected_plan_result_path": plan.result_path,
            }
        )

    case(
        "basic_round_trip",
        source_template="Summarize {{input.topic}}",
        ctx={"input": {"topic": "x"}},
        prompt_type="text",
        schema=None,
        max_chunks=5,
        returned_state=_planner_returned_state("ok", [1, 2], "effects: []\n", True, "prime.merge.value"),
    )
    case(
        "yaml_output_needs_fence_cleaning",
        source_template="Summarize {{input.topic}}",
        ctx={"input": {"topic": "x"}},
        prompt_type="object",
        schema={"type": "object"},
        max_chunks=3,
        returned_state=_planner_returned_state(
            "ok", [1, 2], "```yaml\neffects: []\n```", True, "prime.merge.value"
        ),
    )
    case(
        "result_path_blank_defaults",
        source_template="t",
        ctx={},
        prompt_type="text",
        schema=None,
        max_chunks=5,
        returned_state=_planner_returned_state("ok", [1, 2], "effects: []\n", True, "   "),
    )
    case(
        "result_path_non_string_defaults",
        source_template="t",
        ctx={},
        prompt_type="text",
        schema=None,
        max_chunks=5,
        returned_state=_planner_returned_state("ok", [1, 2], "effects: []\n", True, 5),
    )
    case(
        "source_output_nested_non_ascii_schema",
        source_template="t",
        ctx={"input": {"a": 1}},
        prompt_type="object",
        schema={
            "type": "object",
            "description": "caf\u00e9 \u2014\u65e5\u672c\u8a9e",
            "properties": {"a": {"type": "string"}},
        },
        max_chunks=5,
        returned_state=_planner_returned_state(None, None, None, None, None),
    )
    case(
        "source_output_infinity_schema",
        source_template="t",
        ctx={},
        prompt_type="number",
        schema={"minimum": float("inf")},
        max_chunks=5,
        returned_state=_planner_returned_state(None, None, None, None, None),
    )
    case(
        "source_output_none_schema",
        source_template="t",
        ctx={},
        prompt_type="boolean",
        schema=None,
        max_chunks=5,
        returned_state=_planner_returned_state(None, None, None, None, None),
    )

    return cases


# ---------------------------------------------------------------------
# validate_plan
# ---------------------------------------------------------------------

_YAML_OK = (
    "effects:\n"
    '  - {type: tool, name: a, provider: json, params: {mode: parse, input: "1"}}\n'
    '  - {type: tool, name: merge, provider: json, params: {mode: parse, input: "1"}}\n'
)
_YAML_ONE_EFFECT = 'effects:\n  - {type: tool, name: merge, provider: json, params: {mode: parse, input: "1"}}\n'
_YAML_SHELL = 'effects:\n  - {type: tool, name: merge, provider: shell, params: {cmd: echo}}\n'

# `_validate_plan`'s inline-check branch (step 4, `_validate_inline_yaml`) --
# every way it can fail, reviewed on PR #524 as missing from this corpus.
_YAML_SCAN_ERROR = "effects: [{a: [1, 2}]"
_YAML_NON_MAPPING = "- a"
_YAML_MISSING_EFFECTS_KEY = "name: x"
_YAML_NEAR_MISS_STRUCTURAL_ERROR = (
    "effects:\n"
    "  - type: tool\n"
    "    name: a\n"
    "    provider: json\n"
    "    discription: oops\n"
    '    params: {mode: parse, input: "1"}\n'
)
_YAML_DUPLICATE_KEY = (
    "effects:\n"
    "  - type: tool\n"
    "    name: a\n"
    "    provider: json\n"
    '    params: {mode: parse, input: "1"}\n'
    "effects:\n"
    "  - type: tool\n"
    "    name: b\n"
    "    provider: json\n"
    '    params: {mode: parse, input: "1"}\n'
)


def build_validate_plan_cases() -> list[dict[str, Any]]:
    cases = []

    def case(
        name: str,
        *,
        chunks: Any = None,
        yaml_text: str | None = None,
        done: Any = None,
        result_path: str = "prime.merge.value",
        max_chunks: int = 5,
        runtime_config: dict[str, Any] | None = None,
        third_party_prefix: str | None = None,
    ) -> None:
        plan = _Plan(say=None, chunks=chunks, yaml=yaml_text, done=done, result_path=result_path)
        problems = _validate_plan(plan, max_chunks=max_chunks, runtime_config=runtime_config or {})
        if third_party_prefix is not None:
            # A PyYAML scan error (DESIGN.md §1, §12): compared by prefix and
            # location only on the Rust side, never the exact wording.
            assert len(problems) == 1, f"{name}: expected exactly one problem, got {problems}"
            assert problems[0].startswith(third_party_prefix), (
                f"{name}: {problems[0]!r} does not start with {third_party_prefix!r}"
            )
            assert len(problems[0]) > len(third_party_prefix), f"{name}: empty rest after prefix"
        cases.append(
            {
                "name": name,
                "chunks": encode(chunks),
                "yaml": encode_opt_str(yaml_text),
                "done": encode(done),
                "result_path": result_path,
                "max_chunks": max_chunks,
                "runtime_config": encode(runtime_config or {}),
                "expected": problems,
                "third_party_prefix": third_party_prefix,
            }
        )

    case("blank_yaml_absent", result_path="prime.merge.value")
    case("blank_yaml_whitespace", yaml_text="   ")
    case("done_false_and_too_few_chunks", chunks=[1], yaml_text=_YAML_ONE_EFFECT, done=False)
    case("done_none_is_not_rejected", chunks=[1, 2], yaml_text=_YAML_OK, done=None)
    case("done_zero_is_not_rejected", chunks=[1, 2], yaml_text=_YAML_OK, done=0)
    case("no_chunk_list_at_all", chunks=None, yaml_text=_YAML_OK, done=True)
    case("chunks_not_a_list", chunks="oops", yaml_text=_YAML_OK, done=True)
    case("chunks_empty_list", chunks=[], yaml_text=_YAML_OK, done=True)
    case("one_chunk_too_few", chunks=[1], yaml_text=_YAML_OK, done=True)
    case("six_chunks_over_budget_of_five", chunks=[1, 2, 3, 4, 5, 6], yaml_text=_YAML_OK, done=True, max_chunks=5)
    case("result_path_not_under_prime", chunks=[1, 2], yaml_text=_YAML_OK, done=True, result_path="input.merge.value")
    case(
        "result_path_single_segment",
        chunks=[1, 2],
        yaml_text=_YAML_OK,
        done=True,
        result_path="prime",
    )
    case(
        "no_top_level_effect_for_result_path",
        chunks=[1, 2],
        yaml_text=_YAML_OK,
        done=True,
        result_path="prime.nope.value",
    )
    case("sound_plan", chunks=[1, 2], yaml_text=_YAML_OK, done=True)
    case(
        "allowlist_denies_the_tool",
        chunks=[1, 2],
        yaml_text=_YAML_SHELL,
        done=True,
        runtime_config={"_allowlists": {"tools": ["json"]}},
    )
    case(
        "allowlist_open_by_default",
        chunks=[1, 2],
        yaml_text=_YAML_SHELL,
        done=True,
        runtime_config={},
    )

    # The inline check (step 4) -- every way it can fail, run through the
    # real `_validate_plan` rather than re-typed (review finding 2 on PR
    # #524): a PyYAML scan error, a non-mapping document, a document
    # missing the required `effects` key, a near-miss key (a Circuitry-own
    # structural error), and the "two loaders" hazard's own duplicate key
    # (caught here by the same duplicate-key loader the merge contract and
    # allowlist check don't use).
    case(
        "inline_check_yaml_scan_error",
        chunks=[1, 2],
        yaml_text=_YAML_SCAN_ERROR,
        done=True,
        third_party_prefix="YAML parse error: ",
    )
    case("inline_check_non_mapping", chunks=[1, 2], yaml_text=_YAML_NON_MAPPING, done=True)
    case(
        "inline_check_missing_effects_key",
        chunks=[1, 2],
        yaml_text=_YAML_MISSING_EFFECTS_KEY,
        done=True,
    )
    case(
        "inline_check_near_miss_structural_error",
        chunks=[1, 2],
        yaml_text=_YAML_NEAR_MISS_STRUCTURAL_ERROR,
        done=True,
    )
    case(
        "inline_check_duplicate_key",
        chunks=[1, 2],
        yaml_text=_YAML_DUPLICATE_KEY,
        done=True,
    )

    return cases


# ---------------------------------------------------------------------
# failure / max_depth / success records
# ---------------------------------------------------------------------


def build_failure_cases() -> list[dict[str, Any]]:
    cases = []

    def case(
        name: str,
        *,
        score: float,
        threshold: float,
        depth: int,
        max_depth: int,
        on_failure: str,
        reason: str,
        error: str,
        fallback_model: str | None,
        raw_payload: str | None = None,
    ) -> None:
        base = {
            "decomposed": False,
            "score": score,
            "threshold": threshold,
            "depth": depth,
            "max_depth": max_depth,
        }
        dset = DecompositionSettings(on_failure=on_failure)
        result = _failure(base, dset, reason, error, fallback_model, raw_payload=raw_payload)
        cases.append(
            {
                "name": name,
                "score": score,
                "threshold": threshold,
                "depth": depth,
                "max_depth": max_depth,
                "on_failure": on_failure,
                "reason": reason,
                "error": error,
                "fallback_model": fallback_model,
                "raw_payload": raw_payload,
                "expected_meta": encode(result.meta),
                "expected_failure": result.failure,
                "expected_error": result.error,
                "expected_fail": result.fail,
                "expected_fallback_model": result.fallback_model,
            }
        )

    case(
        "route_up_with_fallback_and_raw_payload",
        score=90.0,
        threshold=80.0,
        depth=0,
        max_depth=2,
        on_failure="route_up",
        reason="planner_failed",
        error="boom",
        fallback_model="big",
        raw_payload="the raw text",
    )
    case(
        "run_as_is_with_no_fallback",
        score=90.0,
        threshold=80.0,
        depth=1,
        max_depth=2,
        on_failure="route_up",
        reason="invalid_plan",
        error="bad plan",
        fallback_model=None,
    )
    case(
        "fail_forces_fallback_to_none_even_with_one",
        score=90.0,
        threshold=80.0,
        depth=0,
        max_depth=2,
        on_failure="fail",
        reason="execution_failed",
        error="child blew up",
        fallback_model="big",
    )
    case(
        "empty_raw_payload_is_dropped",
        score=90.0,
        threshold=80.0,
        depth=0,
        max_depth=2,
        on_failure="route_up",
        reason="invalid_plan",
        error="bad",
        fallback_model=None,
        raw_payload="",
    )

    return cases


def build_max_depth_and_success_cases() -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """`maybe_decompose`'s own two inline record constructions (`:195-203`,
    `:259-263`), reached by actually running the real function end to end
    (review finding 7 on PR #524: these were re-typed dict expressions
    before, never the real function's own output) -- the depth ceiling
    (`_run_planner`/`_execute_plan` never even called, since the ceiling is
    checked first) and a full plan/validate/execute pass with
    `_run_planner`/`_execute_plan` patched, the way the review's own fix
    suggests, the real `_validate_plan` still in the loop in between."""

    def run(
        *,
        score: float,
        dset: dict[str, Any],
        routing: dict[str, Any] | None,
        depth: int,
        planned: _Plan | None,
        executed_value: Any = None,
    ) -> Any:
        complexity: dict[str, Any] = {"scoring": {"enabled": True}, "decomposition": dset}
        if routing is not None:
            complexity["routing"] = routing
        runtime_config: dict[str, Any] = {"complexity": complexity, "_decomposition_depth": depth}
        defn = PromptDefinition(name="ask")
        store = Store(state={})

        def fake_run_planner(*args: Any, **kwargs: Any) -> _Plan:
            assert planned is not None, "the ceiling must fire before the planner ever runs"
            return planned

        def fake_execute_plan(*args: Any, **kwargs: Any) -> Any:
            return executed_value

        with (
            mock.patch("circuitry.core.decompose._run_planner", fake_run_planner),
            mock.patch("circuitry.core.decompose._execute_plan", fake_execute_plan),
        ):
            return maybe_decompose(
                defn,
                store=store,
                node={},
                ctx={},
                score=score,
                source_template="Summarize {{input.topic}}",
                adapter=None,
                model="x",
                runtime_config=runtime_config,
                timeout_seconds=5,
                verbose=False,
                display_depth=0,
            )

    max_depth_cases = []
    dset = {"enabled": True, "threshold": 80.0, "max_depth": 2, "max_chunks": 5, "on_failure": "route_up"}
    for name, bands in [
        ("with_fallback_routes_up", [{"model": "small", "max": 10.0}, {"model": "big", "max": None}]),
        ("without_fallback_runs_as_is", None),
    ]:
        routing = {"enabled": True, "bands": bands, "respect_explicit": True} if bands else None
        result = run(score=95.0, dset=dset, routing=routing, depth=2, planned=None)
        assert result is not None and not result.succeeded and result.failure is None and not result.fail, (
            name,
            result,
        )
        fallback_model = bands[-1]["model"] if bands else None
        assert result.fallback_model == fallback_model, (name, result)
        max_depth_cases.append(
            {
                "name": name,
                "score": 95.0,
                "threshold": 80.0,
                "depth": 2,
                "max_depth": 2,
                "fallback_model": fallback_model,
                "expected_meta": encode(result.meta),
            }
        )

    success_cases = []
    plan = _Plan(say="hi", chunks=[1, 2], yaml=_YAML_OK, done=True, result_path="prime.merge.value")
    result = run(score=90.0, dset=dset, routing=None, depth=0, planned=plan, executed_value="computed")
    assert result is not None and result.succeeded and result.value == "computed", result
    success_cases.append(
        {
            "name": "decomposed_in_place_first_key",
            "score": 90.0,
            "threshold": 80.0,
            "depth": 0,
            "max_depth": 2,
            "plan_say": "hi",
            "plan_chunks": [1, 2],
            "chunk_count": 2,
            "result_path": "prime.merge.value",
            "yaml": _YAML_OK,
            "expected_meta": encode(result.meta),
            "expected_key_order": list(result.meta.keys()),
        }
    )

    return max_depth_cases, success_cases


# ---------------------------------------------------------------------
# _execute_plan: cycle detection, missing result, seed state
# ---------------------------------------------------------------------


def build_cycle_error_cases() -> list[dict[str, Any]]:
    import hashlib

    cases = []
    text = _YAML_ONE_EFFECT
    identity = "inline:" + hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]

    def run(stack: list[str]) -> str | None:
        plan = _Plan(say=None, chunks=[1, 2], yaml=text, done=True, result_path="prime.merge.value")
        defn = PromptDefinition(name="ask")
        store = Store(state={})
        try:
            _execute_plan(
                plan,
                defn=defn,
                store=store,
                node={},
                node_path="prime.ask",
                ctx={},
                adapter=None,
                model="x",
                runtime_config={"_use_call_stack": stack},
                depth=0,
                timeout_seconds=5,
                verbose=False,
                display_depth=0,
            )
        except RecursionError as exc:
            return str(exc)
        except ValueError:
            return None
        return None

    cases.append({"name": "no_cycle_on_empty_stack", "yaml": text, "stack": [], "expected": None})
    message = run([identity])
    assert message is not None
    cases.append({"name": "cycle_detected", "yaml": text, "stack": [identity], "expected": message})
    return cases


def build_missing_result_cases() -> list[dict[str, Any]]:
    cases = []
    text = 'effects:\n  - {type: tool, name: other, provider: json, params: {mode: parse, input: "1"}}\n'
    plan = _Plan(say=None, chunks=[1, 2], yaml=text, done=True, result_path="prime.merge.value")
    defn = PromptDefinition(name="ask")
    store = Store(state={})
    try:
        _execute_plan(
            plan,
            defn=defn,
            store=store,
            node={},
            node_path="prime.ask",
            ctx={},
            adapter=None,
            model="x",
            runtime_config={},
            depth=0,
            timeout_seconds=5,
            verbose=False,
            display_depth=0,
        )
        raised = None
    except ValueError as exc:
        raised = str(exc)
    assert raised is not None
    cases.append(
        {
            "name": "missing_result_at_result_path",
            "yaml": text,
            "result_path": "prime.merge.value",
            "expected_error": raised,
        }
    )
    return cases


def build_seed_state_cases() -> list[dict[str, Any]]:
    cases = []

    def case(name: str, ctx: dict[Any, Any], result_path: str) -> None:
        text = _YAML_ONE_EFFECT
        plan = _Plan(say=None, chunks=[1, 2], yaml=text, done=True, result_path=result_path)
        defn = PromptDefinition(name="ask")
        store = Store(state={})
        captured: dict[str, Any] = {}

        def fake_run_isolated(
            orch: Any,
            *,
            initial_state: Any,
            parent_store: Any,
            node: Any,
            node_path: Any,
            adapter: Any,
            model: Any,
            runtime_config: Any,
            timeout_seconds: Any,
            verbose: Any,
            display_depth: Any,
        ) -> Store:
            captured["initial_state"] = initial_state
            return Store(state={"prime": {"merge": {"value": "computed"}}})

        with mock.patch("circuitry.core.decompose._run_isolated", fake_run_isolated):
            _execute_plan(
                plan,
                defn=defn,
                store=store,
                node={},
                node_path="prime.ask",
                ctx=ctx,
                adapter=None,
                model="x",
                runtime_config={},
                depth=0,
                timeout_seconds=5,
                verbose=False,
                display_depth=0,
            )
        cases.append(
            {
                "name": name,
                "ctx": encode(ctx),
                "result_path": result_path,
                "expected_seed": encode(captured["initial_state"]),
            }
        )

    case(
        "clears_only_the_result_paths_own_segment",
        {
            "input": {"topic": "x"},
            "prime": {"merge": {"value": "stale"}, "other": {"value": "keep"}},
            "runtime": {},
            "_private": 1,
        },
        "prime.merge.value",
    )
    case("non_string_key_dropped", {"input": {"topic": "x"}, 1: "dropped"}, "prime.merge.value")
    case("no_prime_namespace_at_all", {"input": {"topic": "x"}}, "prime.merge.value")
    case(
        "result_path_single_segment_clears_nothing",
        {"prime": {"merge": {"value": "kept, single-segment path never matches"}}},
        "prime",
    )

    return cases


# ---------------------------------------------------------------------
# write / check
# ---------------------------------------------------------------------


def render(corpus: dict) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    max_depth_cases, success_cases = build_max_depth_and_success_cases()
    corpus = {
        "route_up_model_cases": build_route_up_model_cases(),
        "describe_inputs_cases": build_describe_inputs_cases(),
        "source_output_cases": build_source_output_cases(),
        "run_planner_cases": build_run_planner_cases(),
        "validate_plan_cases": build_validate_plan_cases(),
        "failure_cases": build_failure_cases(),
        "max_depth_cases": max_depth_cases,
        "success_cases": success_cases,
        "cycle_error_cases": build_cycle_error_cases(),
        "missing_result_cases": build_missing_result_cases(),
        "seed_state_cases": build_seed_state_cases(),
    }
    text = render(corpus)
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
