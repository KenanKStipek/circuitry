"""JSONL append-log persistence runtime plugin.

Stdlib only — no external dependencies. Unlike the document/KV/object
storage plugins, jsonl-file appends one line per lifecycle event to a
local file rather than rewriting a full-state snapshot. The on-disk
record is an event stream, not the latest state, which makes it well
suited for after-the-fact analysis (grep, jq) and CI logs.

Per-event lines look like:
    {"event": "run_start",   "run_id": "...", "started_at": ..., ...}
    {"event": "effect_complete", "run_id": "...", "effect_path": ..., ...}
    {"event": "run_success", "run_id": "...", "ended_at": ...}
    {"event": "run_failure", "run_id": "...", "ended_at": ..., "error": ...}

Configuration:
  1. Env var ``CIRCUITRY_JSONL_PATH`` /
     ``runtime_plugins.jsonl-file.path`` (default ``./circuitry-runs.jsonl``).
  2. Env var ``CIRCUITRY_JSONL_INCLUDE_EFFECTS`` /
     ``runtime_plugins.jsonl-file.include_effects`` (bool, default True):
     when False, ``effect_complete`` events are omitted (only run-level
     events are recorded).
"""

from __future__ import annotations

import json
import logging
import os
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from ..preflight import CheckResult

logger = logging.getLogger(__name__)
_TRUTHY = ("1", "true", "yes", "y", "on")


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _coerce_bool(val: Any, default: bool) -> bool:
    if val is None:
        return default
    if isinstance(val, bool):
        return val
    if isinstance(val, str):
        return val.strip().lower() in _TRUTHY
    return bool(val)


class JsonlFilePlugin:
    name: str = "jsonl-file"

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._path: Path | None = None
        self._include_effects: bool = True

    def on_run_start(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            cfg = (
                (context.runtime_config or {})
                .get("runtime_plugins", {})
                .get("jsonl-file", {})
            )
            raw_path = (
                os.environ.get("CIRCUITRY_JSONL_PATH")
                or cfg.get("path")
                or "./circuitry-runs.jsonl"
            )
            self._path = Path(raw_path).expanduser()
            self._path.parent.mkdir(parents=True, exist_ok=True)
            self._include_effects = _coerce_bool(
                os.environ.get("CIRCUITRY_JSONL_INCLUDE_EFFECTS"),
                _coerce_bool(cfg.get("include_effects"), True),
            )
            self._append({
                "event": "run_start",
                "run_id": context.run_id,
                "orchestration_path": str(context.orchestration_path),
                "started_at": _now_iso(),
            })

    def on_effect_complete(
        self,
        *,
        state: dict[str, Any],
        context: Any,
        effect_path: str,
        effect_result: dict[str, Any],
    ) -> None:
        del state
        if not self._include_effects:
            return
        with self._lock:
            meta = effect_result.get("meta") if isinstance(effect_result, dict) else {}
            meta = meta if isinstance(meta, dict) else {}
            self._append({
                "event": "effect_complete",
                "run_id": context.run_id,
                "effect_path": effect_path,
                "value": effect_result.get("value") if isinstance(effect_result, dict) else None,
                "tokens_sent": meta.get("tokens_sent"),
                "tokens_received": meta.get("tokens_received"),
                "error": meta.get("error"),
                "completed_at": meta.get("completed_at") or _now_iso(),
            })

    def on_run_success(self, *, state: dict[str, Any], context: Any) -> None:
        del state
        with self._lock:
            self._append({
                "event": "run_success",
                "run_id": context.run_id,
                "ended_at": _now_iso(),
            })

    def on_run_failure(
        self, *, state: dict[str, Any], context: Any, error: str
    ) -> None:
        del state
        with self._lock:
            self._append({
                "event": "run_failure",
                "run_id": context.run_id,
                "ended_at": _now_iso(),
                "error": error,
            })

    def check(self) -> CheckResult:
        return CheckResult(
            ok=True,
            missing=[],
            message="jsonl-file uses stdlib only; the target path is "
                    "validated when the run starts.",
        )

    def _append(self, record: dict[str, Any]) -> None:
        if self._path is None:
            return
        line = json.dumps(record, default=str) + "\n"
        # Open / append per record so partial runs aren't lost on
        # crash and so concurrent writers (different runs sharing the
        # same file) don't lose lines.
        with open(self._path, "a", encoding="utf-8") as fh:
            fh.write(line)


def plugin() -> JsonlFilePlugin:
    return JsonlFilePlugin()
