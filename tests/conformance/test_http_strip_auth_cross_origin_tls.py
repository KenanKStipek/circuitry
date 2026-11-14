"""`http`'s `redirects: strip_auth` across a real TLS hop (issues #725/#598),
on both engines: a secret header is dropped on a cross-origin redirect and
kept on a same-origin one, a plain `http://` hop is refused, and the secret
reaches no state, events, live state, stdout or stderr.

Two TLS servers on different ports are different origins (`_origin` keys on
`(scheme, host, port)`): one PEM certificate, issued by a throwaway CA this
test generates, covers `localhost`/`127.0.0.1` for both. Both engines trust
that CA through `SSL_CERT_FILE` (`rustls-native-certs` 0.8 and Python's
default `ssl` context both read it, falling back to it exclusively) --
never the system trust store, so the certificate can't be a real one.

Not a `tests/conformance/cases/` case: `harness`'s own `mock_http_fixture`
hosts exactly one plain-HTTP origin, not two independent TLS ones -- same
rationale as `test_user_env_parity.py`/`test_run_cli_parity.py`, whose
`electricity_binary` fixture and sandboxed-environment helpers this module
reuses the pattern of.
"""

from __future__ import annotations

import http.server
import json
import os
import shutil
import ssl
import subprocess
import sys
import threading
from pathlib import Path

import pytest

from . import harness

#: Mirrors `test_run_cli_parity.py`'s own list.
_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)

#: A fake-credential marker, never a real secret -- the same convention
#: `c-http-secret-header`'s own `CONFORMANCE_VALUE_598` uses.
_SECRET_VALUE = "circuitry-test-cross-origin-tls-secret-725"
_SECRET_ENV_VAR = "CONFORMANCE_CROSS_ORIGIN_TLS_SECRET_725"


def _run_openssl(args: list[str], cwd: Path) -> None:
    proc = subprocess.run(
        ["openssl", *args], cwd=cwd, capture_output=True, text=True, timeout=30, check=False,
    )
    assert proc.returncode == 0, f"openssl {args}: {proc.stdout}\n{proc.stderr}"


def _make_ca_and_server_cert(tmp_path: Path) -> tuple[Path, Path, Path]:
    """A throwaway CA, plus one `localhost`/`127.0.0.1` certificate it
    signs -- the same certificate serves both TLS origins below (two
    ports, so still two different origins)."""
    if shutil.which("openssl") is None:
        pytest.skip("openssl not available; cannot generate a throwaway TLS certificate")
    tmp_path.mkdir()
    ca_key = tmp_path / "ca.key"
    ca_crt = tmp_path / "ca.crt"
    server_key = tmp_path / "server.key"
    server_csr = tmp_path / "server.csr"
    server_crt = tmp_path / "server.crt"
    # Both certificates carry the extensions strict X.509 verification
    # wants (Python 3.13+ turns `VERIFY_X509_STRICT` on in its default
    # context): a critical CA basicConstraints and keyUsage on the CA, and
    # an Authority Key Identifier on the server certificate.
    ca_conf = tmp_path / "ca.conf"
    ca_conf.write_text(
        "[req]\ndistinguished_name=req_dn\nx509_extensions=v3_ca\n[req_dn]\n[v3_ca]\n"
        "basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
        "subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n",
        encoding="utf-8",
    )
    san_conf = tmp_path / "san.conf"
    san_conf.write_text(
        "[req]\ndistinguished_name=req\n[san]\n"
        "subjectAltName=DNS:localhost,IP:127.0.0.1\n"
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        "subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n",
        encoding="utf-8",
    )
    _run_openssl(
        [
            "req", "-x509", "-newkey", "rsa:2048", "-keyout", str(ca_key), "-out", str(ca_crt),
            "-days", "2", "-nodes", "-subj", "/CN=circuitry-test-ca-725",
            "-config", str(ca_conf), "-extensions", "v3_ca",
        ],
        tmp_path,
    )
    _run_openssl(
        [
            "req", "-newkey", "rsa:2048", "-keyout", str(server_key), "-out", str(server_csr),
            "-nodes", "-subj", "/CN=localhost",
        ],
        tmp_path,
    )
    _run_openssl(
        [
            "x509", "-req", "-in", str(server_csr), "-CA", str(ca_crt), "-CAkey", str(ca_key),
            "-CAcreateserial", "-out", str(server_crt), "-days", "2",
            "-extfile", str(san_conf), "-extensions", "san",
        ],
        tmp_path,
    )
    return ca_crt, server_crt, server_key


