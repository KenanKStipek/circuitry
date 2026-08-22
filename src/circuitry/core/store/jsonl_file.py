"""JSONL-file state persistence backend.

Stdlib only — appends one JSON object per line to a local file, and reads
the latest snapshot for an orchestration by scanning the log backwards.
Suited to local development, CI artifacts, and after-the-fact analysis
(``grep``/``jq``) where standing up a database is overkill.

This is the ``PersistenceBackend`` (load-latest + save-snapshot) sibling of
the write-only ``jsonl-file`` runtime plugin in
``circuitry.runtime_plugins.jsonl_file``; the two are independent and can be
enabled at the same time.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class JsonlFileStatePersistence:
    path: str

    @property
    def backend_name(self) -> str:
        return "jsonl-file"

    @staticmethod
    def from_config(config: dict[str, Any]) -> JsonlFileStatePersistence:
        raw_path = str(config.get("path") or config.get("db_path") or "").strip()
        if not raw_path:
            raise ValueError(
                "Persistence backend 'jsonl-file' requires "
                "runtime.persistence.path"
            )
        return JsonlFileStatePersistence(path=raw_path)

    def describe(self) -> dict[str, Any]:
        return {
            "backend": self.backend_name,
            "path": self.path,
        }

    def load_latest_state(self, *, orchestration_path: str) -> dict[str, Any] | None:
        log_file = self._resolve_path()
        if not log_file.exists():
            return None

        try:
            lines = log_file.read_text(encoding="utf-8").splitlines()
        except Exception as e:
            raise RuntimeError(
                "JSONL state load failed for orchestration "
                f"{orchestration_path}: {e}"
            ) from e

        # Scan backwards: the last matching record is the latest snapshot.
        for raw_line in reversed(lines):
            line = raw_line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                # An append log can be truncated mid-write or interleaved
                # with other writers; skip unreadable lines rather than
                # failing the whole run.
                logger.warning("Skipping malformed JSONL record in %s", log_file)
                continue
            if not isinstance(record, dict):
                continue
            if record.get("orchestration_path") != orchestration_path:
                continue
            state = record.get("state")
            if isinstance(state, dict):
                return state
            raise RuntimeError(
                "Persisted state is not a JSON object in "
                f"{log_file} for orchestration {orchestration_path}"
            )
        return None

    def save_run_snapshot(
        self,
        *,
        orchestration_path: str,
        run_id: str,
        ok: bool,
        error: str | None,
        state: dict[str, Any],
    ) -> None:
        record = {
            "run_id": run_id,
            "orchestration_path": orchestration_path,
            "created_at": datetime.now(timezone.utc).isoformat(),
            "ok": ok,
            "error": error,
            "state": state,
        }
        try:
            log_file = self._resolve_path()
            log_file.parent.mkdir(parents=True, exist_ok=True)
            with log_file.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(record) + "\n")
        except Exception as e:
            raise RuntimeError(
                f"JSONL state save failed for run_id={run_id}: {e}"
            ) from e

    def _resolve_path(self) -> Path:
        return Path(self.path).expanduser()
