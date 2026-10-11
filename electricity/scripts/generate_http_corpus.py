#!/usr/bin/env python3
"""A differential corpus for lane E's `http` tool (issue #485), covering
the pure functions and small decision tables this port has to match
CPython exactly: `urllib.parse.urlencode(doseq=True/False)`,
`plugins/base.py::http_error_excerpt`, UTF-8 "replace"-mode decoding
(`bytes.decode("utf-8", "replace")`), `urllib.request.Request`'s own
header-collapse/body-coercion/URL-validation behaviour,
`HTTPRedirectHandler.redirect_request`'s method/status matrix, and
`urllib.request.getproxies_environment`/`proxy_bypass_environment`
over synthetic environments.

Section 7 below (`_end_to_end_cases`) covers what the other two layers
(`electricity-tools/tests/http_client_send.rs`'s own hand-written wire
bytes, and `tests/http_plugin.rs`'s own hand-written expected values)
can only assert against a value *this generator's own PR* typed in by
hand: a real loopback server this generator starts itself, and
`HttpPlugin.execute` run against it for real -- the empty-reason,
short-body, close-without-response, duplicate-header, non-UTF-8-body
and 307-to-a-POST shapes, each one's expected `value`/`raw`/`stderr`/
`ok` (or raised class/message) taken from CPython directly, never
written down independently of it.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, per every other `generate_*.py`). Usage:

    python3 generate_http_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from circuitry.plugins.base import http_error_excerpt  # noqa: E402
from circuitry.plugins.http import HttpPlugin  # noqa: E402

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "http_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_stdlib_tools_corpus.py's) --
# only `urlencode`'s own inputs need the full `Value` shape (`doseq`
# needs to distinguish bool/None/float/list/dict from a plain string).
# ---------------------------------------------------------------------


def encode(value: object) -> dict[str, Any]:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, bytes):
        return {"t": "bytes", "hex": value.hex()}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"cannot encode {value!r}")


# ---------------------------------------------------------------------
# 1. `urllib.parse.urlencode`
# ---------------------------------------------------------------------


def _urlencode_cases() -> list[dict[str, Any]]:
    scalars: list[Any] = [
        1,
        0,
        -5,
        True,
        False,
        None,
        1.5,
        -0.0,
        "plain",
        "hello world/slash~tilde",
        "héllo wörld",
        "réservé:/?#[]@!$&'()*+,;=",
        b"bytes val",
        "",
    ]
    cases: list[dict[str, Any]] = []
    for doseq in (False, True):
        for value in scalars:
            params = [("q", value)]
            expected = urllib.parse.urlencode(params, doseq=doseq)
            cases.append(
                {
                    "doseq": doseq,
                    "params": [["q", encode(value)]],
                    "expected": expected,
                }
            )
    # doseq-only collection shapes.
    collection_cases: list[tuple[str, Any]] = [
        ("list_of_ints", [1, 2, 3]),
        ("list_of_mixed", [True, None, 1.5, "x"]),
        ("empty_list", []),
        ("list_of_bytes", [b"a", b"b c"]),
        ("dict_value", {"x": 1, "y": 2}),
        ("empty_dict", {}),
        ("nested_list_value", [["not", "a", "pair"]]),
    ]
    for _label, value in collection_cases:
        params = [("tag", value)]
        expected = urllib.parse.urlencode(params, doseq=True)
        cases.append({"doseq": True, "params": [["tag", encode(value)]], "expected": expected})
    # Multiple keys in one call, insertion order.
    multi = [("a", 1), ("b", [1, 2]), ("a", "again")]
    cases.append(
        {
            "doseq": True,
            "params": [[k, encode(v)] for k, v in multi],
            "expected": urllib.parse.urlencode(multi, doseq=True),
        }
    )
    return cases


# ---------------------------------------------------------------------
# 2. `http_error_excerpt`
# ---------------------------------------------------------------------


def _http_error_excerpt_cases() -> list[dict[str, Any]]:
    bodies = [
        "",
        "   ",
        "plain text error",
        "a" * 600,
        "é" * 600,
        '{"error": {"message": "nested message"}}',
        '{"error": "flat error"}',
        '{"message": "message key"}',
        '{"detail": "detail key"}',
        '{"error": {"message": ""}}',  # empty nested message falls through
        '{"error": {}}',
        '{"errors": [{"msg": "graphql"}]}',
        '{"unrelated": "field"}',
        "[1, 2, 3]",
        '["a", "b"]',
        "42",
        "null",
        "true",
        '"just a json string"',
        "not json at all {",
        # Credential-shaped fields the redact() deny-list must catch
        # even inside an excerpt.
        '{"error": {"message": "token=sk-live-abcdefghijklmnopqrstuvwxyz0123456789"}}',
        '{"api_key": "sk-live-abcdefghijklmnopqrstuvwxyz0123456789", "error": "bad key"}',
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        # A long unrecognized JSON body to exercise the 500-code-point
        # slice on multi-byte text.
        json.dumps({"errors": [{"msg": "é" * 600}]}),
    ]
    return [{"body": body, "expected": http_error_excerpt(body)} for body in bodies]


# ---------------------------------------------------------------------
# 3. UTF-8 "replace"-mode decoding, and latin-1 header decoding.
# ---------------------------------------------------------------------


def _utf8_replace_cases() -> list[dict[str, Any]]:
    byte_strings: list[bytes] = [
        b"",
        b"hello",
        b"\xff\xfe\x00\x01",
        b"\xc3\xa9",  # valid 2-byte sequence (é)
        b"\xe2\x82\xac",  # valid 3-byte sequence (€)
        b"\xf0\x9f\x98\x80",  # valid 4-byte sequence (emoji)
        b"\xc3",  # truncated 2-byte sequence (maximal subpart of 1)
        b"\xe2\x82",  # truncated 3-byte sequence (maximal subpart of 2)
        b"\xf0\x9f\x98",  # truncated 4-byte sequence (maximal subpart of 3)
        b"\x80",  # stray continuation byte (invalid as a lead byte)
        b"\xc0\x80",  # overlong encoding of NUL
        b"hello\xffworld",
        b"\xed\xa0\x80",  # a UTF-16 surrogate half encoded in UTF-8 (invalid)
        b"ok\xc3\xa9\xffmore\xe2\x82",
        bytes(range(256)),
    ]
    cases = []
    for raw in byte_strings:
        expected = raw.decode("utf-8", errors="replace")
        cases.append({"hex": raw.hex(), "expected": expected})
    return cases


# ---------------------------------------------------------------------
# 4. `urllib.request.Request`'s own header-collapse/body-coercion/
#    URL-validation behaviour.
# ---------------------------------------------------------------------


def _bytes_ctor(value: Any) -> tuple[str | None, str | None, str]:
    try:
        return None, None, bytes(value).hex()
    except Exception as exc:
        return type(exc).__name__, str(exc), ""


def _bytes_coercion_cases() -> list[dict[str, Any]]:
    values: list[Any] = [
        [1, 2, 3],
        [],
        [0, 255],
        [256],
        [-1],
        5,
        0,
        -1,
        True,
        False,
        1.5,
        "not bytes",
        None,
    ]
    cases = []
    for value in values:
        error, error_message, result = _bytes_ctor(value)
        cases.append(
            {
                "value": encode(value),
                "error": error,
                "error_message": error_message,
                "result": result,
            }
        )
    return cases


def _request_header_cases() -> list[dict[str, Any]]:
    header_sets: list[list[tuple[str, str]]] = [
        [("Content-Type", "text/plain")],
        [("content-type", "text/plain"), ("Content-Type", "application/json")],
        [("X-A", "1"), ("x-a", "2")],
        [("Accept-Encoding", "gzip")],
        [("a-b-c", "x")],
        [],
    ]
    cases = []
    for headers in header_sets:
        req = urllib.request.Request("http://example.com/x", method="GET")
        for k, v in headers:
            req.add_header(k, v)
        cases.append(
            {
                "headers_in": [[k, v] for k, v in headers],
                "header_items": [[k, v] for k, v in req.header_items()],
            }
        )
    return cases


def _unknown_url_type_cases() -> list[dict[str, Any]]:
    urls = [
        "not a url",
        "http://example.com/x",
        "https://example.com/x",
        "ftp://example.com/x",
        ":::",
        "javascript:alert(1)",
        "//example.com/x",
    ]
    cases = []
    for url in urls:
        try:
            req = urllib.request.Request(url)
            req._parse()  # type: ignore[attr-defined]
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"url": url, "error": error})
    return cases


# ---------------------------------------------------------------------
# 5. `HTTPRedirectHandler.redirect_request`'s method/status matrix.
# ---------------------------------------------------------------------


def _redirect_decision_cases() -> list[dict[str, Any]]:
    methods = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"]
    statuses = [300, 301, 302, 303, 304, 305, 307, 308, 309]
    handler = urllib.request.HTTPRedirectHandler()
    cases = []
    for method in methods:
        for status in statuses:
            req = urllib.request.Request("http://example.com/start", method=method)
            try:
                new_req = handler.redirect_request(
                    req, None, status, "reason", {}, "http://example.com/next"
                )
                new_method = new_req.get_method() if new_req is not None else None
                followed = new_req is not None
            except urllib.error.HTTPError:
                new_method = None
                followed = False
            cases.append(
                {
                    "method": method,
                    "status": status,
                    "followed": followed,
                    "new_method": new_method,
                }
            )
    return cases


def _redirect_reason_cases() -> list[dict[str, Any]]:
    """The exact `HTTPError.reason` text for a disallowed-scheme
    redirect and for each of the two loop limits
    (`HTTPRedirectHandler.http_error_302`'s own `inf_msg`/
    `"%s - Redirection to url '%s' is not allowed"`) -- `src/http.rs`
    builds the same two format strings directly (it never raises
    through a real `HTTPRedirectHandler`), so this section pins them
    against CPython rather than trusting the issue's own prose.
    """
    handler = urllib.request.HTTPRedirectHandler()
    cases = []

    req = urllib.request.Request("http://example.com/start", method="GET")
    try:
        handler.http_error_302(req, None, 302, "Found", {"location": "javascript:alert(1)"})
        reason = None
    except urllib.error.HTTPError as exc:
        reason = exc.reason
    cases.append({"case": "disallowed_scheme", "reason": reason})

    req = urllib.request.Request("http://example.com/start", method="GET")
    req.redirect_dict = {"http://example.com/next": handler.max_repeats}  # type: ignore[attr-defined]
    try:
        handler.http_error_302(req, None, 302, "Found", {"location": "http://example.com/next"})
        reason = None
    except urllib.error.HTTPError as exc:
        reason = exc.reason
    cases.append({"case": "max_repeats", "reason": reason})

    req = urllib.request.Request("http://example.com/start", method="GET")
    req.redirect_dict = {  # type: ignore[attr-defined]
        f"http://example.com/{i}": 1 for i in range(handler.max_redirections)
    }
    try:
        handler.http_error_302(
            req, None, 302, "Found", {"location": "http://example.com/new-url"}
        )
        reason = None
    except urllib.error.HTTPError as exc:
        reason = exc.reason
    cases.append({"case": "max_redirections", "reason": reason})

    return cases


# ---------------------------------------------------------------------
# 6. `getproxies_environment`/`proxy_bypass_environment`.
# ---------------------------------------------------------------------


def _proxy_env_cases() -> list[dict[str, Any]]:
    import os
    from unittest import mock

    environments: list[list[tuple[str, str]]] = [
        [("http_proxy", "http://proxy:3128")],
        [("HTTP_PROXY", "http://upper:1"), ("http_proxy", "http://lower:2")],
        [("HTTP_PROXY", "http://upper:1"), ("REQUEST_METHOD", "GET")],
        [
            ("HTTP_PROXY", "http://upper:1"),
            ("http_proxy", "http://lower:2"),
            ("REQUEST_METHOD", "GET"),
        ],
        [("HTTPS_PROXY", "http://upper:1"), ("https_proxy", "")],
        [("http_proxy", "http://proxy:3128"), ("no_proxy", "example.com")],
        [("http_proxy", "http://proxy:3128"), ("no_proxy", "*")],
        [("http_proxy", "http://proxy:3128"), ("no_proxy", ".example.com")],
        [("http_proxy", "http://proxy:3128"), ("no_proxy", "example.com:8080")],
        [],
    ]
    bypass_hosts = ["example.com", "api.example.com", "example.com:8080", "other.com", "notexample.com"]
    cases = []
    for env_pairs in environments:
        env = dict(env_pairs)
        with mock.patch.dict(os.environ, env, clear=True):
            proxies = urllib.request.getproxies_environment()
            bypass_results = {
                host: urllib.request.proxy_bypass_environment(host) for host in bypass_hosts
            }
        cases.append(
            {
                "env": [[k, v] for k, v in env_pairs],
                "proxies": dict(sorted(proxies.items())),
                "bypass": bypass_results,
            }
        )
    return cases


# ---------------------------------------------------------------------
# 7. `HttpPlugin.execute` end to end against a real loopback server --
#    the shapes the other two layers can only assert against a
#    hand-written expected value (this generator's own PR, not
#    CPython): an empty reason phrase, a short body, a close without
#    response, duplicate response headers, a non-UTF-8 body, a 307 to
#    a POST, a body read timeout, and the `Content-Type`
#    user-header/`json`-body `setdefault` collision.
# ---------------------------------------------------------------------


def _serve_once(responder: Any) -> tuple[int, Any]:
    import socket
    import threading

    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    sock.listen(2)
    port = sock.getsockname()[1]

    def serve() -> None:
        try:
            responder(sock, port)
        finally:
            sock.close()

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return port, thread


def _read_request(conn: Any) -> bytes:
    """Reads one full request: headers, then exactly `Content-Length`
    more bytes when present -- `http.client` sends the body as a
    separate `socket.send()` call right after the headers, which can
    land in a later `recv()` than the one that completes the header
    terminator; stopping as soon as `\r\n\r\n` appears (without this
    second read) intermittently drops the body (a real race observed
    directly: identical requests, sometimes missing their own body).
    """
    conn.settimeout(3)
    buf = b""
    try:
        while b"\r\n\r\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                break
            buf += chunk
        header_end = buf.find(b"\r\n\r\n")
        if header_end != -1:
            header_text = buf[:header_end].decode("latin-1", errors="replace")
            content_length = 0
            for line in header_text.split("\r\n")[1:]:
                name, _, value = line.partition(":")
                if name.strip().lower() == "content-length":
                    content_length = int(value.strip())
                    break
            body_start = header_end + 4
            while len(buf) - body_start < content_length:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                buf += chunk
    except OSError:
        pass
    return buf


def _respond_steps(steps: list[tuple[bytes, float]]) -> Any:
    """Writes *steps* in order against the one connection this case's
    own server accepts: each `(chunk, delay_before_seconds)` pair
    sleeps first, when asked, then writes *chunk* -- an empty *chunk*
    closes the connection without writing anything (`close_without_
    response`'s own shape) rather than writing a zero-length `send`.
    Recorded verbatim in the case's own `response_steps` (hex plus the
    delay in milliseconds) so the Rust replay drives an identical
    server from the same steps, never a hand-copied duplicate of this
    function's own bytes.
    """
    import time

    def responder(sock: Any, port: int) -> None:
        conn, _addr = sock.accept()
        _read_request(conn)
        for chunk, delay in steps:
            if delay:
                time.sleep(delay)
            if not chunk:
                conn.close()
                return
            try:
                conn.sendall(chunk)
            except OSError:
                pass
        conn.close()

    return responder


def _normalize_port(case: dict[str, Any], port: int) -> dict[str, Any]:
    """Replaces this case's own ephemeral loopback port with a fixed
    placeholder everywhere it appears in `raw.url`/`raw.headers.host`,
    so the committed corpus never depends on which port a given run
    happened to get.
    """
    raw = case.get("raw")
    if isinstance(raw, dict):
        if isinstance(raw.get("url"), str):
            raw["url"] = raw["url"].replace(f"127.0.0.1:{port}", "127.0.0.1:__PORT__")
        headers = raw.get("headers")
        if isinstance(headers, dict) and isinstance(headers.get("host"), str):
            headers["host"] = headers["host"].replace(f"127.0.0.1:{port}", "127.0.0.1:__PORT__")
    return case


def _run_end_to_end_case(
    name: str,
    steps: list[tuple[bytes, float]],
    params: dict[str, Any] | None = None,
    timeout_seconds: int = 1,
) -> dict[str, Any]:
    port, thread = _serve_once(_respond_steps(steps))
    url = f"http://127.0.0.1:{port}/"
    plugin = HttpPlugin()
    try:
        result = plugin.execute(
            params={"url": url, **(params or {})}, timeout_seconds=timeout_seconds
        )
        case = {
            "case": name,
            "raised": None,
            "message": None,
            "value": result.value,
            "raw": result.raw,
            "stderr": result.stderr,
            "ok": result.ok,
        }
    except Exception as exc:
        case = {
            "case": name,
            "raised": type(exc).__name__,
            "message": str(exc),
            "value": None,
            "raw": None,
            "stderr": None,
            "ok": None,
        }
    thread.join(timeout=5)
    case = _normalize_port(case, port)
    case["response_steps"] = [[chunk.hex(), delay] for chunk, delay in steps]
    case["params"] = params or {}
    case["timeout_seconds"] = timeout_seconds
    return case


def _end_to_end_cases() -> list[dict[str, Any]]:
    cases = [
        _run_end_to_end_case(
            "empty_reason_phrase",
            [(b"HTTP/1.1 404 \r\nContent-Length: 0\r\nConnection: close\r\n\r\n", 0.0)],
        ),
        _run_end_to_end_case(
            "short_body_is_an_incomplete_read",
            [
                (
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345",
                    0.0,
                )
            ],
        ),
        _run_end_to_end_case("close_without_response", [(b"", 0.0)]),
        _run_end_to_end_case(
            "duplicate_response_headers",
            [
                (
                    (
                        b"HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nSet-Cookie: a=2\r\n"
                        b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    0.0,
                )
            ],
        ),
        _run_end_to_end_case(
            "non_utf8_body",
            [
                (
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe",
                    0.0,
                )
            ],
        ),
        _run_end_to_end_case(
            "a_307_to_a_post_is_a_success_with_stderr_set",
            [
                (
                    (
                        b"HTTP/1.1 307 Temporary Redirect\r\n"
                        b"Location: http://example.invalid/next\r\n"
                        b"Content-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    0.0,
                )
            ],
            params={"method": "POST"},
        ),
        _run_end_to_end_case(
            "a_body_read_timeout",
            [
                (
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n12345",
                    0.0,
                ),
                (b"67890", 2.0),
            ],
            timeout_seconds=1,
        ),
    ]

    # The `Content-Type` user-header/`json`-body `setdefault` collision
    # (tools-adapters-plugins's own worked example, issue #485): a
    # user `content-type` header survives as a *separate* dict key
    # until `Request.add_header`'s own capitalize-collapse unifies it
    # with the `json` body's `setdefault("Content-Type", ...)` --
    # recorded here as the raw wire bytes, not just `raw.headers`,
    # since the position (the user's own header came first) matters
    # too.
    request_bytes: list[bytes] = []

    def collision_responder(sock: Any, port: int) -> None:
        conn, _addr = sock.accept()
        request_bytes.append(_read_request(conn))
        try:
            conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        except OSError:
            pass
        conn.close()

    port, thread = _serve_once(collision_responder)
    plugin = HttpPlugin()
    plugin.execute(
        params={
            "url": f"http://127.0.0.1:{port}/",
            "method": "POST",
            "headers": {"content-type": "text/plain"},
            "json": {"a": 1},
        },
        timeout_seconds=1,
    )
    thread.join(timeout=5)
    sent = request_bytes[0].decode("latin-1")
    sent = sent.replace(f"127.0.0.1:{port}", "127.0.0.1:__PORT__")
    header_lines = sent.split("\r\n")[1:]
    header_lines = [line for line in header_lines if line]
    cases.append(
        {
            "case": "content_type_json_setdefault_collision",
            "raised": None,
            "message": None,
            "value": None,
            "raw": None,
            "stderr": None,
            "ok": None,
            "request_header_lines": header_lines,
        }
    )
    return cases


def build_corpus() -> dict[str, Any]:
    return {
        "urlencode_cases": _urlencode_cases(),
        "http_error_excerpt_cases": _http_error_excerpt_cases(),
        "utf8_replace_cases": _utf8_replace_cases(),
        "bytes_coercion_cases": _bytes_coercion_cases(),
        "request_header_cases": _request_header_cases(),
        "unknown_url_type_cases": _unknown_url_type_cases(),
        "redirect_decision_cases": _redirect_decision_cases(),
        "redirect_reason_cases": _redirect_reason_cases(),
        "proxy_env_cases": _proxy_env_cases(),
        "end_to_end_cases": _end_to_end_cases(),
    }


def main() -> int:
    data = (json.dumps(build_corpus(), indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_bytes() if OUTPUT.exists() else b""
        if current != data:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_bytes(data)
    print(f"wrote corpus to {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
