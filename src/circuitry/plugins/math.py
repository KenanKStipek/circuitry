"""Safe arithmetic expression evaluator.

Evaluates simple math expressions without invoking ``eval()``. The
expression is parsed with ``ast`` and only a small whitelist of node
types is permitted: numeric literals, the standard binary/unary
operators, and parentheses. Anything else (function calls, attribute
access, names) is rejected.

Params:
  - ``expression`` (required, str): the math expression to evaluate.

Returns the numeric result as ``ToolResult.value`` (int when the
result is exactly representable as an integer, else float).
"""

from __future__ import annotations

import ast
import operator as _op
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_BIN_OPS: dict[type, Any] = {
    ast.Add: _op.add,
    ast.Sub: _op.sub,
    ast.Mult: _op.mul,
    ast.Div: _op.truediv,
    ast.FloorDiv: _op.floordiv,
    ast.Mod: _op.mod,
    ast.Pow: _op.pow,
}

_UNARY_OPS: dict[type, Any] = {
    ast.UAdd: _op.pos,
    ast.USub: _op.neg,
}


def _eval(node: ast.AST) -> Any:
    if isinstance(node, ast.Expression):
        return _eval(node.body)
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)):
        return node.value
    if isinstance(node, ast.BinOp):
        op_fn = _BIN_OPS.get(type(node.op))
        if op_fn is None:
            raise ValueError(
                f"math: unsupported operator {type(node.op).__name__}"
            )
        return op_fn(_eval(node.left), _eval(node.right))
    if isinstance(node, ast.UnaryOp):
        op_fn = _UNARY_OPS.get(type(node.op))
        if op_fn is None:
            raise ValueError(
                f"math: unsupported unary operator {type(node.op).__name__}"
            )
        return op_fn(_eval(node.operand))
    raise ValueError(
        f"math: disallowed expression node {type(node).__name__}"
    )


@dataclass(frozen=True)
class MathPlugin:
    name: str = "math"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        expr = params.get("expression")
        if not isinstance(expr, str) or not expr.strip():
            raise ValueError(
                "MathPlugin requires params['expression'] as a non-empty string."
            )
        try:
            tree = ast.parse(expr.strip(), mode="eval")
        except SyntaxError as exc:
            raise ValueError(f"math: invalid expression: {exc}") from exc
        result = _eval(tree)
        if isinstance(result, float) and result.is_integer():
            result = int(result)
        return ToolResult(
            value=result,
            raw={"expression": expr.strip(), "result": result},
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
