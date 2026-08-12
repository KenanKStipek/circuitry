"""Process list tool plugin via psutil.

Optional dep: ``psutil``. Install with ``pip install circuitry-cof[psutil]``.

Params:
  - ``filter`` (optional, str): substring match against process name
    (case-insensitive).
  - ``sort_by`` (optional, str): one of
    ``"pid" | "cpu" | "memory" | "name"``. Default ``"pid"``.
  - ``limit`` (optional, int): cap result count.

Returns ``value`` = list of dicts:
``{"pid", "name", "username", "status", "cpu_percent", "memory_percent"}``.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_VALID_SORT = ("pid", "cpu", "memory", "name")


@dataclass(frozen=True)
class ProcessListPlugin:
    name: str = "process_list"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import psutil  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "process_list: psutil not installed. "
                "Install with: pip install psutil"
            ) from exc

        name_filter = params.get("filter")
        if name_filter is not None and not isinstance(name_filter, str):
            raise ValueError("process_list: params['filter'] must be a string.")
        sort_by = str(params.get("sort_by") or "pid").lower()
        if sort_by not in _VALID_SORT:
            raise ValueError(
                f"process_list: sort_by must be {_VALID_SORT}, got {sort_by!r}"
            )
        limit = params.get("limit")

        rows: list[dict[str, Any]] = []
        attrs = ("pid", "name", "username", "status", "cpu_percent", "memory_percent")
        for proc in psutil.process_iter(attrs=attrs):
            try:
                info = proc.info
                if name_filter:
                    if name_filter.lower() not in (info.get("name") or "").lower():
                        continue
                rows.append({
                    "pid": int(info.get("pid") or 0),
                    "name": info.get("name") or "",
                    "username": info.get("username") or "",
                    "status": info.get("status") or "",
                    "cpu_percent": float(info.get("cpu_percent") or 0.0),
                    "memory_percent": float(info.get("memory_percent") or 0.0),
                })
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue

        sort_key_map = {
            "pid": lambda r: r["pid"],
            "cpu": lambda r: r["cpu_percent"],
            "memory": lambda r: r["memory_percent"],
            "name": lambda r: r["name"].lower(),
        }
        rows.sort(key=sort_key_map[sort_by], reverse=(sort_by in ("cpu", "memory")))
        if isinstance(limit, int) and limit > 0:
            rows = rows[:limit]

        return ToolResult(
            value=rows,
            raw={"filter": name_filter, "sort_by": sort_by, "count": len(rows)},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("psutil") is None:
            return CheckResult(
                ok=False,
                missing=["library:psutil"],
                message="pip install psutil",
            )
        return CheckResult(ok=True, missing=[])
