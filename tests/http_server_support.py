"""A local HTTP server pair for the ``http`` tool's tests: an ``http``
server and an ``https`` one on another port (a throwaway self-signed
certificate the client trusts for one test only), serving the same routes and
recording the headers each request arrived with. Never the network.
"""

from __future__ import annotations

import http.server
import shutil
import ssl
import subprocess
import threading
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

PROXY_ENV_VARS = ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "all_proxy", "ALL_PROXY")


@dataclass
class Route:
    status: int = 200
    headers: dict[str, str] = field(default_factory=dict)
    body: str | bytes = "ok"
    #: The Content-Length sent: the body's own length when ``None``, and none
    #: at all with ``omit_content_length`` (the body then ends at the close).
    content_length: int | None = None
    omit_content_length: bool = False


@dataclass
class Received:
    server: str
    method: str
    path: str
    headers: dict[str, str]


class _Handler(http.server.BaseHTTPRequestHandler):
    def _serve(self) -> None:
        server: Any = self.server
        server.received.append(
            Received(
                server=server.label,
                method=self.command,
                path=self.path,
                headers={k.lower(): v for k, v in self.headers.items()},
            )
        )
        route = server.routes.get(self.path, Route(status=404, body="no route"))
        body = route.body.encode() if isinstance(route.body, str) else route.body
        self.send_response(route.status)
        for name, value in route.headers.items():
            self.send_header(name, value)
        if not route.omit_content_length:
            length = len(body) if route.content_length is None else route.content_length
            self.send_header("Content-Length", str(length))
        self.end_headers()
        self.wfile.write(body)

    do_GET = _serve
    do_POST = _serve

    def log_message(self, format: str, *args: Any) -> None:
        pass


@dataclass
class ServerPair:
    """``http://127.0.0.1:<http_port>`` and ``https://127.0.0.1:<https_port>``
    (``localhost`` is the same server under another host name), serving
    :attr:`routes` and recording :attr:`received`."""

    http_port: int
    https_port: int
    routes: dict[str, Route]
    received: list[Received]

    @property
    def http(self) -> str:
        return f"http://127.0.0.1:{self.http_port}"

    @property
    def https(self) -> str:
        return f"https://127.0.0.1:{self.https_port}"

    @property
    def https_localhost(self) -> str:
        return f"https://localhost:{self.https_port}"

    def arrived(self, path: str) -> Received:
        (match,) = [request for request in self.received if request.path == path]
        return match


_OPENSSL_CONFIG = """
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = 127.0.0.1
[v3]
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature,keyCertSign
extendedKeyUsage = serverAuth
subjectAltName = IP:127.0.0.1,DNS:localhost
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always
"""


def _self_signed_cert(directory: Path) -> tuple[Path, Path]:
    openssl = shutil.which("openssl")
    if openssl is None:
        pytest.skip("openssl is needed to make the test server's certificate")
    config = directory / "openssl.cnf"
    config.write_text(_OPENSSL_CONFIG, encoding="utf-8")
    cert, key = directory / "cert.pem", directory / "key.pem"
    subprocess.run(
        [openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-keyout", str(key), "-out", str(cert), "-config", str(config)],
        check=True,
        capture_output=True,
    )
    return cert, key


def serve_pair(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[ServerPair]:
    """Start the pair for one test (a fixture's body: ``yield from``)."""
    for name in PROXY_ENV_VARS:
        monkeypatch.delenv(name, raising=False)
    cert_dir = tmp_path / "tls"
    cert_dir.mkdir()
    cert, key = _self_signed_cert(cert_dir)
    routes: dict[str, Route] = {}
    received: list[Received] = []

    plain = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    secure = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    server_context.load_cert_chain(cert, key)
    secure.socket = server_context.wrap_socket(secure.socket, server_side=True)
    for server, label in ((plain, "http"), (secure, "https")):
        server.label = label  # type: ignore[attr-defined]
        server.routes = routes  # type: ignore[attr-defined]
        server.received = received  # type: ignore[attr-defined]
    threads = [
        threading.Thread(target=server.serve_forever, daemon=True) for server in (plain, secure)
    ]
    for thread in threads:
        thread.start()

    # The client trusts the test certificate, and only for this test. From
    # Python 3.12 an HTTPS handler takes its context when it is built, and
    # `urlopen` keeps the opener it built first, so that one is dropped too.
    client_context = ssl.create_default_context(cafile=str(cert))
    monkeypatch.setattr(ssl, "_create_default_https_context", lambda: client_context)
    monkeypatch.setattr(urllib.request, "_opener", None)
    try:
        yield ServerPair(
            http_port=plain.server_address[1],
            https_port=secure.server_address[1],
            routes=routes,
            received=received,
        )
    finally:
        for server in (plain, secure):
            server.shutdown()
            server.server_close()
