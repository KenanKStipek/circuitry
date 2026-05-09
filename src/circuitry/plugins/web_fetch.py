"""Web page fetch + main-content extraction tool plugin.

Fetches a URL with ``requests`` and runs the response through
``trafilatura`` to strip boilerplate (navigation, ads, footers) and
return only the article content.

Optional deps: ``requests``, ``trafilatura``. Install with
``pip install circuitry-cof[web_fetch]``.

Params:
  - ``url`` (required, str)
  - ``mode`` (optional, default ``"text"``):
    * ``"text"`` — main body text via trafilatura.
    * ``"markdown"`` — main body as Markdown.
    * ``"html"`` — raw response HTML (no extraction).
    * ``"json"`` — parse JSON response body.
  - ``timeout_ms`` (optional, default 15000).
  - ``user_agent`` (optional): override the default UA.
  - ``include_images`` (markdown mode, bool, default False).
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


_DEFAULT_UA = "circuitry/0.1 (+https://github.com/kenankstipek/circuitry)"


@dataclass(frozen=True)
class WebFetchPlugin:
    name: str = "web_fetch"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        try:
            import requests  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "web_fetch: requests not installed. "
                "Install with: pip install requests trafilatura"
            ) from exc

        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("web_fetch requires params['url'].")
        mode = str(params.get("mode") or "text").lower()
        if mode not in ("text", "markdown", "html", "json"):
            raise ValueError(
                f"web_fetch: mode must be text|markdown|html|json, got {mode!r}"
            )
        timeout_ms = int(params.get("timeout_ms") or 15000)
        ua = str(params.get("user_agent") or _DEFAULT_UA)

        try:
            resp = requests.get(
                url.strip(),
                headers={"User-Agent": ua},
                timeout=max(0.1, timeout_ms / 1000.0),
            )
        except requests.exceptions.RequestException as exc:
            raise RuntimeError(f"web_fetch request failed: {exc}") from exc

        status = int(resp.status_code)
        text = resp.text
        raw: dict[str, Any] = {
            "url": url, "status": status, "mode": mode,
            "content_type": resp.headers.get("Content-Type", ""),
        }

        if mode == "html":
            return ToolResult(
                value=text, raw=raw, stdout=None, stderr=None, exit_code=status
            )
        if mode == "json":
            try:
                import json as _json
                value: Any = _json.loads(text) if text else None
            except ValueError as exc:
                raise RuntimeError(
                    f"web_fetch: response is not JSON: {exc}"
                ) from exc
            return ToolResult(
                value=value, raw=raw, stdout=None, stderr=None, exit_code=status
            )

        # text / markdown — needs trafilatura.
        try:
            import trafilatura  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "web_fetch: trafilatura not installed (required for "
                "text/markdown modes). Install with: pip install trafilatura"
            ) from exc

        output_format = "markdown" if mode == "markdown" else "txt"
        extracted = trafilatura.extract(
            text,
            output_format=output_format,
            include_images=bool(params.get("include_images")),
            url=url,
        ) or ""
        return ToolResult(
            value=extracted, raw=raw, stdout=None, stderr=None, exit_code=status
        )

    def check(self) -> CheckResult:
        missing: list[str] = []
        if importlib.util.find_spec("requests") is None:
            missing.append("library:requests")
        if importlib.util.find_spec("trafilatura") is None:
            missing.append("library:trafilatura")
        if missing:
            return CheckResult(
                ok=False,
                missing=missing,
                message="pip install requests trafilatura",
            )
        return CheckResult(ok=True, missing=[])
