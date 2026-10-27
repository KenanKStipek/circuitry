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
disabled node renders empty; a CEL condition on one is false). That rule
follows evaluation rather than preceding it: each unguarded ``state.``
read is decided, against the run state, only when evaluation actually
reaches it, and the rule applies only if the whole expression's result
turns out to be that one read. ``has()``, the ternary ``?:``, ``&&``/
``||`` and the ``all``/``exists`` macros guard a read exactly the way CEL
defines those constructs — an untaken ternary branch, the short-circuited
side of ``&&``/``||``, or an ``exists`` element that already matched
never triggers the rule. When it does apply, it is logged at warning
level naming the path — not swallowed from an exception, which is what
hides real defects. An expression whose unset read does not decide the
result reports whatever other error it has instead of quietly being
``False``.

Two escape hatches from that rule:

* ``has(state.prime.x.value)`` is true exactly when that read would
  evaluate without error — unaffected by this rule either way.
  ``has(state.prime.x.value) && state.prime.x.value > 1`` evaluates
  properly instead of collapsing to ``False``.
* ``strict: true`` on the conditional (or the loop's ``while``) turns an
  unresolved read back into a :class:`CelEvaluationError`, but only for a
  read that decides the result. An order-exit rule like
  ``state.prime.tick.value.price <= state.input.stop_price`` must never
  take the no-exit branch because a field went missing.

The same parser backs :func:`validate_cel_syntax`, which the compiler
calls for every ``mode: cel`` expression so ``cof check`` rejects a bad
expression before a run ever dispatches, and :func:`state_paths`, which
``core.state_ns`` uses to police the ``state.<namespace>`` grammar
against the parse tree rather than against a regex over the source.
"""

from __future__ import annotations

import datetime
import logging
import math
import operator
import threading
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from functools import reduce as functools_reduce
from typing import Any, cast

import celpy
import lark
from celpy import celtypes
from celpy.evaluation import (
    CELEvalError,
    CELSyntaxError,
    Evaluator,
    NameContainer,
    base_functions,
)

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


#: What :func:`_evaluate_in_state` returns when an unguarded ``state.`` path
#: the expression reads is unset and the caller is not strict.
_UNSET_STATE = object()

#: ``_StateEvaluator``'s marks, shared and empty, for a site with no
#: unset-path rule (``expect:``) — every ``member_dot`` just evaluates
#: normally, with the same correct ``?:``/``&&``/``||``/macro behaviour.
_NO_MARKS: dict[int, str] = {}


class _UnsetRead(CELEvalError):
    """Stands in for one unguarded ``state.`` read while it is unset.

    Raised by :class:`_StateEvaluator`'s ``member_dot`` in place of
    evaluating the chain, so the surrounding expression decides — via the
    ordinary CEL rules for ``has()``, ``?:``, ``&&``/``||`` and
    ``all``/``exists`` — whether this read ends up deciding the result.
    Only then does :func:`_evaluate_in_state` apply the absent-state rule,
    naming :attr:`path`.
    """

    def __init__(self, path: str) -> None:
        super().__init__(f"unset state path {path!r}", KeyError, (path,))
        self.path = path


def _error_text(exc: CELEvalError) -> str:
    """*exc* in Circuitry's own words, never cel-python's activation repr.

    cel-python words an undeclared identifier, function or method as
    ``"undeclared reference to '<name>' (in activation '<repr>')"``, with
    the name recoverable from ``exc.args[2][0]`` regardless of which of
    the three raised it. The activation repr holds the whole
    ``google.protobuf`` annotation tree, every registered function, and —
    for :func:`evaluate_cel_expect` — the converted run state, so it is
    never included. Every other evaluation failure keeps cel-python's own
    message.
    """
    args = exc.args
    if (
        args
        and isinstance(args[0], str)
        and args[0].startswith("undeclared reference to ")
        and len(args) > 2
        and isinstance(args[2], tuple)
        and args[2]
    ):
        return f"undeclared reference to {args[2][0]!r}"
    return str(exc)


class _StateEvaluator(Evaluator):
    """Evaluates CEL the way :mod:`cel_eval` needs, not the way cel-python does.

    Every method below replaces its base-class counterpart's body outright
    (never a ``super()`` call, never a shared helper called per tree
    level): cel-python's nesting limit is Python's own recursion limit, the
    CEL golden corpus pins where that limit falls for each shape, and one
    extra frame per level moves it. ``marks`` is ``id(node) -> path`` for
    the ``member_dot`` occurrences this one evaluation found unset —
    computed once, before evaluating, by :func:`_mark_unset`; it is shared
    unchanged with every :meth:`sub_evaluator` this evaluator builds for a
    macro body, so a mark inside ``all``/``exists``/``map``/``filter``
    still fires. The cached parse tree itself is shared and read-only.
    """

    def __init__(self, ast: lark.Tree, activation: Any, marks: dict[int, str]) -> None:
        super().__init__(ast, activation)
        self._marks = marks

    def sub_evaluator(self, ast: lark.Tree) -> _StateEvaluator:
        return _StateEvaluator(ast, self.activation, self._marks)

    def member_dot(self, tree: lark.Tree) -> Any:
        path = self._marks.get(id(tree))
        if path is not None:
            return _UnsetRead(path)
        member_tree, property_name_token = cast(
            "tuple[lark.Tree, lark.Token]", tree.children
        )
        member = self.visit(member_tree)
        property_name = property_name_token.value
        if isinstance(member, CELEvalError):
            return member
        if isinstance(member, NameContainer):
            if property_name in member:
                return member[property_name].value
            err = f"No {property_name!r} in bindings {sorted(member.keys())}"
            return CELEvalError(err, KeyError, None, tree=tree)
        if isinstance(member, celtypes.MessageType):
            return member.get(property_name)
        if isinstance(member, celtypes.MapType):
            try:
                return member[property_name]
            except KeyError:
                err = f"no such member in mapping: {property_name!r}"
                return CELEvalError(err, KeyError, None, tree=tree)
        err = (
            f"{member!r} with type: '{type(member)}' does not support field "
            "selection"
        )
        return CELEvalError(err, TypeError, None, tree=tree)

    def expr(self, tree: lark.Tree) -> Any:
        if len(tree.children) == 1:
            values = self.visit_children(tree)
            return values[0]
        if len(tree.children) == 3:
            cond_value = self.visit(tree.children[0])
            if isinstance(cond_value, CELEvalError):
                return cond_value
            if isinstance(cond_value, celtypes.BoolType):
                return self.visit(tree.children[1] if cond_value else tree.children[2])
            func = self.activation.resolve_function("_?_:_")
            left: Any = celtypes.BoolType(False)
            right: Any = celtypes.BoolType(False)
            try:
                if cond_value:
                    left = self.visit(tree.children[1])
                else:
                    right = self.visit(tree.children[2])
                return func(cond_value, left, right)
            except TypeError as ex:
                err = (
                    f"found no matching overload for _?_:_ applied to "
                    f"'({type(cond_value)}, {type(left)}, {type(right)})'"
                )
                value = CELEvalError(err, ex.__class__, ex.args, tree=tree)
                value.__cause__ = ex
                return value
        raise CELSyntaxError(
            f"{tree.data} {tree.children}: bad expr node",
            line=tree.meta.line,
            column=tree.meta.column,
        )

    def conditionalor(self, tree: lark.Tree) -> Any:
        if len(tree.children) == 1:
            values = self.visit_children(tree)
            return values[0]
        if len(tree.children) == 2:
            # Both sides are visited unconditionally, exactly like the base
            # method's own ``visit_children(tree)`` call (never two direct
            # ``self.visit()`` calls here, which would shed the frames that
            # call goes through and move the nesting-depth boundary this
            # left-recursive grammar rule shares with every other left
            # operand). cel-python turns every relevant failure (an unset
            # read, a bad operand) into a value rather than raising, so
            # visiting the side that does not end up deciding has no
            # observable effect beyond the one real short-circuiting buys:
            # not raising on a *catastrophic* failure (nesting) there — the
            # same limit the base method already has.
            left, right = self.visit_children(tree)
            if isinstance(left, celtypes.BoolType) and bool(left):
                return left
            if isinstance(right, celtypes.BoolType) and bool(right):
                return right
            if isinstance(right, CELEvalError):
                return right
            if isinstance(left, CELEvalError):
                return left
            if isinstance(left, celtypes.BoolType) and isinstance(
                right, celtypes.BoolType
            ):
                return celtypes.BoolType(bool(left) or bool(right))
            err = (
                f"found no matching overload for _||_ applied to "
                f"'({type(left)}, {type(right)})'"
            )
            return CELEvalError(err, TypeError, None, tree=tree)
        raise CELSyntaxError(
            f"{tree.data} {tree.children}: bad conditionalor node",
            line=tree.meta.line,
            column=tree.meta.column,
        )

    def conditionaland(self, tree: lark.Tree) -> Any:
        if len(tree.children) == 1:
            values = self.visit_children(tree)
            return values[0]
        if len(tree.children) == 2:
            # See conditionalor's comment: both sides visited unconditionally.
            left, right = self.visit_children(tree)
            if isinstance(left, celtypes.BoolType) and not bool(left):
                return left
            if isinstance(right, celtypes.BoolType) and not bool(right):
                return right
            if isinstance(right, CELEvalError):
                return right
            if isinstance(left, CELEvalError):
                return left
            if isinstance(left, celtypes.BoolType) and isinstance(
                right, celtypes.BoolType
            ):
                return celtypes.BoolType(bool(left) and bool(right))
            err = (
                f"found no matching overload for _&&_ applied to "
                f"'({type(left)}, {type(right)})'"
            )
            return CELEvalError(err, TypeError, None, tree=tree)
        raise CELSyntaxError(
            f"{tree.data} {tree.children}: bad conditionaland node",
            line=tree.meta.line,
            column=tree.meta.column,
        )

    def mapinits(self, tree: lark.Tree) -> Any:
        # ``visit_children`` (not a per-child ``self.visit()`` loop), to
        # match the base method's own frame count per nesting level — see
        # conditionalor's comment.
        values = self.visit_children(tree)
        for value in values:
            if isinstance(value, CELEvalError):
                return value
        result = celtypes.MapType()
        for key, value in zip(values[0::2], values[1::2], strict=True):
            if key in result:
                raise ValueError(f"Duplicate key {key!r}")
            result[cast(Any, key)] = cast(Any, value)
        return result

    def member_dot_arg(self, tree: lark.Tree) -> Any:
        member_tree, method_name_token = cast(
            "tuple[lark.Tree, lark.Token]", tree.children[:2]
        )
        if method_name_token.value in {
            "map",
            "filter",
            "all",
            "exists",
            "exists_one",
            "reduce",
            "min",
        }:
            member_list = self.visit(member_tree)
            if isinstance(member_list, CELEvalError):
                return member_list
            name = method_name_token.value
            if name == "map":
                # build_ss_macro_eval (not build_macro_eval), matching all/exists
                # below: the base build_macro_eval's sub_expr *raises* a
                # CELEvalError, which would escape list construction as a
                # Python exception instead of a value, bypassing the
                # enclosing &&/|| and reaching strict/the unset-path rule
                # directly rather than letting the operator decide.
                sub_expr = self.build_ss_macro_eval(tree)
                mapped: list[Any] = []
                for item in member_list:
                    value = sub_expr(item)
                    if isinstance(value, CELEvalError):
                        return value
                    mapped.append(value)
                return celtypes.ListType(mapped)
            if name == "filter":
                sub_expr = self.build_ss_macro_eval(tree)
                filtered: list[Any] = []
                for item in member_list:
                    value = sub_expr(item)
                    if isinstance(value, CELEvalError):
                        return value
                    if value:
                        filtered.append(item)
                return celtypes.ListType(filtered)
            if name in ("all", "exists"):
                sub_expr = self.build_ss_macro_eval(tree)
                decisive = name == "exists"
                last_error: Any = None
                for item in member_list:
                    value = sub_expr(item)
                    if isinstance(value, celtypes.BoolType) and bool(value) == decisive:
                        return celtypes.BoolType(decisive)
                    if isinstance(value, CELEvalError):
                        last_error = value
                    elif not isinstance(value, celtypes.BoolType):
                        last_error = CELEvalError(
                            "no such overload", TypeError, None, tree=tree
                        )
                if last_error is not None:
                    return last_error
                return celtypes.BoolType(not decisive)
            if name == "exists_one":
                sub_expr = self.build_ss_macro_eval(tree)
                count = 0
                for item in member_list:
                    value = sub_expr(item)
                    if isinstance(value, CELEvalError):
                        return value
                    if bool(value):
                        count += 1
                return celtypes.BoolType(count == 1)
            if name == "reduce":
                reduce_expr, init_expr_tree = self.build_reduce_macro_eval(tree)
                initial_value = self.visit(init_expr_tree)
                return functools_reduce(reduce_expr, member_list, initial_value)
            # name == "min"
            try:
                return min(member_list)
            except ValueError as ex:
                err = (
                    "Attempt to reduce an empty sequence or a sequence with a "
                    "None value"
                )
                return CELEvalError(err, ex.__class__, ex.args, tree=tree)
        if len(tree.children) == 2:
            member, ident = self.visit_children(tree)
            return self.method_eval(member, cast(lark.Token, ident))
        member, ident, expr_iter = self.visit_children(tree)
        return self.method_eval(
            member, cast(lark.Token, ident), cast(Any, expr_iter)
        )


def _mark_unset(
    occurrences: tuple[tuple[Any, str], ...], ctx: Mapping[str, Any]
) -> dict[int, str]:
    """``id(node) -> path`` for every occurrence in *occurrences* that *ctx* lacks.

    Runs once per evaluation, not once per tree level — unlike
    :class:`_StateEvaluator`'s methods, this has no nesting-depth budget to
    respect.
    """
    return {
        id(node): path
        for node, path in occurrences
        if _first_unresolved([path], ctx) is not None
    }


def _evaluate_in_state(
    expr: str, ctx: dict[str, Any], *, strict: bool, unset_outcome: str
) -> Any:
    """*expr*'s CEL result with *ctx* bound as ``state``: what
    :func:`evaluate_cel` and :func:`evaluate_cel_value` share.

    Returns :data:`_UNSET_STATE`, after a warning that names the path and
    ends with *unset_outcome* (what the caller gives instead), when an
    unguarded ``state.`` path the expression reads is unset, unless
    *strict*. Raises :class:`CelEvaluationError` on an empty, over-long,
    unparseable or failing expression, and on an unset path when *strict*.
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

    marks = _mark_unset(compiled.occurrences, ctx)
    activation = compiled.runner.new_activation()
    evaluator = _StateEvaluator(compiled.runner.ast, activation, marks)
    try:
        result = evaluator.evaluate({"state": compiled.context(ctx)})
    except _UnsetRead as exc:
        if strict:
            raise CelEvaluationError(
                f"CEL expression {expr!r} reads unset state path "
                f"{exc.path!r} and is marked strict.",
                expression=expr,
            ) from exc
        logger.warning(
            "CEL expression %r reads unset state path %r; %s",
            expr,
            exc.path,
            unset_outcome,
        )
        return _UNSET_STATE
    except CELEvalError as exc:
        text = _error_text(exc)
        logger.error("CEL evaluation failed for expr %r: %s", expr, text)
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {text}", expression=expr
        ) from exc
    except Exception as exc:
        logger.error("CEL evaluation failed for expr %r: %s", expr, exc)
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {exc}", expression=expr
        ) from exc
    return result


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
    result = _evaluate_in_state(expr, ctx, strict=strict, unset_outcome="condition is false")
    return False if result is _UNSET_STATE else bool(result)


