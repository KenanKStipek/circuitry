"""Screenshot tool plugin via Playwright.

Optional dep: ``playwright`` plus a one-time browser install
(``playwright install chromium``). Install with
``pip install circuitry-cof[screenshot]``.

A focused subset of the playwright plugin for the common case of "load
a URL, save a screenshot". For richer browser interaction use the
``playwright`` plugin directly.

Params:
  - ``url`` (required, str).
  - ``output`` (required, str): destination image path. ``.png`` or
    ``.jpeg`` extension determines format.
  - ``full_page`` (optional, bool, default True): scroll the entire
    page or capture only the viewport.
  - ``viewport`` (optional, dict): ``{"width": int, "height": int}``.
  - ``wait_for`` (optional, str): CSS selector to wait for before
    capturing.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class ScreenshotPlugin:
    name: str = "screenshot"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            from playwright.sync_api import sync_playwright  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "screenshot: playwright not installed. Install with: "
                "pip install playwright && playwright install chromium"
            ) from exc

        url = params.get("url")
        output = params.get("output")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("screenshot requires params['url'].")
        if not isinstance(output, str) or not output.strip():
            raise ValueError("screenshot requires params['output'].")
        full_page = bool(params.get("full_page", True))
        viewport = params.get("viewport")
        wait_for = params.get("wait_for")

        out_path = Path(output).expanduser()
        out_path.parent.mkdir(parents=True, exist_ok=True)

        with sync_playwright() as pw:
            browser = pw.chromium.launch(headless=True)
            try:
                ctx_kwargs: dict[str, Any] = {}
                if isinstance(viewport, dict):
                    ctx_kwargs["viewport"] = {
                        "width": int(viewport.get("width", 1280)),
                        "height": int(viewport.get("height", 720)),
                    }
                context = browser.new_context(**ctx_kwargs)
                page = context.new_page()
                page.set_default_timeout(int(timeout_seconds) * 1000)
                page.goto(url.strip())
                if isinstance(wait_for, str) and wait_for:
                    page.wait_for_selector(wait_for)
                page.screenshot(path=str(out_path), full_page=full_page)
            finally:
                browser.close()

        return ToolResult(
            value=str(out_path),
            raw={"url": url, "full_page": full_page},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("playwright") is None:
            return CheckResult(
                ok=False,
                missing=["library:playwright"],
                message="pip install playwright && playwright install chromium",
            )
        return CheckResult(ok=True, missing=[])
