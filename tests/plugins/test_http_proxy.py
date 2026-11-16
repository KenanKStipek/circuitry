"""The ``http`` tool's own proxy rule (issue #778 step 7, H1): one
convention, decided by Circuitry itself -- never urllib's or curl's own
environment reading. Every case sets or unsets every proxy variable
explicitly; nothing here depends on the real process environment.
"""

from __future__ import annotations

import base64
import contextlib
import socket
import threading
from collections.abc import Iterator
from pathlib import Path

import pytest
from http_server_support import PROXY_ENV_VARS, Route, ServerPair, serve_pair

from circuitry.plugins.http import HttpPlugin


@pytest.fixture
def servers(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[ServerPair]:
    yield from serve_pair(tmp_path, monkeypatch)


def _clear_proxy_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (*PROXY_ENV_VARS, "no_proxy", "NO_PROXY"):
        monkeypatch.delenv(name, raising=False)


class _RecordingProxy:
    """A forward proxy on 127.0.0.1: a plain (non-CONNECT) request is
    answered directly, recording its absolute-form request line and
    headers; a ``CONNECT`` request tunnels to the real host:port named in
    it, recording the target and any ``Proxy-Authorization`` header."""

    def __init__(self) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(8)
        self._sock.settimeout(0.2)
        self.port = self._sock.getsockname()[1]
        self.requests: list[dict[str, str]] = []
        self.connect_targets: list[str] = []
        self.connect_proxy_auth: list[str | None] = []
        #: When set, the next plain (non-``CONNECT``) request gets a 302
        #: to this URL instead of the canned ``{"via": "proxy"}`` body --
        #: for a redirect-through-the-proxy test (#778 step 7, H-review
        #: P1-8) that needs the proxy itself to answer the first hop.
        self.redirect_to: str | None = None
        self._stop = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        while not self._stop:
            try:
                conn, _ = self._sock.accept()
            except OSError:
                continue
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn: socket.socket) -> None:
        conn.settimeout(5)
        data = b""
        try:
            while b"\r\n\r\n" not in data:
                chunk = conn.recv(4096)
                if not chunk:
                    conn.close()
                    return
                data += chunk
        except OSError:
            conn.close()
            return
        head, _, _rest = data.partition(b"\r\n\r\n")
        lines = head.decode("latin-1").split("\r\n")
        request_line = lines[0]
        header_lines = lines[1:]
        headers = {
            part.split(":", 1)[0].strip().lower(): part.split(":", 1)[1].strip()
            for part in header_lines
            if ":" in part
        }
        method = request_line.split(" ", 1)[0]
        if method == "CONNECT":
            target = request_line.split(" ")[1]
            self.connect_targets.append(target)
            self.connect_proxy_auth.append(headers.get("proxy-authorization"))
            host, _, port_text = target.partition(":")
            try:
                upstream = socket.create_connection((host, int(port_text)), timeout=5)
            except OSError:
                with contextlib.suppress(OSError):
                    conn.sendall(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                conn.close()
                return
            conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            self._splice(conn, upstream)
            return
        self.requests.append({"request_line": request_line, **headers})
        if self.redirect_to is not None:
            response = (
                b"HTTP/1.1 302 Found\r\n"
                b"Location: " + self.redirect_to.encode() + b"\r\n"
                b"Content-Length: 0\r\n\r\n"
            )
            with contextlib.suppress(OSError):
                conn.sendall(response)
            conn.close()
            return
        body = b'{"via": "proxy"}'
        response = (
            b"HTTP/1.1 200 OK\r\n"
            b"Content-Type: application/json\r\n"
            b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body
        )
        with contextlib.suppress(OSError):
            conn.sendall(response)
        conn.close()

    @staticmethod
    def _splice(a: socket.socket, b: socket.socket) -> None:
        def forward(src: socket.socket, dst: socket.socket) -> None:
            try:
                while True:
                    chunk = src.recv(4096)
                    if not chunk:
                        break
                    dst.sendall(chunk)
            except OSError:
                pass
            finally:
                with contextlib.suppress(OSError):
                    dst.shutdown(socket.SHUT_WR)

        t1 = threading.Thread(target=forward, args=(a, b), daemon=True)
        t2 = threading.Thread(target=forward, args=(b, a), daemon=True)
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)
        with contextlib.suppress(OSError):
            a.close()
        with contextlib.suppress(OSError):
            b.close()

    def shutdown(self) -> None:
        self._stop = True
        self._sock.close()
        self._thread.join(timeout=2)


@pytest.fixture
def proxy() -> Iterator[_RecordingProxy]:
    server = _RecordingProxy()
    try:
        yield server
    finally:
        server.shutdown()


# ---------------------------------------------------------------------------
# http_proxy / no_proxy (plain http, through the recording proxy itself)
# ---------------------------------------------------------------------------


