"""Secrets for ``http`` headers and subprocess tools' host ``env``, and
``http``'s ``redirects`` (#598).

Nothing here reads the real Keychain: a fake ``security`` binary first on
``PATH`` stands in for it. Redirects are checked against a local server pair:
an ``http`` server and an ``https`` one on another port, serving the same
routes, both recording the headers each request arrived with.
"""

from __future__ import annotations

import hashlib
import http.server
import json
import os
import shutil
import ssl
import subprocess
import sys
import threading
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.cli.runtime_shim import RunRequest, run, validate
from circuitry.plugins._subprocess import plugin_env_override
from circuitry.plugins.http import HttpPlugin
from circuitry.secrets import SecretError, SecretRef, SecretValues, resolve_secret

TOKEN = "s3cr3t-token-598-abcdef"
TOKEN_SHA256 = hashlib.sha256(TOKEN.encode()).hexdigest()

_CREDENTIAL_ENV_VARS = ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")
_PROXY_ENV_VARS = ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "all_proxy", "ALL_PROXY")


# ---------------------------------------------------------------------------
# A fake `security` binary and a recording server pair.
# ---------------------------------------------------------------------------


@pytest.fixture
def fake_bin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A directory first on PATH holding a fake ``security``: for
    ``find-generic-password -w -s svc [-a acct]`` it prints TOKEN when the
    service is ``svc`` (and the account, if given, is ``acct``), and exits 44
    otherwise, like the real one for a missing item. Each call's argv is
    appended to ``security.log``."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    script = bin_dir / "security"
    script.write_text(
        f"""#!{sys.executable}
import sys
from pathlib import Path
Path({str(bin_dir / "security.log")!r}).open("a").write(" ".join(sys.argv[1:]) + "\\n")
args = sys.argv[1:]
service = args[args.index("-s") + 1] if "-s" in args else None
account = args[args.index("-a") + 1] if "-a" in args else "acct"
if args[:2] == ["find-generic-password", "-w"] and service == "svc" and account == "acct":
    print({TOKEN!r})
    sys.exit(0)
print("security: SecKeychainSearchCopyNext: The specified item could not be found.", file=sys.stderr)
sys.exit(44)
""",
        encoding="utf-8",
    )
    script.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    return bin_dir


@dataclass
class Route:
    status: int = 200
    headers: dict[str, str] = field(default_factory=dict)
    body: str = "ok"


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
        body = route.body.encode()
        self.send_response(route.status)
        for name, value in route.headers.items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
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


@pytest.fixture
def servers(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[ServerPair]:
    for name in _PROXY_ENV_VARS:
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


# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content.lstrip(), encoding="utf-8")
    return path


def _config(secrets: dict[str, Any], **runtime: Any) -> Any:
    from circuitry.cli.config import CircuitryConfig

    return CircuitryConfig(runtime={"secrets": secrets, **runtime})


_KEYCHAIN = {"api": {"keychain": {"service": "svc", "account": "acct"}}}


def _run(path: Path, *, secrets: dict[str, Any] | None = None, trusted: bool = True, **runtime: Any) -> Any:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=_config(_KEYCHAIN if secrets is None else secrets, **runtime),
            skip_preflight=True,
            trust_document=trusted,
        )
    )


