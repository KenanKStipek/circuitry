"""JSON tool plugin — parse, stringify, and dotted-path extraction.

Params (depending on ``mode``):
  - ``mode`` (optional, default ``"parse"``): one of:
      * ``"parse"`` — JSON-decode params['input'] (str) → value.
      * ``"stringify"`` — JSON-encode params['input'] (any) → str.
      * ``"extract"`` — given params['input'] (dict|list|str) and
        params['path'] (dotted path with optional [N] indices),
        return the addressed value or None on miss.
  - ``input``: subject of the operation.
  - ``path`` (extract only): e.g. ``"foo.bar[0].baz"``.
  - ``indent`` (stringify only, int): pretty-print indentation.
  - ``default`` (extract only): value to return when the path misses.
"""

from __future__ import annotations

import json as _json
import re
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_PATH_TOKEN = re.compile(r"([A-Za-z_][A-Za-z0-9_-]*)|\[(-?\d+)\]")


def _walk_path(value: Any, path: str) -> tuple[Any, bool]:
    """Walk a dotted/indexed path. Returns (value, found)."""
    cursor = value
    pos = 0
    while pos < len(path):
        if path[pos] == ".":
            pos += 1
            continue
        m = _PATH_TOKEN.match(path, pos)
        if m is None:
            raise ValueError(f"json: invalid path token at offset {pos}")
        key, index = m.group(1), m.group(2)
        if key is not None:
            if not isinstance(cursor, dict) or key not in cursor:
                return (None, False)
            cursor = cursor[key]
        else:
            if not isinstance(cursor, list):
                return (None, False)
            try:
                cursor = cursor[int(index)]
            except (IndexError, ValueError):
                return (None, False)
        pos = m.end()
    return (cursor, True)


@dataclass(frozen=True)
class JsonPlugin:
    name: str = "json"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode", "parse")).lower()
        if mode == "parse":
            text = params.get("input")
            if not isinstance(text, str):
                raise ValueError(
                    "JsonPlugin: parse mode requires params['input'] as a string."
                )
            try:
                value = _json.loads(text)
            except _json.JSONDecodeError as exc:
                raise ValueError(f"json: parse failed: {exc}") from exc
        elif mode == "stringify":
            indent = params.get("indent")
            value = _json.dumps(
                params.get("input"),
                indent=int(indent) if indent is not None else None,
                ensure_ascii=False,
                default=str,
            )
        elif mode == "extract":
            subject = params.get("input")
            if isinstance(subject, str):
                try:
                    subject = _json.loads(subject)
                except _json.JSONDecodeError as exc:
                    raise ValueError(
                        f"json: extract input is a string but not valid JSON: {exc}"
                    ) from exc
            path = params.get("path")
            if not isinstance(path, str) or not path:
                raise ValueError(
                    "JsonPlugin: extract mode requires params['path']."
                )
            found_value, hit = _walk_path(subject, path)
            value = found_value if hit else params.get("default")
        else:
            raise ValueError(f"json: unknown mode {mode!r}")

        return ToolResult(
            value=value,
            raw={"mode": mode},
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
