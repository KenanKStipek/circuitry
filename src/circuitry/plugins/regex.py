"""Regex tool plugin — match / search / findall / sub on a string.

Params:
  - ``pattern`` (required, str)
  - ``input`` (required, str)
  - ``mode`` (optional, default ``"findall"``): one of ``"match"``,
    ``"search"``, ``"findall"``, ``"sub"``.
  - ``flags`` (optional, list[str]): subset of
    ``["IGNORECASE", "MULTILINE", "DOTALL", "VERBOSE", "ASCII"]``.
  - ``replacement`` (required when mode=="sub"): replacement string.
  - ``count`` (optional, int): max replacements when mode=="sub" (0 = all).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_FLAG_MAP = {
    "IGNORECASE": re.IGNORECASE,
    "MULTILINE": re.MULTILINE,
    "DOTALL": re.DOTALL,
    "VERBOSE": re.VERBOSE,
    "ASCII": re.ASCII,
}


def _resolve_flags(flag_names: Any) -> int:
    if not flag_names:
        return 0
    if not isinstance(flag_names, (list, tuple)):
        raise ValueError("regex: params['flags'] must be a list of flag names.")
    flags = 0
    for name in flag_names:
        key = str(name).upper()
        if key not in _FLAG_MAP:
            raise ValueError(f"regex: unknown flag {name!r}")
        flags |= _FLAG_MAP[key]
    return flags


@dataclass(frozen=True)
class RegexPlugin:
    name: str = "regex"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        pattern = params.get("pattern")
        text = params.get("input")
        mode = str(params.get("mode", "findall")).lower()

        if not isinstance(pattern, str) or not pattern:
            raise ValueError("RegexPlugin requires params['pattern'].")
        if not isinstance(text, str):
            raise ValueError("RegexPlugin requires params['input'] as a string.")
        if mode not in ("match", "search", "findall", "sub"):
            raise ValueError(
                f"regex: unknown mode {mode!r}; "
                "expected match|search|findall|sub."
            )

        flags = _resolve_flags(params.get("flags"))
        try:
            compiled = re.compile(pattern, flags=flags)
        except re.error as exc:
            raise ValueError(f"regex: invalid pattern: {exc}") from exc

        value: Any
        if mode == "match":
            m = compiled.match(text)
            value = list(m.groups()) if m and m.groups() else (m.group(0) if m else None)
        elif mode == "search":
            m = compiled.search(text)
            value = list(m.groups()) if m and m.groups() else (m.group(0) if m else None)
        elif mode == "findall":
            value = compiled.findall(text)
        else:  # sub
            replacement = params.get("replacement")
            if not isinstance(replacement, str):
                raise ValueError(
                    "regex: params['replacement'] required when mode='sub'."
                )
            count = int(params.get("count", 0))
            value = compiled.sub(replacement, text, count=count)

        return ToolResult(
            value=value,
            raw={"pattern": pattern, "mode": mode, "flags": params.get("flags")},
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
