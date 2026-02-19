from __future__ import annotations

from typing import Any, Protocol

from .postgres import PostgresStatePersistence
from .sqlite import SQLiteStatePersistence


class PersistenceBackend(Protocol):
    @property
    def backend_name(self) -> str: ...

    def describe(self) -> dict[str, Any]: ...

    def load_latest_state(
        self, *, orchestration_path: str
    ) -> dict[str, Any] | None: ...

    def save_run_snapshot(
        self,
        *,
        orchestration_path: str,
        run_id: str,
        ok: bool,
        error: str | None,
        state: dict[str, Any],
    ) -> None: ...


def build_persistence_backend(runtime: dict[str, Any]) -> PersistenceBackend | None:
    persistence_cfg = (runtime or {}).get("persistence")
    if not isinstance(persistence_cfg, dict):
        return None
    if not bool(persistence_cfg.get("enabled", False)):
        return None

    backend = str(persistence_cfg.get("backend") or "postgres").strip().lower()
    if backend == "postgres":
        return PostgresStatePersistence.from_config(persistence_cfg)

    if backend == "sqlite":
        return SQLiteStatePersistence.from_config(persistence_cfg)

    if backend not in {"postgres", "sqlite"}:
        raise ValueError(
            f"Unsupported persistence backend: {backend!r}. "
            "Supported backends: postgres, sqlite."
        )
    return None
