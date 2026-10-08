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
    value: object = None,
    meta: object = None,
    state: object = None,
    exact_error: bool = False,
) -> dict:
    value = {} if value is None else value
    meta = {} if meta is None else meta
    state = {} if state is None else state
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
    # `ValueError("overflow")` escapes *uncaught*, which `core.expect`'s
    # caller doesn't handle either; electricity-cel raises a `CelError`
    # there instead of reproducing that crash.
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
