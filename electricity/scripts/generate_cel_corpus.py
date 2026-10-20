#!/usr/bin/env python3
"""Generate electricity-cel's golden corpus from Circuitry's real CEL evaluator.

Every case's expected outcome comes from actually calling
``circuitry.core.cel_eval.evaluate_cel``/``evaluate_cel_expect`` (`cel-python`
underneath) against a synthetic ``state``/``value``/``meta`` -- not a
hand-written guess -- so the corpus is the empirical record DESIGN.md
§7.2 and issue #379 ask for: what `cel-python` actually does for the type
mapping, heterogeneous equality, the naive/aware datetime split, and the
absent-path convention.

Every case's inputs and expressions are synthetic, written for the rules
in `electricity/DESIGN.md` §7.2 and `electricity/docs/spec/runtime-
semantics.md` §4 -- nothing is copied from a real orchestration.

Tagged value encoding: the same small scheme as
`generate_value_corpus.py` (bytes as hex, floats as raw bits, dicts as
``[key, value]`` pairs, dates/date-times as ISO text plus a separate
offset) -- defined again here rather than imported, so this generator
has no dependency on another lane's script.

Error cases record the exact exception text only when it is Circuitry's
own wording (`core/cel_eval.py`'s own messages for an empty expression, a
too-long expression, or an unresolved `strict: true` path) -- anything
sourced from `cel-python`/`lark` itself only has to fail at the same
place with a non-empty message (DESIGN.md §1, §12), so those cases carry
no expected text at all, just "this must raise".

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_cel_corpus.py [--check]
"""

from __future__ import annotations

import datetime
import json
import struct
import sys
from pathlib import Path
from typing import Any

from circuitry.core.cel_eval import (
    CelEvaluationError,
    evaluate_cel,
    evaluate_cel_expect,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-cel"
    / "tests"
    / "golden"
    / "corpus.json"
)

#: Distinguishes "not passed" (default to ``{}``) from an explicit
#: ``None`` for `expect()`'s ``value``/``meta``/``state`` -- a few cases
#: (null ordering) need the latter to actually reach `_to_cel` as CEL
#: `null`, not be silently defaulted away.
_UNSET = object()