def test_http_proxy_is_used_for_an_http_request(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("http_proxy", f"http://127.0.0.1:{proxy.port}")

    result = HttpPlugin().execute(params={"url": "http://example.invalid/widgets"})

    assert result.raw["status"] == 200
    assert len(proxy.requests) == 1
    assert proxy.requests[0]["request_line"] == "GET http://example.invalid/widgets HTTP/1.1"


def test_uppercase_http_proxy_is_never_read(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy, servers: ServerPair
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("HTTP_PROXY", f"http://127.0.0.1:{proxy.port}")
    servers.routes["/"] = Route(status=200, body="ok")

    result = HttpPlugin().execute(params={"url": servers.http + "/"})

    assert result.raw["status"] == 200
    assert proxy.requests == []
    assert servers.arrived("/")


def test_lowercase_http_proxy_wins_over_uppercase(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("HTTP_PROXY", "http://127.0.0.1:1")
    monkeypatch.setenv("http_proxy", f"http://127.0.0.1:{proxy.port}")

    result = HttpPlugin().execute(params={"url": "http://example.invalid/widgets"})

    assert result.raw["status"] == 200
    assert len(proxy.requests) == 1


def test_no_proxy_bypasses_a_matching_host(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy, servers: ServerPair
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("http_proxy", f"http://127.0.0.1:{proxy.port}")
    monkeypatch.setenv("no_proxy", "127.0.0.1")
    servers.routes["/"] = Route(status=200, body="ok")

    result = HttpPlugin().execute(params={"url": servers.http + "/"})

    assert result.raw["status"] == 200
    assert proxy.requests == []
    assert servers.arrived("/")


def test_a_schemeless_proxy_value_is_used_as_http(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("http_proxy", f"127.0.0.1:{proxy.port}")

    result = HttpPlugin().execute(params={"url": "http://example.invalid/widgets"})

    assert result.raw["status"] == 200
    assert len(proxy.requests) == 1


def test_all_proxy_is_the_fallback_for_both_schemes(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("all_proxy", f"http://127.0.0.1:{proxy.port}")

    result = HttpPlugin().execute(params={"url": "http://example.invalid/widgets"})

    assert result.raw["status"] == 200
    assert len(proxy.requests) == 1


def test_no_proxy_does_not_apply_without_a_matching_entry(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("http_proxy", f"http://127.0.0.1:{proxy.port}")
    monkeypatch.setenv("no_proxy", "somewhere-else.example")

    result = HttpPlugin().execute(params={"url": "http://example.invalid/widgets"})

    assert result.raw["status"] == 200
    assert len(proxy.requests) == 1


# ---------------------------------------------------------------------------
# https through a CONNECT-tunneling proxy, userinfo as Basic proxy auth
# ---------------------------------------------------------------------------


def test_https_request_is_tunneled_through_the_proxy_with_basic_auth(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy, servers: ServerPair
) -> None:
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv(
        "https_proxy", f"http://alice:s3cr3t@127.0.0.1:{proxy.port}"
    )
    servers.routes["/"] = Route(status=200, body="ok")

    result = HttpPlugin().execute(params={"url": servers.https_localhost + "/"})

    assert result.raw["status"] == 200
    assert servers.arrived("/")
    assert len(proxy.connect_targets) == 1
    assert proxy.connect_targets[0] == f"localhost:{servers.https_port}"
    expected = "Basic " + base64.b64encode(b"alice:s3cr3t").decode("ascii")
    assert proxy.connect_proxy_auth[0] == expected


def test_proxy_credentials_do_not_leak_to_a_redirect_target_that_is_not_proxied(
    monkeypatch: pytest.MonkeyPatch, proxy: _RecordingProxy, servers: ServerPair
) -> None:
    """Issue #778 step 7, H-review P1-8: only ``http_proxy`` is set, so a
    redirect from a proxied ``http://`` URL to an ``https://`` one (no
    ``https_proxy``/``all_proxy``) must not carry the first hop's
    ``Proxy-authorization`` -- the second hop is unproxied entirely, so the
    proxy never even sees a ``CONNECT`` for it, let alone the credential.
    """
    _clear_proxy_env(monkeypatch)
    monkeypatch.setenv("http_proxy", f"http://alice:s3cr3t@127.0.0.1:{proxy.port}")
    servers.routes["/"] = Route(status=200, body="ok")
    proxy.redirect_to = servers.https_localhost + "/"

    result = HttpPlugin().execute(params={"url": "http://example.invalid/redirect"})

    assert result.raw["status"] == 200
    assert servers.arrived("/")
    assert len(proxy.requests) == 1
    assert proxy.connect_targets == []
    assert proxy.connect_proxy_auth == []


# ---------------------------------------------------------------------------
# Closed ports: strict port / scheme refusals (H3/H4), before any I/O.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "url",
    ["http://127.0.0.1:99999/x", "http://127.0.0.1:0/x", "http://127.0.0.1:+9/x"],
)
def test_strict_port_is_refused_before_any_io(
    monkeypatch: pytest.MonkeyPatch, url: str
) -> None:
    _clear_proxy_env(monkeypatch)
    with pytest.raises(ValueError, match="invalid port"):
        HttpPlugin().execute(params={"url": url})


@pytest.mark.parametrize("url", ["file:///etc/passwd", "ftp://127.0.0.1/x"])
def test_unsupported_scheme_is_refused_before_any_io(
    monkeypatch: pytest.MonkeyPatch, url: str
) -> None:
    _clear_proxy_env(monkeypatch)
    with pytest.raises(ValueError, match="unsupported URL scheme"):
        HttpPlugin().execute(params={"url": url})


def test_a_closed_local_port_fails_as_a_connection_error_not_a_hang(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _clear_proxy_env(monkeypatch)
    # Port 1 is a reserved, almost always closed port on loopback.
    with pytest.raises(RuntimeError, match="HTTP request to"):
        HttpPlugin().execute(params={"url": "http://127.0.0.1:1/x"}, timeout_seconds=3)
