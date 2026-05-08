from __future__ import annotations

import re

_TABLE_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$")


def validate_table_name(name: str) -> str:
    """Validate and return a table name, or raise ValueError."""
    if not _TABLE_RE.match(name):
        raise ValueError(
            "runtime.persistence.table must match [A-Za-z_][A-Za-z0-9_]{0,62}"
        )
    return name


def quote_sqlite_identifier(name: str) -> str:
    """Double-quote a SQLite identifier, escaping internal double-quotes."""
    return '"' + name.replace('"', '""') + '"'