# ---------------------------------------------------------------------
# Tagged encoding (see generate_value_corpus.py for the same scheme)
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
    if isinstance(value, (bytes, bytearray)):
        return {"t": "bytes", "v": bytes(value).hex()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    if isinstance(value, datetime.datetime):
        naive = value.replace(tzinfo=None)
        offset = value.utcoffset()
        return {
            "t": "datetime",
            "naive": naive.isoformat(timespec="microseconds"),
            "offset_seconds": None if offset is None else round(offset.total_seconds()),
        }
    if isinstance(value, datetime.date):
        return {"t": "date", "v": value.isoformat()}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


# ---------------------------------------------------------------------
# Case builders -- the expected outcome always comes from a real call
# ---------------------------------------------------------------------


#: How many extra Python call frames separate this generator's own,
#: shallow top-level call into `evaluate_cel` from the tightest of the
#: three real contexts measured for issue #403 (a top-level `if`, a
#: `while` loop condition, an `if` inside a `use` child) -- those three
#: were measured through the real `cof run` CLI (a subprocess per shape
#: per context, never a padded in-process call); this generator still
#: uses a padded direct call for speed, calibrated so it reproduces the
#: same accept/reject boundary the CLI measured for the shapes below.
#: `electricity-cel`'s `nesting.rs` module docs carry the full
#: CLI-measured numbers.
_DEEPEST_CONTEXT_PAD = 12


def _at_depth(pad: int, call: Any) -> Any:
    """Calls *call* after recursing *pad* frames deeper first.

    Reproduces, from this generator's own shallow top-level call stack,
    how much Python recursion budget `evaluate_cel` actually has left in
    a real run's deepest measured context -- see `_DEEPEST_CONTEXT_PAD`.
    """
    if pad <= 0:
        return call()
    return _at_depth(pad - 1, call)


def _outcome(call: Any, *, pad: int = 0) -> dict:
    try:
        result = _at_depth(pad, call)
    except CelEvaluationError:
        return {"kind": "error", "message": None}
    return {"kind": "ok", "result": result}


def _outcome_exact(call: Any) -> dict:
    try:
        result = call()
    except CelEvaluationError as exc:
        return {"kind": "error", "message": str(exc)}
    return {"kind": "ok", "result": result}


def condition(
    expr: str,
    state: dict,
    *,
    strict: bool = False,
    exact_error: bool = False,
    pad: int = 0,
) -> dict:
    def call() -> bool:
        return evaluate_cel(expr, state, strict=strict)

    outcome = _outcome_exact(call) if exact_error else _outcome(call, pad=pad)
    return {
        "mode": "condition",
        "expr": expr,
        "strict": strict,
        "state": encode(state),
        "outcome": outcome,
    }


def divergent_condition(
    expr: str,
    state: dict,
    *,
    electricity_result: bool,
    strict: bool = False,
) -> dict:
    """A documented, permanent divergence (issue #400): both engines
    evaluate *expr* without ever raising, but to different `bool`
    results -- the one case shape `condition`/`expect` above can't
    express, since each only pins what *one* side's own outcome is.
    *electricity_result* is this crate's own, hand-stated outcome,
    checked against the real Rust evaluator by `golden_corpus.rs`
    itself; the Python side is never hand-written -- `python_result`
    still comes from an actual call into `evaluate_cel`, which this
    function asserts succeeds (a case written with this helper is, by
    definition, one where Circuitry itself never raises). Also asserts
    *python_result* actually differs from *electricity_result* -- a
    case where both engines agree, written with this helper by mistake,
    would otherwise pass silently rather than failing loudly: that
    shape belongs to `condition()`, not this one.
    """
    python_result = evaluate_cel(expr, state, strict=strict)
    if python_result == electricity_result:
        raise ValueError(
            f"divergent_condition({expr!r}, ...): both engines agree "
            f"({python_result!r}) -- this isn't a divergence, use condition() instead"
        )
    return {
        "mode": "condition",
        "expr": expr,
        "strict": strict,
        "state": encode(state),
        "outcome": {
            "kind": "both",
            "python_result": python_result,
            "electricity_result": electricity_result,
        },
    }


def expect(
    expr: str,
    *,
    value: object = _UNSET,
    meta: object = _UNSET,
    state: object = _UNSET,
    exact_error: bool = False,
) -> dict:
    value = {} if value is _UNSET else value
    meta = {} if meta is _UNSET else meta
    state = {} if state is _UNSET else state
    make_outcome = _outcome_exact if exact_error else _outcome
    outcome = make_outcome(
        lambda: evaluate_cel_expect(expr, value=value, meta=meta, state=state)
    )
    return {
        "mode": "expect",
        "expr": expr,
        "state": encode(state),
        "value": encode(value),
        "meta": encode(meta),
        "outcome": outcome,
    }


# ---------------------------------------------------------------------
# The corpus itself
# ---------------------------------------------------------------------


def _nested_list(depth: int) -> Any:
    """*depth* singleton lists deep, bottoming out at a truthy leaf.

    Pairs with a `state[0][0]...` expression of the same *depth*: each
    `[0]` unwraps exactly one level.
    """
    value: Any = True
    for _ in range(depth):
        value = [value]
    return value


def _nested_dict(depth: int) -> Any:
    """*depth* levels of `{"a": ...}` deep, bottoming out at a truthy leaf.

    Pairs with a `state.a.a...` expression of the same *depth*: each
    `.a` descends exactly one level.
    """
    value: Any = True
    for _ in range(depth):
        value = {"a": value}
    return value


def build_corpus() -> list[dict]:
    cases: list[dict] = []

    # --- Basic correctness, nested paths, operators -------------------
    cases += [
        condition("state.input.ok == true", {"input": {"ok": True}}),
        condition("state.input.ok == true", {"input": {"ok": False}}),
        condition("state.prime.get_role.value == 'admin'", {"prime": {"get_role": {"value": "admin"}}}),
        condition("size(state.items) >= 1", {"items": [1, 2, 3]}),
        condition("size(state.items) >= 1", {"items": []}),
        condition("state.a == true && state.b == true", {"a": True, "b": True}),
        condition("state.a == true || state.b == true", {"a": False, "b": True}),
        condition("state.status != 'ok'", {"status": "error"}),
        condition("state.msg == 'done!'", {"msg": "done!"}),
        condition("true", {}),
        condition("false", {}),
        condition("1 == 1", {}),
        condition("state.a.b.c.d.e.f == 42", {"a": {"b": {"c": {"d": {"e": {"f": 42}}}}}}),
    ]

    # --- Fail-loud: own messages, exact text ---------------------------
    cases += [
        condition("", {}, exact_error=True),
        condition("   ", {}, exact_error=True),
        condition("state.x == 'a'" + " && state.x == 'a'" * 500, {"x": "a"}, exact_error=True),
        expect("", exact_error=True),
    ]

    # --- Fail-loud: third-party text, just "must raise" -----------------
    cases += [
        condition("state.a ==", {"a": 1}),
        condition("size(state.a) > nope(1)", {"a": [1]}),
        condition("nope(state.a)", {"a": 1}),
        condition("size(state.input.n) == 1", {"input": {"n": 3}}),
    ]

    # --- Absent state is false by rule, not an error --------------------
    cases += [
        condition("state.nonexistent.key == 'x'", {}),
        condition("state.prime.a.value.count == 2", {"prime": {"a": {"value": None}}}),
        condition("size(state.prime.a.value) > 0", {"prime": {"a": {"value": None}}}),
        condition("state.prime.typo.value == 1", {}),
        condition("state.a.n == 0", {"a": {"n": 0, "s": "", "l": [], "b": False}}),
        condition("state.a.s == ''", {"a": {"n": 0, "s": "", "l": [], "b": False}}),
        condition("size(state.a.l) == 0", {"a": {"n": 0, "s": "", "l": [], "b": False}}),
        condition("state.a.b == false", {"a": {"n": 0, "s": "", "l": [], "b": False}}),
    ]

    # --- has() --------------------------------------------------------
    cases += [
        condition("has(state.input.name)", {"input": {"name": "x"}}),
        condition("has(state.input.name)", {"input": {}}),
        condition("has(state.input.n) && state.input.n > 1", {"input": {"n": 5}}),
        condition("has(state.input.n) && state.input.n > 1", {"input": {}}),
        condition("!has(state.input.n) || state.input.n > 1", {"input": {}}),
        # A doubly-missing path: `input` itself is absent, not just `n`.
        condition("!has(state.input.n) || state.input.n > 1", {}),
        condition("has(state.input.n)", {}),
        # A patched clone of `state` to make a prior `has()` gracious would
        # leak into this second, longer `has()` read of the same prefix
        # (issue #379's review): cel-python's own `has()` is "the
        # argument evaluated without error", never conjured data.
        condition("!has(state.a.b) || has(state.a.b.c)", {}),
        condition("has(state.a.b) || has(state.a.b.c)", {}),
        # `has()` through a non-map value partway down the chain (a
        # disabled node writes `{"value": None}`): cel-python's `has()`
        # reports `False`, not an evaluation error.
        condition(
            "has(state.prime.x.value.price)",
            {"prime": {"x": {"value": None}}},
        ),
    ]

    # --- Negation / ternary -------------------------------------------
    cases += [
        condition("!state.prime.x.value", {"prime": {"x": {"value": False}}}),
        condition("!!state.prime.x.value", {"prime": {"x": {"value": True}}}),
        condition(
            "(state.input.ok ? state.input.a : state.input.b) == 1",
            {"input": {"ok": True, "a": 1, "b": 2}},
        ),
        condition(
            "(state.input.ok ? state.input.a : state.input.b) == 2",
            {"input": {"ok": False, "a": 1, "b": 2}},
        ),
    ]

    # --- Macros ---------------------------------------------------------
    macro_ctx = {"input": {"items": [1, 2, 3], "empty": []}}
    cases += [
        condition("state.input.items.all(i, i > 0)", macro_ctx),
        condition("state.input.items.all(i, i > 1)", macro_ctx),
        condition("state.input.items.exists(i, i == 3)", macro_ctx),
        condition("state.input.items.exists(i, i == 9)", macro_ctx),
        condition("state.input.items.exists_one(i, i > 2)", macro_ctx),
        condition("state.input.items.exists_one(i, i > 1)", macro_ctx),
        condition("size(state.input.items.map(i, i * 2)) == 3", macro_ctx),
        condition("size(state.input.items.filter(i, i > 1)) == 2", macro_ctx),
        condition("state.input.empty.all(i, i > 0)", macro_ctx),
        condition("state.input.rows.exists(r, r.n > 4)", {"input": {"rows": [{"n": 1}, {"n": 5}]}}),
        # cel-python's own `map` macro raises on the three-argument form
        # (`target.map(x, predicate, transform)`) that the underlying
        # `cel` crate's copied expander would otherwise accept
        # (electricity-cel's `macros` module docs, issue #379): a real
        # third-party failure (an undeclared-reference error once
        # evaluated, not a parse-time rejection), not Circuitry's own
        # wording, so no exact message.
        condition("state.l.map(x, x > 1, x * 10) == [20]", {"l": [1, 2]}),
    ]

    # --- Heterogeneous equality (cel-spec #103) -------------------------
    cases += [
        condition("1 == true", {}),
        condition("1 != true", {}),
        condition("state.input.n == state.input.flag", {"input": {"n": 1, "flag": True}}),
        condition("'1' == 1", {}),
        condition("1 == 1.0", {}),
        condition("1 == 1u", {}),
        condition("state.input.a == [1, 2]", {"input": {"a": [1, 2], "m": {"k": "v"}}}),
        condition("state.input.m == {'k': 'v'}", {"input": {"a": [1, 2], "m": {"k": "v"}}}),
    ]

    # --- Standard functions ----------------------------------------------
    std_ctx = {"input": {"s": "abc", "l": [1, 2], "m": {"a": 1, "b": 2, "c": 3}, "n": 7, "sn": "42"}}
    cases += [
        condition("size(state.input.s) == 3", std_ctx),
        condition("size(state.input.l) == 2", std_ctx),
        condition("size(state.input.m) == 3", std_ctx),
        condition("string(state.input.n) == '7'", std_ctx),
        condition("int(state.input.sn) == 42", std_ctx),
        condition("state.input.s.startsWith('a')", std_ctx),
        condition("'bc' in state.input.l.map(x, string(x))", std_ctx),
        condition(
            "state.input.greeting.startsWith('hello')",
            {"input": {"greeting": "hello world"}},
        ),
        condition(
            "state.input.greeting.contains('lo wo')",
            {"input": {"greeting": "hello world"}},
        ),
        condition(
            "state.input.greeting.matches('^h.*d$')",
            {"input": {"greeting": "hello world"}},
        ),
        condition("'a' in state.input.items", {"input": {"items": ["a", "b"]}}),
    ]

    # --- String literals are data, not rewritten paths -------------------
    cases += [
        condition("state.input.msg == 'state.x.y'", {"input": {"msg": "state.x.y"}}),
        condition(
            "state.input.msg == 'a && b || !c ? d : e'",
            {"input": {"msg": "a && b || !c ? d : e"}},
        ),
        condition("state.a == 'state.b.c'", {"a": "state.b.c", "b": {"c": "unrelated"}}),
        condition('state.a == "state.b.c"', {"a": "state.b.c"}),
        condition(r"state.a == 'it\'s state.b.c'", {"a": "it's state.b.c"}),
        condition(
            "state.a == 'state.b.c' && state.x == 1",
            {"a": "state.b.c", "x": 1},
        ),
    ]

    # --- strict: true ----------------------------------------------------
    cases += [
        condition("state.prime.tick.value.price <= 10", {}, strict=True, exact_error=True),
        condition(
            "state.prime.tick.value.price <= 10",
            {"prime": {"tick": {"value": None}}},
            strict=True,
            exact_error=True,
        ),
        condition(
            "state.prime.tick.value.price <= state.input.stop",
            {"prime": {"tick": {"value": {"price": 9}}}, "input": {"stop": 10}},
            strict=True,
        ),
        condition("state.prime.tick.value.price <= 10", {}, strict=False),
        # Circuitry's own strict message is built with Python's `repr()`
        # (`{expr!r}`/`{unresolved!r}`), not a bare `'...'` wrap: a `'` in
        # the expression switches to double quotes (issue #379's review).
        condition(
            "state.prime.tick.value == 'open'", {}, strict=True, exact_error=True
        ),
    ]

    # --- big ints: too large for CEL's 64-bit int, the expression reads it
    # (issue #379's review) -----------------------------------------------
    # Not included here: `evaluate_cel_expect` with a big int in
    # `value`/`meta`/`state` is a deliberate, documented deviation
    # (electricity-cel's lib.rs docs) rather than something this
    # differential corpus asserts sameness on — `evaluate_cel_expect`
    # converts before its own `try`/`except`, so the matching
    # `ValueError("overflow")` escapes Python *uncaught* there (neither a
    # `CelEvaluationError` nor a failed expectation), and `core.tool`'s
    # and `core.use`'s callers handle that raw `ValueError` differently
    # from each other (`lib.rs`'s crate docs trace both); electricity-cel
    # raises a `CelError` for this case instead of reproducing either.
    huge = 10**20
    cases += [
        condition("state.n == null", {"n": huge}),
        # Narrowed to what the expression reads (`_project`): a big int
        # elsewhere in `state`, unread, must not fail this condition.
        condition("state.input.ok == true", {"n": huge, "input": {"ok": True}}),
    ]

    # --- numeric ordering: celpy's `IntType` alone decorates `<`/`<=`/
    # `>`/`>=` with `@type_matched` (same concrete type only);
    # `UintType`/`DoubleType` never override ordering, so crossing
    # numeric types is fine when *they* are the left operand -- the
    # restriction is asymmetric, unlike `==`/`!=`'s spec-mandated
    # heterogeneous exemption (issue #379's review) ------------------------
    cases += [
        condition("state.n < 1.5", {"n": 1}),
        condition("state.n > 1.5", {"n": 1}),
        condition("1.5 > state.n", {"n": 1}),
        condition("1.5 < state.n", {"n": 1}),
        condition("state.n < 2", {"n": 1}),
        condition("state.n < 2u", {"n": 1}),
        condition("2 < 1u", {}),
        condition("1u < 2", {}),
    ]

    # --- bytes -------------------------------------------------------------
    cases += [
        condition("state.a == b'abc'", {"a": b"abc"}),
        condition("state.a == state.b", {"a": b"abc", "b": b"abd"}),
        condition("size(state.a) == 3", {"a": b"abc"}),
    ]

    # --- datetimes: naive vs aware never diverge inside CEL -----------------
    naive_10 = datetime.datetime(2020, 1, 1, 10, 0, 0)  # noqa: DTZ001
    aware_10_utc = datetime.datetime(2020, 1, 1, 10, 0, 0, tzinfo=datetime.timezone.utc)
    aware_15_plus5 = datetime.datetime(
        2020, 1, 1, 15, 0, 0, tzinfo=datetime.timezone(datetime.timedelta(hours=5))
    )
    aware_15_plus1 = datetime.datetime(
        2020, 1, 1, 15, 0, 0, tzinfo=datetime.timezone(datetime.timedelta(hours=1))
    )
    cases += [
        condition("state.a == state.b", {"a": naive_10, "b": aware_10_utc}),
        condition("state.a != state.b", {"a": naive_10, "b": aware_10_utc}),
        condition("state.a == state.b", {"a": naive_10, "b": aware_15_plus5}),
        condition("state.a < state.b", {"a": naive_10, "b": aware_15_plus5}),
        condition("state.a < state.b", {"a": naive_10, "b": aware_15_plus1}),
        condition("state.a > state.b", {"a": naive_10, "b": aware_15_plus1}),
        # Pinned under strict: true too -- ordering a naive against an
        # aware datetime never raises inside CEL, strict or not (DESIGN.md
        # §3.2).
        condition("state.a < state.b", {"a": naive_10, "b": aware_15_plus1}, strict=True),
        expect(
            "value < state.b",
            value=naive_10,
            state={"b": aware_15_plus1},
        ),
    ]

    # --- a Duration's truthiness is "nonzero", not always true: the top-
    # level expression result (not a CEL `bool(...)` call, which doesn't
    # accept a duration) is what `evaluate_cel`'s own `bool(result)` sees
    # (issue #379's review) -----------------------------------------------
    cases += [
        condition("state.a - state.b", {"a": aware_10_utc, "b": aware_10_utc}),
        condition("state.a - state.b", {"a": aware_15_plus1, "b": aware_10_utc}),
    ]

    # --- a bare date has no CEL counterpart: it is null ---------------------
    cases += [
        condition("state.d == null", {"d": datetime.date(2020, 1, 1)}),
    ]

    # --- evaluate_cel_expect: value/meta/state, and Quirk Q6 ------------------
    cases += [
        expect("has(value.prompt_id)", value={}, meta={}, state={}),
        expect("has(value.prompt_id)", value={"prompt_id": 1}, meta={}, state={}),
        expect("value.prompt_id == 1", value={}, meta={}, state={}),
        expect(
            "value.prompt_id == 1 && meta.ok && state.input.n == 2",
            value={"prompt_id": 1},
            meta={"ok": True},
            state={"input": {"n": 2}},
        ),
        expect(
            "value.prompt_id == 1 && meta.ok && state.input.n == 2",
            value={"prompt_id": 1},
            meta={"ok": False},
            state={"input": {"n": 2}},
        ),
        expect("meta.missing == 1", value={}, meta={}, state={}),
    ]

    # --- re-review of #379: `paths::project` must not corrupt
    # a scalar a longer, has()-guarded path extends past it. A previous
    # version inserted `{}` over an already-placed, non-dict value the
    # moment a longer path tried to continue through it, which made an
    # *unrelated, unguarded* read of the shorter path see the wrong thing.
    cases += [
        condition(
            "state.p.v == 'done' || has(state.p.v.price)",
            {"p": {"v": "done"}},
        ),
        condition(
            "has(state.p.v.price) ? state.p.v.price > 1 : state.p.v == 'skip'",
            {"p": {"v": "skip"}},
        ),
        condition(
            "has(state.t.primary) || size(state.t) > 0",
            {"t": [1]},
        ),
    ]

    # --- re-review of #379: has() beyond a plain dotted
    # chain. `cel`'s own, native `has()` only evaluates gracefully for
    # the *last* segment; everything before it is a plain selection that
    # raises the moment it hits an index, a missing key or the wrong
    # type. cel-python's own `has()` is "the whole argument evaluated
    # without error" (`evaluation.py`'s `ident_arg` `has`), covering an
    # index and a comprehension's own loop variable too.
    cases += [
        condition("has(state.l[0].x)", {"l": []}),
        condition("has(state.m[state.k].x)", {"m": {}, "k": "a"}),
        condition(
            "state.rows.all(r, !has(r.a.b) || r.a.b > 0)",
            {"rows": [{}]},
        ),
        # A loop variable named like one of the usual roots must shadow
        # it: a root-matched-by-name version of this rewrite always read
        # the *outer* `value` instead of the comprehension's own.
        expect(
            "value.items.all(value, has(value.x))",
            value={"items": [{"x": 1}]},
        ),
    ]

    # --- re-review of #379: the ordering matrix's
    # remaining gaps --------------------------------------------------
    cases += [
        # `bytes` ordering: a previous fix-pass fallback (`Value::partial_cmp`)
        # had no `Bytes` arm and raised here, a regression from plain `cel`.
        condition("state.a < state.b", {"a": b"a", "b": b"b"}),
        condition("state.a <= state.b", {"a": b"ab", "b": b"ab"}),
        # `bool` is a plain `int` subclass in celpy with no ordering
        # override, so it tolerates a numeric type on either side --
        # unlike `int` itself, which raises against anything but another
        # `int` (`state.n > state.flag` below).
        condition("state.flag < state.n", {"flag": True, "n": 2}),
        condition("state.f > state.flag", {"f": 1.5, "flag": True}),
        condition("state.n > state.flag", {"n": 2, "flag": True}),
        # A `NaN` resolves every comparison to `false` in celpy (plain
        # `float` behaviour), never raising the way plain `cel`'s own
        # comparer does.
        condition("state.n < 1.0", {"n": float("nan")}),
        # `null` never orders against anything, including itself --
        # exercised in `expect` mode so the absent-path convention (a
        # `None` leaf under `state.` is itself "absent") doesn't
        # short-circuit the comparison before it runs.
        expect("value <= value", value=None),
        expect("value < 1", value=None),
    ]

    # --- re-review of #379: equality/`in` inside a container.
    # celpy's own `ListType`/`MapType.__eq__` delegate to each element's
    # `__eq__`, which for `IntType`/`UintType`/`DoubleType` raises unless
    # the other side is the exact same type -- caught by the top-level
    # `_==_`/`_!=_` override and turned into "not equal" (never true for
    # two containers); `in` has no such fallback and raises outright.
    cases += [
        condition("state.a == [1, 2]", {"a": [1.0, 2.0]}),
        condition("state.a != [1, 2]", {"a": [1.0, 2.0]}),
        condition("state.m != {'a': 1.0}", {"m": {"a": 1}}),
        condition("state.m == {'a': 1.0}", {"m": {"a": 1}}),
        condition("1 in state.l", {"l": [1.0]}),
        condition("state.x in ['a', 'b']", {"x": 1}),
    ]

    # --- re-review of #379: exact message text, more
    # characters. `{expr!r}` (Python's `repr()`) switches outer quote and
    # escapes based on content; each of these exercises a character the
    # first fix pass's probe (a bare `'`) didn't.
    cases += [
        condition(
            "state.prime.tick.value == \"it's open\"",
            {},
            strict=True,
            exact_error=True,
        ),
        condition(
            "state.prime.tick.value == 'she said \"hi\"'",
            {},
            strict=True,
            exact_error=True,
        ),
        condition(
            "state.prime.tick.value == 'back\\\\slash'",
            {},
            strict=True,
            exact_error=True,
        ),
        condition(
            "state.prime.tick.value == '''line1\nline2'''",
            {},
            strict=True,
            exact_error=True,
        ),
        condition(
            "state.prime.tick.value == 'h\u00e9llo w\u00f6rld'",
            {},
            strict=True,
            exact_error=True,
        ),
    ]

    # --- issue #400: a dict key that converts to Python's `None` in
    # `_to_cel` (a bare `None` key, or a bare `date`, never a
    # `datetime.datetime`) collapses into a single `null` key holding
    # the last value written -- fixed, not just documented: electricity
    # routes every such key to one reserved sentinel `Key::String`
    # (`convert::NULL_KEY_SENTINEL`), so `size()` and `null in ...`
    # both match.
    cases += [
        condition(
            "size(state.m) == 1",
            {"m": {None: "n", datetime.date(2020, 1, 1): "d"}},
        ),
        condition(
            "null in state.m",
            {"m": {None: "n", datetime.date(2020, 1, 1): "d"}},
        ),
    ]

    # --- issue #400: a `map`/`exists`/`all`/`filter` comprehension's
    # own loop variable, iterating the collapsed `None`/bare-`date` key
    # slot, is bound to Circuitry's real `None` in Python but to
    # electricity's own internal sentinel string (not `null`) here --
    # `k == null` is `true` in Python, `false` here, a divergence the
    # `in`/`size()` fix above does not reach, since that is `cel`'s own
    # map-comprehension machinery iterating its keys, not this crate's
    # own code.
    cases += [
        divergent_condition(
            "state.m.exists(k, k == null)",
            {"m": {None: "n"}},
            electricity_result=False,
        ),
    ]

    # --- issue #400: a key that keeps its own, real CEL value in
    # Python instead of becoming `None` -- a `float` key is kept as its
    # own `DoubleType`, reachable through `in`/`size()`, never
    # collapsing with a `None` key the way the case above does -- is a
    # known, permanent divergence (`lib.rs`'s crate docs): dropped
    # here, because `cel::objects::Key` has no `float` variant at all,
    # unlike the `None`/bare-`date` case above, which needed no new
    # `Key` variant to fix. Pinned with [`divergent_condition`] (issue
    # #400's new corpus case kind) rather than left undocumented in the
    # corpus: both engines succeed, but electricity's own map is one
    # entry short of Circuitry's real one.
    cases += [
        divergent_condition(
            "size(state.m) == 1",
            {"m": {2.5: "f"}},
            electricity_result=False,
        ),
        divergent_condition(
            "2.5 in state.m",
            {"m": {2.5: "f"}},
            electricity_result=False,
        ),
        # A `datetime.datetime`/`bytes` key keeps its own, real CEL
        # value the same way a `float` key does above -- `cel::objects
        # ::Key` has no `Timestamp`/`Bytes` variant either, so the same
        # divergence applies.
        divergent_condition(
            "size(state.m) == 1",
            {"m": {datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc): "t"}},
            electricity_result=False,
        ),
        divergent_condition(
            "size(state.m) == 1",
            {"m": {b"k": "b"}},
            electricity_result=False,
        ),
    ]

    # --- third review of #379: `in` must decide `false` on a
    # type mismatch that doesn't actually raise in celpy (a plain Python
    # `==` resolving through the two-sided `NotImplemented` fallback),
    # not raise on every mismatch -- a regression the second fix pass
    # introduced trying to fix the case below.
    cases += [
        condition("'b' in state.tags", {"tags": ["a", None]}),
        # YAML 1.1 reads a bare `yes`/`no` as a `bool`.
        condition("state.input.answer in ['yes', 'no']", {"input": {"answer": True}}),
        expect("!(value.status in ['failed'])", value={"status": None}),
    ]

    # --- third review of #379: list indexing is celpy's own
    # `list.__getitem__` (Python's), not `cel`'s native one -- negative
    # wraps from the end, `bool` is `0`/`1`, a `double` always raises,
    # both inside `has()` and out.
    cases += [
        condition(
            "state.h[-1].s == 'done'",
            {"h": [{"s": "x"}, {"s": "done"}]},
        ),
        condition("has(state.l[-1].x)", {"l": [{"x": 1}]}),
        condition("has(state.l[0.0].x)", {"l": [{"x": 1}]}),
        condition("state.l[true] == 2", {"l": [1, 2]}),
    ]

    # --- third review of #379: `has()`'s own safe-navigation
    # rewrite previously only accepted an identifier or a literal as the
    # root of the chain it rewrites, leaving anything else (a macro or
    # function-call result) to `cel`'s own, non-graceful `has()`.
    cases += [
        condition(
            "has(state.results.filter(r, r.ok)[0].id)",
            {"results": [{"ok": False}]},
        ),
    ]

    # --- third review of #379: `has()`'s own index *key*
    # expression needs the same strict-operator/nested-`has()` rewriting
    # as everything else, not `cel`'s native evaluation of it -- a nested
    # `has()` over a two-level-missing chain inside the key is exactly
    # the kind of thing `cel`'s own, native `has()` raises on.
    cases += [
        condition(
            "has(state.m[has(state.sub.deep) ? 'a' : 'b'].x)",
            {"m": {"b": {"x": 1}}},
        ),
    ]

    # --- third review of #379: `paths::project` must not
    # corrupt a `NaN` leaf a longer, `has()`-guarded path extends past it
    # -- comparing the freshly read and already-placed values for
    # equality to decide whether to keep descending is itself `false` for
    # `NaN` (`NaN != NaN`), which silently re-broke the P0 the second fix
    # pass had already fixed for every other value.
    cases += [
        condition(
            "has(state.a.n.x) || state.a.n > 0.0",
            {"a": {"n": float("nan")}},
        ),
    ]

    # --- third review of #379: celpy's `BoolType` has no
    # `__eq__` override, so a `bool` on the *left* of an `int`/`uint`
    # resolves through the plain `int` it subclasses instead of raising
    # -- an asymmetry the reverse order, and `double` either way, don't
    # share.
    cases += [
        condition("true == 1", {}),
        condition("1 in [true]", {}),
        condition("true in [1]", {}),
    ]

    # --- third review of #379: comparing a `double` against
    # an `int`/`uint` must not round the integer operand to the nearest
    # representable `double` first -- Python's own `float`/`int`
    # comparison is exact.
    cases += [
        condition(
            "state.f < state.n",
            {"f": 9007199254740992.0, "n": 9007199254740993},
        ),
    ]

    # --- fourth review of #379: a bare `string`/`bytes`
    # operand indexes by character/byte, and `in` iterates it the same
    # way, matching celpy's own `str`/`bytes.__getitem__` and
    # `for c in container` -- `cel`'s own `_[_]`/`@in` have no indexer
    # for either at all.
    cases += [
        condition("state.code[0] == 'A'", {"code": "ABC"}),
        condition("state.s[-1] == 'c'", {"s": "abc"}),
        condition("state.grade in 'ABC'", {"grade": "A"}),
        condition("'lo' in state.s", {"s": "hello"}),
        condition("1 in state.s", {"s": "hello"}),
        condition("state.b[0] == 97", {"b": b"abc"}),
        condition("97 in state.b", {"b": b"abc"}),
    ]

    # --- fourth review of #379: `has(X)` is "X
    # evaluated without error", for *any* `X` -- an argument that was
    # never a field selection (`cel`'s own `has()` macro fails to even
    # parse this), and a root that is itself a macro/function-call result
    # which fails to evaluate (not just one that succeeds but indexes
    # into an empty result, already covered above).
    cases += [
        condition("has(state.items[0])", {"items": [1]}),
        condition("has(state.items[0])", {"items": []}),
        condition("has(state.results.filter(r, r.ok)[0].id)", {}),
    ]

    # --- fourth review of #379: a nested `list`/`map`
    # comparison inside `in` must compare every element before deciding,
    # not stop at the first one that raises -- celpy's own container
    # equality folds its elements with CEL's error-absorbing `&&`, where
    # a `false` element anywhere wins over a raising one anywhere else.
    cases += [
        condition("[1, 2] in [[1.0, 3]]", {}),
        condition("{'a': 1, 'b': 2} in [{'a': 1.0, 'b': 3}]", {}),
    ]

    # --- fourth review of #379: celpy's own `IntType`/
    # `UintType.__eq__` is `@type_matched` and raises on the other
    # integer type as a map key rather than finding the entry the way
    # `cel`'s own cross-converting map indexing does.
    cases += [
        condition("state.m[1u] == 'a'", {"m": {1: "a"}}),
    ]

    # --- issue #400: celpy's own asymmetry between a `bool` key and an
    # `int`/`uint` lookup, now reproduced -- `BoolType` has no
    # `__eq__` override, so a plain dict lookup with an `int`/`uint`
    # probe resolves through the plain `int` it subclasses and finds a
    # `bool`-keyed entry, while the reverse (a `bool` probe against an
    # `int`/`uint`-keyed entry) still raises either way.
    cases += [
        condition("state.m[1] == 'a'", {"m": {True: "a"}}),
        condition("state.m[1u] == 'a'", {"m": {True: "a"}}),
        condition("state.m[0] == 'a'", {"m": {False: "a"}}),
        # The reverse direction is not a divergence at all: a `bool`
        # probe still raises against an `int`/`uint`-keyed entry, since
        # `IntType`/`UintType.__eq__`'s own `@type_matched` has no
        # exemption for it either way.
        condition("state.m[true] == 'a'", {"m": {1: "a"}}),
    ]

    # --- fourth review of #379: shapes the third review's own list of
    # uncovered cases named but no corpus case exercised yet.
    cases += [
        # A `null` list element partway down a `has()` chain.
        condition("has(state.l[0].x)", {"l": [None]}),
        # An `int`/`bool` partway down, not just a `string`/`list`.
        condition("has(state.a.n.x)", {"a": {"n": 5}}),
        condition("has(state.a.n.x)", {"a": {"n": True}}),
        # A negative index still out of range after wrapping.
        condition("state.l[-3] == 1", {"l": [1, 2]}),
        # A `double` index outside `has()` too (not just guarded by one).
        condition("state.l[0.0] == 1", {"l": [1, 2]}),
        # A wrong-type map key, outside and inside `has()`.
        condition("state.m[1] == 'a'", {"m": {"a": "a"}}),
        condition("has(state.m[1].x)", {"m": {"a": {"x": 1}}}),
        # A present key whose value is itself `null`.
        condition("has(state.x.value)", {"x": {"value": None}}),
        # `meta` as a `has()` root in condition mode, where only `state`
        # is ever bound.
        condition("has(meta.x)", {}),
        # `meta`/`state` as the root of a `has()` in expect mode.
        expect("has(meta.x)", meta={}),
        expect("has(meta.x)", meta={"x": 1}),
        expect("has(state.input.n)", state={}),
        # `has()` nested inside the other five standard macros.
        condition("state.rows.exists(r, has(r.a.b))", {"rows": [{}, {"a": {"b": 1}}]}),
        condition("state.rows.exists_one(r, has(r.a))", {"rows": [{"a": 1}, {}]}),
        condition(
            "size(state.rows.filter(r, has(r.a.b))) == 1",
            {"rows": [{"a": {"b": 1}}, {"a": 1}]},
        ),
        condition("state.rows.map(r, has(r.a)) == [true, false]", {"rows": [{"a": 1}, {}]}),
        # Expect mode, `!has(value.error)` with a value that isn't a map
        # at all.
        expect("!has(value.error)", value="ok"),
        expect("!has(value.error)", value=None),
        expect("!has(value.error)", value=[1]),
        # A failing index *expression* (not just the lookup it performs)
        # inside `has()`.
        condition("has(state.m[state.k].x)", {"m": {"a": {"x": 1}}}),
    ]

    # --- issue #403: nesting boundaries per expression shape, matching
    # Circuitry's own electricity-side caps as closely as it is safe to.
    # electricity-cel's `nesting.rs` module docs carry the full
    # measurement, done through the real `cof run` CLI (a subprocess per
    # shape per context -- a top-level `if`, a `while` loop condition, an
    # `if` inside a `use` child -- never a padded in-process call). This
    # generator still calls `evaluate_cel` directly with a calibrated pad
    # (`_DEEPEST_CONTEXT_PAD`) for speed; every depth pinned below was
    # cross-checked to land on the same side of the boundary the CLI
    # measured, not assumed from the padding alone. Only depths clearly
    # inside the range every measured context accepted, and clearly
    # above every cap (electricity's own and Circuitry's measured floor
    # alike), are pinned -- never a depth inside a band where the two
    # engines, or different real contexts, disagree.
    cases += [
        # Nested parentheses / nested global calls (`int(int(...))`):
        # electricity's own `MAX_PAREN_CALL_NESTING_DEPTH` (53) matches
        # Circuitry's own floor for both shapes exactly, in every context
        # measured -- N-1/N accepted, N+1 rejected, by both engines.
        condition("(" * 52 + "1" + ")" * 52, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("(" * 53 + "1" + ")" * 53, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("(" * 54 + "1" + ")" * 54, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("int(" * 52 + "1" + ")" * 52, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("int(" * 53 + "1" + ")" * 53, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("int(" * 54 + "1" + ")" * 54, {}, pad=_DEEPEST_CONTEXT_PAD),
        # Nested list / map literals: `MAX_LIST_MAP_NESTING_DEPTH` (48)
        # matches Circuitry's own floor for both exactly too.
        condition("[" * 47 + "1" + "]" * 47, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("[" * 48 + "1" + "]" * 48, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("[" * 49 + "1" + "]" * 49, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("{'a':" * 47 + "1" + "}" * 47, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("{'a':" * 48 + "1" + "}" * 48, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("{'a':" * 49 + "1" + "}" * 49, {}, pad=_DEEPEST_CONTEXT_PAD),
        # Arithmetic (`+`)/relational (`<`) chains: `MAX_CHAIN_NESTING_DEPTH`
        # (241) now matches Circuitry's own floor for this family exactly
        # (index got its own, tighter cap below instead of
        # sharing this one, so this cap no longer needs to be stricter
        # than Circuitry just to stay safe for the index shape too). 238
        # is pinned as accepted by both -- comfortably inside the real
        # boundary (241), not at it, matching this generator's own
        # practice below of never pinning a case exactly at a boundary
        # the padding can't reproduce with certainty; 250 as rejected by
        # both, comfortably past Circuitry's own floor too.
        condition("0" + "+1" * 238, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("0" + "+1" * 250, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("0" + "<1" * 238, {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("0" + "<1" * 250, {}, pad=_DEEPEST_CONTEXT_PAD),
        # `&&`/`||` chains and a unary `!`/`-` run: Circuitry's own
        # evaluator recurses once per character/operator for these too,
        # despite `cel`'s own grammar never recursing for them at all
        # (`nesting.rs`'s module docs) -- `MAX_LOGICAL_UNARY_NESTING_DEPTH`
        # (484) matches Circuitry's own floor in every context measured.
        # 400, clearly inside 484, is pinned as accepted (never exactly
        # at the real boundary: this generator's own pad does not
        # reproduce Circuitry's real, CLI-measured 484/485 boundary
        # closely enough to pin a case right at it without risking a
        # false reading); 510/601 are rejected, well past the boundary.
        condition(" && ".join(["true"] * 401), {}, pad=_DEEPEST_CONTEXT_PAD),
        condition(" && ".join(["true"] * 510), {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("!" * 400 + "true", {}, pad=_DEEPEST_CONTEXT_PAD),
        condition("!" * 601 + "true", {}, pad=_DEEPEST_CONTEXT_PAD),
        # Member-access (`.`) chains: a plain select chain fails
        # Circuitry at *compile* time, not evaluation time, with a bare,
        # uncaught exception rather than a `CelEvaluationError`
        # (`nesting.rs`'s module docs) -- not a case this generator's
        # `evaluate_cel`-based helpers can safely build (a deep enough
        # one would crash the generator itself, the same way it crashes
        # a real `cof check`); see
        # `tests/conformance/cases/cel-member-access-too-deep` instead,
        # which exercises exactly that failure on both engines through a
        # real document. Only a shallow, well inside
        # `MAX_ACCESSOR_NESTING_DEPTH`, accepted case is pinned here,
        # with a real matching state so the expression actually
        # evaluates rather than hitting the absent-path rule; 20, not
        # the full depth, so the corpus fixture's own tagged JSON
        # encoding of a matching state value stays well under
        # `serde_json`'s own default recursion limit when parsed back on
        # the Rust side (this crate's own dependencies may never add
        # `serde_json`'s `unbounded_depth` feature to work around that).
        condition("state" + ".a" * 20, _nested_dict(20)),
        # Index (`[]`) chains: a *separate*, tighter cap from the
        # operator chains above (`MAX_INDEX_NESTING_DEPTH`, 238),
        # a clean, caught evaluation failure (not the
        # member-access shape's bare one) once the path it indexes into
        # resolves to real data -- confirmed on both engines through a
        # real document at the exact boundary (239 links) in
        # `tests/conformance/cases/cel-nesting-caps-fail-the-run`,
        # not duplicated here. Only a shallow, accepted case, well inside
        # the cap, for the same reasons as the member-access one above.
        condition("state" + "[0]" * 20, _nested_list(20)),
        # Nested function calls (`size(size(...))`): governed by the
        # same paren/call-nesting cap as parentheses/global calls above,
        # but fails for an unrelated reason well before that cap matters
        # at all -- `size()` returns an `int`, which has no `size()` of
        # its own, so Circuitry's own evaluator (and electricity-cel's
        # `cel` dependency, the same stdlib function) rejects any depth
        # beyond 1 on a type mismatch, not a recursion limit.
        condition("size(state)", [1, 2, 3]),
        condition("size(size(state))", [1, 2, 3]),
    ]

    # --- issue #400: the `==`/`!=` short-circuit for a container
    # against a scalar, `null`, or a container of the other kind, and
    # ordering raising for the same shapes -- celpy's own top-level
    # override never crosses a container with anything but its own
    # kind, and never orders one at all.
    cases += [
        condition("state.l == 1", {"l": [1]}),
        condition("1 == state.l", {"l": [1]}),
        condition("state.l == null", {"l": []}),
        condition("state.m != null", {"m": {}}),
        condition("state.l == state.m", {"l": [], "m": {}}),
        condition("[1.0] == 1", {}),
        condition("state.l < 1", {"l": [1]}),
        condition("state.m <= state.m", {"m": {}}),
        condition("[1] < [2]", {}),
    ]

    # --- issue #400: a representative subset of `in`'s remaining gaps
    # against a non-container, mixed-numeric and bool/int map keys, and
    # a `null` probe against a map that holds no None/bare-date key
    # slot for it to match (the sentinel case is pinned above).
    cases += [
        condition("1 in state.n", {"n": 1}),
        condition("1u in state.m", {"m": {1: "a"}}),
        condition("1 in state.m", {"m": {True: "a"}}),
        condition("true in state.m", {"m": {1: "a"}}),
        condition("null in state.l", {"l": [None]}),
        condition("null in state.m", {"m": {"a": 1}}),
        condition("null in state.m", {"m": {1: "a"}}),
        condition("[1] in state.l", {"l": [[1]]}),
    ]

    # --- issue #400: a representative subset of indexing's remaining
    # gaps -- out of range, a uint index (in range and absurdly out of
    # it), a wrong index type, a non-container operand, a missing map
    # key outside `has()`, a non-ASCII string index, bytes out of
    # range, and indexing a comprehension's own result.
    cases += [
        condition("state.l[5] == 1", {"l": [1]}),
        condition("state.l[18446744073709551615u] == 1", {"l": [1]}),
        condition("state.l[0u] == 1", {"l": [1]}),
        condition("state.l['a'] == 1", {"l": [1]}),
        condition("state.m[1.0] == 'a'", {"m": {"a": "a"}}),
        condition("state.n[0] == 1", {"n": 1}),
        condition("state.m['missing'] == 1", {"m": {"a": 1}}),
        condition("state.s[1] == '\u00e9'", {"s": "h\u00e9llo"}),
        condition("state.b[-4] == 97", {"b": b"abc"}),
        condition("state.l.map(x, x)[0] == 1", {"l": [1]}),
    ]

    # --- issue #400: ordering's remaining gaps -- string/string
    # (bytes/bytes is already pinned above), a computed duration
    # against another (celpy's own `DurationType` has no state-side
    # tagged encoding of its own, so both sides are built inside the
    # expression itself, the same way the existing duration-truthiness
    # cases already do), `-0.0`, and `inf` against a large int.
    inf_base = datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc)
    cases += [
        condition("state.a < state.b", {"a": "abc", "b": "abd"}),
        condition(
            "(state.b - state.a) < (state.c - state.b)",
            {
                "a": inf_base,
                "b": inf_base + datetime.timedelta(seconds=5),
                "c": inf_base + datetime.timedelta(seconds=25),
            },
        ),
        condition("state.f <= 0.0", {"f": -0.0}),
        condition("state.f > state.n", {"f": float("inf"), "n": 2**62}),
    ]

    return cases


def render(cases: list[dict]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def main() -> int:
    text = render(build_corpus())
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != text:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
