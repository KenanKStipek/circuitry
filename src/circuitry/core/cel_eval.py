"""Safe CEL expression evaluator using simpleeval (no eval()).

Replaces the previous eval()-based approach which was trivially
bypassable via __class__.__bases__ chains.
"""

from __future__ import annotations

import logging
import re
from typing import Any

from simpleeval import SimpleEval

logger = logging.getLogger(__name__)

_MAX_EXPR_LENGTH = 4096


def evaluate_cel(expr: str, ctx: dict[str, Any]) -> bool:
    """Evaluate a CEL-subset expression against *ctx* and return a bool.

    *ctx* is exposed as ``state`` inside the expression.  Returns ``False``
    on any error (preserves existing error-swallowing behaviour).
    """
    if not expr or not expr.strip():
        return False

    if len(expr) > _MAX_EXPR_LENGTH:
        logger.error("CEL expression too long (%d chars, max %d)", len(expr), _MAX_EXPR_LENGTH)
        return False

    try:
        py_expr = _cel_to_python(expr)

        evaluator = SimpleEval()
        evaluator.names = {"state": ctx, "true": True, "false": False}
        evaluator.functions = {"size": len, "int": int, "string": str}

        result = evaluator.eval(py_expr)
        return bool(result)
    except Exception as exc:
        logger.error("CEL evaluation failed for expr %r: %s", expr, exc, exc_info=True)
        return False


def _cel_to_python(expr: str) -> str:
    """Pre-process CEL syntax into Python that simpleeval can parse."""

    # state.foo.bar  ->  state["foo"]["bar"]
    def _replace_dots(match: re.Match) -> str:
        parts = match.group(0).split(".")
        result = parts[0]
        for part in parts[1:]:
            result += f'["{part}"]'
        return result

    pattern = r"\bstate(?:\.[a-zA-Z_][a-zA-Z0-9_]*)+"
    converted = re.sub(pattern, _replace_dots, expr)

    # Normalise comparison operators (add spacing)
    converted = converted.replace("==", " == ").replace("!=", " != ")

    # CEL boolean operators -> Python
    converted = converted.replace("&&", " and ").replace("||", " or ")

    return converted
