"""`expect:` output check shared by `tool` and `use` effects (#273).

Either CEL (`expect: "has(value.prompt_id)"`, or the long form
`{mode: cel, expr: ...}`) evaluated over the effect's own `value` and
`meta`, plus `state` — see `cel_eval.evaluate_cel_expect` for exactly what
those three bindings mean — or model mode (`{mode: model, template: ...}`),
which asks yes/no the same way a model-mode `if` does, on the run's own
adapter/model, and records its tokens.

A false or unreadable expectation never raises past this module: it comes
back as `ExpectResult(passed=False, ...)`, and the caller (`core.tool`,
`core.use`) is the one that turns that into the effect's own failure
(`expect failed: <expr or template summary>`), where `retries` and
`on_error` apply exactly like any other attempt failure.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from ..adapters import Adapter
from .answers import AnswerParseError, parse_boolean_answer
from .cel_eval import CelEvaluationError, evaluate_cel_expect
from .templates import render_template


@dataclass(frozen=True)
class ExpectDef:
    """`expect:` on a `tool`/`use` effect. A bare CEL string normalizes to
    `mode: cel` at compile time (see `core.compiler._compile_expect`)."""

    mode: Literal["cel", "model"] = "cel"
    expr: str | None = None  # mode: cel
    template: str | None = None  # mode: model


@dataclass(frozen=True)
class ExpectResult:
    passed: bool
    #: What the caller records verbatim at this effect's own `meta.expect`.
    meta: dict[str, Any]


def expect_failure_summary(defn: ExpectDef) -> str:
    """A short description of *defn* for the `expect failed: ...` message."""
    if defn.mode == "cel":
        return defn.expr or ""
    collapsed = " ".join((defn.template or "").split())
    return collapsed if len(collapsed) <= 80 else f"{collapsed[:80]}..."


def evaluate_expect(
    defn: ExpectDef,
    *,
    value: Any,
    meta: dict[str, Any],
    ctx: dict[str, Any],
    adapter: Adapter | None,
    model: str | None,
    timeout_seconds: int,
    effect_label: str,
) -> ExpectResult:
    """Run *defn* against this effect's own result, once it has one.

    Raises only on a configuration error (`mode: model` with no
    adapter/model available — the compiler already required a `template`
    for that mode, so this is an environment problem, not a document one).
    """
    if defn.mode == "model":
        if adapter is None or model is None:
            raise ValueError(
                f"{effect_label}: expect mode 'model' requires the run's "
                "adapter/model, which is not available here."
            )
        rendered = render_template(
            defn.template or "",
            {**ctx, "value": value, "meta": meta},
            label="expect.template",
        )
        prompt = (
            "Evaluate the following and respond with ONLY 'yes' or 'no':\n\n"
            f"{rendered}\n\nAnswer (yes/no):"
        )
        res = adapter.generate(
            model=model, prompt=prompt, timeout_seconds=timeout_seconds
        )
        result_meta: dict[str, Any] = {
            "mode": "model",
            "template": expect_failure_summary(defn),
            "answer": res.text,
            "tokens_sent": res.tokens_sent,
            "tokens_received": res.tokens_received,
        }
        try:
            passed = parse_boolean_answer(res.text or "")
        except AnswerParseError as e:
            result_meta["error"] = str(e)
            result_meta["result"] = False
            return ExpectResult(passed=False, meta=result_meta)
        result_meta["result"] = passed
        return ExpectResult(passed=passed, meta=result_meta)

    expr = defn.expr or ""
    cel_meta: dict[str, Any] = {"mode": "cel", "expr": expr}
    try:
        passed = evaluate_cel_expect(expr, value=value, meta=meta, state=ctx)
    except CelEvaluationError as e:
        cel_meta["error"] = str(e)
        cel_meta["result"] = False
        return ExpectResult(passed=False, meta=cel_meta)
    cel_meta["result"] = passed
    return ExpectResult(passed=passed, meta=cel_meta)
