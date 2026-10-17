"""The ``cel`` tool (#597): a step that computes a value with CEL."""

from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateResult
from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.core.cel_eval import CelEvaluationError, evaluate_cel_value
from circuitry.plugins.capabilities import capabilities_of
from circuitry.plugins.cel import CelPlugin
from circuitry.plugins.factory import build_plugin


class CountingAdapter:
    name = "counting"

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.fail_prompts: set[str] = set()

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        self.calls.append(prompt)
        if prompt in self.fail_prompts:
            raise RuntimeError(f"boom: {prompt}")
        return GenerateResult(text=f"ok:{prompt}", raw={})


def _write(tmp_path: Path, content: str) -> Path:
    path = tmp_path / "doc.yml"
    path.write_text(content.lstrip(), encoding="utf-8")
    return path


def _run(path: Path, **kwargs: Any) -> Any:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            **kwargs,
        )
    )


_STATE = {
    "input": {
        "items": [
            {"id": "a", "status": "open"},
            {"id": "b", "status": "closed"},
            {"id": "c", "status": "open"},
            {"id": "a", "status": "open"},
        ],
        "limits": {"small": 1, "large": 9},
        "first": "2026-01-02T03:04:05Z",
        "second": "2026-03-04T05:06:07.250+01:00",
    },
    "prime": {"count": {"value": 3}},
}


@pytest.mark.parametrize(
    ("expr", "expected"),
    [
        ("[1, 2, 3].filter(x, x > 1)", [2, 3]),
        ("state.input.items.filter(i, i.status == 'open').map(i, i.id)", ["a", "c", "a"]),
        ("state.input.limits['large']", 9),
        ("'medium' in state.input.limits ? state.input.limits['medium'] : 5", 5),
        (
            (
                "timestamp(state.input.first) > timestamp(state.input.second)"
                " ? state.input.first : state.input.second"
            ),
            "2026-03-04T05:06:07.250+01:00",
        ),
        ("state.prime.count.value * 2 + 1", 7),
        ("double(state.prime.count.value) / 2.0", 1.5),
        ("size(state.input.items)", 4),
        ("2u", 2),
        ("'a' + 'b'", "ab"),
        ("state.prime.count.value > 2", True),
        ("null", None),
        ("{'b': 1, 'a': [true, null, 'x']}", {"b": 1, "a": [True, None, "x"]}),
        ("{1: 'one', 2: 'two'}", {"1": "one", "2": "two"}),
        ("{true: 'yes'}", {"true": "yes"}),
        ("timestamp('2026-01-02T03:04:05Z')", "2026-01-02T03:04:05Z"),
        ("timestamp('2026-01-02T03:04:05.120Z')", "2026-01-02T03:04:05.12Z"),
        ("timestamp(state.input.second)", "2026-03-04T04:06:07.25Z"),
        ("duration('5400s')", "PT1H30M"),
        ("duration('1.5s')", "PT1.5S"),
        ("duration('0s')", "PT0S"),
        ("duration('-90s')", "-PT1M30S"),
        ("timestamp(state.input.second) - timestamp(state.input.first)", "PT1465H2M2.25S"),
        ("[timestamp('2026-01-02T03:04:05Z'), duration('60s')]", ["2026-01-02T03:04:05Z", "PT1M"]),
    ],
)
def test_expression_results_as_json(expr: str, expected: Any) -> None:
    assert evaluate_cel_value(expr, _STATE) == expected


def test_a_map_result_keeps_its_key_order() -> None:
    value = evaluate_cel_value("{'zeta': 1, 'alpha': 2, 'mid': {'y': 1, 'b': 2}}", {})
    assert json.dumps(value) == '{"zeta": 1, "alpha": 2, "mid": {"y": 1, "b": 2}}'
    from_state = evaluate_cel_value("state.input", {"input": {"z": 1, "a": 2}})
    assert list(from_state) == ["z", "a"]


