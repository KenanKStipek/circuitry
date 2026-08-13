from __future__ import annotations

from typing import Any, Protocol

from .jsonl_file import JsonlFileStatePersistence
from .mongodb import MongodbStatePersistence
from .postgres import PostgresStatePersistence
from .sqlite import SQLiteStatePersistence

# Accepted spellings for each backend. The canonical name is the key.
_BACKEND_ALIASES = {
    "jsonl-file": {"jsonl-file", "jsonl_file", "jsonl", "file"},
    "mongodb": {"mongodb", "mongo"},
    "postgres": {"postgres", "postgresql"},
    "sqlite": {"sqlite", "sqlite3"},
}


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

    requested = str(persistence_cfg.get("backend") or "postgres").strip().lower()
    backend = next(
        (
            canonical
            for canonical, aliases in _BACKEND_ALIASES.items()
            if requested in aliases
        ),
        None,
    )

    if backend == "postgres":
        return PostgresStatePersistence.from_config(persistence_cfg)

    if backend == "sqlite":
        return SQLiteStatePersistence.from_config(persistence_cfg)

    if backend == "jsonl-file":
        return JsonlFileStatePersistence.from_config(persistence_cfg)

    if backend == "mongodb":
        return MongodbStatePersistence.from_config(persistence_cfg)

    raise ValueError(
        f"Unsupported persistence backend: {requested!r}. "
        "Supported backends: jsonl-file, mongodb, postgres, sqlite."
    )