def _http_doc(tmp_path: Path, url: str, *, extra_params: str = "", on_error: str = "fail") -> Path:
    return _write(
        tmp_path,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: call
    provider: http
    on_error: {on_error}
    params:
      url: "{url}"
      headers:
        Authorization: {{secret: api, prefix: "Bearer "}}
        X-Api-Key: {{secret: api}}
        X-Plain: visible
{extra_params}
""",
    )


def _assert_no_token(text: str) -> None:
    assert TOKEN not in text
    assert TOKEN_SHA256 not in text


# ---------------------------------------------------------------------------
# Resolution.
# ---------------------------------------------------------------------------


def test_a_keychain_secret_is_read_with_security(fake_bin: Path) -> None:
    runtime = {"secrets": _KEYCHAIN}
    assert resolve_secret("api", runtime) == TOKEN
    assert (fake_bin / "security.log").read_text() == "find-generic-password -w -s svc -a acct\n"


def test_a_keychain_secret_without_an_account_omits_it(fake_bin: Path) -> None:
    assert resolve_secret("api", {"secrets": {"api": {"keychain": {"service": "svc"}}}}) == TOKEN
    assert (fake_bin / "security.log").read_text() == "find-generic-password -w -s svc\n"


def test_a_command_secret_is_its_trimmed_stdout(tmp_path: Path) -> None:
    command = [sys.executable, "-c", f"print('  {TOKEN}  ')"]
    assert resolve_secret("cli", {"secrets": {"cli": {"command": command}}}) == TOKEN


def test_an_env_secret_reads_the_variable(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CIRCUITRY_TEST_SECRET_598", TOKEN)
    assert resolve_secret("e", {"secrets": {"e": {"env": "CIRCUITRY_TEST_SECRET_598"}}}) == TOKEN


def test_a_failing_command_secret_names_the_secret_and_never_its_output() -> None:
    command = [
        sys.executable,
        "-c",
        f"import sys; print({TOKEN!r}); print({TOKEN!r}, file=sys.stderr); sys.exit(3)",
    ]
    with pytest.raises(SecretError) as caught:
        resolve_secret("cli", {"secrets": {"cli": {"command": command}}})
    assert str(caught.value) == "secret 'cli': its command exited with status 3."
    _assert_no_token(repr(caught.value))


def test_a_missing_keychain_item_names_the_secret(fake_bin: Path) -> None:
    with pytest.raises(SecretError, match=r"^secret 'api': the Keychain lookup exited with status 44\.$"):
        resolve_secret("api", {"secrets": {"api": {"keychain": {"service": "other"}}}})


@pytest.mark.parametrize(
    ("secrets", "message"),
    [
        ({}, "secret 'x' is not defined in runtime.secrets."),
        ({"x": {"env": "A", "command": ["b"]}}, "runtime.secrets.x must be exactly one of"),
        ({"x": {"vault": "y"}}, "runtime.secrets.x must be exactly one of"),
        ({"x": {"command": []}}, "runtime.secrets.x.command must be a non-empty list of strings."),
        ({"x": {"keychain": {"account": "a"}}}, "runtime.secrets.x.keychain must be"),
        ({"x": {"env": "CIRCUITRY_TEST_UNSET_598"}}, "environment variable 'CIRCUITRY_TEST_UNSET_598' is not set"),
        ({"x": {"command": [sys.executable, "-c", "pass"]}}, "secret 'x' resolved to an empty value."),
        ({"x": {"command": [sys.executable, "-c", "print('a\\nb')"]}}, "holds a line break"),
        ({"x": {"command": ["circuitry-no-such-binary-598"]}}, "could not start"),
    ],
)
def test_resolution_errors_name_the_secret(secrets: dict[str, Any], message: str) -> None:
    with pytest.raises(SecretError) as caught:
        resolve_secret("x", {"secrets": secrets})
    assert message in str(caught.value)


def test_scrub_replaces_prefixed_and_bare_values() -> None:
    values = SecretValues()
    runtime = {"secrets": {"api": {"command": [sys.executable, "-c", f"print({TOKEN!r})"]}}}
    assert values.resolve(SecretRef("api", "Bearer "), runtime) == f"Bearer {TOKEN}"
    assert values.scrub({"k": [f"Bearer {TOKEN} and {TOKEN}", 1], TOKEN: None}) == {
        "k": ["Bearer [secret:api] and [secret:api]", 1],
        "[secret:api]": None,
    }


def test_a_secret_in_host_env_is_left_out_of_a_plugin_built_for_its_check() -> None:
    cfg = {"env": {"A": "1", "TOKEN": {"secret": "tok"}}}
    assert plugin_env_override(cfg, plugin_name="awk") == {"A": "1"}


# ---------------------------------------------------------------------------
# http: the header, the records, the redirects.
# ---------------------------------------------------------------------------


def test_a_keychain_secret_reaches_the_header_and_no_record(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    """Acceptance 1, end to end through `cof run`: the header arrives; the
    `--out` state, the events file and the CLI's own output carry neither the
    value nor its hash."""
    servers.routes["/data"] = Route(body="fine")
    doc = _http_doc(tmp_path, f"{servers.http}/data")
    config = _write(tmp_path, "config.json", json.dumps({"runtime": {"secrets": _KEYCHAIN}}))
    out, events = tmp_path / "out.json", tmp_path / "events.jsonl"
    home = tmp_path / "home"
    home.mkdir()
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in _CREDENTIAL_ENV_VARS and key not in _PROXY_ENV_VARS
    }
    env["HOME"] = str(home)
    proc = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", str(doc), "--config", str(config),
         "--out", str(out), "--events", str(events), "--verbose"],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr

    arrived = servers.arrived("/data")
    assert arrived.headers["authorization"] == f"Bearer {TOKEN}"
    assert arrived.headers["x-api-key"] == TOKEN
    state = json.loads(out.read_text())
    assert state["prime"]["call"]["meta"]["params_rendered"]["headers"] == {
        "Authorization": "Bearer [secret:api]",
        "X-Api-Key": "[secret:api]",
        "X-Plain": "visible",
    }
    for text in (out.read_text(), events.read_text(), proc.stdout, proc.stderr):
        _assert_no_token(text)
    for path in home.rglob("*"):
        if path.is_file():
            _assert_no_token(path.read_text(errors="replace"))


def test_a_302_to_another_host_is_followed_without_the_secret_headers(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    """Acceptance 2. The default is strip_auth, since a header came from a
    secret."""
    servers.routes["/start"] = Route(302, {"Location": f"{servers.https_localhost}/landing"})
    servers.routes["/landing"] = Route(body="landed")
    result = _run(_http_doc(tmp_path, f"{servers.https}/start"))
    assert result.ok, result.error
    assert result.state["prime"]["call"]["value"] == "landed"
    assert servers.arrived("/start").headers["authorization"] == f"Bearer {TOKEN}"
    landing = servers.arrived("/landing").headers
    assert landing["host"] == f"localhost:{servers.https_port}"
    assert "authorization" not in landing
    assert "x-api-key" not in landing
    assert landing["x-plain"] == "visible"


def test_a_redirect_to_another_port_strips_the_secret_headers(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    servers.routes["/start"] = Route(302, {"Location": f"{servers.https}/landing"})
    servers.routes["/landing"] = Route(body="landed")
    # From the http port to the https one: another scheme and port.
    result = _run(
        _http_doc(tmp_path, f"{servers.http}/start", extra_params="      redirects: strip_auth")
    )
    assert result.ok, result.error
    landing = servers.arrived("/landing")
    assert landing.server == "https"
    assert "authorization" not in landing.headers
    assert "x-api-key" not in landing.headers


def test_a_same_origin_redirect_keeps_the_secret_headers(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    servers.routes["/start"] = Route(301, {"Location": "/landing"})
    servers.routes["/landing"] = Route(body="landed")
    result = _run(_http_doc(tmp_path, f"{servers.https}/start"))
    assert result.ok, result.error
    landing = servers.arrived("/landing").headers
    assert landing["authorization"] == f"Bearer {TOKEN}"
    assert landing["x-api-key"] == TOKEN


def test_strip_auth_never_follows_a_redirect_to_http(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    servers.routes["/start"] = Route(302, {"Location": f"{servers.http}/landing"}, body="")
    result = _run(_http_doc(tmp_path, f"{servers.https}/start", on_error="continue"))
    assert result.ok, result.error
    meta = result.state["prime"]["call"]["meta"]
    assert meta["status_code"] == 302
    assert meta["error"] == "HTTP 302: Found: redirect not followed (redirects: strip_auth follows https only)"
    assert [request.path for request in servers.received] == ["/start"]


def test_redirects_never_fails_on_a_3xx(tmp_path: Path, fake_bin: Path, servers: ServerPair) -> None:
    """Acceptance 3, with no secret involved; fail_on_error: false turns it
    back into a success, as for a 4xx."""
    servers.routes["/start"] = Route(302, {"Location": "/landing"}, body="moved")
    servers.routes["/landing"] = Route(body="landed")
    doc = _write(
        tmp_path,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: strict
    provider: http
    on_error: continue
    params: {{url: "{servers.http}/start", redirects: never}}
  - type: tool
    name: lenient
    provider: http
    params: {{url: "{servers.http}/start", redirects: never, fail_on_error: false}}
""",
    )
    result = _run(doc)
    assert result.ok, result.error
    strict = result.state["prime"]["strict"]
    assert strict["meta"]["error"] == "HTTP 302: Found: redirect not followed (redirects: never): moved"
    lenient = result.state["prime"]["lenient"]
    assert lenient["meta"]["error"] is None
    assert lenient["meta"]["status_code"] == 302
    assert lenient["value"] == "moved"
    assert [request.path for request in servers.received] == ["/start", "/start"]


