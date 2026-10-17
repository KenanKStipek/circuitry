"""A step that computes a value with CEL.

The ``cel`` tool evaluates one CEL expression against the bindings an ``if``
at the same position sees (``state``: ``state.input``, ``state.prime``,
``state.runtime``, and the enclosing loops' ``each.as`` and ``iter``) and
returns its result as JSON. It does no I/O and needs no capability.

Params (all literal: ``cof check`` parses and scope-checks ``expr``, so none
is Mustache-rendered, read ``{from: ...}`` or set by ``params_json``):
  - ``expr`` (required, str): the CEL expression.
  - ``strict`` (optional, bool, default ``false``): an unset ``state.`` path
    fails the effect instead of making the value ``null`` with a warning, as
    ``if.strict`` does.
  - ``schema`` (optional, JSON Schema object): the result must validate
    against it, or the effect fails naming each mismatch.

ToolResult shape: ``value`` is the result (lists, maps, strings, numbers,
booleans, null; a timestamp or duration as an ISO 8601 string, see
``core.cel_eval.cel_to_json``), ``raw`` is ``{"expr": <expr>}``.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import jsonschema

from ..core.cel_eval import evaluate_cel_value
from ..preflight import CheckResult
from .base import ToolResult, _as_bool

#: The params the ``cel`` tool reads; ``cof check`` rejects any other.
CEL_PARAM_KEYS: frozenset[str] = frozenset({"expr", "strict", "schema"})

#: How many schema mismatches a failure names before summarising the rest.
_MAX_QUOTED_ERRORS = 5


def static_param_errors(params: Mapping[str, Any], *, has_params_json: bool) -> list[str]:
    """What ``cof check`` rejects in a ``cel`` effect's params, apart from the
    expression's own parse and scope checks (``core.compiler`` runs those)."""
    problems: list[str] = []
    if has_params_json:
        problems.append("params_json is not accepted; write every param literally")
    unknown = sorted(str(key) for key in params if key not in CEL_PARAM_KEYS)
    if unknown:
        problems.append(
            f"unknown param(s) {', '.join(repr(key) for key in unknown)}; "
            "the params are 'expr', 'strict' and 'schema'"
        )
    expr = params.get("expr")
    if not isinstance(expr, str) or not expr.strip():
        problems.append("params.expr must be a non-empty CEL expression string")
    strict = params.get("strict")
    if strict is not None and not isinstance(strict, bool):
        problems.append("params.strict must be true or false")
    schema = params.get("schema")
    if schema is not None:
        if not isinstance(schema, dict):
            problems.append("params.schema must be a JSON Schema object")
        else:
            try:
                jsonschema.validators.validator_for(schema).check_schema(schema)
            except jsonschema.SchemaError as exc:
                problems.append(f"params.schema is not a valid JSON Schema: {exc.message}")
    return problems


def schema_errors(value: Any, schema: Mapping[str, Any]) -> list[str]:
    """Each way *value* does not match *schema*, ``<path>: <message>``."""
    validator = jsonschema.validators.validator_for(schema)(schema)
    errors = [
        f"{'/'.join(str(part) for part in error.absolute_path) or '(root)'}: {error.message}"
        for error in sorted(validator.iter_errors(value), key=lambda e: list(e.absolute_path))
    ]
    if len(errors) > _MAX_QUOTED_ERRORS:
        errors = [*errors[:_MAX_QUOTED_ERRORS], f"... and {len(errors) - _MAX_QUOTED_ERRORS} more"]
    return errors


@dataclass(frozen=True)
class CelPlugin:
    name: str = "cel"

    def evaluate(self, *, params: dict[str, Any], ctx: dict[str, Any]) -> ToolResult:
        """The expression's value against *ctx*, the dispatching effect's
        bindings. ``core.tool.ToolRuntime`` calls this instead of
        :meth:`execute`, which has no bindings to evaluate against."""
        expr = params.get("expr")
        if not isinstance(expr, str) or not expr.strip():
            raise ValueError("cel: params['expr'] must be a non-empty CEL expression string.")
        value = evaluate_cel_value(expr, ctx, strict=_as_bool(params.get("strict")))
        schema = params.get("schema")
        if isinstance(schema, dict):
            errors = schema_errors(value, schema)
            if errors:
                raise ValueError(
                    "cel: the result does not match params.schema: " + "; ".join(errors)
                )
        return ToolResult(value=value, raw={"expr": expr})

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del params, timeout_seconds
        raise RuntimeError("cel: the expression is evaluated by its tool effect, with its bindings.")

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
