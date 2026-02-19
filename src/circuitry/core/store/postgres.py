from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class PostgresStatePersistence:
    dsn: str
    table: str = "circuitry_runs"
    sslmode: str = "require"

    @property
    def backend_name(self) -> str:
        return "postgres"

    @staticmethod
    def from_config(config: dict[str, Any]) -> "PostgresStatePersistence":
        dsn = str(config.get("dsn") or "").strip()
        if not dsn:
            raise ValueError(
                "Persistence backend 'postgres' requires runtime.persistence.dsn"
            )

        table = str(config.get("table") or "circuitry_runs").strip()
        if not table:
            raise ValueError("runtime.persistence.table must be a non-empty string")

        sslmode = str(config.get("sslmode") or "require").strip().lower()
        allow_insecure = bool(config.get("allow_insecure", False))
        if sslmode in {"disable", "allow", "prefer"} and not allow_insecure:
            raise ValueError(
                "Insecure postgres sslmode requested. Set runtime.persistence.sslmode "
                "to 'require'/'verify-ca'/'verify-full', or explicitly set "
                "runtime.persistence.allow_insecure=true for local development."
            )

        return PostgresStatePersistence(dsn=dsn, table=table, sslmode=sslmode)

    def describe(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "table": self.table,
            "sslmode": self.sslmode,
        }

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        try:
            with self._connect() as conn:
                self._ensure_schema(conn)
                with conn.cursor() as cur:
                    cur.execute(
                        f"""
                        SELECT state_json
                        FROM {self.table}
                        WHERE orchestration_path = %s
                        ORDER BY created_at DESC
                        LIMIT 1
                        """,
                        (orchestration_path,),
                    )
                    row = cur.fetchone()
        except Exception as e:
            raise RuntimeError(
                f"Postgres state load failed for orchestration {orchestration_path}: {e}"
            ) from e

        if not row:
            return None

        payload = row[0]
        if isinstance(payload, dict):
            return payload
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
                with conn.cursor() as cur:
                    cur.execute(
                        f"""
                        INSERT INTO {self.table}
                        (run_id, orchestration_path, ok, error, state_json)
                        VALUES (%s, %s, %s, %s, %s::jsonb)
                        """,
                        (
                            run_id,
                            orchestration_path,
                            ok,
                            error,
                            json.dumps(state),
                        ),
                    )
        except Exception as e:
            raise RuntimeError(
                f"Postgres state save failed for run_id={run_id}: {e}"
            ) from e

    def _connect(self) -> Any:
        try:
            import psycopg  # type: ignore[import-not-found]
        except ImportError as e:
            raise RuntimeError(
                "psycopg is required for postgres persistence. Install with: "
                "pip install psycopg[binary]"
            ) from e

        conninfo = self.dsn
        if "sslmode=" not in conninfo:
            sep = "&" if "?" in conninfo else "?"
            conninfo = f"{conninfo}{sep}sslmode={self.sslmode}"

        return psycopg.connect(conninfo, autocommit=True)

    def _ensure_schema(self, conn: Any) -> None:
        with conn.cursor() as cur:
            cur.execute(
                f"""
                CREATE TABLE IF NOT EXISTS {self.table} (
                    run_id TEXT PRIMARY KEY,
                    orchestration_path TEXT NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                    ok BOOLEAN NOT NULL,
                    error TEXT,
                    state_json JSONB NOT NULL
                )
                """
            )
            cur.execute(
                f"""
                CREATE INDEX IF NOT EXISTS {self.table}_path_created_idx
                ON {self.table} (orchestration_path, created_at DESC)
                """
            )
