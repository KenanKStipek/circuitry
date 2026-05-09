"""Clock tool plugin — returns the current wall-clock time.

Params:
  - ``format`` (optional): strftime pattern. Default ``"%Y-%m-%dT%H:%M:%S%z"``.
  - ``timezone`` (optional): IANA tz name (``"UTC"``, ``"America/New_York"``).
    Default UTC.
  - ``epoch`` (optional, bool): when true, ``value`` is the integer epoch
    seconds instead of a formatted string.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone, tzinfo
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class ClockPlugin:
    name: str = "clock"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        epoch_mode = bool(params.get("epoch"))
        tz_name = params.get("timezone")
        tz: tzinfo
        if tz_name:
            try:
                from zoneinfo import ZoneInfo

                tz = ZoneInfo(str(tz_name))
            except Exception as exc:
                raise ValueError(f"clock: unknown timezone {tz_name!r}") from exc
        else:
            tz = timezone.utc

        now = datetime.now(tz)
        if epoch_mode:
            value: Any = int(now.timestamp())
        else:
            fmt = str(params.get("format", "%Y-%m-%dT%H:%M:%S%z"))
            value = now.strftime(fmt)

        return ToolResult(
            value=value,
            raw={
                "iso": now.isoformat(),
                "epoch": int(now.timestamp()),
                "timezone": str(tz),
            },
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
