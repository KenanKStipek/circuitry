"""HTML extraction tool plugin via BeautifulSoup4 + lxml.

Optional deps: ``beautifulsoup4`` and ``lxml``. Install with
``pip install circuitry-cof[html_extract]``.

Params:
  - ``input``: HTML string (or path when ``from_path`` is True).
  - ``selector``: CSS selector (e.g. ``"article > h1"``).
  - ``from_path`` (bool, default False).
  - ``mode`` (optional, default ``"text"``): ``"text"`` returns the
    text of each matched node; ``"html"`` returns the inner HTML;
    ``"attr"`` returns a single attribute (specify ``attribute``).
  - ``attribute`` (str, required when mode=="attr"): attribute name
    (e.g. ``"href"``).
  - ``limit`` (optional, int): cap matched-node count.

Returns ``value`` = list of strings (one per matched node).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class HtmlExtractPlugin:
    name: str = "html_extract"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            from bs4 import BeautifulSoup  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "html_extract: beautifulsoup4 not installed. "
                "Install with: pip install beautifulsoup4 lxml"
            ) from exc

        text = params.get("input")
        if not isinstance(text, str):
            raise ValueError("html_extract requires params['input'] as a string.")
        if params.get("from_path"):
            text = Path(text).expanduser().read_text(encoding="utf-8")
        selector = params.get("selector")
        if not isinstance(selector, str) or not selector.strip():
            raise ValueError("html_extract requires params['selector'].")
        mode = str(params.get("mode") or "text").lower()
        if mode not in ("text", "html", "attr"):
            raise ValueError(
                f"html_extract: mode must be text|html|attr, got {mode!r}"
            )
        attribute = params.get("attribute")
        if mode == "attr" and (not isinstance(attribute, str) or not attribute):
            raise ValueError("html_extract: mode=attr requires params['attribute'].")
        limit = params.get("limit")

        # Prefer lxml; fall back to html.parser when lxml isn't available.
        parser = "lxml" if importlib.util.find_spec("lxml") else "html.parser"
        soup = BeautifulSoup(text, parser)
        nodes = soup.select(selector)
        if isinstance(limit, int) and limit > 0:
            nodes = nodes[:limit]

        results: list[Any] = []
        for node in nodes:
            if mode == "text":
                results.append(node.get_text(strip=True))
            elif mode == "html":
                results.append(node.decode_contents())
            else:
                v = node.get(attribute)
                results.append(v if v is not None else "")

        return ToolResult(
            value=results,
            raw={"selector": selector, "mode": mode, "matches": len(results)},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("bs4") is None:
            missing.append("library:beautifulsoup4")
        # lxml is preferred but not strictly required (we fall back).
        # Don't surface as missing.
        if missing:
            return CheckResult(
                ok=False,
                missing=missing,
                message="pip install beautifulsoup4 lxml",
            )
        return CheckResult(ok=True, missing=[])
