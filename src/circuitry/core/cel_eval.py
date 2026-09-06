"""Safe CEL expression evaluator using simpleeval (no eval()).

Replaces the previous eval()-based approach which was trivially
bypassable via __class__.__bases__ chains.

The evaluator is **fail-loud about the expression**: a malformed
expression, an unsupported construct, an unknown function or a blocked
attribute chain raises :class:`CelEvaluationError` rather than silently
answering ``False``. A silent ``false`` is indistinguishable from a
legitimately false condition, so a typo in an expression used to quietly
route every run down the ``else`` branch.

**Absent state is not an expression error.** An unset ``state.`` path —
a disabled node, an effect that has not run yet, a dry run with no
outputs — makes the whole expression ``False`` by rule, matching the
framework's absent-reads-empty convention (a template referencing a
disabled node renders empty; a CEL condition on one is false). That is
decided structurally, by resolving the paths before evaluating, and it
is logged at warning level naming the path — not swallowed from an
exception, which is what used to hide the real defects too.

The same translate-then-parse pipeline backs :func:`validate_cel_syntax`,
which the compiler calls for every ``mode: cel`` expression so ``cof
check`` rejects a bad expression before a run ever dispatches. Validator
and evaluator share ``_cel_to_python`` deliberately: whatever the
translator cannot express, compile time reports and runtime never sees.

This whole module is a stopgap — a regex→simpleeval translator standing
in for a real CEL implementation (see #185, which swaps it for
cel-python). Keep the public surface (``evaluate_cel``,
``validate_cel_syntax``, the two error types) stable so the swap is a
single-file replacement.
"""

from __future__ import annotations

import ast
import logging
import re
from collections.abc import Mapping
from typing import Any

from simpleeval import SimpleEval

logger = logging.getLogger(__name__)

_MAX_EXPR_LENGTH = 4096


class CelError(Exception):
    """Base class for CEL translation, validation and evaluation errors."""

    def __init__(self, message: str, *, expression: str) -> None:
        super().__init__(message)
        self.expression = expression


class CelValidationError(CelError, ValueError):
    """A CEL expression was rejected at compile time (``cof check``).

    Subclasses ``ValueError`` so it flows through the compiler's existing
    error handling alongside every other compile-time rejection.
    """


class CelEvaluationError(CelError, RuntimeError):
    """A CEL expression failed to translate, parse or evaluate at runtime.

    Carries the offending ``expression``; the underlying failure is
    chained as ``__cause__``.
    """


def evaluate_cel(expr: str, ctx: dict[str, Any]) -> bool:
    """Evaluate a CEL-subset expression against *ctx* and return a bool.

    *ctx* is exposed as ``state`` inside the expression.

    Raises :class:`CelEvaluationError` on an empty, over-long,
    untranslatable, unparseable or failing expression. Callers that want
    a branch instead of a failure must catch it explicitly; see the
    ``on_error`` handling in ``core.conditional`` / ``core.loop``.

    Returns ``False`` — with a warning naming the path — when a
    ``state.`` path the expression reads is unset. See the module
    docstring: absent state is data, not a defect.
    """
    if not expr or not expr.strip():
        raise CelEvaluationError(
            "CEL expression is empty; nothing to evaluate.", expression=expr
        )

    if len(expr) > _MAX_EXPR_LENGTH:
        raise CelEvaluationError(
            f"CEL expression too long ({len(expr)} chars, max "
            f"{_MAX_EXPR_LENGTH}).",
            expression=expr,
        )

    unresolved = unresolved_state_path(expr, ctx)
    if unresolved is not None:
        logger.warning(
            "CEL expression %r reads unset state path %r; condition is false",
            expr,
            unresolved,
        )
        return False

    try:
        py_expr = _cel_to_python(expr)

        evaluator = SimpleEval()
        evaluator.names = {"state": ctx, "true": True, "false": False}
        evaluator.functions = {"size": len, "int": int, "string": str}

        result = evaluator.eval(py_expr)
    except Exception as exc:
        logger.error("CEL evaluation failed for expr %r: %s", expr, exc)
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {_describe(expr, exc)}",
            expression=expr,
        ) from exc

    return bool(result)


