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

Deliberately not generated here (issue #485's own test plan names
these too, but every one is already pinned directly against the real
client in `electricity-tools/tests/http_client_send.rs` and
`tests/http_plugin.rs`, and cross-engine in the conformance suite's
own `c-http-*` cases): `HttpPlugin.execute` end to end against a real
`http.server` (non-UTF-8 bodies, duplicate response headers, an empty
reason phrase, a 307 to a POST, a body read timeout, a close without
response, a short body). Those need a live loopback server at
generation time, which this generator -- run by `--check` in plain CI,
with no case-by-case harness of its own -- doesn't have a seam for;
the two other layers already cover every one of those shapes
differentially.

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
    cases = []
    for body in bodies:
        cases.append({"body": body, "expected": http_error_excerpt(body)})
    return cases


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


def _bytes_ctor(value: Any) -> tuple[str | None, str]:
    try:
        return None, bytes(value).hex()
    except Exception as exc:  # noqa: BLE001
        return type(exc).__name__, str(exc)


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
        error, result = _bytes_ctor(value)
        cases.append({"value": encode(value), "error": error, "result": result})
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


def build_corpus() -> dict[str, Any]:
    return {
        "urlencode_cases": _urlencode_cases(),
        "http_error_excerpt_cases": _http_error_excerpt_cases(),
        "utf8_replace_cases": _utf8_replace_cases(),
        "bytes_coercion_cases": _bytes_coercion_cases(),
        "request_header_cases": _request_header_cases(),
        "unknown_url_type_cases": _unknown_url_type_cases(),
        "redirect_decision_cases": _redirect_decision_cases(),
        "proxy_env_cases": _proxy_env_cases(),
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
