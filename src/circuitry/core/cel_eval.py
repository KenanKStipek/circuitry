"""CEL expression evaluator backed by `cel-python <https://pypi.org/project/cel-python/>`_.

CEL is a core language of the DOL — a ``mode: cel`` conditional is the
only way to branch a run deterministically — so this module runs a real
CEL implementation (the ``celpy`` package) rather than a translation into
some other expression language. Everything the CEL spec defines is
available: the comprehension macros (``all``, ``exists``, ``exists_one``,
``map``, ``filter``), ``has()``, ``!``, the ternary ``?:``, the standard
function library (``size``, ``int``, ``string``, ``matches``, …) and CEL's
own type rules — so ``1 == true`` is ``false``, not ``true`` the way
Python's ``1 == True`` would have it.

Expressions are parsed once and cached by source string; a hot loop
condition pays the parser cost on its first iteration only.

The evaluator is **fail-loud about the expression**: a malformed
expression, an unknown function, or a type error raises
:class:`CelEvaluationError` rather than silently answering ``False``. A
silent ``false`` is indistinguishable from a legitimately false
condition, so a typo in an expression used to quietly route every run
down the ``else`` branch.

**Absent state is not an expression error.** An unset ``state.`` path —
a disabled node, an effect that has not run yet, a dry run with no
outputs — makes the whole expression ``False`` by rule, matching the
framework's absent-reads-empty convention (a template referencing a
disabled node renders empty; a CEL condition on one is false). That is
decided structurally, by resolving the paths the parse tree actually
reads before evaluating, and it is logged at warning level naming the
path — not swallowed from an exception, which is what hides real defects.

Two escape hatches from that rule:

* A path that appears as an argument to ``has()`` anywhere in the
  expression is **guarded** and exempt — that is what ``has()`` is for.
  ``has(state.prime.x.value) && state.prime.x.value > 1`` evaluates
  properly instead of collapsing to ``False``.
* ``strict: true`` on the conditional (or the loop's ``while``) turns an
  unresolved path back into a :class:`CelEvaluationError`. An order-exit
  rule like ``state.prime.tick.value.price <= state.input.stop_price``
  must never take the no-exit branch because a field went missing.

The same parser backs :func:`validate_cel_syntax`, which the compiler
calls for every ``mode: cel`` expression so ``cof check`` rejects a bad
expression before a run ever dispatches, and :func:`state_paths`, which
``core.state_ns`` uses to police the ``state.<namespace>`` grammar
against the parse tree rather than against a regex over the source.
"""

from __future__ import annotations

import datetime
import logging
import threading
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

import celpy
import lark
from celpy import celtypes
from celpy.evaluation import CELEvalError, base_functions

logger = logging.getLogger(__name__)

_MAX_EXPR_LENGTH = 4096

#: How many compiled expressions to keep. An orchestration has a bounded
#: number of distinct CEL expressions; the cap only matters for a
#: long-lived process compiling many different documents.
_CACHE_MAX_ENTRIES = 512


class CelError(Exception):
    """Base class for CEL parse, validation and evaluation errors."""

    def __init__(self, message: str, *, expression: str) -> None:
        super().__init__(message)
        self.expression = expression


class CelValidationError(CelError, ValueError):
    """A CEL expression was rejected at compile time (``cof check``).

    Subclasses ``ValueError`` so it flows through the compiler's existing
    error handling alongside every other compile-time rejection.
    """


class CelEvaluationError(CelError, RuntimeError):
    """A CEL expression failed to parse or evaluate at runtime.

    Carries the offending ``expression``; the underlying failure is
    chained as ``__cause__``.
    """


