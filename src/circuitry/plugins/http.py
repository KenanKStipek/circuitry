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
  - ``fail_on_error`` (optional, bool, default ``True``): a 4xx/5xx response
    sets ``ToolResult.ok = False``, which fails the effect (``meta.error``,
    ``on_error`` applies) exactly like a raised exception. Set ``false`` to
    restore the old behaviour of always succeeding and letting the
    orchestration branch on ``meta.status_code`` itself.
  - ``redirects`` (optional, ``"follow"`` | ``"never"`` | ``"strip_auth"``):
    ``follow`` follows a redirect with the same headers (urllib's own rule);
    ``never`` follows none, and a 3xx response fails like a 4xx;
    ``strip_auth`` follows only to ``https``, and to another origin (scheme,
    host or port) without ``Authorization`` or a header that came from a
    secret. A redirect it refuses fails like a 4xx. The default is
    ``strip_auth`` when a header came from a secret (``secret_headers``),
    ``follow`` otherwise.

ToolResult shape:
  - ``value``: parsed body per ``parse`` (dict/list for JSON, str for text).
  - ``raw``: ``{"status": int, "headers": {...}, "url": str, "body": str}``.
  - ``stdout``: ``None``. ``stderr``: error message on non-2xx, including a
    bounded, redacted excerpt of the response body when one is available
    (the JSON ``error``/``message``/``detail`` field, or the first ~500
    characters).
  - ``exit_code``: always ``None`` — this plugin issues no process. The HTTP
    status lands on ``meta.status_code`` (from ``raw["status"]``), not here.
  - ``ok``: ``False`` on a 4xx/5xx response, on a redirect ``redirects``
    refused, and on any 3xx under ``redirects: never``, unless
    ``fail_on_error: false``.
"""

from __future__ import annotations

import importlib.util
import json as _json
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult, _as_bool, http_error_excerpt

REDIRECT_MODES = ("follow", "never", "strip_auth")

#: The note a refused redirect's failure carries, by ``redirects`` mode.
_REFUSED_REDIRECT_NOTES = {
    "never": "redirect not followed (redirects: never)",
    "strip_auth": "redirect not followed (redirects: strip_auth follows https only)",
}

_DEFAULT_PORTS = {"http": 80, "https": 443}


def _origin(url: str) -> tuple[str, str, int | None] | None:
    """``(scheme, host, port)`` of *url*, a default port filled in; ``None``
    when the URL has no host or an unreadable port, which matches no origin."""
    parts = urllib.parse.urlsplit(url)
    try:
        port = parts.port
    except ValueError:
        return None
    if not parts.hostname:
        return None
    scheme = parts.scheme.lower()
    return scheme, parts.hostname, port if port is not None else _DEFAULT_PORTS.get(scheme)


class _RedirectPolicy(urllib.request.HTTPRedirectHandler):
    """urllib's redirect handling, under ``redirects: never`` or
    ``strip_auth``. A refused redirect returns no request, so urllib raises
    the 3xx as an ``HTTPError``; :attr:`refused` records that it was refused."""

    def __init__(self, mode: str, auth_headers: frozenset[str]) -> None:
        self.mode = mode
        self.auth_headers = auth_headers
        self.refused = False

    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> urllib.request.Request | None:
        if self.mode == "never" or urllib.parse.urlsplit(newurl).scheme.lower() != "https":
            self.refused = True
            return None
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        origin = _origin(req.full_url)
        if new is not None and (origin is None or origin != _origin(newurl)):
            for header in list(new.headers):
                if header.lower() in self.auth_headers:
                    del new.headers[header]
        return new


@dataclass(frozen=True)
class HttpPlugin:
    name: str = "http"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
        secret_headers: frozenset[str] = frozenset(),
        scrub_secrets: Callable[[str], str] | None = None,
    ) -> ToolResult:
        """*secret_headers*: the (lower-cased) names of the headers whose
        value came from a secret (``core.tool`` resolves them, #598).
        *scrub_secrets* replaces their values in an error body before the
        excerpt cuts it, so no part of a value survives the cut."""
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
        fail_on_error = _as_bool(params.get("fail_on_error"), default=True)
        redirects = params.get("redirects")
        if redirects is None:
            redirects = "strip_auth" if secret_headers else "follow"
        if redirects not in REDIRECT_MODES:
            raise ValueError(
                "HttpPlugin: params['redirects'] must be one of 'follow', 'never', "
                f"'strip_auth', got {redirects!r}."
            )

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

        policy: _RedirectPolicy | None = None
        open_request: Any = urllib.request.urlopen
        if redirects != "follow":
            policy = _RedirectPolicy(redirects, frozenset({"authorization", *secret_headers}))
            open_request = urllib.request.build_opener(policy).open

        try:
            with open_request(req, timeout=int(timeout_seconds)) as resp:
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
            if policy is not None and policy.refused:
                stderr = f"{stderr}: {_REFUSED_REDIRECT_NOTES[redirects]}"
            excerpt = http_error_excerpt(scrub_secrets(text) if scrub_secrets else text)
            if excerpt:
                stderr = f"{stderr}: {excerpt}"
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
            exit_code=None,
            ok=not (
                fail_on_error
                and (
                    status >= 400
                    or (policy is not None and policy.refused)
                    or (redirects == "never" and 300 <= status < 400)
                )
            ),
        )

    def check(self) -> CheckResult:
        # urllib is stdlib; nothing else required. Spec-format `library:`
        # marker is omitted because there's no install step possible.
        if importlib.util.find_spec("urllib") is None:
            return CheckResult(ok=False, missing=["library:urllib"])
        return CheckResult(ok=True, missing=[])