class _RecordingHandler(http.server.BaseHTTPRequestHandler):
    """Records every request's headers under its path, then replies
    according to `self.server.responses[path]` (status, headers, body) --
    a 3xx reply's `Location` drives the next hop."""

    def log_message(self, *args: object) -> None:  # quiet by default
        pass

    def do_GET(self) -> None:
        recorded: dict[str, dict[str, str]] = self.server.recorded  # type: ignore[attr-defined]
        recorded[self.path] = dict(self.headers.items())
        status, headers, body = self.server.responses[self.path]  # type: ignore[attr-defined]
        encoded = body.encode("utf-8")
        self.send_response(status)
        for name, value in headers.items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def _tls_server(
    cert: Path, key: Path, responses: dict[str, tuple[int, dict[str, str], str]]
) -> tuple[http.server.HTTPServer, threading.Thread, dict[str, dict[str, str]]]:
    server = http.server.HTTPServer(("127.0.0.1", 0), _RecordingHandler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certfile=str(cert), keyfile=str(key))
    server.socket = context.wrap_socket(server.socket, server_side=True)
    recorded: dict[str, dict[str, str]] = {}
    server.recorded = recorded  # type: ignore[attr-defined]
    server.responses = responses  # type: ignore[attr-defined]
    request_count = len(responses)

    def serve() -> None:
        for _ in range(request_count):
            server.handle_request()

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return server, thread, recorded


def _sandboxed_env(home_dir: Path, ca_cert: Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    env["SSL_CERT_FILE"] = str(ca_cert)
    env[_SECRET_ENV_VAR] = _SECRET_VALUE
    return env


_ORCHESTRATION = f"""\
interface:
  inputs:
    entry_url: {{type: string, required: true}}
    http_url: {{type: string, required: true}}
runtime:
  secrets:
    github:
      env: {_SECRET_ENV_VAR}
effects:
  - name: cross_origin
    type: tool
    provider: http
    params:
      url: "{{{{input.entry_url}}}}"
      headers:
        Authorization: {{secret: github, prefix: "Bearer "}}
        Accept: application/json
      redirects: strip_auth
  - name: plain_http_hop
    type: tool
    provider: http
    on_error: continue
    params:
      url: "{{{{input.http_url}}}}"
      headers:
        Authorization: {{secret: github, prefix: "Bearer "}}
      redirects: strip_auth
"""


def _write_case(case_dir: Path) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(_ORCHESTRATION, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


def _run_cof(
    case_dir: Path, home_dir: Path, ca_cert: Path, *, entry_url: str, http_url: str,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir)
    cmd = [
        sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml",
        "-e", f"entry_url={entry_url}", "-e", f"http_url={http_url}",
        "--config", "config.json", "--out", "state.json",
        "--live-state", "live.json", "--events", "events.jsonl",
    ]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, ca_cert), capture_output=True, text=True,
        timeout=30, check=False,
    )


def _run_electricity(
    binary: Path, case_dir: Path, home_dir: Path, ca_cert: Path, *, entry_url: str, http_url: str,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir)
    cmd = [
        str(binary), "run", "orchestration.yml",
        "-e", f"entry_url={entry_url}", "-e", f"http_url={http_url}",
        "--config", "config.json", "--out", "state.json",
        "--live-state", "live.json", "--events", "events.jsonl",
    ]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, ca_cert), capture_output=True, text=True,
        timeout=30, check=False,
    )


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    """The shared `electricity` binary -- see `harness.resolve_electricity_binary`."""
    binary, failure = harness.resolve_electricity_binary()
    if failure is not None:
        pytest.fail(failure)
    if binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    return binary


def _assert_no_secret_anywhere(case_dir: Path, result: subprocess.CompletedProcess[str]) -> None:
    assert _SECRET_VALUE not in result.stdout
    assert _SECRET_VALUE not in result.stderr
    for name in ("state.json", "live.json", "events.jsonl"):
        path = case_dir / name
        if path.exists():
            assert _SECRET_VALUE not in path.read_text(encoding="utf-8"), name


