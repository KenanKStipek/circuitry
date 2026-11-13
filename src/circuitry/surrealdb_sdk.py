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
    """Run ``query()``'s builder, and unwrap it to one statement's result.

    SDK 3.x's ``query().execute()`` always returns one entry per SurrealQL
    statement in the query string; every ``query()`` call in this codebase
    sends exactly one statement, so this takes that entry -- the same single
    result SDK 2.x's ``query()`` already returns directly. An SDK 2.x result
    (never a builder) passes through unchanged.
    """
    execute = getattr(value, "execute", None)
    if callable(execute) and not isinstance(value, list | dict):
        statements = execute()
        return statements[0] if statements else None
    return value


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
