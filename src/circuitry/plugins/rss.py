"""RSS / Atom feed parser tool plugin via feedparser.

Optional dep: ``feedparser``. Install with ``pip install circuitry-cof[rss]``.

Params:
  - ``url`` (required): feed URL or path.
  - ``limit`` (optional, int): max number of entries to return.

Returns ``value`` = list of entry dicts: title, link, summary,
published, author, guid.

feedparser's own ``parse()`` has no timeout for its internal fetch, so an
http(s) URL is fetched here with the effect's ``timeout_seconds`` first and
the response bytes handed to ``feedparser.parse()``, along with the
response's headers (``response_headers``) so feedparser still resolves
relative entry links against the fetched URL and picks up a charset given
only in ``Content-Type`` — both of which it would otherwise get for free
from ``parse()`` fetching the URL itself. A local path or raw feed string
still goes straight to ``feedparser.parse()`` unchanged.

Unlike feedparser's own fetch (which reports an HTTP error as a soft
``bozo`` result), fetching here raises on a 4xx/5xx response or a network
error.
"""

from __future__ import annotations

import http.client
import importlib.util
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlparse

from .. import http_conventions
from ..preflight import CheckResult
from .base import ToolResult

_DEFAULT_UA = "circuitry-rss/0.1 (+https://github.com/kenankstipek/circuitry)"


@dataclass(frozen=True)
class RssPlugin:
    name: str = "rss"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
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
        stripped = url.strip()
        limit = params.get("limit")

        response_headers: dict[str, str] | None = None
        if urlparse(stripped).scheme in ("http", "https"):
            # Checked, then its userinfo stripped, before urllib ever
            # sees it (issue #819): `check_url` refuses a malformed
            # port with Circuitry's own redacted text, so `http.client`
            # never gets a chance to raise its own credential-bearing
            # `InvalidURL("nonnumeric port: 'longsecret'")` for a
            # userinfo with a colon but no port
            # (`http://alice:longsecret/x@127.0.0.1`), which
            # `scrub_embedded_urls` can't find (that exception text has
            # no `scheme://` of its own).
            try:
                http_conventions.check_url(stripped)
            except ValueError as exc:
                raise RuntimeError(f"rss: {exc}") from exc
            req = urllib.request.Request(
                http_conventions.strip_request_url_userinfo(stripped),
                headers={"User-Agent": _DEFAULT_UA},
            )
            try:
                with urllib.request.urlopen(req, timeout=timeout_seconds) as resp:
                    source: Any = resp.read()
                    response_headers = dict(resp.headers)
                    response_headers["content-location"] = resp.geturl()
            except (
                urllib.error.URLError,
                TimeoutError,
                OSError,
                http.client.HTTPException,
            ) as exc:
                raise RuntimeError(
                    f"rss: failed to fetch {http_conventions.redact_request_url(stripped)!r}: "
                    f"{http_conventions.scrub_embedded_urls(str(exc))}"
                ) from exc
        else:
            source = stripped

        feed = feedparser.parse(source, response_headers=response_headers)
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
                "url": http_conventions.redact_request_url(url),
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
