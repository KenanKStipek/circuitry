"""A loopback-bound mock HTTP server for the conformance suite
(electricity/DESIGN.md §12, issue #450): fakes any HTTP-family tool or
model adapter (`http`, an OpenAI-compatible adapter's `base_url`, ...)
without a real network call, the same way `fakes/` fakes a process-backed
tool.

Owned entirely by the harness, never by a document or an adapter: started
per case, on `127.0.0.1` with an ephemeral port, serving from one
background thread, and always stopped in a `finally` (`mock_http_server`
below). It is scripted by a YAML (or JSON) fixture file in the case
directory — a list of `request`/`response` pairs, loaded through
Circuitry's own loader (`core.yaml_load.load_yaml`, per #450's decision)
so a malformed fixture fails the same way a malformed orchestration
document would. Every request actually received is recorded (method,
path, query, headers apart from `User-Agent`, and body) so a conformance
case can commit the recording as `expected.http_requests.json` and both
engines' runs can be diffed against it (today, only the Python engine
runs this far — electricity still refuses every document that reaches
an HTTP-family tool/adapter through the preview marker, per the M0-H
refusal walker).
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qsl, urlsplit

from circuitry.core.yaml_load import load_yaml

#: Fixture top-level shape: a list of ``{"request": {...}, "response": {...}}``
#: mappings, matched strictly in list order (never by lookup) -- the same
#: "consumed in order" contract `scripted-replies.md` uses for model
#: replies, so a case with several calls to the same path/method is still
#: deterministic.
_REQUEST_KEYS = frozenset({"method", "path"})
_RESPONSE_REQUIRED_KEYS = frozenset({"status"})
_RESPONSE_ALLOWED_KEYS = frozenset({"status", "reason", "headers", "body"})


class MockHttpFixtureError(ValueError):
    """The fixture file itself is malformed."""


@dataclass(frozen=True)
class _Matcher:
    method: str
    path: str
    status: int
    reason: str
    headers: dict[str, str]
    body: str


@dataclass(frozen=True)
class RecordedRequest:
    method: str
    path: str
    query: list[list[str]]
    headers: dict[str, str]
    body: str

    def to_json(self) -> dict[str, Any]:
        return {
            "method": self.method,
            "path": self.path,
            "query": self.query,
            "headers": self.headers,
            "body": self.body,
        }


def _load_fixture(path: Path) -> list[_Matcher]:
    raw = load_yaml(path.read_text(encoding="utf-8"))
    if not isinstance(raw, list):
        raise MockHttpFixtureError(
            f"{path}: mock HTTP fixture must be a list of request/response entries"
        )
    matchers: list[_Matcher] = []
    for i, entry in enumerate(raw):
        if not isinstance(entry, dict) or set(entry) != {"request", "response"}:
            raise MockHttpFixtureError(
                f"{path}: entry {i} must be a mapping with exactly 'request' and "
                "'response' keys"
            )
        req = entry["request"]
        if not isinstance(req, dict) or set(req) != _REQUEST_KEYS:
            raise MockHttpFixtureError(
                f"{path}: entry {i}'s 'request' must be a mapping with exactly "
                f"{sorted(_REQUEST_KEYS)}"
            )
        resp = entry["response"]
        if (
            not isinstance(resp, dict)
            or not set(resp) >= _RESPONSE_REQUIRED_KEYS
            or set(resp) - _RESPONSE_ALLOWED_KEYS
        ):
            raise MockHttpFixtureError(
                f"{path}: entry {i}'s 'response' must be a mapping with 'status' "
                f"and only keys from {sorted(_RESPONSE_ALLOWED_KEYS)}"
            )
        headers = resp.get("headers") or {}
        if not isinstance(headers, dict):
            raise MockHttpFixtureError(
                f"{path}: entry {i}'s 'response.headers' must be a mapping"
            )
        matchers.append(
            _Matcher(
                method=str(req["method"]).upper(),
                path=str(req["path"]),
                status=int(resp["status"]),
                reason=str(resp.get("reason", "")),
                headers={str(k): str(v) for k, v in headers.items()},
                body=str(resp.get("body", "")),
            )
        )
    return matchers


class MockHttpServer:
    """One loopback mock HTTP server for one conformance case. Construct,
    `start()`, run the case, `stop()` -- or use as a context manager, which
    does both. `requests` accumulates every request actually received, in
    arrival order, for the lifetime of the instance."""

    def __init__(self, fixture_path: Path) -> None:
        self._matchers = _load_fixture(fixture_path)
        self._next_index = 0
        self._lock = threading.Lock()
        self.requests: list[RecordedRequest] = []
        self._httpd = HTTPServer(("127.0.0.1", 0), _make_handler(self))
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)

    @property
    def port(self) -> int:
        return int(self._httpd.server_address[1])

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._httpd.shutdown()
        self._httpd.server_close()
        self._thread.join(timeout=5)

    def __enter__(self) -> MockHttpServer:
        self.start()
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.stop()

    def _next_matcher(self, *, method: str, path: str) -> tuple[_Matcher | None, str | None]:
        """Pop and return the next matcher if it matches *method*/*path*,
        else ``(None, <diagnostic>)`` -- never raises, since this runs
        inside the handler thread and an exception there would just produce
        an opaque connection-reset for the real client instead of a
        diagnosable response."""
        with self._lock:
            if self._next_index >= len(self._matchers):
                return None, (
                    f"mock server: no matcher left for {method} {path} "
                    f"({len(self._matchers)} configured)"
                )
            matcher = self._matchers[self._next_index]
            if matcher.method != method or matcher.path != path:
                return None, (
                    f"mock server: expected {matcher.method} {matcher.path}, "
                    f"got {method} {path}"
                )
            self._next_index += 1
            return matcher, None

    def _record(self, request: RecordedRequest) -> None:
        with self._lock:
            self.requests.append(request)


def _make_handler(server: MockHttpServer) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def _handle(self) -> None:
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length).decode("utf-8") if length else ""
            split = urlsplit(self.path)
            headers = {k: v for k, v in self.headers.items() if k.lower() != "user-agent"}
            query = [[k, v] for k, v in parse_qsl(split.query, keep_blank_values=True)]
            server._record(
                RecordedRequest(
                    method=self.command, path=split.path, query=query, headers=headers, body=body
                )
            )
            matcher, error = server._next_matcher(method=self.command, path=split.path)
            if matcher is None:
                assert error is not None
                body_bytes = error.encode("utf-8")
                # `send_response_only`, never `send_response`: the latter
                # adds `Server`/`Date` headers that vary run to run --
                # "headers are fixed, with no Date or Server values that
                # vary" (issue #450 item 3). Every response header here
                # comes only from the fixture (or this one diagnostic path).
                self.send_response_only(599, "mock server: unmatched request")
                self.send_header("Content-Length", str(len(body_bytes)))
                self.end_headers()
                self.wfile.write(body_bytes)
                return
            body_bytes = matcher.body.encode("utf-8")
            self.send_response_only(matcher.status, matcher.reason)
            for k, v in matcher.headers.items():
                self.send_header(k, v)
            self.send_header("Content-Length", str(len(body_bytes)))
            self.end_headers()
            self.wfile.write(body_bytes)

        def do_GET(self) -> None:
            self._handle()

        def do_POST(self) -> None:
            self._handle()

        def do_PUT(self) -> None:
            self._handle()

        def do_DELETE(self) -> None:
            self._handle()

        def do_PATCH(self) -> None:
            self._handle()

        def log_message(self, format: str, *args: Any) -> None:
            pass  # silence BaseHTTPRequestHandler's default stderr logging

    return Handler
