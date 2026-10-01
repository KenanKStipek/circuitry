"""Shared helpers for tests that inspect a `run_curl` invocation.

The URL and headers never reach argv: they travel through a pipe fd passed
via `--config /dev/fd/<n>` and `pass_fds`. A test's fake `subprocess.run`
can read that fd's content synchronously (before `run_curl`'s `finally`
closes it) to recover the URL/headers a call actually sent, without ever
seeing them in `cmd`.
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
_URL_LINE_RE = re.compile(r'^url = "((?:[^"\\]|\\.)*)"$')


def _unescape_config_value(value: str) -> str:
    return value.replace('\\"', '"').replace("\\\\", "\\")


def _read_config_text(cmd: list[str]) -> str:
    """The `--config` file content `cmd` points at — a POSIX `/dev/fd/<n>`
    path (read synchronously off the still-open pipe) or a plain file path
    (the Windows temp-file path). A pipe is not seekable, so this can only
    be read once per call — use :func:`read_config` when a test needs both
    the URL and the headers from the same `cmd`."""
    idx = cmd.index("--config")
    target = cmd[idx + 1]
    if target.startswith("/dev/fd/"):
        fd = int(target.rsplit("/", 1)[-1])
        return os.read(fd, 65536).decode()
    with open(target, encoding="utf-8") as f:
        return f.read()


def read_config(cmd: list[str]) -> tuple[str | None, dict[str, str]]:
    """The URL and headers sent via `--config <fd-path>` in `cmd`, read in
    one pass. Use this (not `read_config_url` *and* `read_config_headers`)
    when a test needs both from the same `cmd`: the POSIX target is a pipe,
    and a second read after the first would drain nothing."""
    try:
        raw = _read_config_text(cmd)
    except ValueError:
        return None, {}
    url: str | None = None
    headers: dict[str, str] = {}
    for line in raw.splitlines():
        url_match = _URL_LINE_RE.match(line)
        if url_match:
            url = _unescape_config_value(url_match.group(1))
            continue
        header_match = _HEADER_LINE_RE.match(line)
        if header_match:
            name, _, value = _unescape_config_value(header_match.group(1)).partition(": ")
            headers[name] = value
    return url, headers


def read_config_headers(cmd: list[str]) -> dict[str, str]:
    """Headers sent via `--config <fd-path>` in `cmd`, or `{}` if none."""
    _, headers = read_config(cmd)
    return headers


def read_config_url(cmd: list[str]) -> str | None:
    """The URL sent via `--config <fd-path>` in `cmd` (#314: the URL travels
    through the config file, never as a bare argv element), or `None` if
    there's no `--config` in `cmd` at all."""
    url, _ = read_config(cmd)
    return url


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
