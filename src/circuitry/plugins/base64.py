"""Base64 encode / decode tool plugin via stdlib base64.

Params:
  - ``mode``: ``"encode" | "decode"``.
  - ``input`` (str).
  - ``encoding`` (default ``"utf-8"``): used to interpret encode input
    and to decode the bytes back to a string after decode.
  - ``urlsafe`` (bool, default False): use URL-safe alphabet
    (``-_`` instead of ``+/``) and strip ``=`` padding on encode /
    add it back on decode.
"""

from __future__ import annotations

import base64 as _base64
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class Base64Plugin:
    name: str = "base64"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode") or "encode").lower()
        text = params.get("input")
        if not isinstance(text, str):
            raise ValueError("base64 requires params['input'] as a string.")
        urlsafe = bool(params.get("urlsafe"))
        encoding = str(params.get("encoding") or "utf-8")

        if mode == "encode":
            data = text.encode(encoding)
            if urlsafe:
                value = _base64.urlsafe_b64encode(data).decode("ascii").rstrip("=")
            else:
                value = _base64.b64encode(data).decode("ascii")
        elif mode == "decode":
            try:
                if urlsafe:
                    # Re-pad: base64 expects len % 4 == 0.
                    pad = "=" * (-len(text) % 4)
                    raw = _base64.urlsafe_b64decode(text + pad)
                else:
                    raw = _base64.b64decode(text, validate=True)
            except Exception as exc:
                raise ValueError(f"base64: decode failed: {exc}") from exc
            value = raw.decode(encoding, errors="replace")
        else:
            raise ValueError(f"base64: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={"mode": mode, "urlsafe": urlsafe},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