def evaluate_cel_value(expr: str, ctx: dict[str, Any], *, strict: bool = False) -> Any:
    """Evaluate a CEL expression against *ctx* and return its result as JSON.

    The ``cel`` tool's evaluator: the same binding (``state``), the same
    absent-state rule and the same ``strict`` switch as :func:`evaluate_cel`,
    but the result is the expression's value converted to JSON
    (:func:`cel_to_json`) rather than a bool. An unguarded ``state.`` path
    that is unset makes the result ``None``, with a warning naming the path,
    unless *strict*.

    Raises :class:`CelEvaluationError` for every case :func:`evaluate_cel`
    raises on, and for a result JSON cannot carry.
    """
    result = _evaluate_in_state(expr, ctx, strict=strict, unset_outcome="value is null")
    if result is _UNSET_STATE:
        return None
    try:
        return cel_to_json(result)
    except ValueError as exc:
        raise CelEvaluationError(
            f"CEL expression {expr!r} has no JSON value: {exc}", expression=expr
        ) from exc


def evaluate_cel_expect(
    expr: str, *, value: Any, meta: Mapping[str, Any], state: Mapping[str, Any]
) -> bool:
    """Evaluate a tool/use ``expect:`` CEL expression and return a bool.

    Three root bindings, not one: ``value`` and ``meta`` are this effect's
    own result (``has(value.prompt_id)``), and ``state`` is the full run
    state exactly as :func:`evaluate_cel` binds it — unlike every other CEL
    site in the framework, which only ever binds ``state``. There is no
    absent-path-is-false convention here (that rule exists so a branch
    doesn't need a ``has()`` guard for every optional field; `expect` is a
    single check, not a branch): a path that doesn't resolve —
    ``value.prompt_id`` on a value with no such key — raises
    :class:`CelEvaluationError` the same as any other evaluation failure,
    which the caller (``core.tool``/``core.use``) treats the same as a
    false expectation.
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
    bindings = {
        "value": _to_cel(value),
        "meta": _to_cel(dict(meta)),
        "state": _to_cel(state),
    }
    activation = compiled.runner.new_activation()
    evaluator = _StateEvaluator(compiled.runner.ast, activation, _NO_MARKS)
    try:
        result = evaluator.evaluate(bindings)
    except CELEvalError as exc:
        text = _error_text(exc)
        raise CelEvaluationError(
            f"CEL evaluation failed for {expr!r}: {text}", expression=expr
        ) from exc
    except Exception as exc:
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


def free_identifiers(expr: str) -> tuple[str, ...]:
    """Every free identifier *expr* reads, in source order.

    A macro variable — the exactly-two-argument form of ``all``,
    ``exists``, ``exists_one``, ``map`` and ``filter`` — is bound only in
    the macro's own body (its last argument), not in its target or in any
    other argument. Nested macros accumulate: an inner variable shadows a
    same-named outer one. Nothing else binds a name — not cel-python's
    ``reduce``, not a three-argument ``map``, not a call to a method named
    ``existsOne`` (an ordinary, undeclared method, not a macro).

    A property name after ``.`` and a function or method name are never
    identifiers here; a message-construction type name (``Msg{f: 1}``) is,
    because it is read the same way any other bare name is.

    Raises :class:`CelValidationError` if *expr* does not parse.
    """
    out: list[str] = []
    _walk_free_identifiers(_compile(expr).runner.ast, frozenset(), out)
    return tuple(out)


#: The exactly-two-argument macros whose first argument names the variable
#: bound in their second (and only other) argument.
_BINDING_MACROS = frozenset({"all", "exists", "exists_one", "filter", "map"})


def _walk_free_identifiers(
    node: Any, bound: frozenset[str], out: list[str]
) -> None:
    if not isinstance(node, lark.Tree):
        return
    data = str(node.data)
    if data == "ident":
        name = str(node.children[0])
        if name not in bound:
            out.append(name)
        return
    if data == "member_dot_arg":
        member_tree = node.children[0]
        method_name = str(node.children[1])
        exprlist = node.children[2] if len(node.children) > 2 else None
        _walk_free_identifiers(member_tree, bound, out)
        args = list(exprlist.children) if exprlist is not None else []
        if method_name in _BINDING_MACROS and len(args) == 2:
            var_idents = list(args[0].find_data("ident"))
            if var_idents:
                bound = bound | {str(var_idents[0].children[0])}
            _walk_free_identifiers(args[1], bound, out)
            return
        for arg in args:
            _walk_free_identifiers(arg, bound, out)
        return
    for child in node.children:
        _walk_free_identifiers(child, bound, out)


# ---------------------------------------------------------------------------
# Compilation and caching
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class _Compiled:
    """A parsed expression plus the state paths its parse tree reads."""

    runner: celpy.Runner
    #: ``(dotted path, guarded by has())`` for every ``state.`` read.
    paths: tuple[tuple[str, bool], ...]
    #: ``(member_dot node, dotted path)`` for every unguarded ``state.``
    #: read occurrence — every occurrence counts, even one whose path also
    #: appears, elsewhere, as a ``has()`` argument.
    occurrences: tuple[tuple[Any, str], ...]
    #: True when the expression names ``state`` other than as the root of a
    #: dotted read (``size(state)``), so :func:`_project` cannot narrow it.
    reads_whole_state: bool

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


def _propagate_first_error(op: Any) -> Any:
    """Wrap a binary operator so a ``CELEvalError`` operand wins outright.

    Without this, ``err + 1`` or ``l[err]`` would reach the real operator,
    which raises a Python ``TypeError`` the tree-level handler turns into
    a generic "no such overload" error — discarding whichever error
    (``_UnsetRead`` or any other) was actually in play. The left operand
    is checked first, matching left-to-right evaluation order.
    """

    def wrapped(left: Any, right: Any) -> Any:
        if isinstance(left, CELEvalError):
            return left
        if isinstance(right, CELEvalError):
            return right
        return op(left, right)

    return wrapped


#: CEL's heterogeneous equality (cel-spec #103): comparing values of
#: different runtime types yields false rather than an error, while
#: int/uint/double are one numeric family and do compare. celpy raises a
#: no-such-overload error for both cases, so the two operators are
#: overridden here rather than left to surface as evaluation failures.
#:
#: ``_+_``/``_-_``/``_*_``/``_/_``/``_%_``/``_[_]`` are overridden so a
#: ``CELEvalError`` operand (an unset read, or any other) is the result
#: outright, rather than becoming celpy's own "no such overload" error.
_FUNCTIONS: dict[str, Any] = {
    "_==_": _spec_eq,
    "_!=_": _spec_ne,
    "_+_": _propagate_first_error(operator.add),
    "_-_": _propagate_first_error(operator.sub),
    "_*_": _propagate_first_error(operator.mul),
    "_/_": _propagate_first_error(operator.truediv),
    "_%_": _propagate_first_error(operator.mod),
    "_[_]": _propagate_first_error(operator.getitem),
}

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
        found = _collect_state_paths(tree)
        paths = tuple((path, guarded) for _node, path, guarded in found)
        compiled = _Compiled(
            runner=_ENV.program(tree, functions=_FUNCTIONS),
            paths=paths,
            occurrences=tuple(
                (node, path) for node, path, guarded in found if not guarded
            ),
            reads_whole_state=_count_state_idents(tree) != len(found),
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
) -> list[tuple[Any, str, bool]]:
    """Every ``state.`` read occurrence under *node*: node, dotted path, and
    whether it guards a ``has()``.

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
                [(node, ".".join(segments), guarded)]
                if segments[0] == "state"
                else []
            )

    if data == "ident_arg" and node.children and str(node.children[0]) == "has":
        return [
            occurrence
            for child in node.children[1:]
            for occurrence in _collect_state_paths(child, guarded=True)
        ]

    return [
        occurrence
        for child in node.children
        for occurrence in _collect_state_paths(child, guarded=guarded)
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


def cel_to_json(value: Any) -> Any:
    """Convert a CEL result into a JSON value.

    Lists and maps keep their order; a map key that is an ``int``, ``uint``
    or ``bool`` becomes its JSON text (``"1"``, ``"true"``), as ``json.dumps``
    would write it. A timestamp becomes an ISO 8601 UTC string
    (``2026-01-02T03:04:05Z``, with a fraction only when it has one) and a
    duration an ISO 8601 duration (``PT1H30M``, ``PT0.5S``, ``-PT1M30S``).

    Raises ``ValueError`` for bytes, a non-finite double, a type value, or
    anything else with no JSON form.
    """
    # celpy hands back its own types, and plain Python ones from some
    # operations (string concatenation gives a `str`); `bool` is checked
    # before `int`, which it subclasses in both.
    if value is None:
        return None
    if isinstance(value, (celtypes.BoolType, bool)):
        return bool(value)
    if isinstance(value, int):
        return int(value)
    if isinstance(value, float):
        number = float(value)
        if not math.isfinite(number):
            raise ValueError(f"{number} is not a finite number")
        return number
    if isinstance(value, str):
        return str(value)
    if isinstance(value, bytes):
        raise ValueError("bytes have no JSON form; convert them with string()")
    if isinstance(value, datetime.datetime):
        return _iso_timestamp(value)
    if isinstance(value, datetime.timedelta):
        return _iso_duration(value)
    if isinstance(value, Mapping):
        return {_json_key(key): cel_to_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [cel_to_json(item) for item in value]
    raise ValueError(f"a {type(value).__name__} has no JSON form")


def _json_key(key: Any) -> str:
    if isinstance(key, (celtypes.BoolType, bool)):
        return "true" if key else "false"
    if isinstance(key, int):
        return str(int(key))
    if isinstance(key, str):
        return str(key)
    raise ValueError(f"a {type(key).__name__} map key has no JSON form")


def _iso_timestamp(value: datetime.datetime) -> str:
    moment = (
        value.astimezone(datetime.timezone.utc)
        if value.tzinfo is not None
        else value
    )
    text = moment.strftime("%Y-%m-%dT%H:%M:%S")
    if moment.microsecond:
        text += f".{moment.microsecond:06d}".rstrip("0")
    return text + "Z"


def _iso_duration(value: datetime.timedelta) -> str:
    micros = (value.days * 86_400 + value.seconds) * 1_000_000 + value.microseconds
    sign = "-" if micros < 0 else ""
    micros = abs(micros)
    hours, micros = divmod(micros, 3_600 * 1_000_000)
    minutes, micros = divmod(micros, 60 * 1_000_000)
    seconds, fraction = divmod(micros, 1_000_000)
    parts = []
    if hours:
        parts.append(f"{hours}H")
    if minutes:
        parts.append(f"{minutes}M")
    if seconds or fraction or not parts:
        second_text = str(seconds)
        if fraction:
            second_text += f".{fraction:06d}".rstrip("0")
        parts.append(f"{second_text}S")
    return f"{sign}PT{''.join(parts)}"


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
