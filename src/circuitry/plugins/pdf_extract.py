"""PDF text-extraction tool plugin via pdfplumber.

Optional dep: ``pdfplumber``. Install with
``pip install circuitry-cof[pdf_extract]``.

Params:
  - ``path`` (required): PDF file path.
  - ``mode`` (optional, default ``"text"``): ``"text"`` returns the full
    document as a single string; ``"per_page"`` returns a list of page
    strings; ``"tables"`` returns a list-of-lists per page.
  - ``pages`` (optional, list[int]): 1-indexed pages to include
    (default: all pages).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class PdfExtractPlugin:
    name: str = "pdf_extract"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import pdfplumber  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "pdf_extract: pdfplumber not installed. "
                "Install with: pip install pdfplumber"
            ) from exc

        path = params.get("path")
        if not isinstance(path, str) or not path:
            raise ValueError("pdf_extract requires params['path'].")
        mode = str(params.get("mode") or "text").lower()
        if mode not in ("text", "per_page", "tables"):
            raise ValueError(
                f"pdf_extract: mode must be text|per_page|tables, got {mode!r}"
            )

        page_filter = params.get("pages")
        if page_filter is not None and not isinstance(page_filter, list):
            raise ValueError("pdf_extract: params['pages'] must be a list of ints.")
        page_filter_set: set[int] | None = (
            {int(p) for p in page_filter} if page_filter else None
        )

        per_page_text: list[str] = []
        per_page_tables: list[list[list[Any]]] = []
        with pdfplumber.open(str(Path(path).expanduser())) as pdf:
            for i, page in enumerate(pdf.pages, start=1):
                if page_filter_set is not None and i not in page_filter_set:
                    continue
                if mode == "tables":
                    per_page_tables.append(page.extract_tables() or [])
                else:
                    per_page_text.append(page.extract_text() or "")

        if mode == "text":
            value: Any = "\n".join(per_page_text)
        elif mode == "per_page":
            value = per_page_text
        else:
            value = per_page_tables

        return ToolResult(
            value=value,
            raw={"path": path, "mode": mode, "pages": list(page_filter_set or [])},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("pdfplumber") is None:
            return CheckResult(
                ok=False,
                missing=["library:pdfplumber"],
                message="pip install pdfplumber",
            )
        return CheckResult(ok=True, missing=[])
