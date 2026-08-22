"""RSS / Atom feed parser tool plugin via feedparser.

Optional dep: ``feedparser``. Install with ``pip install circuitry-cof[rss]``.

Params:
  - ``url`` (required): feed URL or path.
  - ``limit`` (optional, int): max number of entries to return.

Returns ``value`` = list of entry dicts: title, link, summary,
published, author, guid.
"""

from __future__ import annotations

import importlib.util
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class RssPlugin:
    name: str = "rss"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        try:
            import feedparser  # type: ignore[import-not-found]
        except ImportError as exc:
            raise RuntimeError(
                "rss: feedparser not installed. "
                "Install with: pip install feedparser"
            ) from exc

        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("rss requires params['url'].")
        limit = params.get("limit")

        feed = feedparser.parse(url.strip())
        # feedparser populates `bozo` to 1 on malformed feeds with the
        # underlying exception in `bozo_exception`. Treat as soft warning.
        bozo = bool(getattr(feed, "bozo", False))

        entries: list[dict[str, Any]] = [
            {
                "title": getattr(entry, "title", ""),
                "link": getattr(entry, "link", ""),
                "summary": getattr(entry, "summary", ""),
                "published": getattr(entry, "published", ""),
                "author": getattr(entry, "author", ""),
                "guid": getattr(entry, "id", "") or getattr(entry, "guid", ""),
            }
            for entry in feed.entries
        ]
        if isinstance(limit, int) and limit > 0:
            entries = entries[:limit]

        return ToolResult(
            value=entries,
            raw={
                "url": url,
                "feed_title": getattr(feed.feed, "title", ""),
                "bozo": bozo,
                "count": len(entries),
            },
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("feedparser") is None:
            return CheckResult(
                ok=False,
                missing=["library:feedparser"],
                message="pip install feedparser",
            )
        return CheckResult(ok=True, missing=[])
