"""Browser automation tool plugin via Playwright (sync API).

Optional dep: ``playwright`` plus a one-time browser install
(``playwright install chromium``). Install with
``pip install circuitry-cof[playwright]``.

Each ``execute()`` call launches an ephemeral browser, runs the
specified steps, and tears it down. This keeps the plugin stateless at
the cost of startup latency per call — for high-volume use the
orchestration should batch operations into a single ``steps`` list.

Params:
  - ``url`` (required, str): page to load.
  - ``mode`` (optional, default ``"text"``):
    * ``"text"`` — extract visible text via ``page.inner_text("body")``.
    * ``"html"`` — return ``page.content()``.
    * ``"steps"`` — execute a sequence of operations; ``value`` is the
      result of the final extraction step (or None).
  - ``steps`` (steps mode, list[dict]): each step is one of:
    ``{"action": "click", "selector": "..."}``,
    ``{"action": "fill", "selector": "...", "value": "..."}``,
    ``{"action": "wait_for", "selector": "..."}``,
    ``{"action": "extract", "selector": "...", "attribute": "text"|"html"|<attr>}``.
  - ``browser`` (optional, default ``"chromium"``).
  - ``headless`` (optional, bool, default True).
  - ``user_agent`` (optional, str).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_VALID_BROWSERS = ("chromium", "firefox", "webkit")


def _run_steps(page: Any, steps: list[dict[str, Any]]) -> Any:
    """Execute ordered ``steps`` against ``page``. Returns the last
    extraction result (or None if no extract step ran)."""
    last_extract: Any = None
    for i, step in enumerate(steps):
        if not isinstance(step, dict):
            raise ValueError(f"playwright: steps[{i}] must be a dict.")
        action = str(step.get("action") or "").lower()
        selector = step.get("selector")
        if action in ("click", "fill", "wait_for", "extract") and not isinstance(
            selector, str
        ):
            raise ValueError(
                f"playwright: steps[{i}] action {action!r} requires 'selector'."
            )
        if action == "click":
            page.click(selector)
        elif action == "fill":
            page.fill(selector, str(step.get("value") or ""))
        elif action == "wait_for":
            page.wait_for_selector(selector)
        elif action == "extract":
            attr = str(step.get("attribute") or "text").lower()
            element = page.query_selector(selector)
            if element is None:
                last_extract = None
            elif attr == "text":
                last_extract = element.inner_text()
            elif attr == "html":
                last_extract = element.inner_html()
            else:
                last_extract = element.get_attribute(attr)
        else:
            raise ValueError(
                f"playwright: steps[{i}] unknown action {action!r}"
            )
    return last_extract


@dataclass(frozen=True)
class PlaywrightPlugin:
    name: str = "playwright"

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
                "playwright: playwright not installed. Install with: "
                "pip install playwright && playwright install chromium"
            ) from exc

        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("playwright requires params['url'].")
        mode = str(params.get("mode") or "text").lower()
        if mode not in ("text", "html", "steps"):
            raise ValueError(f"playwright: unknown mode {mode!r}")
        browser_name = str(params.get("browser") or "chromium").lower()
        if browser_name not in _VALID_BROWSERS:
            raise ValueError(
                f"playwright: browser must be {_VALID_BROWSERS}, got {browser_name!r}"
            )
        headless = bool(params.get("headless", True))
        user_agent = params.get("user_agent")

        with sync_playwright() as pw:
            browser = getattr(pw, browser_name).launch(headless=headless)
            try:
                ctx_kwargs: dict[str, Any] = {}
                if isinstance(user_agent, str) and user_agent:
                    ctx_kwargs["user_agent"] = user_agent
                context = browser.new_context(**ctx_kwargs)
                page = context.new_page()
                page.set_default_timeout(int(timeout_seconds) * 1000)
                page.goto(url.strip())
                if mode == "text":
                    value: Any = page.inner_text("body")
                elif mode == "html":
                    value = page.content()
                else:
                    steps = params.get("steps") or []
                    if not isinstance(steps, list):
                        raise ValueError(
                            "playwright steps mode requires params['steps'] as a list."
                        )
                    value = _run_steps(page, steps)
                final_url = page.url
            finally:
                browser.close()

        return ToolResult(
            value=value,
            raw={"url": url, "final_url": final_url, "mode": mode},
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
