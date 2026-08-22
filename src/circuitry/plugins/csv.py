"""CSV tool plugin — parse and serialise via stdlib csv.

Params:
  - ``mode``: ``"parse"`` (default) or ``"write"``.
  - ``input``:
      * parse mode: CSV string (or path when ``params['from_path']=True``).
      * write mode: list of dicts (uses keys as header) OR list of lists
        (and optional ``header``).
  - ``delimiter`` (optional, default ``,``).
  - ``has_header`` (parse, default ``True``): when true, returns list of
    dicts; when false, returns list of lists.
  - ``from_path`` (parse, default ``False``): treat ``input`` as a path
    and read it.
  - ``header`` (write, optional list of column names): forces output
    columns when writing list-of-lists rows.
"""

from __future__ import annotations

import csv as _csv
import io
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class CsvPlugin:
    name: str = "csv"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode", "parse")).lower()
        delimiter = str(params.get("delimiter") or ",")

        if mode == "parse":
            return self._parse(params, delimiter)
        if mode == "write":
            return self._write(params, delimiter)
        raise ValueError(f"csv: unknown mode {mode!r}")

    @staticmethod
    def _parse(params: dict[str, Any], delimiter: str) -> ToolResult:
        text = params.get("input")
        if not isinstance(text, str):
            raise ValueError("csv: parse requires params['input'] as a string.")
        if params.get("from_path"):
            text = Path(text).expanduser().read_text(encoding="utf-8")
        has_header = bool(params.get("has_header", True))
        reader = _csv.reader(io.StringIO(text), delimiter=delimiter)
        rows = list(reader)
        if has_header and rows:
            header, *body = rows
            value: Any = [dict(zip(header, row, strict=False)) for row in body]
        else:
            value = rows
        return ToolResult(
            value=value,
            raw={"rows": len(rows), "has_header": has_header},
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    @staticmethod
    def _write(params: dict[str, Any], delimiter: str) -> ToolResult:
        rows = params.get("input")
        if not isinstance(rows, list):
            raise ValueError(
                "csv: write requires params['input'] as a list of rows."
            )
        buf = io.StringIO()
        if rows and isinstance(rows[0], dict):
            fieldnames = params.get("header")
            if not fieldnames:
                # Preserve insertion order across all rows.
                fieldnames = list(
                    {k: None for r in rows if isinstance(r, dict) for k in r}.keys()
                )
            dict_writer = _csv.DictWriter(
                buf, fieldnames=list(fieldnames), delimiter=delimiter
            )
            dict_writer.writeheader()
            for row in rows:
                dict_writer.writerow(row)
        else:
            list_writer = _csv.writer(buf, delimiter=delimiter)
            header = params.get("header")
            if header:
                list_writer.writerow(header)
            for row in rows:
                list_writer.writerow(row)
        text = buf.getvalue()
        return ToolResult(
            value=text,
            raw={"bytes": len(text.encode("utf-8"))},
            stdout=None,
            stderr=None,
            exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