def test_follow_resends_the_headers_when_the_document_asks_for_it(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    servers.routes["/start"] = Route(302, {"Location": f"{servers.https_localhost}/landing"})
    servers.routes["/landing"] = Route(body="landed")
    result = _run(_http_doc(tmp_path, f"{servers.https}/start", extra_params="      redirects: follow"))
    assert result.ok, result.error
    assert servers.arrived("/landing").headers["authorization"] == f"Bearer {TOKEN}"


def test_an_error_body_that_echoes_the_token_is_redacted(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    """Acceptance 4."""
    servers.routes["/fail"] = Route(
        401,
        {"Content-Type": "application/json", "X-Echo": TOKEN},
        json.dumps({"error": f"bad credentials: Bearer {TOKEN}", "seen": TOKEN}),
    )
    result = run(
        RunRequest(
            orchestration_path=_http_doc(tmp_path, f"{servers.http}/fail"),
            state_path=None,
            out_path=None,
            events_path=tmp_path / "events.jsonl",
            dry_run=False,
            validate_only=False,
            config=_config(_KEYCHAIN),
            skip_preflight=True,
            trust_document=True,
        )
    )
    assert not result.ok
    meta = result.state["prime"]["call"]["meta"]
    assert meta["error"] == "HTTP 401: Unauthorized: bad credentials: Bearer [secret:api]"
    assert meta["raw"]["headers"]["x-echo"] == "[secret:api]"
    assert json.loads(meta["raw"]["body"])["seen"] == "[secret:api]"
    _assert_no_token(result.error or "")
    for text in (json.dumps(result.state), (tmp_path / "events.jsonl").read_text()):
        _assert_no_token(text)


def test_a_token_that_straddles_the_error_excerpts_cut_leaves_no_part_behind(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    # The excerpt keeps a body's first 500 characters; the token starts at 490.
    servers.routes["/fail"] = Route(400, {"Content-Type": "text/plain"}, "x" * 490 + TOKEN)
    result = run(
        RunRequest(
            orchestration_path=_http_doc(tmp_path, f"{servers.http}/fail"),
            state_path=None,
            out_path=None,
            events_path=tmp_path / "events.jsonl",
            dry_run=False,
            validate_only=False,
            config=_config(_KEYCHAIN),
            skip_preflight=True,
            trust_document=True,
        )
    )
    assert not result.ok
    meta = result.state["prime"]["call"]["meta"]
    assert meta["error"] == "HTTP 400: Bad Request: " + "x" * 490 + "[secret:ap"
    for text in (
        json.dumps(result.state),
        (tmp_path / "events.jsonl").read_text(),
        result.error or "",
    ):
        assert TOKEN[:10] not in text


def test_a_failing_command_secret_fails_the_effect_naming_it(
    tmp_path: Path, servers: ServerPair
) -> None:
    """Acceptance 6: no request is sent, and the error names the secret."""
    token_file = tmp_path / "token.txt"
    token_file.write_text(TOKEN, encoding="utf-8")
    command = [
        sys.executable,
        "-c",
        (
            "import sys; text = open(sys.argv[1]).read(); print(text);"
            " print(text, file=sys.stderr); raise SystemExit(2)"
        ),
        str(token_file),
    ]
    result = _run(
        _http_doc(tmp_path, f"{servers.http}/data"),
        secrets={"api": {"command": command}},
    )
    assert not result.ok
    assert result.state["prime"]["call"]["meta"]["error"] == (
        "secret 'api': its command exited with status 2."
    )
    _assert_no_token(json.dumps(result.state) + (result.error or ""))
    assert servers.received == []


def test_a_limited_document_naming_a_secret_is_refused_before_it_runs(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    """Acceptance 5: nothing runs, not even the effect before it, and the
    Keychain is never asked."""
    servers.routes["/first"] = Route(body="first")
    doc = _write(
        tmp_path,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: first
    provider: http
    params: {{url: "{servers.http}/first"}}
  - type: tool
    name: call
    provider: http
    params:
      url: "{servers.http}/data"
      headers: {{Authorization: {{secret: api}}}}
""",
    )
    report = validate(doc, trust_document=False)
    assert not report["ok"]
    (error,) = report["errors"]
    assert error == (
        "Tool effect 'call' at 'prime.effects[1]' names secret 'api': only a document run "
        "by path may name a secret (a limited document, a library entry or a generated "
        "plan cannot)."
    )
    result = _run(doc, trusted=False)
    assert not result.ok
    assert "only a document run by path may name a secret" in (result.error or "")
    assert servers.received == []
    assert not (fake_bin / "security.log").exists()


def test_a_limited_documents_runtime_env_secret_is_refused(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        "doc.yml",
        """
runtime:
  plugins:
    awk:
      env: {TOKEN: {secret: api}}
effects:
  - type: tool
    name: noop
    provider: json
    params: {mode: parse, input: "{}"}
""",
    )
    report = validate(doc, trust_document=False)
    assert report["errors"] == [
        (
            "runtime.plugins.awk.env.TOKEN names secret 'api': only a document run by path "
            "may name a secret (a limited document, a library entry or a generated plan cannot)."
        )
    ]
    assert validate(doc, trust_document=True)["ok"]


def test_check_lists_the_secrets_a_document_names(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        "doc.yml",
        """
runtime:
  plugins:
    awk:
      env: {TOKEN: {secret: tok}}
effects:
  - type: if
    name: maybe
    if: {mode: cel, expr: "true"}
    then:
      - type: tool
        name: call
        provider: http
        params:
          url: "https://api.example.com/"
          headers: {Authorization: {secret: api, prefix: "Bearer "}, X-Other: {secret: other}}
""",
    )
    report = validate(doc, trust_document=True)
    assert report["ok"], report["errors"]
    assert report["secrets"] == ["api", "other", "tok"]

    proc = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "check", str(doc), "--skip-preflight"],
        capture_output=True,
        text=True,
        env={
            **{k: v for k, v in os.environ.items() if k not in _CREDENTIAL_ENV_VARS},
            "HOME": str(tmp_path / "home"),
        },
        cwd=tmp_path,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "Secrets: api, other, tok" in proc.stdout


@pytest.mark.parametrize(
    ("params", "message"),
    [
        (
            "params: {url: x, body: {secret: api}}",
            (
                "params.body is a {secret: ...} value, which is allowed only as a value in an "
                "http effect's params.headers"
            ),
        ),
        (
            "params: {url: x, headers: {A: [{secret: api}]}}",
            "params.headers.A[0] is a {secret: ...} value",
        ),
        (
            "params: {url: x, headers: {A: {secret: api, extra: 1}}}",
            "params.headers.A: a secret reference has unknown key(s) 'extra'",
        ),
        (
            "params: {url: x, headers: {A: {secret: 5}}}",
            "params.headers.A: a secret reference 'secret' must name a runtime.secrets entry",
        ),
        (
            "params: {url: x, headers: {A: {secret: api, prefix: 3}}}",
            "'prefix' must be a string, got int",
        ),
    ],
)
def test_check_rejects_a_secret_anywhere_but_an_http_header(
    tmp_path: Path, params: str, message: str
) -> None:
    doc = _write(
        tmp_path,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: call
    provider: http
    {params}
""",
    )
    report = validate(doc, trust_document=True)
    assert not report["ok"]
    assert message in report["errors"][0]


_INPUT_KEY = """
interface:
  inputs:
    key: {type: string, default: k-1}
effects:
"""

_SITEVERIFY_EFFECT = """
  - type: tool
    name: verify
    provider: http
    params:
      url: "https://challenges.example.com/siteverify"
      method: POST
      json: {secret: "{{input.key}}", response: "r-1"}
"""

_LOCAL_SECRET_KEY_EFFECTS = """
  - type: tool
    name: encode
    provider: json
    params:
      mode: stringify
      input: {secret: "{{input.key}}", name: db}
  - type: tool
    name: shaped
    provider: cel
    params:
      expr: '{"secret": "s"}'
      schema: {type: object, properties: {secret: {type: string}}}
"""


def test_a_mapping_with_a_secret_key_and_other_keys_is_data_and_is_rendered(
    tmp_path: Path,
) -> None:
    # Like a `{from:}` leaf, only `secret` plus an optional `prefix` is a
    # reference outside an http header: a siteverify body, a stringified
    # object or a schema with a `secret` property stays valid, as before #598.
    checked = _write(
        tmp_path, "checked.yml", _INPUT_KEY + _SITEVERIFY_EFFECT + _LOCAL_SECRET_KEY_EFFECTS
    )
    report = validate(checked, trust_document=True)
    assert report["ok"], report["errors"]
    assert report["secrets"] == []

    result = _run(_write(tmp_path, "run.yml", _INPUT_KEY + _LOCAL_SECRET_KEY_EFFECTS), secrets={})
    assert result.ok, result.error
    assert json.loads(result.state["prime"]["encode"]["value"]) == {"secret": "k-1", "name": "db"}
    assert result.state["prime"]["shaped"]["value"] == {"secret": "s"}


def test_check_rejects_a_secret_in_another_tools_params(tmp_path: Path) -> None:
    doc = _write(
        tmp_path,
        "doc.yml",
        """
effects:
  - type: tool
    name: hook
    provider: webhook
    params: {url: "https://example.com", headers: {Authorization: {secret: api}}}
""",
    )
    report = validate(doc, trust_document=True)
    assert "params.headers.Authorization is a {secret: ...} value" in report["errors"][0]


def test_a_secret_reference_from_state_or_params_json_is_never_resolved(
    tmp_path: Path, fake_bin: Path, servers: ServerPair
) -> None:
    doc = _write(
        tmp_path,
        "doc.yml",
        f"""
interface:
  inputs:
    header: {{type: object, default: {{secret: api}}}}
effects:
  - type: tool
    name: from_state
    provider: http
    on_error: continue
    params:
      url: "{servers.http}/data"
      headers: {{Authorization: {{from: input.header}}}}
  - type: tool
    name: from_params_json
    provider: http
    on_error: continue
    params: {{url: "{servers.http}/data"}}
    params_json: '{{"headers": {{"Authorization": {{"secret": "api"}}}}}}'
""",
    )
    result = _run(doc)
    assert result.ok, result.error
    for name in ("from_state", "from_params_json"):
        assert result.state["prime"][name]["meta"]["error"] == (
            f"Tool effect '{name}' params.headers.Authorization: a {{secret: ...}} value is "
            "honoured only when the document writes it in params.headers itself."
        )
    assert servers.received == []
    assert not (fake_bin / "security.log").exists()


def test_an_unknown_redirects_value_fails_before_the_request() -> None:
    with pytest.raises(ValueError) as caught:
        HttpPlugin().execute(params={"url": "http://127.0.0.1:9/", "redirects": "sometimes"})
    assert str(caught.value) == (
        "HttpPlugin: params['redirects'] must be one of 'follow', 'never', 'strip_auth', "
        "got 'sometimes'."
    )


# ---------------------------------------------------------------------------
# A subprocess tool's host env.
# ---------------------------------------------------------------------------


def test_a_secret_in_a_subprocess_tools_host_env_reaches_the_process(
    tmp_path: Path, fake_bin: Path
) -> None:
    """Acceptance 7: the process gets the value; the saved state and the
    events file never do, even though the process prints it."""
    seen = tmp_path / "seen.txt"
    fake_awk = tmp_path / "fake-awk"
    fake_awk.write_text(
        f"""#!{sys.executable}
import os
from pathlib import Path
Path({str(seen)!r}).write_text(os.environ["SERVICE_TOKEN"])
print("token=" + os.environ["SERVICE_TOKEN"])
print("auth=" + os.environ["SERVICE_AUTH"])
""",
        encoding="utf-8",
    )
    fake_awk.chmod(0o755)
    doc = _write(
        tmp_path,
        "doc.yml",
        """
effects:
  - type: tool
    name: run_it
    provider: awk
    params: {args: ["{print}"]}
""",
    )
    events = tmp_path / "events.jsonl"
    result = run(
        RunRequest(
            orchestration_path=doc,
            state_path=None,
            out_path=None,
            events_path=events,
            dry_run=False,
            validate_only=False,
            config=_config(
                _KEYCHAIN,
                plugins={
                    "awk": {
                        "binary": str(fake_awk),
                        "env": {
                            "SERVICE_TOKEN": {"secret": "api"},
                            "SERVICE_AUTH": {"secret": "api", "prefix": "Bearer "},
                            "PLAIN": "1",
                        },
                    }
                },
            ),
            skip_preflight=True,
            trust_document=True,
        )
    )
    assert result.ok, result.error
    assert seen.read_text() == TOKEN
    assert result.state["prime"]["run_it"]["value"] == (
        "token=[secret:api]\nauth=Bearer [secret:api]\n"
    )
    for text in (events.read_text(), json.dumps(result.state)):
        _assert_no_token(text)