@pytest.mark.parametrize(
    ("expr", "reason"),
    [
        ("b'abc'", "bytes have no JSON form"),
        ("1.0 / 0.0", "is not a finite number"),
        ("type(1)", "has no JSON form"),
    ],
)
def test_a_result_with_no_json_form_is_an_error(expr: str, reason: str) -> None:
    with pytest.raises(CelEvaluationError, match=reason):
        evaluate_cel_value(expr, {})


def test_an_unset_path_gives_null_and_a_warning(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.WARNING, logger="circuitry.core.cel_eval"):
        assert evaluate_cel_value("state.prime.missing.value + 1", {"prime": {}}) is None
    assert "reads unset state path 'state.prime.missing.value'; value is null" in caplog.text


def test_an_unset_path_under_strict_is_an_error() -> None:
    with pytest.raises(CelEvaluationError, match="is marked strict"):
        evaluate_cel_value("state.prime.missing.value", {"prime": {}}, strict=True)


def test_a_has_guard_is_exempt_from_the_unset_path_rule() -> None:
    expr = "has(state.prime.missing.value) ? state.prime.missing.value : 'fallback'"
    assert evaluate_cel_value(expr, {"prime": {}}, strict=True) == "fallback"


def test_cel_is_a_registered_plugin_that_needs_no_capability() -> None:
    assert isinstance(build_plugin(plugin_name="cel", runtime={}), CelPlugin)
    assert capabilities_of("cel") == frozenset()


def test_a_run_writes_the_value_and_records_the_expression(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        """
interface:
  inputs:
    items: {type: array}
effects:
  - type: tool
    name: open_ids
    provider: cel
    params:
      expr: "state.input.items.filter(i, i.status == 'open').map(i, i.id)"
      schema: {type: array, items: {type: string}}
  - type: tool
    name: braces
    provider: cel
    params:
      expr: "'{{not a template}}'"
""",
    )
    result = _run(doc, initial_state={"input": {"items": _STATE["input"]["items"]}})
    assert result.ok, result.error
    node = result.state["prime"]["open_ids"]
    assert node["value"] == ["a", "c", "a"]
    assert node["meta"]["error"] is None
    assert node["meta"]["raw"] == {
        "expr": "state.input.items.filter(i, i.status == 'open').map(i, i.id)"
    }
    # Never Mustache-rendered: the expression runs exactly as written.
    assert result.state["prime"]["braces"]["value"] == "{{not a template}}"


def test_an_unset_path_in_a_run_gives_null_and_a_strict_one_fails(tmp_path: Path) -> None:
    lenient = _write(
        tmp_path,
        """
effects:
  - type: tool
    name: later
    provider: cel
    params: {expr: "state.prime.never.value"}
""",
    )
    result = _run(lenient)
    assert result.ok, result.error
    assert result.state["prime"]["later"]["value"] is None

    strict = _write(
        tmp_path,
        """
effects:
  - type: tool
    name: later
    provider: cel
    params: {expr: "state.prime.never.value", strict: true}
""",
    )
    result = _run(strict)
    assert not result.ok
    assert "is marked strict" in result.state["prime"]["later"]["meta"]["error"]


def test_a_schema_mismatch_fails_the_effect(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        """
effects:
  - type: tool
    name: pick
    provider: cel
    on_error: continue
    params:
      expr: "[1, 'two']"
      schema: {type: array, items: {type: integer}}
""",
    )
    result = _run(doc)
    assert result.ok, result.error
    node = result.state["prime"]["pick"]
    assert node["value"] is None
    assert node["meta"]["error"] == (
        "cel: the result does not match params.schema: 1: 'two' is not of type 'integer'"
    )


def test_it_reads_each_as_and_iter_bindings_inside_a_loop(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        """
interface:
  inputs:
    items: {type: array}
effects:
  - type: loop
    name: per_item
    each: {in: input.items, as: it}
    collect: label
    body:
      - type: tool
        name: label
        provider: cel
        params:
          expr: "state.it.id + '#' + string(state.iter.index)"
""",
    )
    result = _run(doc, initial_state={"input": {"items": _STATE["input"]["items"][:2]}})
    assert result.ok, result.error
    loop = result.state["prime"]["per_item"]
    assert loop["iter_0"]["label"]["value"] == "a#0"
    assert loop["iter_1"]["label"]["value"] == "b#1"


