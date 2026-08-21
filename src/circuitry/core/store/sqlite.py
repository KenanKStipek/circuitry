from __future__ import annotations

import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ._identifiers import quote_sqlite_identifier, validate_table_name


@dataclass(frozen=True)
class SQLiteStatePersistence:
    db_path: str
    table: str = "circuitry_runs"

    @property
    def backend_name(self) -> str:
        return "sqlite"

    @property
    def _quoted_table(self) -> str:
        return quote_sqlite_identifier(self.table)

    @property
    def _quoted_index(self) -> str:
        return quote_sqlite_identifier(f"{self.table}_path_created_idx")

    @staticmethod
    def from_config(config: dict[str, Any]) -> SQLiteStatePersistence:
        # ``path`` is accepted as an alias for ``db_path`` so a profile's
        # ``persistence:`` block can use one spelling across backends.
        db_path = str(config.get("db_path") or config.get("path") or "").strip()
        if not db_path:
            raise ValueError(
                "Persistence backend 'sqlite' requires runtime.persistence.db_path"
            )

        table = str(config.get("table") or "circuitry_runs").strip()
        if not table:
            raise ValueError("runtime.persistence.table must be a non-empty string")
        validate_table_name(table)

        return SQLiteStatePersistence(db_path=db_path, table=table)

    def describe(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "table": self.table,
            "db_path": self.db_path,
        }

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        try:
            with self._connect() as conn:
                self._ensure_schema(conn)
                cur = conn.execute(
                    f"""
                    SELECT state_json
                    FROM {self._quoted_table}
                    WHERE orchestration_path = ?
                    ORDER BY created_at DESC, rowid DESC
                    LIMIT 1
                    """,
                    (orchestration_path,),
                )
                row = cur.fetchone()
        except Exception as e:
            raise RuntimeError(
                "SQLite state load failed for orchestration "
                f"{orchestration_path}: {e}"
            ) from e

        if not row:
            return None

        payload = row[0]
        if isinstance(payload, str):
            decoded = json.loads(payload)
            if isinstance(decoded, dict):
                return decoded
            raise RuntimeError("Persisted state_json is not a JSON object")
        raise RuntimeError(
            f"Persisted state_json has unsupported type: {type(payload).__name__}"
        )

    def save_run_snapshot(
        self,
        *,
        orchestration_path: str,
        run_id: str,
        ok: bool,
        error: str | None,
        state: dict[str, Any],
    ) -> None:
        try:
            with self._connect() as conn:
                self._ensure_schema(conn)
                conn.execute(
                    f"""
                    INSERT INTO {self._quoted_table}
                    (run_id, orchestration_path, ok, error, state_json)
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (
                        run_id,
                        orchestration_path,
                        1 if ok else 0,
                        error,
                        json.dumps(state),
                    ),
                )
                conn.commit()
        except Exception as e:
            raise RuntimeError(f"SQLite state save failed for run_id={run_id}: {e}") from e

    def _connect(self) -> sqlite3.Connection:
        db_file = Path(self.db_path)
        db_file.parent.mkdir(parents=True, exist_ok=True)
        return sqlite3.connect(self.db_path)

    def _ensure_schema(self, conn: sqlite3.Connection) -> None:
        conn.execute(
            f"""
            CREATE TABLE IF NOT EXISTS {self._quoted_table} (
                run_id TEXT PRIMARY KEY,
                orchestration_path TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                ok INTEGER NOT NULL,
                error TEXT,
                state_json TEXT NOT NULL
            )
            """
        )
        conn.execute(
            f"""
            CREATE INDEX IF NOT EXISTS {self._quoted_index}
            ON {self._quoted_table} (orchestration_path, created_at DESC)
            """
        )
