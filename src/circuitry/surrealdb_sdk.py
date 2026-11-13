"""Compatibility shim between the ``surrealdb`` SDK's 2.x and 3.x APIs.

Shared by ``circuitry.plugins.surrealdb`` and
``circuitry.runtime_plugins.surrealdb``.

SDK 2.x's ``query()`` / ``select()`` / ``create()`` / ``upsert()`` /
``delete()`` run immediately and hand back the result. SDK 3.x's
``query()``, ``select()``, ``delete()``, and the no-data forms of
``create()`` / ``upsert()``, hand back a builder instead; nothing is
sent until its ``.execute()`` is called. SDK 3.x also reads a bare
string passed as a record target as a table name, never a specific
record, unless it is wrapped in ``StringRecordId`` — SDK 2.x parses a
``"table:id"`` string into a record id on its own.

SDK 3.x also unwraps a single-record result further than SDK 2.x does:
a ``select()`` of one record id returns the bare record (``None`` if
missing) rather than a one-element list, and a ``create()``/``upsert()``/
``delete()`` that touches no rows returns ``None`` rather than ``[]``.
``run_select`` and ``run_write`` normalise both back to SDK 2.x's shape.

``query_raw()`` is the one method that behaves identically on both majors:
it always runs eagerly (never a builder) and always hands back
``{"result": [{"status": ..., "result": ...}, ...], ...}``, one entry per
SurrealQL statement, without raising for a failed statement itself.
``run_query_raw`` is built on that shared shape, so the ``query`` mode
(arbitrary, possibly multi-statement SurrealQL) is driven through it
instead of through ``query()``.
"""

from __future__ import annotations

from typing import Any


def run(value: Any) -> Any:
    """Run *value* if it is an SDK 3.x builder; pass an SDK 2.x result through.

    A response is never a bare list/dict with an ``execute`` attribute of
    its own, so checking for a callable ``.execute()`` is enough to tell a
    builder from an already-fetched result.
    """
    execute = getattr(value, "execute", None)
    if callable(execute) and not isinstance(value, list | dict):
        return execute()
    return value


def run_query(value: Any) -> Any:
    """Run ``query()``'s builder, and unwrap it to the first statement's result.

    Used only where a single, trusted, fixed SurrealQL statement is run
    (schema bootstrap and row writes) and a later statement's error could
    never apply. User-supplied SurrealQL (the ``query`` tool mode) goes
    through ``run_query_raw`` instead, which checks every statement.

    SDK 3.x's ``query().execute()`` returns one entry per SurrealQL statement
    in the query string, and this takes the first -- the same single result
    SDK 2.x's ``query()`` already returns directly. An SDK 2.x result (never
    a builder) passes through unchanged.
    """
    execute = getattr(value, "execute", None)
    if callable(execute) and not isinstance(value, list | dict):
        statements = execute()
        return statements[0] if statements else None
    return value


def run_select(value: Any) -> Any:
    """Run ``select()``'s builder/result, normalised to SDK 2.x's shape.

    SDK 2.x's ``select()`` never unwraps: the result is always a list, even
    for a single record id. SDK 3.x's ``select()`` unwraps a single-record
    target to the bare record, or to ``None`` if it doesn't exist. This
    undoes that so callers never have to branch on which SDK is installed.
    """
    result = run(value)
    if result is None:
        return []
    if isinstance(result, dict):
        return [result]
    return result


def run_write(value: Any) -> Any:
    """Run ``create()``/``upsert()``/``delete()``'s builder/result, normalised to SDK 2.x's shape.

    Both SDKs unwrap a single-record write to the bare record. They differ
    only when no row was written or deleted (e.g. a permissions clause
    silently dropped the write): SDK 2.x gives ``[]``, SDK 3.x gives
    ``None``.
    """
    result = run(value)
    return [] if result is None else result


def run_query_raw(response: Any, *, mode: str = "query") -> Any:
    """Check every statement in a ``query_raw()`` response, raising on the
    first one that failed; return the first statement's own result otherwise.

    ``query_raw()`` exists on both SDK majors with the identical shape
    above, so this is the one place a multi-statement SurrealQL string gets
    the same answer regardless of which major is installed: a failure in
    *any* statement raises, in statement order, with the same text either
    way -- SDK 2.x's own ``query()`` only ever checks the first statement
    and silently drops a later one's error, and SDK 3.x's own
    ``query().execute()`` raises its own (differently worded) exception for
    the same case, so neither SDK's own method is used here.
    """
    statements = response.get("result") if isinstance(response, dict) else None
    if not isinstance(statements, list):
        raise RuntimeError(f"surrealdb {mode}: unexpected response shape: {response!r}")
    for statement in statements:
        if isinstance(statement, dict) and str(statement.get("status", "")).upper() == "ERR":
            detail = statement.get("result") or statement.get("detail") or statement
            raise RuntimeError(f"surrealdb {mode}: SurrealQL error: {detail}")
    if not statements:
        return None
    first = statements[0]
    return first.get("result") if isinstance(first, dict) else first


def record_target(value: str) -> Any:
    """A ``select`` / ``create`` / ``upsert`` / ``delete`` target string.

    A string containing ``:`` (and not a ``..`` range) names one record.
    SDK 2.x reads that out of a plain string by itself; SDK 3.x requires it
    wrapped in ``StringRecordId`` — which only exists there — or it treats
    the whole string, colon included, as a table name.
    """
    if ":" not in value or ".." in value:
        return value
    try:
        from surrealdb import StringRecordId  # type: ignore[import-not-found]
    except ImportError:
        return value
    return StringRecordId(value)