def test_resume_does_not_evaluate_a_finished_cel_step_again(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    doc = _write(
        tmp_path,
        """
effects:
  - type: prompt
    name: first
    template: "one"
  - type: tool
    name: computed
    provider: cel
    params: {expr: "state.prime.first.value + '!'"}
  - type: prompt
    name: last
    template: "two"
""",
    )
    evaluations: list[str] = []
    original = CelPlugin.evaluate

    def counting(self: CelPlugin, *, params: dict[str, Any], ctx: dict[str, Any]) -> Any:
        evaluations.append(params["expr"])
        return original(self, params=params, ctx=ctx)

    monkeypatch.setattr(CelPlugin, "evaluate", counting)
    adapter = CountingAdapter()
    adapter.fail_prompts = {"two"}
    first = _run(doc, adapter=adapter)
    assert not first.ok
    assert first.state["prime"]["computed"]["value"] == "ok:one!"
    assert len(evaluations) == 1

    adapter.fail_prompts = set()
    second = _run(doc, adapter=adapter, initial_state=first.state, resume=True)
    assert second.ok, second.error
    assert len(evaluations) == 1
    assert second.state["prime"]["computed"]["value"] == "ok:one!"
    assert second.state["prime"]["last"]["value"] == "ok:two"


def _check_errors(tmp_path: Path, content: str) -> list[str]:
    report = validate(_write(tmp_path, content), trust_document=True)
    assert not report["ok"]
    return [str(error) for error in report["errors"]]


def test_check_reports_a_parse_error_before_anything_runs(tmp_path: Path) -> None:
    content = """
effects:
  - type: tool
    name: broken
    provider: cel
    params: {expr: "state.input.x &&"}
"""
    (error,) = _check_errors(tmp_path, content)
    assert error.startswith(
        "CEL expression at 'prime.effects[0]' (effect 'broken'): expression does not parse"
    )
    adapter = CountingAdapter()
    result = _run(_write(tmp_path, content), adapter=adapter)
    assert not result.ok
    assert "does not parse" in (result.error or "")
    assert "prime" not in (result.state or {}) or "broken" not in result.state["prime"]


def test_check_rejects_a_loop_binding_used_outside_its_loop(tmp_path: Path) -> None:
    (error,) = _check_errors(
        tmp_path,
        """
effects:
  - type: loop
    name: per_item
    each: {in: input.items, as: it}
    body:
      - type: tool
        name: inside
        provider: cel
        params: {expr: "state.it"}
  - type: tool
    name: outside
    provider: cel
    params: {expr: "state.it.id"}
""",
    )
    assert error.startswith(
        "CEL expression at 'prime.effects[1]': 'state.it' does not name a state namespace"
    )


def test_check_rejects_an_unknown_name(tmp_path: Path) -> None:
    (error,) = _check_errors(
        tmp_path,
        """
effects:
  - type: tool
    name: unknown
    provider: cel
    params: {expr: "state.nothing == 1"}
""",
    )
    assert "'state.nothing' does not name a state namespace" in error


@pytest.mark.parametrize(
    ("effect", "message"),
    [
        ("params: {}", "params.expr must be a non-empty CEL expression string"),
        ("params: {expr: '1', strict: 'yes'}", "params.strict must be true or false"),
        ("params: {expr: '1', extra: 1}", "unknown param(s) 'extra'"),
        ("params: {expr: '1', schema: [1]}", "params.schema must be a JSON Schema object"),
        (
            "params: {expr: '1', schema: {type: 12}}",
            "params.schema is not a valid JSON Schema",
        ),
        (
            "params: {expr: '1'}\n    params_json: '{\"strict\": true}'",
            "params_json is not accepted",
        ),
    ],
)
def test_check_rejects_bad_params(tmp_path: Path, effect: str, message: str) -> None:
    (error,) = _check_errors(
        tmp_path,
        f"""
effects:
  - type: tool
    name: bad
    provider: cel
    {effect}
""",
    )
    assert error.startswith("Tool effect 'bad' (cel) at 'prime.effects[0]': ")
    assert message in error
