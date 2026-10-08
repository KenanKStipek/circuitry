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


def _outcome(call: Any) -> dict:
    try:
        result = call()
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
) -> dict:
    make_outcome = _outcome_exact if exact_error else _outcome
    outcome = make_outcome(lambda: evaluate_cel(expr, state, strict=strict))
    return {
        "mode": "condition",
        "expr": expr,
        "strict": strict,
        "state": encode(state),
        "outcome": outcome,
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
        # (issue #379 review finding 1): cel-python's own `has()` is "the
        # argument evaluated without error", never conjured data.
        condition("!has(state.a.b) || has(state.a.b.c)", {}),
        condition("has(state.a.b) || has(state.a.b.c)", {}),
        # `has()` through a non-map value partway down the chain (a
        # disabled node writes `{"value": None}`): cel-python's `has()`
        # reports `False`, not an evaluation error (finding 1).
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
        # the expression switches to double quotes (issue #379 review
        # finding 2).
        condition(
            "state.prime.tick.value == 'open'", {}, strict=True, exact_error=True
        ),
    ]

    # --- big ints: too large for CEL's 64-bit int, the expression reads it
    # (issue #379 review finding 3) -------------------------------------
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
    # heterogeneous exemption (issue #379 review finding 4) --------------
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
    # (issue #379 review finding 7) --------------------------------------
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

    # --- re-review of #379, finding 1: `paths::project` must not corrupt
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

    # --- re-review of #379, finding 3 (+4): has() beyond a plain dotted
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

    # --- re-review of #379, finding 2 (+8): the ordering matrix's
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

    # --- re-review of #379, finding 5: equality/`in` inside a container.
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

    # --- re-review of #379, finding 2/9: exact message text, more
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

    # --- re-review of #379, finding 6 (declined, documented in
    # electricity-cel's `lib.rs` crate docs instead of fixed): a
    # `Value::Dict` key with no CEL `Key` counterpart collapses, in
    # Python, into a single `null` key holding the last value written --
    # `cel::objects::Key` cannot represent `null` as a map key at all, so
    # electricity-cel drops every such entry instead of colliding them
    # into one. Deliberately NOT a corpus case: pinning it here would
    # assert parity on a documented, permanent divergence rather than a
    # bug this crate can fix.

    # --- third review of #379, finding 1: `in` must decide `false` on a
    # type mismatch that doesn't actually raise in celpy (a plain Python
    # `==` resolving through the two-sided `NotImplemented` fallback),
    # not raise on every mismatch -- a regression the second fix pass
    # introduced trying to fix finding 5 below.
    cases += [
        condition("'b' in state.tags", {"tags": ["a", None]}),
        # YAML 1.1 reads a bare `yes`/`no` as a `bool`.
        condition("state.input.answer in ['yes', 'no']", {"input": {"answer": True}}),
        expect("!(value.status in ['failed'])", value={"status": None}),
    ]

    # --- third review of #379, finding 3: list indexing is celpy's own
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

    # --- third review of #379, finding 2: `has()`'s own safe-navigation
    # rewrite previously only accepted an identifier or a literal as the
    # root of the chain it rewrites, leaving anything else (a macro or
    # function-call result) to `cel`'s own, non-graceful `has()`.
    cases += [
        condition(
            "has(state.results.filter(r, r.ok)[0].id)",
            {"results": [{"ok": False}]},
        ),
    ]

    # --- third review of #379, finding 6: `has()`'s own index *key*
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

    # --- third review of #379, finding 7: `paths::project` must not
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

    # --- third review of #379, finding 5: celpy's `BoolType` has no
    # `__eq__` override, so a `bool` on the *left* of an `int`/`uint`
    # resolves through the plain `int` it subclasses instead of raising
    # -- an asymmetry the reverse order, and `double` either way, don't
    # share.
    cases += [
        condition("true == 1", {}),
        condition("1 in [true]", {}),
        condition("true in [1]", {}),
    ]

    # --- third review of #379, finding 8: comparing a `double` against
    # an `int`/`uint` must not round the integer operand to the nearest
    # representable `double` first -- Python's own `float`/`int`
    # comparison is exact.
    cases += [
        condition(
            "state.f < state.n",
            {"f": 9007199254740992.0, "n": 9007199254740993},
        ),
    ]

    # --- fourth review of #379, finding 1: a bare `string`/`bytes`
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

    # --- fourth review of #379, findings 2 and 3: `has(X)` is "X
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

    # --- fourth review of #379, finding 6: a nested `list`/`map`
    # comparison inside `in` must compare every element before deciding,
    # not stop at the first one that raises -- celpy's own container
    # equality folds its elements with CEL's error-absorbing `&&`, where
    # a `false` element anywhere wins over a raising one anywhere else.
    cases += [
        condition("[1, 2] in [[1.0, 3]]", {}),
        condition("{'a': 1, 'b': 2} in [{'a': 1.0, 'b': 3}]", {}),
    ]

    # --- fourth review of #379, finding 1/5: celpy's own `IntType`/
    # `UintType.__eq__` is `@type_matched` and raises on the other
    # integer type as a map key rather than finding the entry the way
    # `cel`'s own cross-converting map indexing does.
    cases += [
        condition("state.m[1u] == 'a'", {"m": {1: "a"}}),
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
