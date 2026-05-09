"""Gzip compression tool plugin via stdlib gzip.

Params:
  - ``mode``: ``"compress" | "decompress"``.
  - ``input`` (str): when ``from_path`` is False (default), this is the
    text payload; when True, it's a path to the source file.
  - ``output`` (optional str): destination path. Required when
    ``from_path`` is True. Otherwise the result is returned as bytes
    (b64-encoded, since ToolResult JSON-serialises) on ``value``.
  - ``from_path`` (bool, default False).
  - ``encoding`` (str, default ``"utf-8"``): used to decode decompressed
    bytes back to text when output is in-memory and decompressing.
"""

from __future__ import annotations

import base64
import gzip as _gzip
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class GzipPlugin:
    name: str = "gzip"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        mode = str(params.get("mode") or "compress").lower()
        if mode not in ("compress", "decompress"):
            raise ValueError(f"gzip: unknown mode {mode!r}")
        from_path = bool(params.get("from_path"))
        output_param = params.get("output")
        encoding = str(params.get("encoding") or "utf-8")

        if from_path:
            src = params.get("input")
            if not isinstance(src, str) or not src:
                raise ValueError("gzip: from_path=True requires params['input'] path.")
            if not isinstance(output_param, str) or not output_param:
                raise ValueError(
                    "gzip: from_path=True requires params['output'] path."
                )
            src_path = Path(src).expanduser()
            dst_path = Path(output_param).expanduser()
            dst_path.parent.mkdir(parents=True, exist_ok=True)
            if mode == "compress":
                with open(src_path, "rb") as src_f, _gzip.open(dst_path, "wb") as out_f:
                    out_f.writelines(src_f)
            else:
                with _gzip.open(src_path, "rb") as src_f, open(dst_path, "wb") as out_f:
                    out_f.writelines(src_f)
            return ToolResult(
                value=str(dst_path),
                raw={"mode": mode, "input_bytes": src_path.stat().st_size},
                stdout=None, stderr=None, exit_code=None,
            )

        # In-memory path — input is a string payload.
        text = params.get("input")
        if not isinstance(text, str):
            raise ValueError("gzip: in-memory mode requires params['input'] as str.")
        if mode == "compress":
            compressed = _gzip.compress(text.encode(encoding))
            value: Any = base64.b64encode(compressed).decode("ascii")
            raw = {"compressed_bytes": len(compressed), "encoding": "base64"}
        else:
            try:
                blob = base64.b64decode(text, validate=True)
            except Exception as exc:
                raise ValueError(
                    "gzip: in-memory decompress expects base64 input."
                ) from exc
            value = _gzip.decompress(blob).decode(encoding)
            raw = {"decompressed_bytes": len(value.encode(encoding))}
        return ToolResult(
            value=value, raw=raw, stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        return CheckResult(ok=True, missing=[])
