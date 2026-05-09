"""Hexadecimal encode / decode tool plugin via stdlib bytes.hex / bytes.fromhex.

Params:
  - ``mode``: ``"encode" | "decode"``.
  - ``input`` (str).
  - ``encoding`` (default ``"utf-8"``): used on the string side of the
    operation (encoding source / decoded result).
  - ``separator`` (encode only, optional str, default ``""``): inserted
    between every pair of hex digits (e.g. ``" "`` or ``":"``).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class HexPlugin:
    name: str = "hex"

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
            raise ValueError("hex requires params['input'] as a string.")
        encoding = str(params.get("encoding") or "utf-8")

        if mode == "encode":
            sep = str(params.get("separator") or "")
            data = text.encode(encoding)
            value = data.hex(sep) if sep else data.hex()
        elif mode == "decode":
            cleaned = "".join(ch for ch in text if ch.isalnum())
            try:
                raw = bytes.fromhex(cleaned)
            except ValueError as exc:
                raise ValueError(f"hex: decode failed: {exc}") from exc
            value = raw.decode(encoding, errors="replace")
        else:
            raise ValueError(f"hex: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