def _run_one_engine(
    *, run: subprocess.CompletedProcess[str] | None, case_dir: Path, entry: dict, origin_b: dict,
) -> None:
    assert run is not None
    assert run.returncode == 0, run.stderr
    _assert_no_secret_anywhere(case_dir, run)
    state = json.loads((case_dir / "state.json").read_text(encoding="utf-8"))
    cross_origin = state["prime"]["cross_origin"]
    assert cross_origin["meta"]["error"] is None, cross_origin
    assert cross_origin["meta"]["status_code"] == 200, cross_origin
    assert cross_origin["value"] == {"ok": True}, cross_origin

    # The same-origin hop (entry -> /same, same host and port) kept the
    # secret header; the cross-origin hop (/same -> origin B's /dest, a
    # different port) dropped it, keeping the plain `Accept` header.
    same_origin_headers = {k.lower(): v for k, v in entry["/same"].items()}
    assert "authorization" in same_origin_headers, entry["/same"]
    cross_origin_headers = {k.lower(): v for k, v in origin_b["/dest"].items()}
    assert "authorization" not in cross_origin_headers, origin_b["/dest"]
    assert cross_origin_headers.get("accept") == "application/json", origin_b["/dest"]

    plain_http_hop = state["prime"]["plain_http_hop"]
    assert plain_http_hop["value"] is None
    assert "redirects: strip_auth follows https only" in (
        plain_http_hop["meta"]["error"] or ""
    ), plain_http_hop


def test_strip_auth_drops_the_secret_header_only_on_the_cross_origin_hop(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    ca_cert, server_cert, server_key = _make_ca_and_server_cert(tmp_path / "ca")

    origin_a_responses = {
        "/entry": (302, {"Location": "/same"}, ""),
        "/same": (302, {"Location": ""}, ""),  # filled in once origin B's port is known
        "/to-http": (301, {"Location": "http://127.0.0.1:1/never-reached"}, ""),
    }
    origin_b_responses = {
        "/dest": (200, {"Content-Type": "application/json"}, '{"ok": true}'),
    }
    server_b, thread_b, recorded_b = _tls_server(server_cert, server_key, origin_b_responses)
    port_b = server_b.server_address[1]
    origin_a_responses["/same"] = (302, {"Location": f"https://127.0.0.1:{port_b}/dest"}, "")
    server_a, thread_a, recorded_a = _tls_server(server_cert, server_key, origin_a_responses)
    port_a = server_a.server_address[1]

    entry_url = f"https://127.0.0.1:{port_a}/entry"
    http_url = f"https://127.0.0.1:{port_a}/to-http"

    try:
        cof_result = _run_cof(
            tmp_path / "cof_case", tmp_path / "cof_home", ca_cert,
            entry_url=entry_url, http_url=http_url,
        )
    finally:
        thread_a.join(timeout=10)
        thread_b.join(timeout=10)
        server_a.server_close()
        server_b.server_close()

    _run_one_engine(
        run=cof_result, case_dir=tmp_path / "cof_case", entry=recorded_a, origin_b=recorded_b,
    )

    # A fresh pair of servers for electricity's own run -- the first
    # pair's servers each answered their own fixed request count already.
    server_b2, thread_b2, recorded_b2 = _tls_server(server_cert, server_key, origin_b_responses)
    port_b2 = server_b2.server_address[1]
    origin_a_responses["/same"] = (302, {"Location": f"https://127.0.0.1:{port_b2}/dest"}, "")
    server_a2, thread_a2, recorded_a2 = _tls_server(server_cert, server_key, origin_a_responses)
    port_a2 = server_a2.server_address[1]
    entry_url2 = f"https://127.0.0.1:{port_a2}/entry"
    http_url2 = f"https://127.0.0.1:{port_a2}/to-http"

    try:
        electricity_result = _run_electricity(
            electricity_binary, tmp_path / "electricity_case", tmp_path / "electricity_home",
            ca_cert, entry_url=entry_url2, http_url=http_url2,
        )
    finally:
        thread_a2.join(timeout=10)
        thread_b2.join(timeout=10)
        server_a2.server_close()
        server_b2.server_close()

    _run_one_engine(
        run=electricity_result, case_dir=tmp_path / "electricity_case", entry=recorded_a2,
        origin_b=recorded_b2,
    )
