"""B-prime schema for SQL persistence runtime plugins.

Two tables form the persistence model shared by all SQL stores
(sqlite, postgres, mysql, duckdb, mssql, cockroachdb, clickhouse):

  * ``runs`` — one row per orchestration run, indexed by ``run_id``.
    ``inputs`` holds the captured ``{{user_*}}`` keys as a single JSON
    blob; orchestrations are reconstructible from
    ``inputs + effect_results`` so no full-state column is needed.

  * ``effect_results`` — one row per effect result. ``state_path`` is
    the canonical dotted path; ``parent_path`` and ``iteration_index``
    let queries reconstruct dynamic / loop structure without parsing
    strings.

The ``raw`` column stores the largest data (full provider responses
or full tool raw output). It defaults ``true`` in dev and ``false``
in prod/test to control storage cost; per-plugin config overrides.

Per-dialect SQL adjustments (autoincrement keyword, JSON column type,
parameter placeholder) live on :class:`SqlDialect`. The DDL builders
below render the schema with the appropriate substitutions.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class SqlDialect:
    """SQL flavour parameters that vary across stores.

    Attributes:
        autoincrement: column definition fragment for the ``effect_results.id``
            primary key (e.g. ``"INTEGER PRIMARY KEY AUTOINCREMENT"`` for
            sqlite, ``"BIGSERIAL PRIMARY KEY"`` for postgres).
        json_type: column type for ``inputs`` / ``value`` / ``raw``
            (e.g. ``"TEXT"`` for sqlite, ``"JSONB"`` for postgres).
        timestamp_type: column type for ``started_at`` / ``ended_at``.
        placeholder: parameter binding marker (``"?"`` for sqlite/duckdb,
            ``"%s"`` for postgres).
    """

    autoincrement: str
    json_type: str
    timestamp_type: str
    placeholder: str


SQLITE = SqlDialect(
    autoincrement="INTEGER PRIMARY KEY AUTOINCREMENT",
    json_type="TEXT",
    timestamp_type="TEXT",
    placeholder="?",
)


POSTGRES = SqlDialect(
    autoincrement="BIGSERIAL PRIMARY KEY",
    json_type="JSONB",
    timestamp_type="TIMESTAMPTZ",
    placeholder="%s",
)


def runs_ddl(dialect: SqlDialect) -> str:
    return (
        "CREATE TABLE IF NOT EXISTS runs (\n"
        "  run_id TEXT PRIMARY KEY,\n"
        "  orchestration_path TEXT NOT NULL,\n"
        "  status TEXT NOT NULL,\n"
        f"  started_at {dialect.timestamp_type} NOT NULL,\n"
        f"  ended_at {dialect.timestamp_type},\n"
        "  error TEXT,\n"
        f"  inputs {dialect.json_type}\n"
        ")"
    )


def effect_results_ddl(dialect: SqlDialect) -> str:
    return (
        "CREATE TABLE IF NOT EXISTS effect_results (\n"
        f"  id {dialect.autoincrement},\n"
        "  run_id TEXT NOT NULL REFERENCES runs(run_id),\n"
        "  state_path TEXT NOT NULL,\n"
        "  effect_name TEXT NOT NULL,\n"
        "  effect_type TEXT NOT NULL,\n"
        "  parent_path TEXT,\n"
        "  iteration_index INTEGER,\n"
        f"  value {dialect.json_type},\n"
        f"  raw {dialect.json_type},\n"
        "  tokens_sent INTEGER,\n"
        "  tokens_received INTEGER,\n"
        f"  started_at {dialect.timestamp_type} NOT NULL,\n"
        f"  ended_at {dialect.timestamp_type} NOT NULL,\n"
        "  status TEXT NOT NULL,\n"
        "  error TEXT\n"
        ")"
    )


INDEX_DDL = (
    "CREATE INDEX IF NOT EXISTS idx_effect_results_run "
    "ON effect_results (run_id)",
    "CREATE INDEX IF NOT EXISTS idx_effect_results_path "
    "ON effect_results (state_path)",
)


def parse_effect_path(state_path: str) -> tuple[str, str | None, int | None, str]:
    """Decompose a canonical effect path into ``(effect_name, parent_path,
    iteration_index, effect_type_hint)``.

    Examples:
      ``"prime.greet"`` → ``("greet", "prime", None, "")``
      ``"prime.my_loop.iter_3.handle"`` → ``("handle", "prime.my_loop.iter_3", 3, "")``
      ``"prime"`` → ``("prime", None, None, "")``

    The ``effect_type_hint`` is always empty here — the type comes from
    the runtime plugin, not the path. Returned for future extension.
    """
    if not state_path:
        return ("", None, None, "")
    parts = state_path.split(".")
    name = parts[-1]
    parent = ".".join(parts[:-1]) if len(parts) > 1 else None
    iter_index: int | None = None
    # parent ends with iter_N when we're inside a loop body.
    if parent:
        last_segment = parent.split(".")[-1]
        if last_segment.startswith("iter_"):
            try:
                iter_index = int(last_segment[len("iter_") :])
            except ValueError:
                iter_index = None
    return (name, parent, iter_index, "")


def default_store_raw(environment: str) -> bool:
    """``true`` in dev, ``false`` elsewhere — per spec defaults."""
    return environment == "dev"
