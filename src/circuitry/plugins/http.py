"""General-purpose HTTP tool plugin.

Issues a single HTTP request and returns the response. Uses stdlib
``urllib.request`` so circuitry stays dependency-free for this plugin —
unlike the spec's pre-mortem default of ``requests``, the standard
library is sufficient for the plugin's contract (single request, JSON
or text body, basic auth headers) and keeps the dependency footprint
narrow.

Params:
  - ``url`` (required): full URL.
  - ``method`` (optional, default ``"GET"``): HTTP verb.
  - ``headers`` (optional): mapping of header name → value.
  - ``json`` (optional): dict serialised as the request body and a
    ``Content-Type: application/json`` header is added if not present.
  - ``body`` (optional): raw string body, sent as-is (mutually exclusive
    with ``json``).
  - ``params`` (optional): mapping appended as the query string.
  - ``parse`` (optional, ``"json"`` | ``"text"`` | ``"auto"``, default
    ``"auto"``): controls how the response body becomes ``ToolResult.value``.
    ``auto`` parses JSON when the response Content-Type advertises it,
    otherwise returns text.

ToolResult shape:
  - ``value``: parsed body per ``parse`` (dict/list for JSON, str for text).
  - ``raw``: ``{"status": int, "headers": {...}, "url": str, "body": str}``.
  - ``stdout``: ``None``. ``stderr``: error message on non-2xx (since 4xx/5xx
    are surfaced rather than raised — let the orchestration decide).
  - ``exit_code``: HTTP status code so YAML callers can route on it.
"""

from __future__ import annotations

import importlib.util
import json as _json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult


@dataclass(frozen=True)
class HttpPlugin:
    name: str = "http"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("HttpPlugin requires params['url'] as a non-empty string.")
        url = url.strip()

        method = str(params.get("method", "GET")).upper()
        headers = dict(params.get("headers") or {})
        query_params = params.get("params")
        json_body = params.get("json")
        raw_body = params.get("body")
        parse = str(params.get("parse", "auto")).lower()

        if json_body is not None and raw_body is not None:
            raise ValueError(
                "HttpPlugin: pass either params['json'] or params['body'], not both."
            )

        if isinstance(query_params, dict) and query_params:
            sep = "&" if "?" in url else "?"
            url = url + sep + urllib.parse.urlencode(query_params, doseq=True)

        body_bytes: bytes | None = None
        if json_body is not None:
            body_bytes = _json.dumps(json_body).encode("utf-8")
            headers.setdefault("Content-Type", "application/json")
        elif raw_body is not None:
            body_bytes = (
                raw_body.encode("utf-8") if isinstance(raw_body, str) else bytes(raw_body)
            )

        req = urllib.request.Request(url, data=body_bytes, method=method)
        for k, v in headers.items():
            req.add_header(str(k), str(v))

        status: int = 0
        resp_headers: dict[str, str] = {}
        text: str = ""
        stderr: str | None = None

        try:
            with urllib.request.urlopen(req, timeout=int(timeout_seconds)) as resp:
                status = int(resp.status)
                resp_headers = {k.lower(): v for k, v in resp.headers.items()}
                text = resp.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as exc:
            # 4xx/5xx still produce a body — surface to caller, don't raise.
            status = int(exc.code)
            resp_headers = {k.lower(): v for k, v in (exc.headers or {}).items()}
            try:
                text = exc.read().decode("utf-8", errors="replace") if exc.fp else ""
            except Exception:
                text = ""
            stderr = f"HTTP {status}: {exc.reason}"
        except urllib.error.URLError as exc:
            # DNS / connection failure — true execution error.
            raise RuntimeError(f"HTTP request to {url} failed: {exc.reason}") from exc

        # Parse body per `parse`.
        content_type = (resp_headers.get("content-type") or "").lower()
        looks_like_json = "json" in content_type
        value: Any
        if parse == "json" or (parse == "auto" and looks_like_json):
            try:
                value = _json.loads(text) if text else None
            except _json.JSONDecodeError as exc:
                if parse == "json":
                    raise RuntimeError(
                        f"HttpPlugin: parse='json' but response is not JSON: {exc}"
                    ) from exc
                value = text
        else:
            value = text

        return ToolResult(
            value=value,
            raw={
                "status": status,
                "headers": resp_headers,
                "url": url,
                "body": text,
            },
            stdout=None,
            stderr=stderr,
            exit_code=status,
        )

    def check(self) -> CheckResult:
        # urllib is stdlib; nothing else required. Spec-format `library:`
        # marker is omitted because there's no install step possible.
        if importlib.util.find_spec("urllib") is None:
            return CheckResult(ok=False, missing=["library:urllib"])
        return CheckResult(ok=True, missing=[])
