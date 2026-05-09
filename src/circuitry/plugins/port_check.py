"""TCP port-reachability tool plugin via stdlib socket.

Params:
  - ``host`` (required, str)
  - ``port`` (required, int)
  - ``timeout_ms`` (optional, default 2000)

Returns ``value`` = bool (open/closed), ``exit_code`` 0 when open and
1 when closed for shell-style routing.
"""

from __future__ import annotations

import socket
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class PortCheckPlugin:
    name: str = "port_check"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        host = params.get("host")
        port = params.get("port")
        if not isinstance(host, str) or not host:
            raise ValueError("port_check requires params['host'].")
        try:
            port_int = int(port)  # type: ignore[arg-type]
        except (TypeError, ValueError) as exc:
            raise ValueError("port_check requires params['port'] as int.") from exc
        if not (1 <= port_int <= 65535):
            raise ValueError("port_check: port out of range.")
        timeout_s = max(0.1, float(params.get("timeout_ms") or 2000) / 1000.0)

        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.settimeout(timeout_s)
        try:
            sock.connect((host, port_int))
            open_state = True
            error: str | None = None
        except (OSError, socket.timeout) as exc:
            open_state = False
            error = type(exc).__name__ + ": " + str(exc)
        finally:
            try:
                sock.close()
            except Exception:
                pass

        return ToolResult(
            value=open_state,
            raw={
                "host": host,
                "port": port_int,
                "timeout_seconds": timeout_s,
                "error": error,
            },
            stdout=None,
            stderr=None,
            exit_code=0 if open_state else 1,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