def evaluate_cel(expr: str, ctx: dict[str, Any], *, strict: bool = False) -> bool:
    """Evaluate a CEL expression against *ctx* and return a bool.

    *ctx* is exposed as ``state`` inside the expression, converted to CEL
    types. Nothing else is in scope: there is no way to reach a Python
    object's attributes, module or class from an expression.

    Raises :class:`CelEvaluationError` on an empty, over-long,
    unparseable or failing expression. Callers that want a branch instead
    of a failure must catch it explicitly; see the ``on_error`` handling
    in ``core.conditional`` / ``core.loop``.

    Returns ``False`` — with a warning naming the path — when an
    unguarded ``state.`` path the expression reads is unset. Pass
    ``strict=True`` to raise :class:`CelEvaluationError` for that case
    instead. See the module docstring: absent state is data, not a defect,
    unless the author says otherwise.
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

    try:
        compiled = _compile(expr)
    except CelError as exc:
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {exc}", expression=expr
        ) from exc

    unresolved = _first_unresolved(compiled.read_paths, ctx)
    if unresolved is not None:
        if strict:
            raise CelEvaluationError(
                f"CEL expression {expr!r} reads unset state path "
                f"{unresolved!r} and is marked strict.",
                expression=expr,
            )
        logger.warning(
            "CEL expression %r reads unset state path %r; condition is false",
            expr,
            unresolved,
        )
        return False

    try:
        result = compiled.runner.evaluate({"state": compiled.context(ctx)})
        if isinstance(result, CELEvalError):
            raise result
    except Exception as exc:
        logger.error("CEL evaluation failed for expr %r: %s", expr, exc)
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {exc}", expression=expr
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

    "Parses" means the CEL parser accepts it — the same parse the
    evaluator performs, so whatever compile time accepts, runtime can
    read.

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

    try:
        _compile(expr)
    except CelError as exc:
        raise CelValidationError(
            f"{where}: expression does not parse ({exc}). Expression: {expr!r}",
            expression=expr,
        ) from exc


def state_paths(expr: str) -> tuple[str, ...]:
    """Every dotted ``state.`` path *expr* reads, taken from the parse tree.

    Includes paths that appear as ``has()`` arguments — a guard is still a
    reference, and the ``state.<namespace>`` grammar applies to it. A
    quoted ``'state.x.y'`` is a string literal, not a path, and does not
    appear here.

    Raises :class:`CelValidationError` if *expr* does not parse.
    """
    return tuple(path for path, _guarded in _compile(expr).paths)


def unresolved_state_path(expr: str, ctx: Mapping[str, Any]) -> str | None:
    """The first unguarded ``state.`` path in *expr* that *ctx* does not supply.

    A path is unresolved when a segment is missing, when traversal hits
    something that is not a mapping, or when it lands on ``None`` — a
    disabled node writes ``{"value": None}``, and reading through it is
    the same "nothing there" as a key that was never written.

    Returns ``None`` when every path resolves. Paths guarded by ``has()``
    are skipped; the expression is expected to handle their absence
    itself.
    """
    return _first_unresolved(_compile(expr).read_paths, ctx)


# ---------------------------------------------------------------------------
# Compilation and caching
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class _Compiled:
    """A parsed expression plus the state paths its parse tree reads."""

    runner: celpy.Runner
    #: ``(dotted path, guarded by has())`` for every ``state.`` read.
    paths: tuple[tuple[str, bool], ...]
    #: True when the expression names ``state`` other than as the root of a
    #: dotted read (``size(state)``), so :func:`_project` cannot narrow it.
    reads_whole_state: bool

    @property
    def read_paths(self) -> tuple[str, ...]:
        """Paths subject to the absent-state rule — the unguarded ones."""
        guarded = {path for path, is_guard in self.paths if is_guard}
        return tuple(
            path
            for path, is_guard in self.paths
            if not is_guard and path not in guarded
        )

    def context(self, ctx: Mapping[str, Any]) -> Any:
        """*ctx* as CEL values, narrowed to what this expression reads.

        Converting a whole run's state costs time proportional to the state,
        not to the expression, and a condition typically reads two or three
        paths out of a store holding every effect's output.
        """
        if self.reads_whole_state:
            return _to_cel(ctx)
        return _to_cel(_project(ctx, [path for path, _ in self.paths]))


def _project(ctx: Mapping[str, Any], paths: Sequence[str]) -> dict[str, Any]:
    """A minimal mapping carrying just *paths* out of *ctx*.

    Interior mappings are rebuilt rather than shared, so nothing here can
    write back into the caller's state; leaf values are referenced as-is.
    Shortest paths are placed first, so a whole subtree taken by
    ``state.a`` is never re-entered and partially overwritten by
    ``state.a.b``.
    """
    root: dict[str, Any] = {}
    for path in sorted(set(paths), key=lambda p: (p.count("."), p)):
        source: Any = ctx
        target = root
        segments = path.split(".")[1:]
        for index, part in enumerate(segments):
            if not isinstance(source, Mapping) or part not in source:
                break
            value = source[part]
            placed = target.get(part, _ABSENT)
            if placed is value:
                break  # a shorter path already took this subtree whole
            if index == len(segments) - 1:
                target[part] = value
                break
            if not isinstance(placed, dict):
                target[part] = {}
            target = target[part]
            source = value
    return root


#: Sentinel for "no value placed yet" — ``None`` is a legitimate value.
_ABSENT = object()


#: CEL's numeric family. ``BoolType`` subclasses ``int`` in celpy, so it
#: has to be excluded explicitly — ``1 == true`` is precisely the case
#: heterogeneous equality has to get right.
_NUMERIC = (celtypes.IntType, celtypes.UintType, celtypes.DoubleType)


def _numeric(value: Any) -> bool:
    return isinstance(value, _NUMERIC) and not isinstance(value, celtypes.BoolType)


def _spec_eq(left: Any, right: Any) -> Any:
    """``==`` per the CEL spec: cross-numeric compares, other cross-type is false."""
    if isinstance(left, CELEvalError) or isinstance(right, CELEvalError):
        return _BASE_EQ(left, right)
    try:
        return _BASE_EQ(left, right)
    except TypeError:
        if _numeric(left) and _numeric(right):
            return celtypes.BoolType(float(left) == float(right))
        return celtypes.BoolType(False)


def _spec_ne(left: Any, right: Any) -> Any:
    """``!=`` per the CEL spec — the negation of :func:`_spec_eq`."""
    if isinstance(left, CELEvalError) or isinstance(right, CELEvalError):
        return _BASE_NE(left, right)
    try:
        return _BASE_NE(left, right)
    except TypeError:
        if _numeric(left) and _numeric(right):
            return celtypes.BoolType(float(left) != float(right))
        return celtypes.BoolType(True)


_BASE_EQ = base_functions["_==_"]
_BASE_NE = base_functions["_!=_"]

#: CEL's heterogeneous equality (cel-spec #103): comparing values of
#: different runtime types yields false rather than an error, while
#: int/uint/double are one numeric family and do compare. celpy raises a
#: no-such-overload error for both cases, so the two operators are
#: overridden here rather than left to surface as evaluation failures.
_FUNCTIONS: dict[str, Any] = {"_==_": _spec_eq, "_!=_": _spec_ne}

_ENV = celpy.Environment()

#: ``celpy.Environment`` wraps a lark parser; parsing is not documented as
#: thread-safe and the cache is shared, so both live under one lock.
_LOCK = threading.Lock()
_CACHE: dict[str, _Compiled] = {}


def _compile(expr: str) -> _Compiled:
    """Parse *expr* once and memoise the runner and its state paths."""
    with _LOCK:
        cached = _CACHE.get(expr)
        if cached is not None:
            return cached
        try:
            tree = _ENV.compile(expr)
        except Exception as exc:
            raise CelValidationError(str(exc).strip(), expression=expr) from exc
        paths = tuple(_collect_state_paths(tree))
        compiled = _Compiled(
            runner=_ENV.program(tree, functions=_FUNCTIONS),
            paths=paths,
            reads_whole_state=_count_state_idents(tree) != len(paths),
        )
        if len(_CACHE) >= _CACHE_MAX_ENTRIES:
            _CACHE.clear()
        _CACHE[expr] = compiled
        return compiled


def clear_expression_cache() -> None:
    """Drop every compiled expression. For tests and benchmarks."""
    with _LOCK:
        _CACHE.clear()


# ---------------------------------------------------------------------------
# Parse-tree inspection
# ---------------------------------------------------------------------------

#: Single-child wrapper nodes a dotted chain passes through on its way to
#: the root ``ident``.
_PASSTHROUGH = frozenset({"member", "primary"})


def _dotted_chain(node: Any) -> list[str] | None:
    """The segments of *node* if it is a pure ``a.b.c`` chain, else ``None``.

    Anything else in the chain — an index, a call, an arithmetic term —
    disqualifies it, because the result would no longer be a path that
    can be resolved structurally against a state mapping.
    """
    if not isinstance(node, lark.Tree):
        return None
    data = str(node.data)
    if data == "member_dot":
        base, name = node.children[0], node.children[1]
        prefix = _dotted_chain(base)
        if prefix is None:
            return None
        return [*prefix, str(name)]
    if data == "ident" and node.children:
        return [str(node.children[0])]
    if data in _PASSTHROUGH and len(node.children) == 1:
        return _dotted_chain(node.children[0])
    return None


def _collect_state_paths(
    node: Any, *, guarded: bool = False
) -> list[tuple[str, bool]]:
    """Every ``state.`` path under *node*, flagged when it guards a ``has()``.

    Walks top-down and stops at the longest dotted chain, so
    ``state.a.b.c`` is reported once rather than once per prefix.
    """
    if not isinstance(node, lark.Tree):
        return []
    data = str(node.data)

    if data == "member_dot":
        segments = _dotted_chain(node)
        if segments is not None:
            return (
                [(".".join(segments), guarded)] if segments[0] == "state" else []
            )

    if data == "ident_arg" and node.children and str(node.children[0]) == "has":
        return [
            path
            for child in node.children[1:]
            for path in _collect_state_paths(child, guarded=True)
        ]

    return [
        path
        for child in node.children
        for path in _collect_state_paths(child, guarded=guarded)
    ]


def _count_state_idents(node: Any) -> int:
    """How many times ``state`` is named anywhere under *node*.

    Compared against the number of collected paths to tell a plain dotted
    read from a use of ``state`` as a value in its own right.
    """
    if not isinstance(node, lark.Tree):
        return 0
    if str(node.data) == "ident" and node.children:
        return 1 if str(node.children[0]) == "state" else 0
    return sum(_count_state_idents(child) for child in node.children)


def _first_unresolved(
    paths: Sequence[str], ctx: Mapping[str, Any]
) -> str | None:
    """The first path in *paths* that *ctx* does not supply."""
    for path in paths:
        current: Any = ctx
        for part in path.split(".")[1:]:
            if isinstance(current, Mapping) and part in current:
                current = current[part]
            else:
                return path
        if current is None:
            return path
    return None


# ---------------------------------------------------------------------------
# Python -> CEL value conversion
# ---------------------------------------------------------------------------


def _to_cel(value: Any) -> Any:
    """Convert a Python value into the CEL type system.

    This is also the sandbox boundary: a value with no CEL counterpart
    becomes CEL ``null`` rather than being handed to the evaluator as a
    live Python object, so no expression can reach an attribute, a method
    or a class through state.
    """
    if value is None:
        return None
    if isinstance(value, bool):
        return celtypes.BoolType(value)
    if isinstance(value, int):
        return celtypes.IntType(value)
    if isinstance(value, float):
        return celtypes.DoubleType(value)
    if isinstance(value, str):
        return celtypes.StringType(value)
    if isinstance(value, bytes):
        return celtypes.BytesType(value)
    if isinstance(value, Mapping):
        return celtypes.MapType(
            {_to_cel(key): _to_cel(item) for key, item in value.items()}
        )
    if isinstance(value, (list, tuple, set, frozenset)):
        return celtypes.ListType([_to_cel(item) for item in value])
    if isinstance(value, datetime.datetime):
        return celtypes.TimestampType(value)
    if isinstance(value, datetime.timedelta):
        return celtypes.DurationType(value)
    logger.debug(
        "CEL: %s has no CEL counterpart; exposing it as null", type(value).__name__
    )
    return None