def validate_cel_syntax(
    expr: str,
    *,
    effect_path: str,
    effect_name: str | None = None,
    label: str = "CEL expression",
) -> None:
    """Compile-time check for a single ``mode: cel`` expression.

    "Parses" means: the translator accepts the expression *and* the
    Python it produces is parseable (the same parse ``simpleeval``
    performs before evaluating). Known-unsupported CEL syntax is named
    explicitly rather than passing silently or hiding behind a generic
    syntax error.

    Raises :class:`CelValidationError` naming the effect and the
    expression. Never evaluates anything — no state is available at
    compile time, so an expression that parses may still fail at run
    time (loudly, via :class:`CelEvaluationError`).
    """
    where = f"{label} at '{effect_path}'"
    if effect_name:
        where = f"{where} (effect '{effect_name}')"

    if not expr or not expr.strip():
        raise CelValidationError(f"{where}: expression is empty.", expression=expr)

    if len(expr) > _MAX_EXPR_LENGTH:
        raise CelValidationError(
            f"{where}: expression is too long ({len(expr)} chars, max "
            f"{_MAX_EXPR_LENGTH}).",
            expression=expr,
        )

    unsupported = _unsupported_reason(expr)
    if unsupported is not None:
        raise CelValidationError(
            f"{where}: {unsupported} Expression: {expr!r}", expression=expr
        )

    try:
        py_expr = _cel_to_python(expr)
        ast.parse(py_expr, mode="eval")
    except Exception as exc:
        raise CelValidationError(
            f"{where}: expression does not parse ({exc}). Expression: {expr!r}",
            expression=expr,
        ) from exc


#: A dotted read rooted at ``state`` — the only state grammar the
#: translator understands, and so the only one worth resolving up front.
_STATE_PATH = re.compile(r"\bstate(?:\.[a-zA-Z_][a-zA-Z0-9_]*)+")


def unresolved_state_path(expr: str, ctx: Mapping[str, Any]) -> str | None:
    """The first ``state.`` path in *expr* that *ctx* does not supply.

    A path is unresolved when a segment is missing, when traversal hits
    something that is not a mapping, or when it lands on ``None`` — a
    disabled node writes ``{"value": None}``, and reading through it is
    the same "nothing there" as a key that was never written.

    Returns ``None`` when every path resolves. String literals are masked
    first so ``'state.x'`` inside a quoted value is not treated as a read.
    """
    for match in _STATE_PATH.finditer(_mask_string_literals(expr)):
        current: Any = ctx
        for part in match.group(0).split(".")[1:]:
            if isinstance(current, Mapping) and part in current:
                current = current[part]
            else:
                return match.group(0)
        if current is None:
            return match.group(0)
    return None


def _cel_to_python(expr: str) -> str:
    """Pre-process CEL syntax into Python that simpleeval can parse."""

    # state.foo.bar  ->  state["foo"]["bar"]
    def _replace_dots(match: re.Match) -> str:
        parts = match.group(0).split(".")
        result = parts[0]
        for part in parts[1:]:
            result += f'["{part}"]'
        return result

    converted = _STATE_PATH.sub(_replace_dots, expr)

    # Normalise comparison operators (add spacing)
    converted = converted.replace("==", " == ").replace("!=", " != ")

    # CEL boolean operators -> Python
    return converted.replace("&&", " and ").replace("||", " or ")


# --------------------------------------------------------------------------
# Unsupported-construct detection
#
# The translator covers a small slice of CEL. Everything below is real CEL
# that this evaluator cannot run; each entry is reported by name so an
# author is not left reading a Python SyntaxError about their CEL. Drop
# entries here as #185 (cel-python) makes them work.
# --------------------------------------------------------------------------

_UNSUPPORTED: tuple[tuple[re.Pattern[str], str], ...] = (
    (
        re.compile(r"!(?!=)"),
        "CEL negation '!' is not supported yet; write '== false' instead.",
    ),
    (
        re.compile(r"\bhas\s*\("),
        (
            "the CEL 'has()' macro is not supported yet; compare the field "
            "directly instead."
        ),
    ),
    (
        re.compile(r"\.\s*(?:filter|map|all|exists_one|exists)\s*\("),
        (
            "CEL comprehension macros (.filter/.map/.all/.exists/.exists_one) "
            "are not supported yet; use 'size(...)' or a loop effect instead."
        ),
    ),
    (
        re.compile(r"\bfor\b"),
        "comprehensions are not supported yet; use a loop effect instead.",
    ),
    (
        re.compile(r"\?"),
        (
            "the conditional/ternary operator ('?:', '?.') is not supported "
            "yet; use a conditional effect instead."
        ),
    ),
)


def _unsupported_reason(expr: str) -> str | None:
    """Name the first known-unsupported construct in *expr*, if any.

    String literals are masked first so a ``'!'`` or ``'?'`` inside a
    quoted value is not mistaken for an operator.
    """
    masked = _mask_string_literals(expr)
    for pattern, reason in _UNSUPPORTED:
        if pattern.search(masked):
            return reason
    return None


_STRING_LITERAL = re.compile(r"'(?:\\.|[^'\\])*'|\"(?:\\.|[^\"\\])*\"")


def _mask_string_literals(expr: str) -> str:
    """Blank out the *contents* of quoted literals, preserving length."""
    return _STRING_LITERAL.sub(lambda m: m.group(0)[0] * len(m.group(0)), expr)


def _describe(expr: str, exc: Exception) -> str:
    """Runtime failure message, enriched when the cause is a known gap."""
    unsupported = _unsupported_reason(expr)
    if unsupported is not None:
        return f"{unsupported} ({exc})"
    return str(exc)
