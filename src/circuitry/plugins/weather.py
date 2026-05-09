"""Weather tool plugin via wttr.in (free, no key, ASCII-by-default).

Params:
  - ``location`` (optional, str): city, airport code, or coordinates
    (e.g. ``"Boston"``, ``"BOS"``, ``"@42.36,-71.06"``). Default ``""``
    uses wttr.in's IP-based geo-detection.
  - ``format`` (optional, str): wttr.in shorthand format string
    (e.g. ``"%C+%t"`` for "Cloudy +60°F"). When set, value is plain text.
    When omitted, value is wttr.in's default text/ASCII output.
  - ``json`` (optional, bool, default False): when true, request the
    full JSON forecast (``?format=j1``) and return parsed JSON.

Mutually exclusive: ``format`` and ``json``.
"""

from __future__ import annotations

import json as _json
import shlex
import shutil
import subprocess
import urllib.parse
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class WeatherPlugin:
    name: str = "weather"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        location = str(params.get("location") or "").strip()
        format_str = params.get("format")
        as_json = bool(params.get("json"))
        if format_str and as_json:
            raise ValueError(
                "weather: pass either params['format'] or params['json'], not both."
            )

        if shutil.which("curl") is None:
            raise RuntimeError("weather: curl not on PATH.")

        # wttr.in expects the location as the path segment.
        path = urllib.parse.quote(location, safe="@,.+-")
        query: dict[str, str] = {}
        if as_json:
            query["format"] = "j1"
        elif format_str:
            query["format"] = str(format_str)

        url = f"https://wttr.in/{path}"
        if query:
            url += "?" + urllib.parse.urlencode(query)

        cmd = [
            "curl", "--silent", "--show-error", "--fail-with-body",
            "--max-time", str(int(timeout_seconds)),
            "-H", "Accept-Language: en",
            url,
        ]
        proc = subprocess.run(
            cmd, capture_output=True, text=True, check=False
        )
        if proc.returncode != 0:
            err = (proc.stderr or proc.stdout or "").strip()
            raise RuntimeError(
                f"weather request failed (curl exit {proc.returncode}): {err} "
                f"cmd={' '.join(shlex.quote(c) for c in cmd)}"
            )

        body = proc.stdout
        if as_json:
            try:
                value: Any = _json.loads(body)
            except _json.JSONDecodeError as exc:
                raise RuntimeError(
                    f"weather: expected JSON but parse failed: {exc}"
                ) from exc
        else:
            value = body.strip()

        return ToolResult(
            value=value,
            raw={"url": url, "body": body},
            stdout=None, stderr=None, exit_code=proc.returncode,
        )

    def check(self) -> CheckResult:
        if shutil.which("curl") is None:
            return CheckResult(ok=False, missing=["binary:curl"])
        return CheckResult(ok=True, missing=[])
