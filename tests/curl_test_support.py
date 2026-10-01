"""Shared helpers for tests that inspect a `run_curl` invocation.

Headers never reach argv: they travel through a pipe fd passed via
`--config /dev/fd/<n>` and `pass_fds`. A test's fake `subprocess.run` can
read that fd's content synchronously (before `run_curl`'s `finally` closes
it) to recover the headers a call actually sent, without ever seeing them
in `cmd`.
"""

from __future__ import annotations

import http.server
import os
import re
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any, ClassVar

_HEADER_LINE_RE = re.compile(r'^header = "((?:[^"\\]|\\.)*)"$')


def read_config_headers(cmd: list[str]) -> dict[str, str]:
    """Headers sent via `--config <fd-path>` in `cmd`, or `{}` if none."""
    try:
        idx = cmd.index("--config")
    except ValueError:
        return {}
    fd = int(cmd[idx + 1].rsplit("/", 1)[-1])
    raw = os.read(fd, 65536).decode()
    headers: dict[str, str] = {}
    for line in raw.splitlines():
        m = _HEADER_LINE_RE.match(line)
        if not m:
            continue
        unescaped = m.group(1).replace('\\"', '"').replace("\\\\", "\\")
        name, _, value = unescaped.partition(": ")
        headers[name] = value
    return headers


def assert_q_first(cmd: list[str]) -> None:
    assert cmd[0] == "curl"
    assert cmd[1] == "-q", f"-q must be curl's first argument, got {cmd[:3]!r}"


def assert_not_in_argv(cmd: list[str], *needles: str) -> None:
    """None of `needles` appears in any argv element (not even as a substring)."""
    joined = " ".join(str(c) for c in cmd)
    for needle in needles:
        assert needle not in joined, f"{needle!r} leaked into curl argv: {cmd!r}"


class RecordingJSONHandler(http.server.BaseHTTPRequestHandler):
    """A local HTTP server that records the last request it received and
    replies with a fixed JSON body. Subclass and override `response_body`
    (and `captured_*` storage) for a provider-shaped reply."""

    response_body: ClassVar[bytes] = b"{}"
    captured_headers: ClassVar[dict[str, str]] = {}
    captured_body: ClassVar[bytes] = b""

    def _handle(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        type(self).captured_body = self.rfile.read(length)
        type(self).captured_headers = dict(self.headers)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(type(self).response_body)

    def do_GET(self) -> None:
        self._handle()

    def do_POST(self) -> None:
        self._handle()

    def log_message(self, *args: Any) -> None:  # quiet test output
        pass


@contextmanager
def local_server(
    handler_cls: type[http.server.BaseHTTPRequestHandler] = RecordingJSONHandler,
) -> Iterator[str]:
    server = http.server.HTTPServer(("127.0.0.1", 0), handler_cls)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        thread.join(timeout=5)
